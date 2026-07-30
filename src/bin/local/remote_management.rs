use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::local::{
    ssh::SshClient,
    support::{plugin_status, secure_random_hex},
};
use serde::{Deserialize, Serialize};

use crate::local::support::require_ssh;

const REMOTE_PLUGIN_ID: &str = "herdr.fwd";
const REMOTE_PLUGIN_SOURCE: &str = "go-min/herdr-fwd";

struct ReleaseCachePaths {
    binary: PathBuf,
    checksum: PathBuf,
}

fn release_cache_paths(base: &Path, version: &str, platform: &str) -> ReleaseCachePaths {
    let directory = base
        .join("releases")
        .join(format!("v{version}"))
        .join(platform);
    ReleaseCachePaths {
        binary: directory.join("herdr-fwd-plugin"),
        checksum: directory.join("SHA256"),
    }
}

pub(crate) fn manage_remote(arguments: &[String]) -> Result<(), String> {
    let operation = arguments
        .first()
        .map(String::as_str)
        .ok_or_else(remote_management_usage)?;
    let target = arguments.get(1).ok_or_else(remote_management_usage)?;
    if arguments.len() != 2 {
        return Err(remote_management_usage());
    }
    validate_ssh_target(target)?;
    require_ssh()?;

    let command = match operation {
        "install" | "update" => install_remote_plugin_command(operation == "update"),
        "status" => remote_prelude(&format!(
            "herdr plugin list --plugin {REMOTE_PLUGIN_ID} --json"
        )),
        "uninstall" => remote_prelude(&format!(
            "{}; herdr plugin disable {REMOTE_PLUGIN_ID} >/dev/null 2>&1 || true; \
             herdr plugin uninstall {REMOTE_PLUGIN_ID}",
            reject_active_sessions_command()
        )),
        _ => return Err(remote_management_usage()),
    };

    let current = matches!(operation, "install" | "update")
        .then(|| remote_plugin_status_for_target(target))
        .transpose()?
        .flatten();
    let previous_origin = current
        .is_some()
        .then(|| remote_plugin_origin_for_target(target))
        .transpose()?
        .flatten();
    let output = remote_command_for_target(target, &command)?;
    if operation == "status" {
        print!("{output}");
    }
    if matches!(operation, "install" | "update") {
        let status = remote_plugin_status_for_target(target)?;
        verify_plugin_status(status, target)?;
        if should_persist_remote_origin(current.is_none(), previous_origin.as_ref()) {
            let root = remote_plugin_root_for_target(target)?;
            remote_command_for_target(target, &persist_remote_origin_command(&root))?;
        }
    }
    Ok(())
}

/// Ensures the server that will host the plugin has the matching managed
/// plugin before attaching. The caller's private ControlMaster is reused, so
/// this preflight does not create another SSH authentication exchange.
pub(crate) fn ensure_remote_plugin(client: &SshClient) -> Result<(), String> {
    let current = remote_plugin_status(client)?;
    if matches!(current, Some((true, ref version)) if version == env!("CARGO_PKG_VERSION")) {
        return Ok(());
    }

    println!("Preparing remote port forwarding on {}…", client.target);
    let created_by_hfwd = current.is_none();
    let previous_origin = (!created_by_hfwd)
        .then(|| remote_plugin_origin(client))
        .transpose()?
        .flatten();
    if let Err(remote_error) =
        client.remote_command(&install_remote_plugin_command(current.is_some()))
    {
        let platform = remote_platform(client)?;
        let bundle = release_bundle(&platform).map_err(|local_error| format!(
            "remote plugin install failed ({remote_error}); local verified release fallback is unavailable: {local_error}"
        ))?;
        deploy_release_bundle(client, &bundle, created_by_hfwd)?;
    }
    verify_remote_plugin(client)?;
    if should_persist_remote_origin(created_by_hfwd, previous_origin.as_ref()) {
        persist_remote_origin(client, &remote_plugin_root(client)?)?;
    }
    Ok(())
}

