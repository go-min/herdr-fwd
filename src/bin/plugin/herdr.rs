use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    env,
    ffi::OsString,
    io::{self, BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Command, ExitStatus, Output, Stdio},
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub(crate) const RECONCILIATION_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

const SERVER_TOOLS: &[(&str, &str)] = &[
    ("storybook", "Storybook"),
    ("next", "Next.js"),
    ("astro", "Astro"),
    ("nuxt", "Nuxt"),
    ("svelte", "Svelte"),
    ("angular", "Angular"),
    ("react", "React"),
    ("webpack", "Webpack"),
    ("vite", "Vite"),
    ("parcel", "Parcel"),
    ("express", "Express"),
    ("fastify", "Fastify"),
    ("nest", "NestJS"),
    ("django", "Django"),
    ("fastapi", "FastAPI"),
    ("flask", "Flask"),
    ("uvicorn", "Uvicorn"),
    ("gunicorn", "Gunicorn"),
    ("rails", "Rails"),
    ("spring", "Spring Boot"),
    ("laravel", "Laravel"),
    ("artisan", "Laravel"),
    ("nginx", "Nginx"),
    ("caddy", "Caddy"),
    ("apache", "Apache"),
];

const RUNTIMES: &[(&str, &str)] = &[
    ("bun", "Bun"),
    ("deno", "Deno"),
    ("node", "Node"),
    ("python", "Python"),
    ("ruby", "Ruby"),
    ("cargo", "Rust"),
    ("rust", "Rust"),
    ("java", "Java"),
    ("php", "PHP"),
    ("docker", "Docker"),
];

