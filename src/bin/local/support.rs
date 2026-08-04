use std::{
    env, fs,
    io::Read,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::Serialize;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

use crate::local::{
    companion::establish_reverse_rpc,
    ssh::{OwnedMaster, SshClient},
};

pub(crate) struct RuntimeDirectory {
    path: PathBuf,
}

impl RuntimeDirectory {
    pub(crate) fn create(prefix: &str) -> Result<Self, String> {
        // macOS commonly exposes a very long per-user TMPDIR. OpenSSH creates
        // a temporary sibling of ControlPath before renaming it, and that
        // suffix can exceed the Unix-domain socket path limit. A random 0700
        // directory directly under /tmp is both private and predictably short.
        #[cfg(unix)]
        let base = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = env::temp_dir();
        for _ in 0..20 {
            let path = base.join(format!("{prefix}{}", secure_random_hex(12)?));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("failed to create runtime directory: {error}"));
                }
            }
        }
        Err("failed to allocate a unique runtime directory after 20 attempts".into())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RuntimeDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "warning: failed to remove runtime directory {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}

pub(crate) fn state_directory() -> Result<PathBuf, String> {
    let base = match env::var_os("XDG_RUNTIME_DIR") {
        Some(directory) => PathBuf::from(directory),
        None => PathBuf::from(env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?)
            .join(".cache"),
    };
    let directory = base.join("herdr-fwd");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("failed to create state directory: {error}"))?;
    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(&directory)
            .map_err(|error| format!("failed to inspect state directory: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("state path must be a real directory".into());
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err("state directory is not owned by the current user".into());
        }
        let mode = metadata.permissions().mode() & 0o777;
        // Sandboxed callers may be allowed to read an already-secure runtime
        // directory but not chmod it. Avoid an unnecessary metadata write.
        if mode != 0o700 {
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("failed to secure state directory: {error}"))?;
        }
    }
    Ok(directory)
}

pub(crate) fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    herdr_fwd::atomic::write_file(path, &bytes, 0o600)
}

pub(crate) fn secure_random_hex(bytes: usize) -> Result<String, String> {
    Ok(secure_random_bytes(bytes)?
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(crate) fn secure_random_bytes(bytes: usize) -> Result<Vec<u8>, String> {
    let mut random = vec![0; bytes];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .map_err(|error| format!("failed to read OS randomness: {error}"))?;
    Ok(random)
}

pub(crate) fn debug_log(message: &str) {
    if env::var("HERDR_FWD_LOG").as_deref() == Ok("debug") {
        eprintln!("hfwd: {message}");
    }
}

pub(crate) fn open_browser(url: &str) -> Result<(), String> {
    let launcher = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(launcher)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("failed to start {launcher}: {error}"))
}

pub(crate) fn require_ssh() -> Result<(), String> {
    let status = Command::new("ssh")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "required command not found: ssh".to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("required command is not usable: ssh".into())
    }
}

pub(crate) fn require_local_herdr_compatibility() -> Result<(), String> {
    require_herdr_compatibility(&local_herdr_version()?, "local")
}

pub(crate) fn doctor(target: Option<&String>) -> Result<(), String> {
    require_ssh()?;
    let local_version = local_herdr_version()?;
    require_herdr_compatibility(&local_version, "local")?;
    println!(
        "✓ OpenSSH available\n✓ Local Herdr compatible ({})",
        local_version.trim()
    );
    if let Some(target) = target {
        let temporary = RuntimeDirectory::create("herdr-fwd-doctor-")?;
        let client = SshClient {
            target: target.clone(),
            control_path: temporary.path().join("c"),
        };
        let master = OwnedMaster::start(client.clone())
            .map_err(|error| format!("SSH target is not reachable: {target}: {error}"))?;

        println!("✓ Private SSH ControlMaster operational: {target}");

        let probe_listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("failed to bind RPC probe: {error}"))?;
        let probe_port = probe_listener
            .local_addr()
            .map_err(|error| format!("failed to inspect RPC probe: {error}"))?
            .port();
        let _remote_probe = establish_reverse_rpc(&client, probe_port)?;
        println!("✓ Loopback reverse RPC forwarding supported");

        let remote = client.remote_command(
            "export PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:$PATH\"; herdr --version; herdr plugin list --plugin herdr.fwd --json",
        )?;
        let (remote_version, plugin_json) = remote
            .split_once('\n')
            .ok_or_else(|| "remote Herdr did not return plugin status".to_string())?;
        require_herdr_compatibility(remote_version, "remote")?;
        let plugin_json: serde_json::Value = serde_json::from_str(plugin_json)
            .map_err(|error| format!("invalid remote plugin status: {error}"))?;
        let Some((enabled, version)) = plugin_status(&plugin_json) else {
            return Err("remote plugin herdr.fwd is not installed".into());
        };
        if !enabled {
            return Err("remote plugin herdr.fwd is not enabled".into());
        }
        if version != env!("CARGO_PKG_VERSION") {
            return Err(format!(
                "remote plugin version {version} does not match local wrapper {}; run remote update after disconnecting active sessions",
                env!("CARGO_PKG_VERSION")
            ));
        }
        println!(
            "✓ Remote Herdr compatible ({})\n✓ Remote plugin installed and enabled",
            remote_version.trim()
        );
        drop(probe_listener);
        drop(master);
    }
    Ok(())
}

