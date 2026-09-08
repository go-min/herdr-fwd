use std::{
    collections::HashSet,
    env,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use crate::local::http::{Method, Request, Server};
use herdr_fwd::{
    constant_time_eq,
    registry::{Forward, Registry, Ssh},
    ForwardRequest, RemoteSessionConfig, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};

use crate::local::{
    ssh::{remote_session_install_command, SshClient},
    support::{debug_log, open_browser, secure_random_bytes, write_private_json},
};

const MAX_ACTIVE_REQUESTS: usize = 64;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalSessionState {
    pub(crate) session_id: String,
    pub(crate) target: String,
    pub(crate) companion_url: String,
    pub(crate) token: String,
    #[serde(default)]
    pub(crate) wrapper_pid: u32,
    pub(crate) forwards: Vec<Forward>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManualForward {
    pub(crate) remote_port: u16,
    pub(crate) local_port: u16,
    #[serde(default = "default_remote_host")]
    pub(crate) remote_host: String,
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
}

fn default_enabled() -> bool {
    true
}

fn default_remote_host() -> String {
    "localhost".into()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PausedAutomaticForward {
    remote_port: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToggleRequest {
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct LocalPortRequest {
    local_port: u16,
}

pub(crate) struct CompanionState<S: Ssh> {
    pub(crate) session_id: String,
    pub(crate) target: String,
    pub(crate) companion_url: String,
    pub(crate) token: String,
    pub(crate) wrapper_pid: u32,
    pub(crate) registry: Mutex<Registry<S>>,
    pub(crate) state_path: PathBuf,
    pub(crate) manual_path: PathBuf,
    pub(crate) paused_path: PathBuf,
    pub(crate) last_heartbeat: Mutex<Option<Instant>>,
    pub(crate) open_new: bool,
    pub(crate) reconnecting: AtomicBool,
}

impl<S: Ssh> CompanionState<S> {
    pub(crate) fn persist(&self) -> Result<(), String> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| "forward registry lock poisoned".to_string())?;
        self.persist_locked(&registry)
    }

    pub(crate) fn persist_locked(&self, registry: &Registry<S>) -> Result<(), String> {
        let forwards = registry.forwards.values().cloned().collect();
        write_private_json(
            &self.state_path,
            &LocalSessionState {
                session_id: self.session_id.clone(),
                target: self.target.clone(),
                companion_url: self.companion_url.clone(),
                token: self.token.clone(),
                wrapper_pid: self.wrapper_pid,
                forwards,
            },
        )
    }

    pub(crate) fn restore_manual(&self) -> Result<(), String> {
        let bytes = match std::fs::read(&self.manual_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let mappings = serde_json::from_slice::<Vec<ManualForward>>(&bytes)
            .map_err(|error| error.to_string())?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "forward registry lock poisoned".to_string())?;
        for mapping in mappings {
            let detected_host = if mapping.remote_host == "::1" {
                "[::1]".to_string()
            } else {
                mapping.remote_host.clone()
            };
            let request = ForwardRequest {
                remote_port: mapping.remote_port,
                preferred_local_port: mapping.local_port,
                remote_host: mapping.remote_host,
                pane_id: "manual".into(),
                process: "Manual".into(),
                detected_url: format!("http://{}:{}", detected_host, mapping.remote_port),
                automatic: false,
                server_started_at: None,
                process_id: None,
            };
            if mapping.enabled {
                registry.create(&request)?;
            } else {
                registry.create_paused(&request)?;
            }
        }
        Ok(())
    }

    fn persist_preferences_delta(
        &self,
        before: &std::collections::BTreeMap<String, Forward>,
        registry: &Registry<S>,
    ) -> Result<(), String> {
        let mut manual: Vec<ManualForward> = read_saved(&self.manual_path)?;
        let mut paused = self.paused_automatic_ports()?;
        for (id, previous) in before {
            if !previous.automatic && !registry.forwards.contains_key(id) {
                manual.retain(|saved| !manual_matches(saved, previous));
            }
        }
        for (id, current) in &registry.forwards {
            let previous = before.get(id);
            if current.automatic {
                if !current.enabled && previous.is_none_or(|old| old.enabled) {
                    paused.insert(current.remote_port);
                } else if current.enabled && previous.is_some_and(|old| !old.enabled) {
                    paused.remove(&current.remote_port);
                }
            } else {
                let updated = manual_mapping(current);
                if previous.map(manual_mapping).as_ref() != Some(&updated) {
                    manual.retain(|saved| !manual_matches(saved, current));
                    manual.push(updated);
                }
            }
        }
        write_private_json(&self.manual_path, &manual)?;
        let mut paused = paused.into_iter().collect::<Vec<_>>();
        paused.sort_unstable();
        let paused = paused
            .into_iter()
            .map(|remote_port| PausedAutomaticForward { remote_port })
            .collect::<Vec<_>>();
        write_private_json(&self.paused_path, &paused)
    }

    fn mutate_and_persist<T>(
        &self,
        persist_on_mutation_error: bool,
        mutate: impl FnOnce(&mut Registry<S>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "registry unavailable".to_string())?;
        // All companions sharing these preferences serialize their read/modify/write
        // transaction. Only this mutation's delta is applied to the latest disk state.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.manual_path.with_extension("lock"))
            .map_err(|error| format!("failed to persist forwarding state: {error}"))?;
        fs2::FileExt::lock_exclusive(&lock).map_err(|error| error.to_string())?;
        let saved = [&self.manual_path, &self.paused_path]
            .into_iter()
            .map(|path| match std::fs::read(path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(format!("failed to persist forwarding state: {error}")),
            })
            .collect::<Result<Vec<_>, String>>()?;
        let snapshot = registry.snapshot();
        let before = registry.forwards.clone();
        let mutation = mutate(&mut registry);
        if mutation.is_err() && !persist_on_mutation_error {
            return mutation;
        }
        let persistence = self
            .persist_preferences_delta(&before, &registry)
            .and_then(|_| self.persist_locked(&registry));
        if let Err(persist_error) = persistence {
            let mut error = format!("failed to persist forwarding state: {persist_error}");
            if let Err(rollback) = registry.restore(snapshot) {
                error.push_str(&format!("; tunnel rollback failed: {rollback}"));
            }
            for (path, bytes) in [&self.manual_path, &self.paused_path]
                .into_iter()
                .zip(saved)
            {
                let restored = match bytes {
                    Some(bytes) => herdr_fwd::atomic::write_file(path, &bytes, 0o600),
                    None => match std::fs::remove_file(path) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(error) => Err(error.to_string()),
                    },
                };
                if let Err(restore_error) = restored {
                    error.push_str(&format!("; rollback preferences failed: {restore_error}"));
                }
            }
            if let Err(restore_error) = self.persist_locked(&registry) {
                error.push_str(&format!(
                    "; rollback state persistence failed: {restore_error}"
                ));
            }
            return Err(error);
        }
        mutation
    }

    pub(crate) fn paused_automatic_ports(&self) -> Result<HashSet<u16>, String> {
        let bytes = match std::fs::read(&self.paused_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
            Err(error) => return Err(error.to_string()),
        };
        serde_json::from_slice::<Vec<PausedAutomaticForward>>(&bytes)
            .map_err(|error| error.to_string())
            .map(|forwards| {
                forwards
                    .into_iter()
                    .map(|forward| forward.remote_port)
                    .collect()
            })
    }
}

fn read_saved<T: for<'de> Deserialize<'de>>(path: &std::path::Path) -> Result<Vec<T>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn manual_mapping(forward: &Forward) -> ManualForward {
    ManualForward {
        remote_port: forward.remote_port,
        local_port: forward.local_port,
        remote_host: forward.remote_host.clone(),
        enabled: forward.enabled,
    }
}

fn manual_matches(saved: &ManualForward, forward: &Forward) -> bool {
    let normalize = |host: &str| {
        if matches!(host, "::1" | "[::1]") {
            "::1"
        } else {
            "127.0.0.1"
        }
    };
    saved.remote_port == forward.remote_port
        && normalize(&saved.remote_host) == normalize(&forward.remote_host)
}

pub(crate) fn manual_forwards_path(host: &str, herdr_session: &str) -> Result<PathBuf, String> {
    persistent_forwards_path(host, herdr_session, "manual")
}

pub(crate) fn paused_forwards_path(host: &str, herdr_session: &str) -> Result<PathBuf, String> {
    persistent_forwards_path(host, herdr_session, "paused")
}

fn persistent_forwards_path(
    host: &str,
    herdr_session: &str,
    kind: &str,
) -> Result<PathBuf, String> {
    let home = env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    let directory = PathBuf::from(home).join(".config/herdr-fwd");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    Ok(directory.join(persistent_forwards_file_name(host, herdr_session, kind)))
}

fn persistent_forwards_file_name(host: &str, herdr_session: &str, kind: &str) -> String {
    format!(
        "{kind}-{}-{}.json",
        persistent_key_component(host),
        persistent_key_component(herdr_session),
    )
}

fn persistent_key_component(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) struct CompanionServer {
    accept_thread: thread::JoinHandle<()>,
    requests: Arc<ActiveRequests>,
}

impl CompanionServer {
    pub(crate) fn join(self) -> thread::Result<()> {
        let result = self.accept_thread.join();
        self.requests.wait_until_idle();
        result
    }
}

#[derive(Default)]
struct ActiveRequests {
    count: Mutex<usize>,
    idle: Condvar,
}

impl ActiveRequests {
    fn begin(self: &Arc<Self>) -> Option<ActiveRequest> {
        let mut count = self.count.lock().ok()?;
        if *count >= MAX_ACTIVE_REQUESTS {
            return None;
        }
        *count += 1;
        Some(ActiveRequest {
            requests: Arc::clone(self),
        })
    }

    fn wait_until_idle(&self) {
        let Ok(mut count) = self.count.lock() else {
            return;
        };
        while *count != 0 {
            let Ok(next) = self.idle.wait(count) else {
                return;
            };
            count = next;
        }
    }
}

struct ActiveRequest {
    requests: Arc<ActiveRequests>,
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        if let Ok(mut count) = self.requests.count.lock() {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.requests.idle.notify_all();
            }
        }
    }
}