pub(crate) fn run_command_with_timeout(
    program: impl AsRef<std::ffi::OsStr>,
    arguments: &[&str],
    timeout: Duration,
) -> Result<Output, String> {
    let program = program.as_ref();
    let program_name = program.to_string_lossy();
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to run {program_name}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("failed to capture {program_name} stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("failed to capture {program_name} stderr"))?;
    let stdout_reader = std::thread::spawn(move || read_command_stream(stdout));
    let stderr_reader = std::thread::spawn(move || read_command_stream(stderr));
    let started = Instant::now();

    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to wait for {program_name}: {error}"))?
        {
            #[cfg(unix)]
            // A completed leader can still have descendants holding the
            // inherited pipes open. Reap the whole private group before the
            // reader joins below.
            unsafe {
                let _ = libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
            }
            return collect_command_output(status, stdout_reader, stderr_reader, &program_name);
        }
        if started.elapsed() >= timeout {
            #[cfg(unix)]
            if unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) } == -1 {
                let error = io::Error::last_os_error();
                if child
                    .try_wait()
                    .map_err(|wait_error| {
                        format!("failed to wait for {program_name}: {wait_error}")
                    })?
                    .is_none()
                {
                    return Err(format!(
                        "failed to stop {program_name} after timeout: {error}"
                    ));
                }
                let _ = child.kill();
            }
            #[cfg(not(unix))]
            child
                .kill()
                .map_err(|error| format!("failed to stop {program_name} after timeout: {error}"))?;
            let status = child
                .wait()
                .map_err(|error| format!("failed to reap {program_name} after timeout: {error}"))?;
            let _ = collect_command_output(status, stdout_reader, stderr_reader, &program_name)?;
            return Err(format!(
                "{program_name} timed out after {}ms",
                timeout.as_millis()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_command_stream(mut stream: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    stream.read_to_end(&mut output)?;
    Ok(output)
}

fn collect_command_output(
    status: ExitStatus,
    stdout_reader: JoinHandle<io::Result<Vec<u8>>>,
    stderr_reader: JoinHandle<io::Result<Vec<u8>>>,
    program_name: &str,
) -> Result<Output, String> {
    Ok(Output {
        status,
        stdout: join_command_stream(stdout_reader, "stdout", program_name)?,
        stderr: join_command_stream(stderr_reader, "stderr", program_name)?,
    })
}

fn join_command_stream(
    reader: JoinHandle<io::Result<Vec<u8>>>,
    stream_name: &str,
    program_name: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("failed to read {stream_name} from {program_name}"))?
        .map_err(|error| format!("failed to read {stream_name} from {program_name}: {error}"))
}

pub(crate) fn herdr_output(arguments: &[&str]) -> Result<Output, String> {
    let binary = herdr_binary();
    let output = run_command_with_timeout(binary, arguments, RECONCILIATION_COMMAND_TIMEOUT)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn herdr_binary() -> OsString {
    let installed = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/bin/herdr"))
        .filter(|path| path.is_file());
    select_herdr_binary(env::var_os("HERDR_BIN_PATH"), installed)
}

fn select_herdr_binary(explicit: Option<OsString>, installed: Option<PathBuf>) -> OsString {
    if let Some(binary) = explicit.filter(|binary| !binary.is_empty()) {
        return binary;
    }
    if let Some(installed) = installed {
        return installed.into_os_string();
    }
    "herdr".into()
}

pub(crate) fn herdr_json(arguments: &[&str]) -> Result<Value, String> {
    let output = herdr_output(arguments)?;
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid Herdr JSON for {:?}: {error}", arguments))
}

pub(crate) fn loopback_listener_processes(
    process_ids: &[u32],
) -> Result<BTreeMap<u16, (u32, String)>, String> {
    let mut ports = BTreeMap::new();
    for process_id in process_ids {
        let process_id = process_id.to_string();
        let output = run_command_with_timeout(
            "lsof",
            &["-nP", "-a", "-p", &process_id, "-iTCP", "-sTCP:LISTEN"],
            RECONCILIATION_COMMAND_TIMEOUT,
        )
        .map_err(|error| format!("failed to run lsof for process {process_id}: {error}"))?;
        if output.status.success() {
            ports.extend(loopback_listener_processes_from_lsof(
                &String::from_utf8_lossy(&output.stdout),
            ));
        }
    }
    #[cfg(target_os = "linux")]
    if ports.is_empty() && !process_ids.is_empty() {
        if let Ok(output) =
            run_command_with_timeout("ss", &["-ltnp"], RECONCILIATION_COMMAND_TIMEOUT)
        {
            if output.status.success() {
                ports.extend(loopback_listener_processes_from_ss(
                    &String::from_utf8_lossy(&output.stdout),
                    process_ids,
                ));
            }
        }
    }
    Ok(ports)
}

pub(crate) fn loopback_listener_processes_from_lsof(output: &str) -> BTreeMap<u16, (u32, String)> {
    let mut ports = BTreeMap::new();
    for line in output.lines() {
        let Some(process_id) = line
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(endpoint) = line.split_once(" TCP ").map(|(_, value)| value) else {
            continue;
        };
        let endpoint = endpoint.split_whitespace().next().unwrap_or_default();
        let Some((host, port)) = endpoint.rsplit_once(':') else {
            continue;
        };
        if matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            if let Ok(port) = port.parse::<u16>() {
                if port != 0 {
                    let host = host.trim_matches(['[', ']']).to_string();
                    ports.entry(port).or_insert((process_id, host));
                }
            }
        }
    }
    ports
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn loopback_listener_processes_from_ss(
    output: &str,
    process_ids: &[u32],
) -> BTreeMap<u16, (u32, String)> {
    let process_ids = process_ids.iter().copied().collect::<HashSet<_>>();
    let mut ports = BTreeMap::new();
    for line in output.lines() {
        let Some(endpoint) = line.split_whitespace().nth(3) else {
            continue;
        };
        let Some((host, port)) = endpoint.rsplit_once(':') else {
            continue;
        };
        let host = host.trim_matches(['[', ']']);
        if !matches!(host, "127.0.0.1" | "::1") {
            continue;
        }
        let (Ok(port), Some(process_id)) = (
            port.parse::<u16>(),
            line.split("pid=")
                .skip(1)
                .filter_map(|value| {
                    value
                        .split_once(',')
                        .and_then(|(value, _)| value.parse::<u32>().ok())
                })
                .find(|process_id| process_ids.contains(process_id)),
        ) else {
            continue;
        };
        if port != 0 {
            ports.entry(port).or_insert((process_id, host.into()));
        }
    }
    ports
}

pub(crate) fn process_command_lines(process_ids: &[u32]) -> Result<HashMap<u32, String>, String> {
    if process_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let process_ids = process_ids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = run_command_with_timeout(
        "ps",
        &["-o", "pid=,command=", "-p", &process_ids],
        RECONCILIATION_COMMAND_TIMEOUT,
    )
    .map_err(|error| format!("failed to inspect listener commands: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "failed to inspect listener commands: ps exited with {}",
            output.status
        ));
    }
    Ok(process_command_lines_from_ps(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

pub(crate) fn process_command_lines_from_ps(output: &str) -> HashMap<u32, String> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (process_id, command) = line.split_once(char::is_whitespace)?;
            Some((process_id.parse().ok()?, command.trim_start().to_string()))
        })
        .filter(|(_, command)| !command.is_empty())
        .collect()
}

pub(crate) fn process_started_at(process_id: u32) -> Option<u64> {
    let process_id = process_id.to_string();
    let output = run_command_with_timeout(
        "ps",
        &["-o", "etime=", "-p", &process_id],
        RECONCILIATION_COMMAND_TIMEOUT,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let elapsed = parse_process_elapsed(&String::from_utf8_lossy(&output.stdout))?;
    Some(started_at(unix_time_now(), elapsed))
}

const MAX_PROCESS_TREE_NODES: usize = 512;

pub(crate) fn process_tree_ids(root_ids: &[u32], max_depth: u8) -> Result<Vec<u32>, String> {
    if max_depth == 0 {
        return Ok(process_tree_ids_from_parent_map(root_ids, &[], 0));
    }
    let output = run_command_with_timeout(
        "ps",
        &["-axo", "pid=,ppid="],
        RECONCILIATION_COMMAND_TIMEOUT,
    )
    .map_err(|error| format!("failed to discover child processes: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "failed to discover child processes: ps exited with {}",
            output.status
        ));
    }
    let parents = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect::<Vec<_>>();
    Ok(process_tree_ids_from_parent_map(
        root_ids, &parents, max_depth,
    ))
}

pub(crate) fn process_tree_ids_from_parent_map(
    root_ids: &[u32],
    parents: &[(u32, u32)],
    max_depth: u8,
) -> Vec<u32> {
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for &(process_id, parent_id) in parents {
        children.entry(parent_id).or_default().push(process_id);
    }
    for process_ids in children.values_mut() {
        process_ids.sort_unstable();
    }

    let max_depth = max_depth.min(herdr_fwd::MAX_PROCESS_TREE_DEPTH);
    let mut discovered = HashSet::new();
    let mut output = Vec::new();
    let mut pending = VecDeque::new();
    for &root_id in root_ids {
        if output.len() >= MAX_PROCESS_TREE_NODES {
            break;
        }
        if discovered.insert(root_id) {
            output.push(root_id);
            pending.push_back((root_id, 0));
        }
    }
    while let Some((process_id, depth)) = pending.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for &child_id in children.get(&process_id).into_iter().flatten() {
            if output.len() >= MAX_PROCESS_TREE_NODES {
                break;
            }
            if discovered.insert(child_id) {
                output.push(child_id);
                pending.push_back((child_id, depth + 1));
            }
        }
    }
    output
}

fn parse_process_elapsed(value: &str) -> Option<u64> {
    let (days, clock) = match value.trim().split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, value.trim()),
    };
    let parts = clock
        .split(':')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0, *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return None,
    };
    if minutes >= 60 || seconds >= 60 {
        return None;
    }
    days.checked_mul(86_400)?
        .checked_add(hours.checked_mul(3_600)?)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)
}

