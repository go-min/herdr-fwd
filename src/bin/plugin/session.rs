use std::{
    env, fs,
    path::{Path, PathBuf},
};

use herdr_fwd::herdr_session_storage_key;
use herdr_fwd::RemoteSessionConfig;
use serde::{Deserialize, Serialize};

use crate::plugin::{herdr::herdr_output, rpc::api_request};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DashboardMarker {
    pub(crate) workspace_id: String,
    pub(crate) pane_id: String,
    #[serde(default)]
    pub(crate) label: String,
}

pub(crate) fn session_directory() -> Result<PathBuf, String> {
    if let Some(directory) = env::var_os("HERDR_FWD_SESSION_DIR") {
        return Ok(PathBuf::from(directory));
    }
    let home = env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    let scope = herdr_session_storage_key(&current_herdr_session()?)?;
    Ok(PathBuf::from(home)
        .join(".cache/herdr-fwd/sessions")
        .join(scope))
}

pub(crate) fn current_herdr_session() -> Result<String, String> {
    herdr_session_from_inputs(
        env::var_os("HERDR_SOCKET_PATH").as_deref(),
        env::var_os("HERDR_SESSION").as_deref(),
    )
}

pub(crate) fn validate_current_herdr_session(config: &RemoteSessionConfig) -> Result<(), String> {
    let current_session = current_herdr_session()?;
    if config.herdr_session == current_session {
        Ok(())
    } else {
        Err(format!(
            "session file belongs to Herdr session {:?}, current session is {:?}",
            config.herdr_session, current_session
        ))
    }
}

fn herdr_session_from_inputs(
    socket_path: Option<&std::ffi::OsStr>,
    session: Option<&std::ffi::OsStr>,
) -> Result<String, String> {
    if let Some(socket_path) = socket_path.filter(|path| !path.is_empty()) {
        let socket_path = Path::new(socket_path);
        if socket_path.file_name().and_then(|name| name.to_str()) == Some("herdr.sock") {
            let parent = socket_path.parent();
            if parent
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some("sessions")
            {
                return parent
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| "HERDR_SOCKET_PATH has no valid session name".to_string());
            }
            if parent
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some("herdr")
            {
                return Ok("default".into());
            }
        }
    }
    match session.filter(|session| !session.is_empty()) {
        Some(session) => session
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| "HERDR_SESSION is not valid UTF-8".to_string()),
        None => Ok("default".into()),
    }
}

pub(crate) fn installation_state_directory() -> Result<PathBuf, String> {
    if let Some(directory) = env::var_os("HERDR_FWD_STATE_DIR") {
        return Ok(PathBuf::from(directory));
    }
    if let Some(directory) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(directory).join("herdr-fwd"));
    }
    let home = env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".local/state/herdr-fwd"))
}

pub(crate) fn active_session_path() -> Result<PathBuf, String> {
    optional_active_session_path()?.ok_or_else(|| "no reachable remote-forward session".into())
}

pub(crate) fn optional_active_session_path() -> Result<Option<PathBuf>, String> {
    if let Some(explicit) = env::var_os("HERDR_FWD_SESSION_PATH").filter(|path| !path.is_empty()) {
        let path = PathBuf::from(explicit);
        let config: RemoteSessionConfig = read_json_file(&path)?;
        config.validate()?;
        validate_current_herdr_session(&config)?;
        api_request::<serde_json::Value>(&config, "GET", "/v1/settings/local", None)?;
        return Ok(Some(path));
    }
    let directory = session_directory()?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", directory.display())),
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if !is_session_file(&path) {
            continue;
        }
        let config: RemoteSessionConfig = read_json_file(&path)?;
        config.validate()?;
        validate_current_herdr_session(&config)?;
        candidates.push(path);
    }
    // A temporarily unreachable connector is still a candidate. Do not route
    // its action to another connector (or the host's local config) during an outage.
    let selected = unique_session_path(candidates)?;
    if let Some(path) = &selected {
        let config: RemoteSessionConfig = read_json_file(path)?;
        api_request::<serde_json::Value>(&config, "GET", "/v1/settings/local", None)?;
    }
    Ok(selected)
}

fn unique_session_path(mut candidates: Vec<PathBuf>) -> Result<Option<PathBuf>, String> {
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        count => Err(format!("{count} forwarding sessions are registered; open the dashboard in your forwarding Space, or set HERDR_FWD_SESSION_PATH to the intended session file")),
    }
}

