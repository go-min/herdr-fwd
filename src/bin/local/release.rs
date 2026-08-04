use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use fs2::FileExt;

use crate::local::support::{secure_random_hex, RuntimeDirectory};

const REMOTE_PLUGIN_SOURCE: &str = "go-min/herdr-fwd";

pub(crate) struct ReleaseBundle {
    pub(crate) manifest: Vec<u8>,
    pub(crate) binary: Vec<u8>,
}

struct ReleaseCachePaths {
    binary: PathBuf,
    checksum: PathBuf,
}

#[derive(Debug)]
enum ReleaseFetchError {
    Unavailable(String),
    Integrity(String),
    InvalidRelease(String),
}

impl ReleaseFetchError {
    fn allows_cache(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl std::fmt::Display for ReleaseFetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Unavailable(message)
            | Self::Integrity(message)
            | Self::InvalidRelease(message) => message,
        };
        formatter.write_str(message)
    }
}

pub(crate) fn release_bundle(platform: &str) -> Result<ReleaseBundle, String> {
    let paths = release_cache_paths(&release_cache_base()?, env!("CARGO_PKG_VERSION"), platform);
    let binary = match download_release_binary(platform, &paths) {
        Ok(binary) => binary,
        Err(error) if error.allows_cache() => verified_cached_binary(&paths)
            .map_err(|cache_error| format!("{error}; offline cache: {cache_error}"))?,
        Err(error) => return Err(error.to_string()),
    };
    let manifest = deployment_manifest()?.into_bytes();
    Ok(ReleaseBundle { manifest, binary })
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
    let metadata = fs::read_to_string(&paths.checksum)
        .map_err(|error| format!("{}: {error}", paths.checksum.display()))?;
    let archive_checksum = cache_checksum(&metadata, "archive_sha256")?;
    let binary_checksum = cache_checksum(&metadata, "binary_sha256")?;
    validate_sha256(archive_checksum)?;
    verify_sha256(&binary, binary_checksum)?;
    Ok(binary)
}

fn cache_checksum<'a>(metadata: &'a str, key: &str) -> Result<&'a str, String> {
    metadata
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == key).then(|| fields.next()).flatten()
        })
        .ok_or_else(|| format!("offline cache metadata is missing {key}"))
}

fn download_release_binary(
    platform: &str,
    paths: &ReleaseCachePaths,
) -> Result<Vec<u8>, ReleaseFetchError> {
    let temporary = RuntimeDirectory::create("herdr-fwd-release-")
        .map_err(ReleaseFetchError::InvalidRelease)?;
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
        &fs::read_to_string(&sums)
            .map_err(|error| ReleaseFetchError::InvalidRelease(error.to_string()))?,
        &asset,
    )
    .map_err(ReleaseFetchError::InvalidRelease)?;
    verify_sha256(
        &fs::read(&archive)
            .map_err(|error| ReleaseFetchError::InvalidRelease(error.to_string()))?,
        &expected,
    )
    .map_err(ReleaseFetchError::Integrity)?;
    let archive_string = archive.display().to_string();
    let output = Command::new("tar")
        .args(["-xOzf", &archive_string, "./herdr-fwd-plugin"])
        .output()
        .map_err(|error| {
            ReleaseFetchError::InvalidRelease(format!("failed to extract release plugin: {error}"))
        })?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(ReleaseFetchError::InvalidRelease(
            "release archive does not contain herdr-fwd-plugin".into(),
        ));
    }
    publish_cached_binary(paths, &output.stdout, &expected)
        .map_err(ReleaseFetchError::InvalidRelease)?;
    Ok(output.stdout)
}

fn download_file(url: &str, destination: &Path) -> Result<(), ReleaseFetchError> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "15",
            "--max-time",
            "90",
            "--retry",
            "2",
            "-o",
        ])
        .arg(destination)
        .arg(url)
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ReleaseFetchError::Unavailable(format!(
                    "cannot download {url}: curl is unavailable"
                ))
            } else {
                ReleaseFetchError::InvalidRelease(format!("failed to start curl: {error}"))
            }
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(classify_curl_failure(
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
            url,
        ))
    }
}

fn classify_curl_failure(code: Option<i32>, stderr: &str, url: &str) -> ReleaseFetchError {
    let message = format!(
        "failed to download release asset from {url}: {}",
        stderr.trim()
    );
    if matches!(code, Some(5 | 6 | 7 | 28 | 35 | 52 | 55 | 56))
        || (code == Some(22)
            && [" 500", " 502", " 503", " 504"]
                .iter()
                .any(|status| stderr.contains(status)))
    {
        ReleaseFetchError::Unavailable(message)
    } else {
        ReleaseFetchError::InvalidRelease(message)
    }
}