fn unix_time_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn started_at(now: u64, elapsed: u64) -> u64 {
    now.saturating_sub(elapsed)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneLocation {
    pub(crate) workspace_id: String,
    pub(crate) workspace: String,
    pub(crate) workspace_number: u64,
    pub(crate) tab_id: String,
    pub(crate) tab: String,
    pub(crate) tab_number: u64,
    pub(crate) pane_label: Option<String>,
}

impl Default for PaneLocation {
    fn default() -> Self {
        Self {
            workspace_id: String::new(),
            workspace: String::new(),
            workspace_number: u64::MAX,
            tab_id: String::new(),
            tab: String::new(),
            tab_number: u64::MAX,
            pane_label: None,
        }
    }
}

pub(crate) fn pane_locations(snapshot: &Value) -> HashMap<String, PaneLocation> {
    let mut workspaces = HashMap::new();
    let mut tabs = HashMap::new();
    let mut panes = Vec::new();
    collect_location_records(snapshot, &mut workspaces, &mut tabs, &mut panes);
    panes
        .into_iter()
        .map(|(pane_id, workspace_id, tab_id, pane_label)| {
            let (workspace, workspace_number) = workspaces
                .get(&workspace_id)
                .cloned()
                .unwrap_or_else(|| (workspace_id.clone(), u64::MAX));
            let (tab, tab_number) = tabs
                .get(&tab_id)
                .cloned()
                .unwrap_or_else(|| (tab_id.clone(), u64::MAX));
            (
                pane_id,
                PaneLocation {
                    workspace_id,
                    workspace,
                    workspace_number,
                    tab_id,
                    tab,
                    tab_number,
                    pane_label,
                },
            )
        })
        .collect()
}

fn collect_location_records(
    value: &Value,
    workspaces: &mut HashMap<String, (String, u64)>,
    tabs: &mut HashMap<String, (String, u64)>,
    panes: &mut Vec<(String, String, String, Option<String>)>,
) {
    match value {
        Value::Object(object) => {
            if object.get("tab_id").is_none() && object.get("pane_id").is_none() {
                if let (Some(workspace_id), Some(label)) = (
                    object.get("workspace_id").and_then(Value::as_str),
                    object.get("label").and_then(Value::as_str),
                ) {
                    workspaces.insert(
                        workspace_id.into(),
                        (
                            label.into(),
                            object
                                .get("number")
                                .and_then(Value::as_u64)
                                .unwrap_or(u64::MAX),
                        ),
                    );
                }
            }
            if object.get("pane_id").is_none() {
                if let (Some(tab_id), Some(label)) = (
                    object.get("tab_id").and_then(Value::as_str),
                    object.get("label").and_then(Value::as_str),
                ) {
                    tabs.insert(
                        tab_id.into(),
                        (
                            label.into(),
                            object
                                .get("number")
                                .and_then(Value::as_u64)
                                .unwrap_or(u64::MAX),
                        ),
                    );
                }
            }
            if let (Some(pane_id), Some(workspace_id), Some(tab_id)) = (
                object.get("pane_id").and_then(Value::as_str),
                object.get("workspace_id").and_then(Value::as_str),
                object.get("tab_id").and_then(Value::as_str),
            ) {
                let pane_label = object
                    .get("label")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .map(str::to_owned);
                panes.push((
                    pane_id.into(),
                    workspace_id.into(),
                    tab_id.into(),
                    pane_label,
                ));
            }
            for value in object.values() {
                collect_location_records(value, workspaces, tabs, panes);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_location_records(value, workspaces, tabs, panes);
            }
        }
        _ => {}
    }
}

pub(crate) fn process_label(value: &Value) -> String {
    process_label_from_strings(
        ["process_name", "name", "command"]
            .into_iter()
            .flat_map(|key| collect_string_values(value, key)),
    )
}

pub(crate) fn process_label_for_command(command: &str) -> String {
    process_label_from_strings([command.to_string()])
}

fn process_label_from_strings(labels: impl IntoIterator<Item = String>) -> String {
    let labels = labels
        .into_iter()
        .map(|label| herdr_fwd::detect::sanitize_display_text(&label))
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>();
    let normalized = labels
        .iter()
        .map(|label| label.to_ascii_lowercase())
        .collect::<Vec<_>>();

    if let Some(display) = matching_tool_label(&normalized, SERVER_TOOLS)
        .or_else(|| matching_tool_label(&normalized, RUNTIMES))
    {
        return display.into();
    }
    if normalized
        .iter()
        .any(|label| label == "go" || label.contains("golang") || label.starts_with("go "))
    {
        return "Go".into();
    }
    labels
        .first()
        .map(|label| label.chars().take(128).collect())
        .unwrap_or_else(|| "dev server".into())
}

fn matching_tool_label<'a>(labels: &[String], tools: &'a [(&str, &'a str)]) -> Option<&'a str> {
    tools
        .iter()
        .find(|(needle, _)| labels.iter().any(|label| label.contains(needle)))
        .map(|(_, display)| *display)
}

pub(crate) fn collect_string_values(value: &Value, key: &str) -> Vec<String> {
    let mut output = Vec::new();
    collect_values(value, key, &mut |value| {
        if let Some(value) = value.as_str() {
            output.push(value.to_string());
        }
    });
    output
}

pub(crate) fn collect_array_values(value: &Value, key: &str) -> Vec<Value> {
    let mut output = Vec::new();
    collect_values(value, key, &mut |value| {
        if let Some(values) = value.as_array() {
            output.extend(values.iter().cloned());
        }
    });
    output
}

pub(crate) fn first_string_value(value: &Value, key: &str) -> Option<String> {
    collect_string_values(value, key).into_iter().next()
}

fn collect_values(value: &Value, key: &str, visit: &mut impl FnMut(&Value)) {
    match value {
        Value::Object(object) => {
            for (candidate, value) in object {
                if candidate == key {
                    visit(value);
                }
                collect_values(value, key, visit);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_values(value, key, visit);
            }
        }
        _ => {}
    }
}

pub(crate) fn focus_pane(pane_id: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        socket_request("pane.focus", serde_json::json!({ "pane_id": pane_id }))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = pane_id;
        Err("pane focus is unsupported on this platform".into())
    }
}

pub(crate) fn report_workspace_port_forward_status(
    workspace_id: &str,
    status: Option<&str>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        socket_request(
            "workspace.report_metadata",
            serde_json::json!({
                "workspace_id": workspace_id,
                "source": "plugin:herdr.fwd",
                "tokens": { "port_forward_status": status },
            }),
        )?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace_id, status);
        Err("workspace metadata is unsupported on this platform".into())
    }
}