pub(crate) fn spawn_server(
    server: Server,
    state: Arc<CompanionState<impl Ssh>>,
    stop: Arc<AtomicBool>,
) -> CompanionServer {
    let requests = Arc::new(ActiveRequests::default());
    let request_tracker = Arc::clone(&requests);
    let accept_thread = thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match server.recv_timeout(Duration::from_millis(250)) {
                Ok(Some(mut stream)) => {
                    let state = Arc::clone(&state);
                    let Some(active_request) = request_tracker.begin() else {
                        let _ = crate::local::http::respond(
                            &mut stream,
                            503,
                            "{\"error\":\"companion is busy\"}",
                        );
                        continue;
                    };
                    let stop = stop.clone();
                    thread::spawn(move || {
                        let _active_request = active_request;
                        match Request::read(stream, &stop) {
                            Ok(request) => handle_request(request, &state),
                            Err(error) => debug_log(&error),
                        }
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    debug_log(&format!("companion server error: {error}"));
                    break;
                }
            }
        }
    });
    CompanionServer {
        accept_thread,
        requests,
    }
}

fn handle_request(request: Request, state: &Arc<CompanionState<impl Ssh>>) {
    let method = request.method();
    let path = request
        .url()
        .split('?')
        .next()
        .unwrap_or(request.url())
        .to_string();
    if method == Method::Get && path == "/health" {
        respond_json(
            request,
            200,
            &serde_json::json!({
                "status": "ok",
                "protocolVersion": PROTOCOL_VERSION,
                "version": env!("CARGO_PKG_VERSION")
            }),
        );
        return;
    }
    if !authorized(&request, &state.token) {
        respond_error(request, 401, "unauthorized");
        return;
    }
    if path.starts_with("/v1/forwards") && state.reconnecting.load(Ordering::SeqCst) {
        respond_error(request, 503, "SSH connection is recovering; retry shortly");
        return;
    }
    match (method, path.as_str()) {
        (Method::Get, "/v1/settings/local") => {
            match herdr_fwd::herdr_config::dashboard_setup_status() {
                Ok(status) => respond_json(request, 200, &status),
                Err(error) => respond_error(request, 500, &error),
            }
        }
        (Method::Post, "/v1/settings/sidebar") => {
            let payload = match read_json::<ToggleRequest>(&request) {
                Ok(payload) => payload,
                Err(error) => {
                    respond_error(request, 400, &error);
                    return;
                }
            };
            match herdr_fwd::herdr_config::set_ports_row(payload.enabled, || {
                let output = std::process::Command::new("herdr")
                    .arg("--default-config")
                    .output()
                    .map_err(|error| error.to_string())?;
                if !output.status.success() {
                    return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
                }
                String::from_utf8(output.stdout).map_err(|error| error.to_string())
            }) {
                Ok(status) => respond_json(request, 200, &status),
                Err(error) => respond_error(request, 500, &error),
            }
        }
        (Method::Post, "/v1/settings/notifications") => {
            match herdr_fwd::herdr_config::enable_herdr_notifications() {
                Ok(status) => respond_json(request, 200, &status),
                Err(error) => respond_error(request, 500, &error),
            }
        }
        (Method::Post, "/v1/heartbeat") => {
            if let Ok(mut heartbeat) = state.last_heartbeat.lock() {
                *heartbeat = Some(Instant::now());
            }
            respond_json(request, 200, &serde_json::json!({"status": "ok"}));
        }
        (Method::Get, "/v1/forwards") => {
            let forwards = state
                .registry
                .lock()
                .map(|registry| registry.forwards.values().cloned().collect::<Vec<_>>());
            match forwards {
                Ok(forwards) => respond_json(request, 200, &forwards),
                Err(_) => respond_error(request, 500, "registry unavailable"),
            }
        }
        (Method::Post, "/v1/forwards") => {
            let payload = match read_json::<ForwardRequest>(&request) {
                Ok(payload) => payload,
                Err(error) => {
                    respond_error(request, 400, &error);
                    return;
                }
            };
            let result = state.mutate_and_persist(false, |registry| {
                let start_paused = payload.automatic
                    && state
                        .paused_automatic_ports()?
                        .contains(&payload.remote_port);
                if start_paused {
                    registry.create_paused(&payload)
                } else {
                    registry.create(&payload)
                }
            });
            match result {
                Ok((forward, created)) => {
                    if created && forward.enabled && state.open_new {
                        if let Err(error) = open_browser(&forward.local_url()) {
                            debug_log(&format!("failed to open browser: {error}"));
                        }
                    }
                    respond_json(request, if created { 201 } else { 200 }, &forward);
                }
                Err(error) if error.starts_with("failed to persist forwarding state:") => {
                    respond_error(request, 500, &error)
                }
                Err(error) => respond_error(request, 422, &error),
            }
        }
        (Method::Delete, path) if path.starts_with("/v1/forwards/") => {
            let id = path.trim_start_matches("/v1/forwards/");
            if id.is_empty() || id.contains('/') {
                respond_error(request, 404, "forward not found");
                return;
            }
            let result = state.mutate_and_persist(false, |registry| registry.remove(id));
            match result {
                Ok(Some(forward)) => respond_json(request, 200, &forward),
                Ok(None) => respond_error(request, 404, "forward not found"),
                Err(error) if error.starts_with("failed to persist forwarding state:") => {
                    respond_error(request, 500, &error)
                }
                Err(error) => respond_error(request, 502, &error),
            }
        }
        (Method::Post, path) if path.starts_with("/v1/forwards/") && path.ends_with("/open") => {
            let id = path
                .trim_start_matches("/v1/forwards/")
                .trim_end_matches("/open")
                .trim_end_matches('/');
            let forward = state
                .registry
                .lock()
                .ok()
                .and_then(|registry| registry.forwards.get(id).cloned());
            if let Some(forward) = forward.filter(|forward| forward.enabled) {
                match open_browser(&forward.local_url()) {
                    Ok(()) => respond_json(request, 200, &serde_json::json!({"status": "opened"})),
                    Err(error) => respond_error(request, 500, &error),
                }
            } else {
                respond_error(request, 404, "forward not found");
            }
        }
        (Method::Post, path) if path.starts_with("/v1/forwards/") && path.ends_with("/toggle") => {
            let id = path
                .trim_start_matches("/v1/forwards/")
                .trim_end_matches("/toggle")
                .trim_end_matches('/');
            if id.is_empty() || id.contains('/') {
                respond_error(request, 404, "forward not found");
                return;
            }
            let payload = match read_json::<ToggleRequest>(&request) {
                Ok(payload) => payload,
                Err(error) => {
                    respond_error(request, 400, &error);
                    return;
                }
            };
            let result = state
                .mutate_and_persist(false, |registry| registry.set_enabled(id, payload.enabled));
            match result {
                Ok(Some(forward)) => respond_json(request, 200, &forward),
                Ok(None) => respond_error(request, 404, "forward not found"),
                Err(error) if error.starts_with("failed to persist forwarding state:") => {
                    respond_error(request, 500, &error)
                }
                Err(error) => respond_error(request, 502, &error),
            }
        }
        (Method::Post, path)
            if path.starts_with("/v1/forwards/") && path.ends_with("/local-port") =>
        {
            let id = path
                .trim_start_matches("/v1/forwards/")
                .trim_end_matches("/local-port")
                .trim_end_matches('/');
            if id.is_empty() || id.contains('/') {
                respond_error(request, 404, "forward not found");
                return;
            }
            let payload = match read_json::<LocalPortRequest>(&request) {
                Ok(payload) => payload,
                Err(error) => {
                    respond_error(request, 400, &error);
                    return;
                }
            };
            let result = state.mutate_and_persist(true, |registry| {
                registry.set_local_port(id, payload.local_port)
            });
            match result {
                Ok(Some(forward)) => respond_json(request, 200, &forward),
                Ok(None) => respond_error(request, 404, "forward not found"),
                Err(error) if error.starts_with("failed to persist forwarding state:") => {
                    respond_error(request, 500, &error)
                }
                Err(error) => respond_error(request, 422, &error),
            }
        }
        _ => respond_error(request, 404, "not found"),
    }
}