pub(crate) fn is_session_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("session-") && name.ends_with(".json"))
        && !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".dashboard.json"))
}

pub(crate) fn dashboard_marker_path(session_path: &Path) -> PathBuf {
    session_path.with_file_name(format!(
        "{}.dashboard.json",
        session_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("session")
    ))
}

pub(crate) fn cleanup_orphan_dashboards(
    entries: &[PathBuf],
    sessions: &[PathBuf],
) -> Result<(), String> {
    cleanup_orphan_dashboards_with(entries, sessions, &mut |marker| close_dashboard(marker))
}

fn cleanup_orphan_dashboards_with(
    entries: &[PathBuf],
    sessions: &[PathBuf],
    close_dashboard: &mut impl FnMut(&Path) -> Result<(), String>,
) -> Result<(), String> {
    for marker in entries.iter().filter(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".dashboard.json"))
    }) {
        let belongs_to_session = sessions
            .iter()
            .any(|session| dashboard_marker_path(session) == *marker);
        if !belongs_to_session {
            if let Err(error) = close_dashboard(marker) {
                if env::var_os("HERDR_FWD_LOG").as_deref() == Some(std::ffi::OsStr::new("debug")) {
                    eprintln!("orphan dashboard {}: {error}", marker.display());
                }
            }
        }
    }
    Ok(())
}

fn close_dashboard(marker_path: &Path) -> Result<(), String> {
    close_dashboard_with(marker_path, &mut |workspace_id| {
        herdr_output(&["workspace", "close", workspace_id]).map(|_| ())
    })
}

fn close_dashboard_with(
    marker_path: &Path,
    close_workspace: &mut impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    if !marker_path.exists() {
        return Ok(());
    }
    let marker = read_json_file::<DashboardMarker>(marker_path)?;
    match close_workspace(&marker.workspace_id) {
        Ok(()) => {}
        Err(error) if workspace_is_already_absent(&error) => {}
        Err(error) => return Err(error),
    }
    fs::remove_file(marker_path).map_err(|error| format!("{}: {error}", marker_path.display()))
}

fn workspace_is_already_absent(error: &str) -> bool {
    let error = error.trim().to_ascii_lowercase();
    matches!(
        error.as_str(),
        "workspace not found" | "workspace already closed"
    ) || error.starts_with("workspace not found:")
        || error.starts_with("workspace already closed:")
}

pub(crate) fn cleanup_remote_session(session_path: &Path) -> Result<(), String> {
    cleanup_remote_session_with(session_path, &mut |workspace_id| {
        herdr_output(&["workspace", "close", workspace_id]).map(|_| ())
    })
}

fn cleanup_remote_session_with(
    session_path: &Path,
    close_workspace: &mut impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    close_dashboard_with(&dashboard_marker_path(session_path), close_workspace)?;
    fs::remove_file(session_path).map_err(|error| format!("{}: {error}", session_path.display()))
}

pub(crate) fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

pub(crate) fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    herdr_fwd::atomic::write_file(path, &bytes, 0o600)
}

