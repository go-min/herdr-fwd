use std::{
    collections::{HashMap, HashSet},
    env, fs,
    fs::OpenOptions,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use fs2::FileExt;
use herdr_fwd::{
    registry::Forward, shell::quote as shell_quote, ForwardRequest, RemoteSessionConfig,
};
use serde_json::Value;

use crate::plugin::{
    dashboard_terminal::dashboard,
    herdr::{
        collect_array_values, collect_string_values, first_string_value, herdr_json, herdr_output,
        loopback_listener_processes, process_label, process_started_at, process_tree_ids,
        report_workspace_port_forward_status,
    },
    notifications::notify_changes,
    onboarding,
    preferences::{load_preferences, AfterForward},
    rpc::{api_request, list_forwards},
    session::{
        active_session_path, cleanup_orphan_dashboards, cleanup_remote_session,
        dashboard_marker_path, is_session_file, read_json_file, session_directory, write_json_file,
        DashboardMarker,
    },
    sidebar_config::enable_ports_row,
};

const RECONCILIATION_INTERVAL: Duration = Duration::from_secs(15);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const FAILED_HEARTBEATS_BEFORE_CLEANUP: u8 = 3;

trait DashboardOperations {
    fn create_dashboard(&mut self, label: &str) -> Result<DashboardMarker, String>;
    fn run_dashboard(&mut self, pane_id: &str, command: &str) -> Result<(), String>;
    fn report_status(&mut self, workspace_id: &str, status: &str) -> Result<(), String>;
    fn write_marker(&mut self, path: &Path, marker: &DashboardMarker) -> Result<(), String>;
    fn close_workspace(&mut self, workspace_id: &str) -> Result<(), String>;
}

struct HerdrDashboardOperations;

impl DashboardOperations for HerdrDashboardOperations {
    fn create_dashboard(&mut self, label: &str) -> Result<DashboardMarker, String> {
        let created = herdr_json(&workspace_create_arguments(label))?;
        let workspace_id = first_string_value(&created, "workspace_id")
            .ok_or_else(|| "workspace.create did not return workspace_id".to_string())?;
        let pane_id = first_string_value(&created, "pane_id").unwrap_or_default();
        Ok(DashboardMarker {
            workspace_id,
            pane_id,
            label: label.into(),
        })
    }

    fn run_dashboard(&mut self, pane_id: &str, command: &str) -> Result<(), String> {
        herdr_output(&["pane", "run", pane_id, command]).map(|_| ())
    }

    fn report_status(&mut self, workspace_id: &str, status: &str) -> Result<(), String> {
        report_workspace_port_forward_status(workspace_id, Some(status))
    }

    fn write_marker(&mut self, path: &Path, marker: &DashboardMarker) -> Result<(), String> {
        write_json_file(path, marker)
    }

    fn close_workspace(&mut self, workspace_id: &str) -> Result<(), String> {
        herdr_output(&["workspace", "close", workspace_id]).map(|_| ())
    }
}

pub(crate) fn main() {
    let result = match env::args().nth(1).as_deref() {
        Some("start") => start(),
        Some("watch") => spawn_watcher(),
        Some("watch-loop") => watch_loop(),
        Some("scan") => scan_once(&mut HashMap::new(), false).map(|_| ()),
        Some("dashboard") => env::args()
            .nth(2)
            .ok_or_else(|| "dashboard requires a session file".to_string())
            .and_then(|path| dashboard(Path::new(&path))),
        Some("dashboard-current") => dashboard_current(),
        Some("open-dashboard-here") => open_dashboard_here(),
        Some("open-dashboard-popup") => open_dashboard_popup(),
        Some("enable-sidebar-status") => enable_sidebar_status(),
        Some("welcome") => onboarding::welcome(),
        _ => Ok(()),
    };
    if let Err(error) = result {
        eprintln!("herdr-fwd-plugin: {error}");
        std::process::exit(1);
    }
}

fn start() -> Result<(), String> {
    if let Err(error) = onboarding::maybe_open() {
        if env::var_os("HERDR_FWD_LOG").as_deref() == Some(std::ffi::OsStr::new("debug")) {
            eprintln!("onboarding: {error}");
        }
    }
    spawn_watcher()
}

fn spawn_watcher() -> Result<(), String> {
    Command::new(env::current_exe().map_err(|error| error.to_string())?)
        .arg("watch-loop")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to spawn watcher: {error}"))?;
    Ok(())
}

fn watch_loop() -> Result<(), String> {
    let directory = session_directory()?;
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let lock_path = directory.join("watcher.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|error| format!("failed to open watcher lock: {error}"))?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }

    let mut failures = HashMap::new();
    let mut empty_scans = 0;
    #[cfg(unix)]
    let mut events = None;

    // Session files are created by the wrapper, not by Herdr, so use a short
    // retry until the first one appears. Once it does, events drive expensive
    // pane reads while a light heartbeat keeps the companion lease alive.
    loop {
        let sessions = scan_once(&mut failures, true)?;
        if sessions == 0 {
            // Allow startup hooks to run just before the wrapper uploads its
            // session file, but do not leave an orphan watcher running.
            empty_scans += 1;
            if empty_scans >= 8 {
                return Ok(());
            }
        } else {
            break;
        }
        std::thread::sleep(HEARTBEAT_INTERVAL);
    }

    let mut last_reconciliation = Instant::now();
    loop {
        #[cfg(unix)]
        let event_received = {
            if events.is_none() {
                events = crate::plugin::herdr::EventSubscriber::connect().ok();
            }
            match events.as_mut() {
                Some(subscriber) => match subscriber.wait(HEARTBEAT_INTERVAL) {
                    Ok(event_received) => event_received,
                    Err(_) => {
                        events = None;
                        false
                    }
                },
                None => {
                    std::thread::sleep(HEARTBEAT_INTERVAL);
                    false
                }
            }
        };
        #[cfg(not(unix))]
        let event_received = {
            std::thread::sleep(HEARTBEAT_INTERVAL);
            false
        };

        if heartbeat_once(&mut failures, true)? == 0 {
            return Ok(());
        }
        if event_received || last_reconciliation.elapsed() >= RECONCILIATION_INTERVAL {
            scan_once(&mut failures, true)?;
            last_reconciliation = Instant::now();
        }
    }
}

fn scan_once(failures: &mut HashMap<PathBuf, u8>, lifecycle: bool) -> Result<usize, String> {
    let session_files = session_files()?;

    for path in &session_files {
        match process_session(path) {
            Ok(()) => {
                failures.remove(path);
            }
            Err(error) => {
                let failure_count = failures.entry(path.clone()).or_default();
                *failure_count = failure_count.saturating_add(1);
                if lifecycle && *failure_count >= FAILED_HEARTBEATS_BEFORE_CLEANUP {
                    cleanup_remote_session(path).map_err(|cleanup_error| {
                        format!(
                            "session {}: {error}; dashboard cleanup failed: {cleanup_error}",
                            path.display()
                        )
                    })?;
                    failures.remove(path);
                } else if env::var_os("HERDR_FWD_LOG").as_deref()
                    == Some(std::ffi::OsStr::new("debug"))
                {
                    eprintln!("session {}: {error}", path.display());
                }
            }
        }
    }
    failures.retain(|path, _| session_files.contains(path));
    Ok(session_files.len())
}

fn heartbeat_once(failures: &mut HashMap<PathBuf, u8>, lifecycle: bool) -> Result<usize, String> {
    let session_files = session_files()?;
    for path in &session_files {
        let result = read_json_file::<RemoteSessionConfig>(path).and_then(|config| {
            config.validate()?;
            api_request::<Value>(&config, "POST", "/v1/heartbeat", None).map(|_| ())
        });
        match result {
            Ok(()) => {
                failures.remove(path);
            }
            Err(error) => {
                let failure_count = failures.entry(path.clone()).or_default();
                *failure_count = failure_count.saturating_add(1);
                if lifecycle && *failure_count >= FAILED_HEARTBEATS_BEFORE_CLEANUP {
                    cleanup_remote_session(path).map_err(|cleanup_error| {
                        format!(
                            "session {}: {error}; dashboard cleanup failed: {cleanup_error}",
                            path.display()
                        )
                    })?;
                    failures.remove(path);
                } else if env::var_os("HERDR_FWD_LOG").as_deref()
                    == Some(std::ffi::OsStr::new("debug"))
                {
                    eprintln!("session {}: {error}", path.display());
                }
            }
        }
    }
    failures.retain(|path, _| session_files.contains(path));
    Ok(session_files.len())
}

fn session_files() -> Result<Vec<PathBuf>, String> {
    let directory = session_directory()?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let session_files = entries
        .iter()
        .filter(|path| is_session_file(path))
        .cloned()
        .collect::<Vec<_>>();
    cleanup_orphan_dashboards(&entries, &session_files)?;
    Ok(session_files)
}

fn process_session(path: &Path) -> Result<(), String> {
    let config: RemoteSessionConfig = read_json_file(path)?;
    config.validate()?;
    api_request::<Value>(&config, "POST", "/v1/heartbeat", None)?;
    if !config.auto_detect {
        return Ok(());
    }

    let previous = list_forwards(&config)?;
    let process_tree_depth = load_preferences()?.process_tree_depth;
    let pane_list = herdr_json(&["pane", "list"])?;
    let pane_ids = collect_string_values(&pane_list, "pane_id");
    let pane_workspaces = pane_workspace_map(&pane_list);
    let mut active_panes = HashSet::new();
    let mut active_processes = HashMap::new();
    let mut active_process_ids = HashMap::new();
    let mut active_ports = HashMap::new();
    let mut detected = Vec::new();

    for pane_id in pane_ids {
        let process_info = herdr_json(&["pane", "process-info", "--pane", &pane_id])?;
        let processes = collect_array_values(&process_info, "foreground_processes");
        if processes.is_empty() {
            continue;
        }
        active_panes.insert(pane_id.clone());
        let process = process_label(&process_info);
        active_processes.insert(pane_id.clone(), process.clone());
        let root_process_ids = processes
            .iter()
            .filter_map(|process| process.get("pid").and_then(Value::as_u64))
            .filter_map(|process_id| u32::try_from(process_id).ok())
            .collect::<Vec<_>>();
        let process_ids = process_tree_ids(&root_process_ids, process_tree_depth)?;
        active_process_ids.insert(
            pane_id.clone(),
            process_ids.iter().copied().collect::<HashSet<_>>(),
        );
        let listeners = loopback_listener_processes(&process_ids)?;
        let ports = listeners.keys().copied().collect::<Vec<_>>();
        active_ports.insert(
            pane_id.clone(),
            ports.iter().copied().collect::<HashSet<_>>(),
        );
        for (port, (process_id, remote_host)) in listeners {
            let detected_url_host = if remote_host == "::1" {
                "[::1]".to_string()
            } else {
                remote_host.clone()
            };
            detected.push(ForwardRequest {
                remote_port: port,
                preferred_local_port: port,
                remote_host,
                pane_id: pane_id.clone(),
                process: process.clone(),
                detected_url: format!("http://{detected_url_host}:{port}/"),
                automatic: true,
                server_started_at: process_started_at(process_id),
                process_id: Some(process_id),
            });
        }
    }

    for forward in &previous {
        if !forward.automatic {
            continue;
        }
        let process_changed = process_changed(forward, &active_processes, &active_process_ids);
        if !active_panes.contains(&forward.pane_id)
            || process_changed
            || !active_ports
                .get(&forward.pane_id)
                .is_some_and(|ports| ports.contains(&forward.remote_port))
        {
            let _ = api_request::<Value>(
                &config,
                "DELETE",
                &format!("/v1/forwards/{}", forward.id),
                None,
            );
        }
    }

    let automatic = automatic_requests_to_create(detected, &previous);
    let opened_forwards = !automatic.is_empty();
    for request in automatic {
        let _ = api_request::<Forward>(
            &config,
            "POST",
            "/v1/forwards",
            Some(serde_json::to_value(request).map_err(|error| error.to_string())?),
        );
    }
    if opened_forwards {
        show_after_forward(path, load_preferences()?.after_forward)?;
    }

    let current = list_forwards(&config)?;
    report_workspace_forward_metadata(&previous, &current, &pane_workspaces);
    reconcile_dashboard(path, &current)?;
    notify_changes(&previous, &current);
    Ok(())
}

fn automatic_requests_to_create(
    detected: Vec<ForwardRequest>,
    previous: &[Forward],
) -> Vec<ForwardRequest> {
    detected
        .into_iter()
        .filter(|request| {
            !previous.iter().any(|forward| {
                forward.automatic
                    && forward.pane_id == request.pane_id
                    && forward.process_id == request.process_id
                    && forward.remote_port == request.remote_port
            })
        })
        .collect()
}

fn pane_workspace_map(value: &Value) -> HashMap<String, String> {
    let mut output = HashMap::new();
    collect_pane_workspaces(value, &mut output);
    output
}

fn process_changed(
    forward: &Forward,
    active_processes: &HashMap<String, String>,
    active_process_ids: &HashMap<String, HashSet<u32>>,
) -> bool {
    active_processes
        .get(&forward.pane_id)
        .is_some_and(|process| process != &forward.process)
        || forward.process_id.is_some_and(|process_id| {
            !active_process_ids
                .get(&forward.pane_id)
                .is_some_and(|process_ids| process_ids.contains(&process_id))
        })
}

fn collect_pane_workspaces(value: &Value, output: &mut HashMap<String, String>) {
    match value {
        Value::Object(object) => {
            if let (Some(pane_id), Some(workspace_id)) = (
                object.get("pane_id").and_then(Value::as_str),
                object.get("workspace_id").and_then(Value::as_str),
            ) {
                output.insert(pane_id.to_string(), workspace_id.to_string());
            }
            for value in object.values() {
                collect_pane_workspaces(value, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_pane_workspaces(value, output);
            }
        }
        _ => {}
    }
}

fn report_workspace_forward_metadata(
    previous: &[Forward],
    current: &[Forward],
    pane_workspaces: &HashMap<String, String>,
) {
    for (workspace_id, ports) in workspace_port_tokens(previous, current, pane_workspaces) {
        let _ = report_workspace_port_forward_status(&workspace_id, ports.as_deref());
    }
}

fn workspace_port_tokens(
    previous: &[Forward],
    current: &[Forward],
    pane_workspaces: &HashMap<String, String>,
) -> HashMap<String, Option<String>> {
    let mut affected = HashSet::new();
    for forward in previous.iter().chain(current) {
        if forward.automatic {
            if let Some(workspace_id) = pane_workspaces.get(&forward.pane_id) {
                affected.insert(workspace_id.clone());
            }
        }
    }
    let mut tokens = HashMap::new();
    for workspace_id in affected {
        let mut ports = current
            .iter()
            .filter(|forward| {
                forward.automatic
                    && forward.enabled
                    && pane_workspaces.get(&forward.pane_id) == Some(&workspace_id)
            })
            .map(forward_sidebar_token)
            .collect::<Vec<_>>();
        ports.sort();
        ports.dedup();
        tokens.insert(workspace_id, (!ports.is_empty()).then(|| ports.join(", ")));
    }
    tokens
}

fn forward_sidebar_token(forward: &Forward) -> String {
    if forward.remote_port == forward.local_port {
        format!("→{}", forward.remote_port)
    } else {
        format!("{}→{}", forward.remote_port, forward.local_port)
    }
}

fn reconcile_dashboard(session_path: &Path, forwards: &[Forward]) -> Result<(), String> {
    let marker_path = dashboard_marker_path(session_path);
    let label = "Port Forwarding";
    let status = forwarding_space_status(forwards);
    if marker_path.exists() {
        let mut marker = read_json_file::<DashboardMarker>(&marker_path)?;
        if marker.label != label {
            herdr_output(&["workspace", "rename", &marker.workspace_id, label])?;
            marker.label = label.into();
            write_json_file(&marker_path, &marker)?;
        }
        report_workspace_port_forward_status(&marker.workspace_id, Some(&status))?;
        return Ok(());
    }
    Ok(())
}

pub(crate) fn show_after_forward(
    session_path: &Path,
    after_forward: AfterForward,
) -> Result<(), String> {
    match after_forward {
        AfterForward::Space => open_dashboard_space(session_path),
        AfterForward::Popup => open_dashboard_popup_for(session_path),
        AfterForward::Nothing => Ok(()),
    }
}

fn open_dashboard_space(session_path: &Path) -> Result<(), String> {
    let forwards = read_json_file::<RemoteSessionConfig>(session_path)
        .and_then(|config| list_forwards(&config))?;
    let status = forwarding_space_status(&forwards);
    let marker_path = dashboard_marker_path(session_path);
    if marker_path.exists() {
        return Ok(());
    }
    let executable = env::current_exe().map_err(|error| error.to_string())?;
    let command = dashboard_command(&executable, session_path);
    let mut operations = HerdrDashboardOperations;
    open_dashboard_space_with(&mut operations, &marker_path, &status, &command)
}

fn open_dashboard_space_with<O: DashboardOperations>(
    operations: &mut O,
    marker_path: &Path,
    status: &str,
    command: &str,
) -> Result<(), String> {
    if marker_path.exists() {
        return Ok(());
    }
    let marker = operations.create_dashboard("Port Forwarding")?;
    let result = (|| {
        if marker.pane_id.trim().is_empty() {
            return Err("workspace.create did not return root pane_id".to_string());
        }
        operations.run_dashboard(&marker.pane_id, command)?;
        operations.report_status(&marker.workspace_id, status)?;
        operations.write_marker(marker_path, &marker)
    })();
    if let Err(error) = result {
        if let Err(close_error) = operations.close_workspace(&marker.workspace_id) {
            return Err(format!("{error}; dashboard cleanup failed: {close_error}"));
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
fn close_dashboard_with(
    marker_path: &Path,
    close: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    let marker = read_json_file::<DashboardMarker>(marker_path)?;
    close(&marker.workspace_id)?;
    fs::remove_file(marker_path).map_err(|error| format!("{}: {error}", marker_path.display()))
}

fn workspace_create_arguments(label: &str) -> [&str; 5] {
    ["workspace", "create", "--label", label, "--no-focus"]
}

fn forwarding_space_status(forwards: &[Forward]) -> String {
    let active = forwards.iter().filter(|forward| forward.enabled).count();
    let paused = forwards.len().saturating_sub(active);
    if paused == 0 {
        format!("{active} active")
    } else {
        format!("{active} active · {paused} paused")
    }
}

fn open_dashboard_here() -> Result<(), String> {
    let session_path = active_session_path()?;
    let pane_id = env::var("HERDR_PANE_ID")
        .map_err(|_| "select a Herdr pane before invoking this action".to_string())?;
    let executable = env::current_exe().map_err(|error| error.to_string())?;
    let command = dashboard_command(&executable, &session_path);
    herdr_output(&["pane", "run", &pane_id, &command])?;
    Ok(())
}

fn open_dashboard_popup() -> Result<(), String> {
    let session_path = active_session_path()?;
    open_dashboard_popup_for(&session_path)
}

fn open_dashboard_popup_for(session_path: &Path) -> Result<(), String> {
    open_popup_pane("dashboard-popup", session_path)
}

fn open_popup_pane(entrypoint: &str, session_path: &Path) -> Result<(), String> {
    let arguments = popup_pane_arguments(entrypoint, session_path);
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    herdr_output(&arguments).map(|_| ())
}

fn popup_pane_arguments(entrypoint: &str, session_path: &Path) -> Vec<String> {
    vec![
        "plugin".into(),
        "pane".into(),
        "open".into(),
        "--plugin".into(),
        "herdr.fwd".into(),
        "--entrypoint".into(),
        entrypoint.into(),
        "--env".into(),
        format!("HERDR_FWD_SESSION_PATH={}", session_path.display()),
    ]
}

fn dashboard_current() -> Result<(), String> {
    let session_path = env::var_os("HERDR_FWD_SESSION_PATH")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(active_session_path)?;
    dashboard(&session_path)
}

fn enable_sidebar_status() -> Result<(), String> {
    let path = enable_ports_row()?;
    herdr_output(&["server", "reload-config"])?;
    let _ = herdr_output(&[
        "notification",
        "show",
        "Port status enabled",
        "--body",
        "Space sidebar now shows forwarded ports",
    ]);
    println!("Enabled $port_forward_status in {}", path.display());
    Ok(())
}

fn dashboard_command(executable: &std::path::Path, session_path: &Path) -> String {
    dashboard_command_with_config(
        executable,
        session_path,
        env::var_os("HERDR_PLUGIN_CONFIG_DIR")
            .as_deref()
            .map(std::path::Path::new),
    )
}

fn dashboard_command_with_config(
    executable: &std::path::Path,
    session_path: &Path,
    config_directory: Option<&Path>,
) -> String {
    let config_prefix = config_directory.map_or_else(String::new, |path| {
        format!(
            "HERDR_PLUGIN_CONFIG_DIR={} ",
            shell_quote(&path.to_string_lossy())
        )
    });
    format!(
        "{config_prefix}{} dashboard {}",
        shell_quote(&executable.to_string_lossy()),
        shell_quote(&session_path.to_string_lossy())
    )
}

#[cfg(test)]
mod lifecycle_tests {
    use std::{
        collections::{HashMap, HashSet},
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use herdr_fwd::{registry::Forward, ForwardRequest};
    use serde_json::json;

    use super::{
        automatic_requests_to_create, close_dashboard_with, dashboard_command_with_config,
        forward_sidebar_token, forwarding_space_status, open_dashboard_space_with,
        pane_workspace_map, popup_pane_arguments, process_changed, workspace_create_arguments,
        workspace_port_tokens, DashboardMarker, DashboardOperations,
    };

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);
            let unique = format!(
                "herdr-fwd-lifecycle-test-{}-{}-{}",
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
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum DashboardFailure {
        PaneRun,
        Status,
        MarkerWrite,
    }

    struct TestDashboardOperations {
        failure: Option<DashboardFailure>,
        marker: DashboardMarker,
        calls: Vec<&'static str>,
    }

    impl TestDashboardOperations {
        fn failing(failure: DashboardFailure) -> Self {
            Self {
                failure: Some(failure),
                marker: DashboardMarker {
                    workspace_id: "workspace-1".into(),
                    pane_id: "workspace-1:pane-1".into(),
                    label: "Port Forwarding".into(),
                },
                calls: Vec::new(),
            }
        }

        fn with_marker(marker: DashboardMarker) -> Self {
            Self {
                failure: None,
                marker,
                calls: Vec::new(),
            }
        }
    }

    impl DashboardOperations for TestDashboardOperations {
        fn create_dashboard(&mut self, label: &str) -> Result<DashboardMarker, String> {
            self.calls.push("create");
            Ok(DashboardMarker {
                workspace_id: self.marker.workspace_id.clone(),
                pane_id: self.marker.pane_id.clone(),
                label: label.into(),
            })
        }

        fn run_dashboard(&mut self, _pane_id: &str, _command: &str) -> Result<(), String> {
            self.calls.push("pane run");
            (self.failure != Some(DashboardFailure::PaneRun))
                .then_some(())
                .ok_or_else(|| "pane launch failed".into())
        }

        fn report_status(&mut self, _workspace_id: &str, _status: &str) -> Result<(), String> {
            self.calls.push("status");
            (self.failure != Some(DashboardFailure::Status))
                .then_some(())
                .ok_or_else(|| "status failed".into())
        }

        fn write_marker(
            &mut self,
            _marker_path: &Path,
            _marker: &DashboardMarker,
        ) -> Result<(), String> {
            self.calls.push("marker write");
            (self.failure != Some(DashboardFailure::MarkerWrite))
                .then_some(())
                .ok_or_else(|| "marker write failed".into())
        }

        fn close_workspace(&mut self, _workspace_id: &str) -> Result<(), String> {
            self.calls.push("close");
            Ok(())
        }
    }

    #[test]
    fn reports_forwarding_space_live_and_paused_counts() {
        let active = Forward {
            id: "active".into(),
            remote_port: 5173,
            local_port: 5173,
            remote_host: "127.0.0.1".into(),
            pane_id: "w1:p1".into(),
            process: "Vite".into(),
            detected_url: "http://localhost:5173".into(),
            automatic: true,
            enabled: true,
            server_started_at: None,
            process_id: None,
            tunnel_opened_at: 0,
        };
        let paused = Forward {
            enabled: false,
            ..active.clone()
        };
        assert_eq!(
            forwarding_space_status(&[active, paused]),
            "1 active · 1 paused"
        );
    }

    #[test]
    fn detects_a_process_restart_by_pid_even_when_the_label_is_unchanged() {
        let forward = Forward {
            id: "vite".into(),
            remote_port: 5173,
            local_port: 5173,
            remote_host: "127.0.0.1".into(),
            pane_id: "w1:p1".into(),
            process: "Vite".into(),
            detected_url: "http://localhost:5173".into(),
            automatic: true,
            enabled: true,
            server_started_at: Some(1_722_000_000),
            process_id: Some(41),
            tunnel_opened_at: 1_722_000_000,
        };
        let active_processes = HashMap::from([("w1:p1".into(), "Vite".into())]);
        let active_process_ids = HashMap::from([("w1:p1".into(), HashSet::from([42]))]);

        assert!(process_changed(
            &forward,
            &active_processes,
            &active_process_ids
        ));
    }

    #[test]
    fn shortens_same_port_forwards_in_the_sidebar() {
        let forward = Forward {
            id: "vite".into(),
            remote_port: 5173,
            local_port: 5173,
            remote_host: "127.0.0.1".into(),
            pane_id: "w1:p1".into(),
            process: "Vite".into(),
            detected_url: "http://localhost:5173".into(),
            automatic: true,
            enabled: true,
            server_started_at: None,
            process_id: None,
            tunnel_opened_at: 0,
        };
        assert_eq!(
            forwarding_space_status(std::slice::from_ref(&forward)),
            "1 active"
        );
        assert_eq!(forward_sidebar_token(&forward), "→5173");
        let remapped = Forward {
            local_port: 5174,
            ..forward
        };
        assert_eq!(forward_sidebar_token(&remapped), "5173→5174");
    }

    #[test]
    fn passes_the_plugin_config_directory_to_managed_dashboards() {
        assert_eq!(
            dashboard_command_with_config(
                std::path::Path::new("/opt/plugin/dashboard"),
                std::path::Path::new("/tmp/session.json"),
                Some(std::path::Path::new("/tmp/plugin config")),
            ),
            "HERDR_PLUGIN_CONFIG_DIR='/tmp/plugin config' '/opt/plugin/dashboard' dashboard '/tmp/session.json'"
        );
    }

    #[test]
    fn maps_panes_to_their_source_workspaces() {
        let panes = json!({"result": {"panes": [
            {"pane_id": "w1:p1", "workspace_id": "w1"},
            {"pane_id": "w2:p3", "workspace_id": "w2"}
        ]}});
        assert_eq!(
            pane_workspace_map(&panes),
            HashMap::from([
                ("w1:p1".to_string(), "w1".to_string()),
                ("w2:p3".to_string(), "w2".to_string()),
            ])
        );
    }

    #[test]
    fn metadata_contains_only_enabled_automatic_workspace_forwards() {
        let automatic = Forward {
            id: "vite".into(),
            remote_port: 5173,
            local_port: 5174,
            remote_host: "127.0.0.1".into(),
            pane_id: "w1:p1".into(),
            process: "Vite".into(),
            detected_url: "http://localhost:5173".into(),
            automatic: true,
            enabled: true,
            server_started_at: None,
            process_id: None,
            tunnel_opened_at: 0,
        };
        let manual = Forward {
            id: "manual".into(),
            pane_id: "manual".into(),
            automatic: false,
            ..automatic.clone()
        };
        let paused = Forward {
            id: "paused".into(),
            enabled: false,
            ..automatic.clone()
        };
        let current_automatic = automatic.clone();
        let map = HashMap::from([("w1:p1".to_string(), "w1".to_string())]);
        assert_eq!(
            workspace_port_tokens(
                std::slice::from_ref(&automatic),
                &[current_automatic, manual, paused],
                &map
            ),
            HashMap::from([("w1".to_string(), Some("5173→5174".to_string()))])
        );
    }

    #[test]
    fn opens_popup_entrypoints_without_cli_only_placement_options() {
        assert_eq!(
            popup_pane_arguments("dashboard-popup", std::path::Path::new("/tmp/session.json")),
            [
                "plugin",
                "pane",
                "open",
                "--plugin",
                "herdr.fwd",
                "--entrypoint",
                "dashboard-popup",
                "--env",
                "HERDR_FWD_SESSION_PATH=/tmp/session.json",
            ]
            .map(str::to_owned)
            .to_vec()
        );
    }

    #[test]
    fn creates_the_forwarding_space_without_changing_focus() {
        assert_eq!(
            workspace_create_arguments("Port Forwarding"),
            [
                "workspace",
                "create",
                "--label",
                "Port Forwarding",
                "--no-focus"
            ]
        );
    }

    #[test]
    fn creates_every_new_detected_listener_without_approval() {
        let existing = Forward {
            id: "vite".into(),
            remote_port: 5173,
            local_port: 5173,
            remote_host: "127.0.0.1".into(),
            pane_id: "w1:p1".into(),
            process: "Vite".into(),
            detected_url: "http://localhost:5173/".into(),
            automatic: true,
            enabled: true,
            server_started_at: Some(100),
            process_id: Some(41),
            tunnel_opened_at: 100,
        };
        let detected = vec![
            ForwardRequest {
                remote_port: 5173,
                preferred_local_port: 5173,
                remote_host: "localhost".into(),
                pane_id: "w1:p1".into(),
                process: "Vite".into(),
                detected_url: "http://localhost:5173/".into(),
                automatic: true,
                server_started_at: Some(100),
                process_id: Some(41),
            },
            ForwardRequest {
                remote_port: 5174,
                preferred_local_port: 5174,
                remote_host: "localhost".into(),
                pane_id: "w1:p1".into(),
                process: "Vite".into(),
                detected_url: "http://localhost:5174/".into(),
                automatic: true,
                server_started_at: Some(100),
                process_id: Some(41),
            },
        ];

        assert_eq!(
            automatic_requests_to_create(detected, &[existing]),
            vec![ForwardRequest {
                remote_port: 5174,
                preferred_local_port: 5174,
                remote_host: "localhost".into(),
                pane_id: "w1:p1".into(),
                process: "Vite".into(),
                detected_url: "http://localhost:5174/".into(),
                automatic: true,
                server_started_at: Some(100),
                process_id: Some(41),
            }]
        );
    }

    #[test]
    fn closes_a_created_workspace_when_pane_launch_fails() {
        let directory = TestDirectory::new();
        let mut operations = TestDashboardOperations::failing(DashboardFailure::PaneRun);

        let error = open_dashboard_space_with(
            &mut operations,
            &directory.path().join("session.dashboard.json"),
            "0 active",
            "dashboard command",
        )
        .expect_err("pane launch should fail");

        assert_eq!(error, "pane launch failed");
        assert_eq!(operations.calls, ["create", "pane run", "close"]);
    }

    #[test]
    fn closes_a_created_workspace_when_status_reporting_fails() {
        let directory = TestDirectory::new();
        let mut operations = TestDashboardOperations::failing(DashboardFailure::Status);

        let error = open_dashboard_space_with(
            &mut operations,
            &directory.path().join("session.dashboard.json"),
            "0 active",
            "dashboard command",
        )
        .expect_err("status reporting should fail");

        assert_eq!(error, "status failed");
        assert_eq!(operations.calls, ["create", "pane run", "status", "close"]);
    }

    #[test]
    fn closes_a_created_workspace_when_marker_persistence_fails() {
        let directory = TestDirectory::new();
        let mut operations = TestDashboardOperations::failing(DashboardFailure::MarkerWrite);

        let error = open_dashboard_space_with(
            &mut operations,
            &directory.path().join("session.dashboard.json"),
            "0 active",
            "dashboard command",
        )
        .expect_err("marker persistence should fail");

        assert_eq!(error, "marker write failed");
        assert_eq!(
            operations.calls,
            ["create", "pane run", "status", "marker write", "close"]
        );
    }

    #[test]
    fn closes_a_created_workspace_when_the_root_pane_identifier_is_missing() {
        let directory = TestDirectory::new();

        for pane_id in ["", "   "] {
            let mut operations = TestDashboardOperations::with_marker(DashboardMarker {
                workspace_id: "workspace-1".into(),
                pane_id: pane_id.into(),
                label: "Port Forwarding".into(),
            });

            let error = open_dashboard_space_with(
                &mut operations,
                &directory.path().join(format!("{pane_id:?}.dashboard.json")),
                "0 active",
                "dashboard command",
            )
            .expect_err("a missing root pane identifier should fail");

            assert_eq!(error, "workspace.create did not return root pane_id");
            assert_eq!(operations.calls, ["create", "close"]);
        }
    }

    #[test]
    fn retains_the_marker_when_dashboard_close_fails() {
        let directory = TestDirectory::new();
        let marker_path = directory.path().join("session.dashboard.json");
        let marker = DashboardMarker {
            workspace_id: "workspace-1".into(),
            pane_id: "workspace-1:pane-1".into(),
            label: "Port Forwarding".into(),
        };
        fs::write(
            &marker_path,
            serde_json::to_vec(&marker).expect("marker should serialize"),
        )
        .expect("marker should be written");

        let error = close_dashboard_with(&marker_path, |_| Err("close failed".into()))
            .expect_err("close failure should be returned");

        assert_eq!(error, "close failed");
        assert!(marker_path.exists(), "marker should remain for a retry");
    }
}