fn remote_platform(client: &SshClient) -> Result<String, String> {
    let output = client.remote_command(&remote_prelude("uname -s; uname -m"))?;
    let mut lines = output.lines();
    let os = match lines.next().map(str::trim) {
        Some("Darwin") => "macos",
        Some("Linux") => "linux",
        Some(value) => return Err(format!("unsupported remote platform: {value}")),
        None => return Err("remote platform check returned no operating system".into()),
    };
    let architecture = match lines.next().map(str::trim) {
        Some("x86_64" | "amd64") => "x86_64",
        Some("arm64" | "aarch64") => "aarch64",
        Some(value) => return Err(format!("unsupported remote architecture: {value}")),
        None => return Err("remote platform check returned no architecture".into()),
    };
    Ok(format!("{os}-{architecture}"))
}

fn verify_remote_plugin(client: &SshClient) -> Result<(), String> {
    verify_plugin_status(remote_plugin_status(client)?, &client.target)
}

fn verify_plugin_status(status: Option<(bool, String)>, target: &str) -> Result<(), String> {
    match status {
        Some((true, version)) if version == env!("CARGO_PKG_VERSION") => Ok(()),
        Some((enabled, version)) => Err(format!(
            "remote plugin setup on {} did not produce the required enabled version {} (enabled={enabled}, version={version})",
            target,
            env!("CARGO_PKG_VERSION")
        )),
        None => Err(format!(
            "remote plugin setup on {} completed but herdr.fwd is not registered",
            target
        )),
    }
}

struct ReleaseBundle {
    manifest: Vec<u8>,
    binary: Vec<u8>,
}

fn release_bundle(platform: &str) -> Result<ReleaseBundle, String> {
    let paths = release_cache_paths(&release_cache_base()?, env!("CARGO_PKG_VERSION"), platform);
    let binary = download_release_binary(platform, &paths).or_else(|download_error| {
        verified_cached_binary(&paths)
            .map_err(|cache_error| format!("{download_error}; offline cache: {cache_error}"))
    })?;
    let manifest = deployment_manifest()?.into_bytes();
    Ok(ReleaseBundle { manifest, binary })
}

fn release_cache_base() -> Result<PathBuf, String> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|base| base.join("herdr-fwd"))
        .ok_or_else(|| "HOME is not set".into())
}

fn verified_cached_binary(paths: &ReleaseCachePaths) -> Result<Vec<u8>, String> {
    let binary =
        fs::read(&paths.binary).map_err(|error| format!("{}: {error}", paths.binary.display()))?;
    let expected = fs::read_to_string(&paths.checksum)
        .map_err(|error| format!("{}: {error}", paths.checksum.display()))?;
    verify_sha256(&binary, expected.trim())?;
    Ok(binary)
}

fn download_release_binary(platform: &str, paths: &ReleaseCachePaths) -> Result<Vec<u8>, String> {
    let temporary = crate::local::support::RuntimeDirectory::create("herdr-fwd-release-")?;
    let asset = format!("herdr-fwd-{platform}.tar.gz");
    let archive = temporary.path().join(&asset);
    let sums = temporary.path().join("SHA256SUMS");
    let base = format!(
        "https://github.com/{REMOTE_PLUGIN_SOURCE}/releases/download/v{}",
        env!("CARGO_PKG_VERSION")
    );
    download_file(&format!("{base}/{asset}"), &archive)?;
    download_file(&format!("{base}/SHA256SUMS"), &sums)?;
    let expected = checksum_for(
        &fs::read_to_string(&sums).map_err(|error| error.to_string())?,
        &asset,
    )?;
    verify_sha256(
        &fs::read(&archive).map_err(|error| error.to_string())?,
        &expected,
    )?;
    let archive_string = archive.display().to_string();
    let output = Command::new("tar")
        .args(["-xOzf", &archive_string, "./herdr-fwd-plugin"])
        .output()
        .map_err(|error| format!("failed to extract release plugin: {error}"))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err("release archive does not contain herdr-fwd-plugin".into());
    }
    let parent = paths
        .binary
        .parent()
        .ok_or_else(|| "invalid release cache path".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::write(&paths.binary, &output.stdout).map_err(|error| error.to_string())?;
    fs::write(&paths.checksum, format!("{expected}\n")).map_err(|error| error.to_string())?;
    Ok(output.stdout)
}