#[cfg(test)]
mod session_tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::{
        cleanup_orphan_dashboards_with, cleanup_remote_session_with, close_dashboard_with,
        dashboard_marker_path, herdr_session_from_inputs, is_session_file, read_json_file,
        write_json_file, DashboardMarker,
    };

    #[test]
    fn session_selection_never_guesses_between_connectors() {
        let first = PathBuf::from("session-first.json");
        let second = PathBuf::from("session-second.json");
        assert_eq!(super::unique_session_path(Vec::new()).unwrap(), None);
        assert_eq!(
            super::unique_session_path(vec![first.clone()]).unwrap(),
            Some(first.clone())
        );
        for candidates in [vec![first.clone(), second.clone()], vec![second, first]] {
            let error = super::unique_session_path(candidates).unwrap_err();
            assert!(error.contains("2 forwarding sessions"));
            assert!(error.contains("HERDR_FWD_SESSION_PATH"));
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);
            let unique = format!(
                "herdr-fwd-session-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock should be after the Unix epoch")
                    .as_nanos(),
                NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed),
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir(&path).expect("test directory should be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            #[cfg(unix)]
            fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700))
                .expect("test directory permissions should be restored");
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn marker(workspace_id: &str) -> DashboardMarker {
        DashboardMarker {
            workspace_id: workspace_id.into(),
            pane_id: format!("{workspace_id}:p1"),
            label: "Port Forwarding".into(),
        }
    }

    #[test]
    fn distinguishes_session_files_from_dashboard_markers() {
        let session = Path::new("/tmp/session-abc.json");
        let marker = Path::new("/tmp/session-abc.dashboard.json");
        assert!(is_session_file(session));
        assert!(!is_session_file(marker));
        assert_eq!(dashboard_marker_path(session), marker);
    }

    #[cfg(unix)]
    #[test]
    fn preserves_existing_marker_when_atomic_publish_cannot_create_a_temporary_file() {
        let directory = TestDirectory::new();
        let path = directory.path().join("session.dashboard.json");
        let expected = marker("existing");
        write_json_file(&path, &expected).expect("existing marker should be written");

        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o500))
            .expect("test directory should become read-only");
        let result = write_json_file(&path, &marker("replacement"));
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("test directory should become writable again");

        assert!(result.is_err());
        let actual: DashboardMarker = read_json_file(&path).expect("marker should remain readable");
        assert_eq!(actual.workspace_id, "existing");
    }

    #[test]
    fn cleanup_remote_session_retains_session_and_marker_when_workspace_close_fails() {
        let directory = TestDirectory::new();
        let session_path = directory.path().join("session-abc.json");
        let marker_path = dashboard_marker_path(&session_path);
        fs::write(&session_path, "{}").expect("session should be written");
        write_json_file(&marker_path, &marker("workspace-1"))
            .expect("dashboard marker should be written");

        let error = cleanup_remote_session_with(&session_path, &mut |_| Err("close failed".into()))
            .expect_err("workspace close failure should be returned");

        assert_eq!(error, "close failed");
        assert!(session_path.exists(), "session should remain for a retry");
        assert!(marker_path.exists(), "marker should remain for a retry");
    }

    #[test]
    fn cleanup_remote_session_removes_state_when_workspace_is_already_absent() {
        let directory = TestDirectory::new();
        let session_path = directory.path().join("session-abc.json");
        let marker_path = dashboard_marker_path(&session_path);
        fs::write(&session_path, "{}").expect("session should be written");
        write_json_file(&marker_path, &marker("workspace-1"))
            .expect("dashboard marker should be written");

        cleanup_remote_session_with(&session_path, &mut |_| {
            Err("workspace not found: workspace-1".into())
        })
        .expect("an already absent workspace should be cleaned up");

        assert!(!session_path.exists(), "session should be removed");
        assert!(!marker_path.exists(), "marker should be removed");
    }

    #[test]
    fn retains_failed_orphan_markers_without_blocking_later_scans() {
        let directory = TestDirectory::new();
        let marker_path = directory.path().join("session-orphan.dashboard.json");
        write_json_file(&marker_path, &marker("workspace-1"))
            .expect("orphan marker should be written");
        let entries = vec![marker_path.clone()];

        cleanup_orphan_dashboards_with(&entries, &[], &mut |marker| {
            close_dashboard_with(marker, &mut |_| Err("close failed".into()))
        })
        .expect("a failed orphan cleanup should not stop a scan");
        assert!(
            marker_path.exists(),
            "failed cleanup should retain the marker"
        );

        cleanup_orphan_dashboards_with(&entries, &[], &mut |marker| {
            close_dashboard_with(marker, &mut |_| Ok(()))
        })
        .expect("a later scan should continue retrying orphan cleanup");
        assert!(
            !marker_path.exists(),
            "a later successful scan should remove the retried marker"
        );
    }

    #[test]
    fn derives_the_active_herdr_session_from_the_socket_before_the_environment() {
        assert_eq!(
            herdr_session_from_inputs(
                Some(std::ffi::OsStr::new(
                    "/home/test/.config/herdr/sessions/review/herdr.sock"
                )),
                Some(std::ffi::OsStr::new("stale")),
            )
            .unwrap(),
            "review"
        );
        assert_eq!(
            herdr_session_from_inputs(
                Some(std::ffi::OsStr::new("/home/test/.config/herdr/herdr.sock")),
                Some(std::ffi::OsStr::new("stale")),
            )
            .unwrap(),
            "default"
        );
        assert_eq!(
            herdr_session_from_inputs(None, Some(std::ffi::OsStr::new("preview"))).unwrap(),
            "preview"
        );
    }
}