pub(crate) fn close_popup() -> Result<(), String> {
    #[cfg(unix)]
    {
        socket_request("popup.close", serde_json::json!({}))?;
        Ok(())
    }
    #[cfg(not(unix))]
    Err("popup close is unsupported on this platform".into())
}

#[cfg(unix)]
pub(crate) struct EventSubscriber {
    reader: BufReader<UnixStream>,
}

#[cfg(unix)]
impl EventSubscriber {
    pub(crate) fn connect() -> Result<Self, String> {
        let socket = herdr_socket_path()?;
        let mut stream = UnixStream::connect(socket)
            .map_err(|error| format!("failed to connect to Herdr socket: {error}"))?;
        let request = serde_json::json!({
            "id": "herdr-fwd:watch",
            "method": "events.subscribe",
            "params": {
                "subscriptions": [
                    { "type": "pane.updated" },
                    { "type": "pane.created" },
                    { "type": "pane.exited" },
                    { "type": "pane.closed" },
                ]
            }
        });
        writeln!(stream, "{request}").map_err(|error| error.to_string())?;
        let mut reader = BufReader::new(stream);
        let mut acknowledgement = String::new();
        reader
            .read_line(&mut acknowledgement)
            .map_err(|error| format!("failed to subscribe to Herdr events: {error}"))?;
        if acknowledgement.trim().is_empty() {
            return Err("Herdr closed the event subscription before acknowledgement".into());
        }
        let acknowledgement: Value = serde_json::from_str(&acknowledgement)
            .map_err(|error| format!("invalid Herdr event acknowledgement: {error}"))?;
        if let Some(error) = acknowledgement.get("error") {
            return Err(format!("Herdr event subscription failed: {error}"));
        }
        Ok(Self { reader })
    }