fn authorized(request: &Request, expected_token: &str) -> bool {
    request
        .header("Authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token, expected_token))
}

fn read_json<T: for<'de> Deserialize<'de>>(request: &Request) -> Result<T, String> {
    serde_json::from_slice(request.body()).map_err(|error| format!("invalid JSON: {error}"))
}

fn respond_json<T: Serialize>(request: Request, status: u16, value: &T) {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".into());
    let _ = request.respond(status, &body);
}

fn respond_error(request: Request, status: u16, message: &str) {
    respond_json(request, status, &serde_json::json!({"error": message}));
}

pub(crate) fn establish_reverse_rpc(client: &SshClient, local_port: u16) -> Result<u16, String> {
    establish_reverse_rpc_until(client, local_port, || false)
}

pub(crate) fn establish_reverse_rpc_until(
    client: &SshClient,
    local_port: u16,
    cancelled: impl Fn() -> bool,
) -> Result<u16, String> {
    for _ in 0..20 {
        if cancelled() {
            return Err("reverse tunnel recovery cancelled".into());
        }
        let bytes = secure_random_bytes(2)?;
        let candidate = 20_000 + (u16::from_be_bytes([bytes[0], bytes[1]]) % 30_000);
        if client.reverse(candidate, local_port).is_ok() {
            return Ok(candidate);
        }
    }
    Err("failed to allocate remote loopback RPC port after 20 attempts".into())
}

pub(crate) fn install_remote_session(
    client: &SshClient,
    remote_path: &str,
    config: &RemoteSessionConfig,
) -> Result<(), String> {
    config.validate()?;
    let json = serde_json::to_string(config).map_err(|error| error.to_string())?;
    client.remote_command_with_stdin(
        &remote_session_install_command(remote_path),
        json.as_bytes(),
    )?;
    Ok(())
}

pub(crate) fn cleanup_registry(state: &CompanionState<impl Ssh>) {
    let errors = state
        .registry
        .lock()
        .map(|mut registry| registry.close_all())
        .unwrap_or_else(|_| vec!["registry lock poisoned".into()]);
    for error in errors {
        eprintln!("warning: failed to cancel forward: {error}");
    }
}

#[cfg(test)]
mod tests;