fn download_file(url: &str, destination: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "15", "--retry", "2", "-o"])
        .arg(destination)
        .arg(url)
        .status()
        .map_err(|error| format!("failed to start curl: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("failed to download release asset from {url}"))
    }
}

fn checksum_for(sums: &str, asset: &str) -> Result<String, String> {
    sums.lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let checksum = fields.next()?;
            (fields.next()?.trim_start_matches('*') == asset).then(|| checksum.to_string())
        })
        .ok_or_else(|| format!("release checksum is missing for {asset}"))
}

fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), String> {
    let temporary = crate::local::support::RuntimeDirectory::create("herdr-fwd-sha-")?;
    let input = temporary.path().join("input");
    fs::write(&input, bytes).map_err(|error| error.to_string())?;
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(&input)
        .output()
        .or_else(|_| Command::new("sha256sum").arg(&input).output())
        .map_err(|error| format!("SHA-256 verifier unavailable: {error}"))?;
    let actual = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    if output.status.success() && actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err("release SHA-256 verification failed".into())
    }
}

fn deployment_manifest() -> Result<String, String> {
    let mut manifest = include_str!("../../../herdr-plugin.toml")
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("invalid bundled plugin manifest: {error}"))?;
    manifest.remove("build");
    Ok(manifest.to_string())
}

fn deploy_release_bundle(
    client: &SshClient,
    bundle: &ReleaseBundle,
    _created_by_hfwd: bool,
) -> Result<(), String> {
    let deployment_id = secure_random_hex(12)?;
    let prepare = local_bundle_command(
        &deployment_id,
        "install -d -m 700 \"$stage/target/release\"",
    );
    client.remote_command(&prepare)?;
    client.remote_command_with_stdin(
        &local_bundle_command(&deployment_id, "cat > \"$stage/herdr-plugin.toml\""),
        &bundle.manifest,
    )?;
    client.remote_command_with_stdin(
        &local_bundle_command(
            &deployment_id,
            "cat > \"$stage/target/release/herdr-fwd-plugin\"",
        ),
        &bundle.binary,
    )?;
    let activate = format!(
        "chmod 755 \"$stage/target/release/herdr-fwd-plugin\"; current=\"$root/current\"; previous=\"$root/.previous-{deployment_id}\"; if [ -e \"$current\" ]; then mv \"$current\" \"$previous\"; fi; mv \"$stage\" \"$current\"; if ! herdr plugin link \"$current\" --enabled; then rm -rf \"$current\"; if [ -e \"$previous\" ]; then mv \"$previous\" \"$current\"; herdr plugin link \"$current\" --enabled >/dev/null || true; fi; exit 1; fi"
    );
    client.remote_command(&local_bundle_command(&deployment_id, &activate))?;
    if let Err(error) = verify_remote_plugin(client) {
        let rollback = format!(
            "current=\"$root/current\"; previous=\"$root/.previous-{deployment_id}\"; rm -rf \"$current\"; if [ -e \"$previous\" ]; then mv \"$previous\" \"$current\"; herdr plugin link \"$current\" --enabled >/dev/null || true; fi"
        );
        client.remote_command(&local_bundle_command(&deployment_id, &rollback))?;
        return Err(error);
    }
    client.remote_command(&local_bundle_command(
        &deployment_id,
        &format!("rm -rf \"$root/.previous-{deployment_id}\""),
    ))?;
    Ok(())
}