    /// Returns false on a reconciliation timeout and true for any server event.
    pub(crate) fn wait(&mut self, timeout: Duration) -> Result<bool, String> {
        self.reader
            .get_mut()
            .set_read_timeout(Some(timeout))
            .map_err(|error| format!("failed to set Herdr event timeout: {error}"))?;
        let mut event = String::new();
        match self.reader.read_line(&mut event) {
            Ok(0) => Err("Herdr event subscription closed".into()),
            Ok(_) => Ok(!event.trim().is_empty()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(format!("failed to read Herdr event: {error}")),
        }
    }
}

#[cfg(unix)]
fn socket_request(method: &str, params: Value) -> Result<Value, String> {
    let socket = herdr_socket_path()?;
    let mut stream = UnixStream::connect(socket)
        .map_err(|error| format!("failed to connect to Herdr socket: {error}"))?;
    let request = serde_json::json!({
        "id": "herdr-fwd:request",
        "method": method,
        "params": params,
    });
    writeln!(stream, "{request}").map_err(|error| error.to_string())?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|error| format!("failed to read Herdr response: {error}"))?;
    let response: Value = serde_json::from_str(&response)
        .map_err(|error| format!("invalid Herdr socket response: {error}"))?;
    if let Some(error) = response.get("error") {
        return Err(format!("Herdr socket request failed: {error}"));
    }
    Ok(response)
}