fn local_herdr_version() -> Result<String, String> {
    let output = Command::new("herdr")
        .arg("--version")
        .output()
        .map_err(|error| format!("failed to inspect local Herdr: {error}"))?;
    if !output.status.success() {
        return Err("failed to inspect local Herdr version".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) fn plugin_status(value: &serde_json::Value) -> Option<(bool, String)> {
    match value {
        serde_json::Value::Object(object) => {
            if object.get("plugin_id").and_then(serde_json::Value::as_str) == Some("herdr.fwd") {
                return Some((
                    object
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    object
                        .get("version")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                ));
            }
            object.values().find_map(plugin_status)
        }
        serde_json::Value::Array(values) => values.iter().find_map(plugin_status),
        _ => None,
    }
}

pub(crate) fn require_herdr_compatibility(
    version_output: &str,
    location: &str,
) -> Result<(), String> {
    let version = version_output
        .split_whitespace()
        .find_map(parse_version)
        .ok_or_else(|| format!("could not parse {location} Herdr version: {version_output}"))?;
    if !((0, 8, 0)..(0, 9, 0)).contains(&version) {
        Err(format!(
            "{location} Herdr {}.{}.{} is unsupported; this release supports >=0.8.0 and <0.9.0",
            version.0, version.1, version.2
        ))
    } else {
        Ok(())
    }
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.trim_start_matches('v');
    let mut components = value.split('.');
    let major = components.next()?.parse().ok()?;
    let minor = components.next()?.parse().ok()?;
    let patch = components
        .next()?
        .split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod support_tests {
    use super::{
        parse_version, plugin_status, require_herdr_compatibility, secure_random_hex,
        RuntimeDirectory,
    };

    #[test]
    fn generated_tokens_are_long_and_distinct() {
        let first = secure_random_hex(32).unwrap();
        let second = secure_random_hex(32).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(second.len(), 64);
        assert_ne!(first, second);
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn checks_herdr_semantic_version() {
        assert_eq!(parse_version("v0.7.5"), Some((0, 7, 5)));
        assert_eq!(parse_version("0.8.0-beta.1"), Some((0, 8, 0)));
        assert!(require_herdr_compatibility("herdr 0.7.5", "test").is_err());
        assert!(require_herdr_compatibility("herdr 0.7.4", "test").is_err());
        assert!(require_herdr_compatibility("herdr 0.8.0", "test").is_ok());
        assert!(require_herdr_compatibility("herdr 0.9.0", "test").is_err());
    }

    #[test]
    fn reads_exact_remote_plugin_status() {
        let status = serde_json::json!({
            "result": {"plugins": [{
                "plugin_id": "herdr.fwd",
                "enabled": true,
                "version": "0.1.0"
            }]}
        });
        assert_eq!(plugin_status(&status), Some((true, "0.1.0".into())));
        assert_eq!(plugin_status(&serde_json::json!({"plugins": []})), None);
    }

    #[test]
    fn runtime_directory_is_private_and_removed_on_drop() {
        let path = {
            let directory = RuntimeDirectory::create("herdr-fwd-test-").unwrap();
            let path = directory.path().to_path_buf();
            assert!(path.is_dir());
            assert!(path.join("c").as_os_str().len() < 80);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            path
        };
        assert!(!path.exists());
    }
}