fn publish_cached_binary(
    paths: &ReleaseCachePaths,
    binary: &[u8],
    expected: &str,
) -> Result<(), String> {
    let target = paths
        .binary
        .parent()
        .ok_or_else(|| "invalid release cache path".to_string())?;
    let parent = target
        .parent()
        .ok_or_else(|| "invalid release cache parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let nonce = secure_random_hex(8)?;
    validate_sha256(expected)?;
    let binary_checksum = sha256(binary)?;
    let platform = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "invalid release cache platform".to_string())?;
    let lock_path = parent.join(format!(".{platform}.lock"));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| error.to_string())?;
    lock.lock_exclusive().map_err(|error| error.to_string())?;
    let stage = parent.join(format!(".{platform}.tmp-{nonce}"));
    let backup = parent.join(format!(".{platform}.previous-{nonce}"));
    fs::create_dir(&stage).map_err(|error| error.to_string())?;
    let result = (|| {
        write_cache_file(&stage.join("herdr-fwd-plugin"), binary, 0o755)?;
        write_cache_file(
            &stage.join("SHA256"),
            format!("archive_sha256 {expected}\nbinary_sha256 {binary_checksum}\n").as_bytes(),
            0o600,
        )?;
        if target.exists() {
            fs::rename(target, &backup).map_err(|error| error.to_string())?;
        }
        if let Err(error) = fs::rename(&stage, target) {
            if backup.exists() {
                let _ = fs::rename(&backup, target);
            }
            return Err(error.to_string());
        }
        if backup.exists() {
            let _ = fs::remove_dir_all(&backup);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
        if backup.exists() && !target.exists() {
            let _ = fs::rename(&backup, target);
        }
    }
    let _ = FileExt::unlock(&lock);
    result
}

fn write_cache_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())
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
    validate_sha256(expected)?;
    let actual = sha256(bytes)?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err("release SHA-256 verification failed".into())
    }
}

fn validate_sha256(expected: &str) -> Result<(), String> {
    if expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("release SHA-256 value is malformed".into())
    }
}

fn sha256(bytes: &[u8]) -> Result<String, String> {
    let temporary = RuntimeDirectory::create("herdr-fwd-sha-")?;
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
    if output.status.success() && actual.len() == 64 {
        Ok(actual)
    } else {
        Err("SHA-256 verifier failed".into())
    }
}

pub(crate) fn deployment_manifest() -> Result<String, String> {
    let mut manifest = include_str!("../../../herdr-plugin.toml")
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("invalid bundled plugin manifest: {error}"))?;
    manifest.remove("build");
    Ok(manifest.to_string())
}

#[cfg(test)]
mod release_tests {
    use super::{
        classify_curl_failure, publish_cached_binary, release_cache_paths, sha256,
        verified_cached_binary, ReleaseFetchError, RuntimeDirectory,
    };

    #[test]
    fn release_cache_is_scoped_to_the_exact_version_and_platform() {
        let paths = release_cache_paths(std::path::Path::new("/cache"), "0.1.2", "linux-aarch64");
        assert_eq!(
            paths.binary,
            std::path::PathBuf::from("/cache/releases/v0.1.2/linux-aarch64/herdr-fwd-plugin")
        );
        assert_eq!(
            paths.checksum,
            std::path::PathBuf::from("/cache/releases/v0.1.2/linux-aarch64/SHA256")
        );
    }

    #[test]
    fn release_cache_is_only_eligible_for_connectivity_failures() {
        assert!(ReleaseFetchError::Unavailable("offline".into()).allows_cache());
        assert!(!ReleaseFetchError::Integrity("checksum mismatch".into()).allows_cache());
        assert!(!ReleaseFetchError::InvalidRelease("HTTP 404".into()).allows_cache());
    }

    #[test]
    fn curl_http_404_is_not_treated_as_an_offline_host() {
        let error = classify_curl_failure(
            Some(22),
            "curl: (22) The requested URL returned error: 404",
            "https://example.test/v0.1.5/asset.tar.gz",
        );
        assert!(matches!(error, ReleaseFetchError::InvalidRelease(_)));

        let error = classify_curl_failure(
            Some(6),
            "curl: (6) Could not resolve host: github.com",
            "https://github.com/example/asset.tar.gz",
        );
        assert!(matches!(error, ReleaseFetchError::Unavailable(_)));
    }

    #[test]
    fn cached_binary_retains_archive_provenance_and_verifies_its_own_bytes() {
        let runtime = RuntimeDirectory::create("herdr-fwd-cache-metadata-test-").unwrap();
        let paths = release_cache_paths(runtime.path(), "0.1.5", "linux-aarch64");
        let binary = b"verified plugin binary";
        let archive_checksum = sha256(b"verified release archive").unwrap();

        publish_cached_binary(&paths, binary, &archive_checksum).unwrap();

        assert_eq!(verified_cached_binary(&paths).unwrap(), binary);
        let metadata = std::fs::read_to_string(&paths.checksum).unwrap();
        assert!(metadata.contains(&format!("archive_sha256 {archive_checksum}")));
        assert!(metadata.contains("binary_sha256 "));
    }
}