#[cfg(unix)]
fn herdr_socket_path() -> Result<std::ffi::OsString, String> {
    env::var_os("HERDR_SOCKET_PATH").ok_or_else(|| "HERDR_SOCKET_PATH is not set".to_string())
}

#[cfg(test)]
mod herdr_tests {
    use std::{
        collections::{BTreeMap, HashMap},
        ffi::OsString,
        fs,
        path::PathBuf,
        process::Command,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::{
        collect_array_values, collect_string_values, herdr_output,
        loopback_listener_processes_from_lsof, loopback_listener_processes_from_ss, pane_locations,
        parse_process_elapsed, process_command_lines_from_ps, process_label,
        process_tree_ids_from_parent_map, run_command_with_timeout, select_herdr_binary,
        started_at, PaneLocation,
    };

    #[test]
    fn expands_process_tree_only_to_the_configured_depth() {
        let parents = [(11, 10), (12, 11), (13, 12), (21, 20), (10, 13)];

        assert_eq!(
            process_tree_ids_from_parent_map(&[10], &parents, 0),
            vec![10]
        );
        assert_eq!(
            process_tree_ids_from_parent_map(&[10], &parents, 2),
            vec![10, 11, 12]
        );
        assert_eq!(
            process_tree_ids_from_parent_map(&[10, 20], &parents, 1),
            vec![10, 20, 11, 21]
        );
    }

    #[test]
    fn bounds_process_tree_expansion_while_preserving_the_root() {
        let parents = (1..=600)
            .map(|process_id| (process_id + 10, 10))
            .collect::<Vec<_>>();
        let ids = process_tree_ids_from_parent_map(&[10], &parents, 1);

        assert_eq!(ids.len(), 512);
        assert_eq!(&ids[..3], &[10, 11, 12]);
    }

    #[cfg(unix)]
    #[test]
    fn command_runner_returns_stdout_for_a_successful_command() {
        let output =
            run_command_with_timeout("sh", &["-c", "printf ready"], Duration::from_secs(1))
                .expect("successful command should return output");

        assert_eq!(output.stdout, b"ready");
    }

    #[cfg(unix)]
    #[test]
    fn command_runner_drains_large_stdout_before_the_child_exits() {
        let output = run_command_with_timeout(
            "sh",
            &["-c", "head -c 131072 /dev/zero"],
            Duration::from_secs(1),
        )
        .expect("large stdout should not block the child");

        assert_eq!(output.stdout.len(), 131_072);
    }

    #[cfg(unix)]
    #[test]
    fn command_runner_returns_output_for_a_non_zero_command() {
        let output = run_command_with_timeout(
            "sh",
            &["-c", "printf failure >&2; exit 7"],
            Duration::from_secs(1),
        )
        .expect("runner should return completed non-zero output");

        assert!(!output.status.success());
        assert_eq!(output.stderr, b"failure");
    }

    #[cfg(unix)]
    #[test]
    fn herdr_output_returns_trimmed_cli_stderr_for_a_non_zero_command() {
        let previous_binary = std::env::var_os("HERDR_BIN_PATH");
        std::env::set_var("HERDR_BIN_PATH", "sh");
        let result = herdr_output(&["-c", "printf '  Herdr failed\\n' >&2; exit 7"]);
        match previous_binary {
            Some(binary) => std::env::set_var("HERDR_BIN_PATH", binary),
            None => std::env::remove_var("HERDR_BIN_PATH"),
        }

        assert_eq!(result, Err("Herdr failed".into()));
    }

    #[cfg(unix)]
    #[test]
    fn command_runner_returns_a_timeout_error_within_the_deadline() {
        let started = Instant::now();
        let error = run_command_with_timeout("sh", &["-c", "sleep 5"], Duration::from_millis(50))
            .expect_err("sleeping command should time out");

        assert!(error.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn command_runner_reaps_a_timed_out_child() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let pid_path = std::env::temp_dir().join(format!("herdr-fwd-child-{unique}.pid"));
        let pid_path = pid_path
            .to_str()
            .expect("temporary directory path should be valid UTF-8");
        let script = "printf '%s\\n' \"$$\" > \"$1\"; exec sleep 5";

        let error = run_command_with_timeout(
            "sh",
            &["-c", script, "sh", pid_path],
            Duration::from_millis(50),
        )
        .expect_err("sleeping command should time out");
        assert!(error.contains("timed out"));

        let process_id = fs::read_to_string(pid_path)
            .expect("timed-out child should have written its process ID")
            .trim()
            .parse::<u32>()
            .expect("child process ID should be numeric");
        fs::remove_file(pid_path).expect("child process ID file should be removable");
        let process_id = process_id.to_string();
        let status = Command::new("sh")
            .args(["-c", "kill -0 \"$1\"", "sh", &process_id])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("process probe should run");

        assert!(!status.success(), "timed-out child must be reaped");
    }

    #[cfg(unix)]
    #[test]
    fn command_runner_kills_descendants_that_hold_output_pipes() {
        let started = Instant::now();
        let error =
            run_command_with_timeout("sh", &["-c", "(sleep 5)& wait"], Duration::from_millis(50))
                .expect_err("descendant should be terminated with the timed-out command");

        assert!(error.contains("timed out"));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "runner must not wait for descendants after timeout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_runner_does_not_join_a_descendant_after_leader_exits() {
        let started = Instant::now();
        let output = run_command_with_timeout(
            "sh",
            &["-c", "(sleep 5)& printf ready"],
            Duration::from_millis(500),
        )
        .expect("reader joins must be bounded after the leader exits");

        assert_eq!(output.stdout, b"ready");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "runner must not wait for a descendant after leader exit"
        );
    }

    #[test]
    fn recursively_extracts_herdr_response_fields() {
        let response = json!({
            "result": {
                "workspace": {"workspace_id": "w2"},
                "root_pane": {"pane_id": "w2:p1"},
                "foreground_processes": [{"name": "vite"}]
            }
        });
        assert_eq!(collect_string_values(&response, "workspace_id"), ["w2"]);
        assert_eq!(collect_string_values(&response, "pane_id"), ["w2:p1"]);
        assert_eq!(
            collect_array_values(&response, "foreground_processes").len(),
            1
        );
        assert_eq!(process_label(&response), "Vite");
    }

    #[test]
    fn prefers_a_specific_server_tool_over_its_runtime() {
        for (runtime, command, expected) in [
            (
                "node",
                "node node_modules/storybook/bin/index.cjs dev",
                "Storybook",
            ),
            (
                "node",
                "node node_modules/next/dist/bin/next dev",
                "Next.js",
            ),
            (
                "node",
                "node node_modules/fastify-cli/cli.js start",
                "Fastify",
            ),
            ("python", "python -m uvicorn app:app", "Uvicorn"),
            ("python", "python -m fastapi dev main.py", "FastAPI"),
            ("ruby", "bin/rails server", "Rails"),
            ("java", "java -jar spring-boot-app.jar", "Spring Boot"),
        ] {
            let response = json!({
                "foreground_processes": [{"name": runtime, "command": command}]
            });
            assert_eq!(process_label(&response), expected, "{command}");
        }
    }

    #[test]
    fn resolves_the_installer_path_when_dashboard_lacks_herdr_bin_path() {
        let installed = PathBuf::from("/home/example/.local/bin/herdr");
        assert_eq!(
            select_herdr_binary(None, Some(installed.clone())),
            installed.clone().into_os_string()
        );
        assert_eq!(
            select_herdr_binary(Some(OsString::from("/custom/herdr")), Some(installed)),
            OsString::from("/custom/herdr")
        );
    }

    #[test]
    fn maps_listener_ports_to_their_owning_processes() {
        let output = "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\nnode 123 user 20u IPv4 0x0 0t0 TCP 127.0.0.1:5173 (LISTEN)\nnode 456 user 21u IPv4 0x0 0t0 TCP localhost:3000 (LISTEN)\nnode 789 user 22u IPv6 0x0 0t0 TCP [::1]:4000 (LISTEN)\nnode 123 user 23u IPv4 0x0 0t0 TCP *:8080 (LISTEN)\nnode 123 user 24u IPv4 0x0 0t0 TCP 192.0.2.1:9000 (LISTEN)\n";

        assert_eq!(
            loopback_listener_processes_from_lsof(output),
            BTreeMap::from([
                (3000, (456, "localhost".into())),
                (4000, (789, "::1".into())),
                (5173, (123, "127.0.0.1".into())),
            ])
        );
    }

    #[test]
    fn maps_loopback_listener_processes_from_ss_when_lsof_omits_them() {
        let output = "State  Recv-Q Send-Q Local Address:Port Peer Address:PortProcess\nLISTEN 0      511        127.0.0.1:4000      0.0.0.0:*    users:((\"next-server (v1\",pid=679946,fd=24))\nLISTEN 0      511            [::1]:4001         [::]:*    users:((\"node\",pid=42,fd=18))\nLISTEN 0      511               *:4002            *:*    users:((\"node\",pid=43,fd=18))\n";
        assert_eq!(
            loopback_listener_processes_from_ss(output, &[679946, 42]),
            BTreeMap::from([
                (4000, (679946, "127.0.0.1".into())),
                (4001, (42, "::1".into())),
            ])
        );
    }

    #[test]
    fn maps_listener_process_ids_to_their_full_commands() {
        let output =
            "  123 node node_modules/storybook/bin/index.cjs dev\n456 python -m uvicorn app:app\n";
        assert_eq!(
            process_command_lines_from_ps(output),
            HashMap::from([
                (123, "node node_modules/storybook/bin/index.cjs dev".into()),
                (456, "python -m uvicorn app:app".into()),
            ])
        );
    }

    #[test]
    fn converts_process_elapsed_seconds_to_a_start_timestamp() {
        assert_eq!(started_at(1_722_000_120, 120), 1_722_000_000);
        assert_eq!(started_at(60, 120), 0);
    }

    #[test]
    fn parses_portable_ps_elapsed_time_formats() {
        assert_eq!(parse_process_elapsed("07:05"), Some(425));
        assert_eq!(parse_process_elapsed("01:02:03"), Some(3_723));
        assert_eq!(parse_process_elapsed("2-01:02:03"), Some(176_523));
        assert_eq!(parse_process_elapsed("not-a-duration"), None);
    }

    #[test]
    fn maps_panes_to_named_workspace_and_tab_locations() {
        let snapshot = json!({
            "result": {
                "workspaces": [{"workspace_id": "w1", "label": "Apps", "number": 3}],
                "tabs": [{"tab_id": "w1:t2", "workspace_id": "w1", "label": "Tooling", "number": 2}],
                "panes": [{"pane_id": "w1:p3", "workspace_id": "w1", "tab_id": "w1:t2", "label": "server"}]
            }
        });

        assert_eq!(
            pane_locations(&snapshot).get("w1:p3"),
            Some(&PaneLocation {
                workspace_id: "w1".into(),
                workspace: "Apps".into(),
                workspace_number: 3,
                tab_id: "w1:t2".into(),
                tab: "Tooling".into(),
                tab_number: 2,
                pane_label: Some("server".into()),
            })
        );
    }
}