fn local_bundle_command(deployment_id: &str, command: &str) -> String {
    remote_prelude(&format!(
        "root=\"${{XDG_DATA_HOME:-$HOME/.local/share}}/herdr-fwd/plugins\"; stage=\"$root/.staging-{deployment_id}\"; install -d -m 700 \"$root\"; {command}"
    ))
}

fn remote_plugin_status(client: &SshClient) -> Result<Option<(bool, String)>, String> {
    let status = client.remote_command(&remote_prelude(&format!(
        "herdr plugin list --plugin {REMOTE_PLUGIN_ID} --json"
    )))?;
    let value = serde_json::from_str::<serde_json::Value>(&status)
        .map_err(|error| format!("invalid remote plugin status: {error}"))?;
    Ok(plugin_status(&value))
}

fn remote_plugin_root(client: &SshClient) -> Result<String, String> {
    let status = client.remote_command(&remote_prelude(&format!(
        "herdr plugin list --plugin {REMOTE_PLUGIN_ID} --json"
    )))?;
    let value = serde_json::from_str::<serde_json::Value>(&status)
        .map_err(|error| format!("invalid remote plugin status: {error}"))?;
    plugin_root(&value).ok_or_else(|| "remote plugin status has no plugin_root".into())
}

fn remote_plugin_status_for_target(target: &str) -> Result<Option<(bool, String)>, String> {
    let status = remote_command_for_target(
        target,
        &remote_prelude(&format!(
            "herdr plugin list --plugin {REMOTE_PLUGIN_ID} --json"
        )),
    )?;
    let value = serde_json::from_str::<serde_json::Value>(&status)
        .map_err(|error| format!("invalid remote plugin status: {error}"))?;
    Ok(plugin_status(&value))
}

fn remote_plugin_root_for_target(target: &str) -> Result<String, String> {
    let status = remote_command_for_target(
        target,
        &remote_prelude(&format!(
            "herdr plugin list --plugin {REMOTE_PLUGIN_ID} --json"
        )),
    )?;
    let value = serde_json::from_str::<serde_json::Value>(&status)
        .map_err(|error| format!("invalid remote plugin status: {error}"))?;
    plugin_root(&value).ok_or_else(|| "remote plugin status has no plugin_root".into())
}

fn remote_command_for_target(target: &str, command: &str) -> Result<String, String> {
    let output = Command::new("ssh")
        .arg(target)
        .arg(command)
        .output()
        .map_err(|error| format!("failed to start ssh: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn plugin_root(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(object)
            if object.get("plugin_id").and_then(serde_json::Value::as_str)
                == Some(REMOTE_PLUGIN_ID) =>
        {
            object
                .get("plugin_root")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        }
        serde_json::Value::Object(object) => object.values().find_map(plugin_root),
        serde_json::Value::Array(values) => values.iter().find_map(plugin_root),
        _ => None,
    }
}

fn persist_remote_origin(client: &SshClient, plugin_root: &str) -> Result<(), String> {
    client
        .remote_command(&persist_remote_origin_command(plugin_root))
        .map(|_| ())
}

fn persist_remote_origin_command(plugin_root: &str) -> String {
    let contents = herdr_fwd::shell::quote(&remote_origin_contents(plugin_root));
    remote_prelude(&format!(
        "state=\"${{XDG_STATE_HOME:-$HOME/.local/state}}/herdr-fwd\"; install -d -m 700 \"$state\"; temporary=\"$state/.plugin-origin.toml.$$\"; umask 077; printf '%s' {contents} > \"$temporary\"; mv -f \"$temporary\" \"$state/plugin-origin.toml\""
    ))
}

#[derive(Deserialize, Serialize)]
struct RemotePluginOrigin {
    origin: String,
    plugin_root: String,
    version: String,
}

fn remote_origin_contents(plugin_root: &str) -> String {
    toml_edit::ser::to_string_pretty(&RemotePluginOrigin {
        origin: "hfwd_remote".into(),
        plugin_root: plugin_root.into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
    .expect("remote plugin origin serialization must succeed")
}

fn remote_plugin_origin(client: &SshClient) -> Result<Option<RemotePluginOrigin>, String> {
    let contents = client.remote_command(&remote_plugin_origin_command())?;
    Ok(toml_edit::de::from_str(&contents).ok())
}

fn remote_plugin_origin_for_target(target: &str) -> Result<Option<RemotePluginOrigin>, String> {
    let contents = remote_command_for_target(target, &remote_plugin_origin_command())?;
    Ok(toml_edit::de::from_str(&contents).ok())
}

fn remote_plugin_origin_command() -> String {
    remote_prelude(
        "origin=\"${XDG_STATE_HOME:-$HOME/.local/state}/herdr-fwd/plugin-origin.toml\"; if [ -f \"$origin\" ]; then cat \"$origin\"; fi",
    )
}

fn should_persist_remote_origin(
    created_by_hfwd: bool,
    previous_origin: Option<&RemotePluginOrigin>,
) -> bool {
    created_by_hfwd || previous_origin.is_some_and(|origin| origin.origin == "hfwd_remote")
}

fn install_remote_plugin_command(is_update: bool) -> String {
    let mut steps = vec![reject_active_sessions_command()];
    steps.extend([
        "command -v git >/dev/null 2>&1 || { echo 'error: remote git is required' >&2; exit 127; }"
            .into(),
        format!(
            "git ls-remote --exit-code https://github.com/{REMOTE_PLUGIN_SOURCE}.git refs/tags/v{} >/dev/null || {{ echo 'error: remote cannot reach required Herdr Fwd tag v{}' >&2; exit 1; }}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION")
        ),
        "command -v curl >/dev/null 2>&1 || { echo 'error: remote curl is required to install the plugin binary' >&2; exit 127; }"
            .into(),
        format!(
            "HERDR_FWD_MANAGED_REMOTE_INSTALL=1 herdr plugin install {REMOTE_PLUGIN_SOURCE} --ref v{} --yes",
            env!("CARGO_PKG_VERSION")
        ),
        format!("herdr plugin enable {REMOTE_PLUGIN_ID}"),
        format!("herdr plugin list --plugin {REMOTE_PLUGIN_ID} --json"),
    ]);
    if is_update {
        steps.insert(
            1,
            "echo 'Updating managed remote port-forward plugin…'".into(),
        );
    }
    remote_prelude(&steps.join("; "))
}

fn remote_prelude(command: &str) -> String {
    format!(
        "set -eu; export PATH=\"$HOME/.local/bin:/usr/local/bin:$PATH\"; \
         command -v herdr >/dev/null 2>&1 || {{ echo 'error: remote Herdr is required' >&2; exit 127; }}; \
         {command}"
    )
}

fn reject_active_sessions_command() -> String {
    "if find \"$HOME/.cache/herdr-fwd\" -maxdepth 1 -name 'session-*.json' \
     ! -name '*.dashboard.json' -type f -print -quit 2>/dev/null | grep -q .; then \
     echo 'error: an active remote-forward session exists; disconnect it before update/uninstall' >&2; \
     exit 1; fi"
        .into()
}

pub(crate) fn validate_ssh_target(target: &str) -> Result<(), String> {
    if target.is_empty()
        || target.starts_with('-')
        || target
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        Err("SSH target must be a non-empty host or alias without whitespace or options".into())
    } else {
        Ok(())
    }
}

fn remote_management_usage() -> String {
    "usage: hfwd remote {install|update|status|uninstall} <target>".into()
}

#[cfg(test)]
mod remote_management_tests {
    use super::{
        deployment_manifest, install_remote_plugin_command, local_bundle_command,
        reject_active_sessions_command, remote_origin_contents, should_persist_remote_origin,
        validate_ssh_target, RemotePluginOrigin, REMOTE_PLUGIN_ID, REMOTE_PLUGIN_SOURCE,
    };
    #[test]
    fn validates_remote_management_targets() {
        assert!(validate_ssh_target("workbox").is_ok());
        assert!(validate_ssh_target("dev@example.test").is_ok());
        assert!(validate_ssh_target("").is_err());
        assert!(validate_ssh_target("-oProxyCommand=bad").is_err());
        assert!(validate_ssh_target("work box").is_err());
    }

    #[test]
    fn remote_install_command_has_only_fixed_plugin_identifiers() {
        let command = install_remote_plugin_command(false);
        assert!(command.contains(REMOTE_PLUGIN_SOURCE));
        assert!(command.contains(REMOTE_PLUGIN_ID));
        assert!(command.contains("plugin install"));
        assert!(command.contains("HERDR_FWD_MANAGED_REMOTE_INSTALL=1"));
        assert!(!command.contains("HERDR_FWD_INSTALLED_BY_WRAPPER"));
        assert!(command.contains("git ls-remote --exit-code"));
        assert!(command.contains(&format!("refs/tags/v{}", env!("CARGO_PKG_VERSION"))));
        assert!(!command.contains("installation_source"));
        assert!(command.contains(&format!("--ref v{}", env!("CARGO_PKG_VERSION"))));
        assert!(command.contains(&reject_active_sessions_command()));

        let update = install_remote_plugin_command(true);
        assert!(update.contains(&reject_active_sessions_command()));
    }

    #[test]
    fn remote_origin_contents_is_valid_toml_for_a_plugin_root() {
        let plugin_root = "/Users/example/.config/herdr/plugins/github/herdr.fwd-abc";
        let contents = remote_origin_contents(plugin_root);
        let origin = toml_edit::de::from_str::<serde_json::Value>(&contents).unwrap();

        assert_eq!(origin["origin"], "hfwd_remote");
        assert_eq!(origin["plugin_root"], plugin_root);
        assert_eq!(origin["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn refreshes_provenance_only_for_hfwd_created_or_managed_plugins() {
        let managed = RemotePluginOrigin {
            origin: "hfwd_remote".into(),
            plugin_root: "/previous/root".into(),
            version: "0.1.3".into(),
        };
        let manual = RemotePluginOrigin {
            origin: "manual".into(),
            plugin_root: "/manual/root".into(),
            version: "0.1.3".into(),
        };

        assert!(should_persist_remote_origin(true, None));
        assert!(should_persist_remote_origin(false, Some(&managed)));
        assert!(!should_persist_remote_origin(false, Some(&manual)));
        assert!(!should_persist_remote_origin(false, None));
    }

    #[test]
    fn active_session_guard_ignores_dashboard_markers() {
        let command = reject_active_sessions_command();
        assert!(command.contains("! -name '*.dashboard.json'"));
    }

    #[test]
    fn local_bundle_uses_a_managed_remote_path_without_a_build_hook() {
        let manifest = deployment_manifest().unwrap();
        assert!(!manifest.contains("[[build]]"));
        assert!(manifest.contains("herdr-fwd-plugin"));

        let command = local_bundle_command("abc123", "cat > \"$stage/plugin\"");
        assert!(command.contains(".local/share"));
        assert!(command.contains(".staging-abc123"));
        assert!(command.contains("cat > \"$stage/plugin\""));
    }

    #[test]
    fn release_cache_is_scoped_to_the_exact_version_and_platform() {
        let paths =
            super::release_cache_paths(std::path::Path::new("/cache"), "0.1.2", "linux-aarch64");
        assert_eq!(
            paths.binary,
            std::path::PathBuf::from("/cache/releases/v0.1.2/linux-aarch64/herdr-fwd-plugin")
        );
        assert_eq!(
            paths.checksum,
            std::path::PathBuf::from("/cache/releases/v0.1.2/linux-aarch64/SHA256")
        );
    }
}
