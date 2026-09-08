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

fn handle_request(mut request: Request, state: &Arc<CompanionState<impl Ssh>>) {
    let method = request.method().clone();
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
            let payload = match read_json::<ToggleRequest>(&mut request) {
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
            let payload = match read_json::<ForwardRequest>(&mut request) {
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
            let payload = match read_json::<ToggleRequest>(&mut request) {
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
            let payload = match read_json::<LocalPortRequest>(&mut request) {
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

fn read_json<T: for<'de> Deserialize<'de>>(request: &mut Request) -> Result<T, String> {
    serde_json::from_slice(&request.body).map_err(|error| format!("invalid JSON: {error}"))
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
mod companion_tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::local::{management::http_request, support::RuntimeDirectory};

    #[derive(Clone, Default)]
    struct FakeSsh {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Ssh for FakeSsh {
        fn forward(&self, local: u16, host: &str, remote: u16) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("forward:{local}:{host}:{remote}"));
            Ok(())
        }

        fn cancel(&self, local: u16, host: &str, remote: u16) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("cancel:{local}:{host}:{remote}"));
            Ok(())
        }
    }

    #[test]
    fn persistent_forwards_are_scoped_to_the_remote_herdr_session() {
        let default_manual = persistent_forwards_file_name("demo-host", "default", "manual");
        let review_manual = persistent_forwards_file_name("demo-host", "review", "manual");
        let default_paused = persistent_forwards_file_name("demo-host", "default", "paused");
        let review_paused = persistent_forwards_file_name("demo-host", "review", "paused");

        assert_ne!(default_manual, review_manual);
        assert_ne!(default_paused, review_paused);
    }

    fn raw_request(address: &str, request: &str) -> String {
        use std::net::TcpStream;
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn local_settings_require_auth_preserve_config_and_serialize_writes() {
        // No other hfwd test changes or reads HERDR_CONFIG_PATH. Restore it even
        // on assertion failure so this test never leaves a test config selected.
        struct RestoreConfig(Option<std::ffi::OsString>);
        impl Drop for RestoreConfig {
            fn drop(&mut self) {
                match &self.0 {
                    Some(path) => env::set_var("HERDR_CONFIG_PATH", path),
                    None => env::remove_var("HERDR_CONFIG_PATH"),
                }
            }
        }
        let temporary = RuntimeDirectory::create("hfwd-local-settings-").unwrap();
        let config_path = temporary.path().join("client.toml");
        let original = "# keep client settings\n[theme]\nname = \"catppuccin-latte\"\n[ui.sidebar.spaces]\nrows = [[\"workspace\"]]\n[ui.toast]\ndelivery = \"off\"\n";
        std::fs::write(&config_path, original).unwrap();
        let _restore = RestoreConfig(env::var_os("HERDR_CONFIG_PATH"));
        env::set_var("HERDR_CONFIG_PATH", &config_path);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = Server::from_listener(listener).unwrap();
        let token = "ab".repeat(32);
        let state = Arc::new(CompanionState {
            session_id: "0123456789abcdef01234567".into(),
            target: "unused".into(),
            companion_url: format!("http://{address}"),
            token: token.clone(),
            wrapper_pid: std::process::id(),
            registry: Mutex::new(Registry::new(FakeSsh::default())),
            state_path: temporary.path().join("session.json"),
            manual_path: temporary.path().join("manual.json"),
            paused_path: temporary.path().join("paused.json"),
            last_heartbeat: Mutex::new(None),
            open_new: false,
            reconnecting: AtomicBool::new(false),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let server_thread = spawn_server(server, state.clone(), stop.clone());
        let base = &state.companion_url;
        for (method, path) in [
            ("GET", "/v1/settings/local"),
            ("POST", "/v1/settings/sidebar"),
            ("POST", "/v1/settings/notifications"),
        ] {
            let response = http_request(base, "wrong-token", method, path, None).unwrap_err();
            assert!(response.starts_with("HTTP/1.1 401"));
        }
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
        let status = http_request(base, &token, "GET", "/v1/settings/local", None).unwrap();
        assert!(status.contains("catppuccin-latte"));
        assert!(status.contains("\"sidebar_ports_enabled\":false"));
        let invalid = http_request(
            base,
            &token,
            "POST",
            "/v1/settings/sidebar",
            Some(r#"{"enabled":true,"path":"/arbitrary/file"}"#),
        )
        .unwrap_err();
        assert!(invalid.starts_with("HTTP/1.1 400"));
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
        std::thread::scope(|scope| {
            let sidebar = scope.spawn(|| {
                http_request(
                    base,
                    &token,
                    "POST",
                    "/v1/settings/sidebar",
                    Some(r#"{"enabled":true}"#),
                )
                .unwrap()
            });
            let notifications = scope.spawn(|| {
                http_request(base, &token, "POST", "/v1/settings/notifications", None).unwrap()
            });
            assert!(sidebar.join().unwrap().starts_with("HTTP/1.1 200"));
            assert!(notifications.join().unwrap().starts_with("HTTP/1.1 200"));
        });
        let updated = std::fs::read_to_string(&config_path).unwrap();
        assert!(updated.contains("# keep client settings"));
        assert!(updated.contains("catppuccin-latte"));
        assert!(updated.contains("$port_forward_status"));
        assert!(updated.contains("delivery = \"herdr\""));
        assert!(!updated.contains("keys.command"));
        assert!(config_path.with_extension("toml.herdr-fwd.bak").exists());
        let disabled = http_request(
            base,
            &token,
            "POST",
            "/v1/settings/sidebar",
            Some(r#"{"enabled":false}"#),
        )
        .unwrap();
        assert!(disabled.contains("\"sidebar_ports_enabled\":false"));
        let broken = "[invalid toml";
        std::fs::write(&config_path, broken).unwrap();
        let response =
            http_request(base, &token, "POST", "/v1/settings/notifications", None).unwrap_err();
        assert!(response.starts_with("HTTP/1.1 500"));
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), broken);
        stop.store(true, Ordering::SeqCst);
        server_thread.join().unwrap();
    }

    #[test]
    fn serves_authenticated_idempotent_forward_lifecycle() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = Server::from_listener(listener).unwrap();
        let temporary = RuntimeDirectory::create("herdr-rpf-companion-test-").unwrap();
        let ssh = FakeSsh::default();
        let calls = ssh.calls.clone();
        let token = "ab".repeat(32);
        let state = Arc::new(CompanionState {
            session_id: "0123456789abcdef01234567".into(),
            target: "workbox".into(),
            companion_url: format!("http://{address}"),
            token: token.clone(),
            wrapper_pid: std::process::id(),
            registry: Mutex::new(Registry::new(ssh)),
            state_path: temporary.path().join("session.json"),
            manual_path: temporary.path().join("manual.json"),
            paused_path: temporary.path().join("paused.json"),
            last_heartbeat: Mutex::new(None),
            open_new: false,
            reconnecting: AtomicBool::new(false),
        });
        state.registry.lock().unwrap().port_available = |_| true;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn_server(server, state.clone(), stop.clone());
        let base = format!("http://{address}");

        let health = raw_request(
            &address.to_string(),
            &format!("GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
        );
        assert!(health.starts_with("HTTP/1.1 200"));
        assert!(health.contains(&format!("\"protocolVersion\":{PROTOCOL_VERSION}")));

        let unauthorized = raw_request(
            &address.to_string(),
            &format!("GET /v1/forwards HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
        );
        assert!(unauthorized.starts_with("HTTP/1.1 401"));

        state.reconnecting.store(true, Ordering::SeqCst);
        let unavailable = http_request(&base, &token, "GET", "/v1/forwards", None).unwrap_err();
        assert!(unavailable.starts_with("HTTP/1.1 503"));
        let heartbeat = http_request(&base, &token, "POST", "/v1/heartbeat", None).unwrap();
        assert!(heartbeat.starts_with("HTTP/1.1 200"));
        state.reconnecting.store(false, Ordering::SeqCst);

        let payload = serde_json::json!({
            "remotePort": 5173,
            "preferredLocalPort": 5173,
            "remoteHost": "127.0.0.1",
            "paneId": "w1:p1",
            "process": "Vite",
            "detectedUrl": "http://localhost:5173/",
            "automatic": true
        })
        .to_string();
        let created = http_request(&base, &token, "POST", "/v1/forwards", Some(&payload)).unwrap();
        assert!(created.starts_with("HTTP/1.1 201"));
        let duplicate =
            http_request(&base, &token, "POST", "/v1/forwards", Some(&payload)).unwrap();
        assert!(duplicate.starts_with("HTTP/1.1 200"));
        let listed = http_request(&base, &token, "GET", "/v1/forwards", None).unwrap();
        assert!(listed.contains("\"remotePort\":5173"));
        let deleted = http_request(&base, &token, "DELETE", "/v1/forwards/fwd-1", None).unwrap();
        assert!(deleted.starts_with("HTTP/1.1 200"));
        assert_eq!(
            *calls.lock().unwrap(),
            ["forward:5173:127.0.0.1:5173", "cancel:5173:127.0.0.1:5173"]
        );

        write_private_json(
            &state.paused_path,
            &vec![PausedAutomaticForward { remote_port: 3001 }],
        )
        .unwrap();
        let paused_automatic = serde_json::json!({
            "remotePort": 3001,
            "preferredLocalPort": 3001,
            "remoteHost": "127.0.0.1",
            "paneId": "w1:p2",
            "process": "Astro",
            "detectedUrl": "http://localhost:3001/",
            "automatic": true
        })
        .to_string();
        let paused = http_request(
            &base,
            &token,
            "POST",
            "/v1/forwards",
            Some(&paused_automatic),
        )
        .unwrap();
        assert!(paused.starts_with("HTTP/1.1 201"));
        assert!(paused.contains("\"enabled\":false"));
        assert_eq!(
            *calls.lock().unwrap(),
            ["forward:5173:127.0.0.1:5173", "cancel:5173:127.0.0.1:5173"]
        );

        let manual = serde_json::json!({
            "remotePort": 4173,
            "preferredLocalPort": 4173,
            "remoteHost": "127.0.0.1",
            "paneId": "manual",
            "process": "Manual",
            "detectedUrl": "http://localhost:4173/",
            "automatic": false
        })
        .to_string();
        let created = http_request(&base, &token, "POST", "/v1/forwards", Some(&manual)).unwrap();
        assert!(created.starts_with("HTTP/1.1 201"));
        assert!(
            std::fs::read_to_string(temporary.path().join("manual.json"))
                .unwrap()
                .contains("4173")
        );

        stop.store(true, Ordering::SeqCst);
        thread.join().unwrap();
    }

    #[test]
    fn serializes_concurrent_mutations_and_publishes_valid_session_state() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = Server::from_listener(listener).unwrap();
        let temporary = RuntimeDirectory::create("herdr-fwd-concurrent-state-test-").unwrap();
        let token = "ab".repeat(32);
        let state = Arc::new(CompanionState {
            session_id: "0123456789abcdef01234567".into(),
            target: "workbox".into(),
            companion_url: format!("http://{address}"),
            token: token.clone(),
            wrapper_pid: std::process::id(),
            registry: Mutex::new(Registry::new(FakeSsh::default())),
            state_path: temporary.path().join("session.json"),
            manual_path: temporary.path().join("manual.json"),
            paused_path: temporary.path().join("paused.json"),
            last_heartbeat: Mutex::new(None),
            open_new: false,
            reconnecting: AtomicBool::new(false),
        });
        state.registry.lock().unwrap().port_available = |_| true;
        let stop = Arc::new(AtomicBool::new(false));
        let server = spawn_server(server, state, stop.clone());
        let base = format!("http://{address}");
        let requests = (0..12)
            .map(|offset| {
                let base = base.clone();
                let token = token.clone();
                std::thread::spawn(move || {
                    let port = 4_200 + offset;
                    let payload = serde_json::json!({
                        "remotePort": port,
                        "preferredLocalPort": port,
                        "remoteHost": "127.0.0.1",
                        "paneId": format!("w1:p{offset}"),
                        "process": "Vite",
                        "detectedUrl": format!("http://localhost:{port}/"),
                        "automatic": true
                    })
                    .to_string();
                    http_request(&base, &token, "POST", "/v1/forwards", Some(&payload)).unwrap()
                })
            })
            .collect::<Vec<_>>();
        for request in requests {
            assert!(request.join().unwrap().starts_with("HTTP/1.1 201"));
        }
        let persisted = serde_json::from_slice::<LocalSessionState>(
            &std::fs::read(temporary.path().join("session.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted.forwards.len(), 12);

        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    #[test]
    fn reports_manual_forward_persistence_failures_to_the_client() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = Server::from_listener(listener).unwrap();
        let temporary = RuntimeDirectory::create("herdr-rpf-persist-error-test-").unwrap();
        let manual_path = temporary.path().join("manual-state");
        std::fs::create_dir(&manual_path).unwrap();
        let token = "ab".repeat(32);
        let ssh = FakeSsh::default();
        let calls = ssh.calls.clone();
        let state = Arc::new(CompanionState {
            session_id: "0123456789abcdef01234567".into(),
            target: "workbox".into(),
            companion_url: format!("http://{address}"),
            token: token.clone(),
            wrapper_pid: std::process::id(),
            registry: Mutex::new(Registry::new(ssh)),
            state_path: temporary.path().join("session.json"),
            manual_path,
            paused_path: temporary.path().join("paused.json"),
            last_heartbeat: Mutex::new(None),
            open_new: false,
            reconnecting: AtomicBool::new(false),
        });
        state.registry.lock().unwrap().port_available = |_| true;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn_server(server, state.clone(), stop.clone());
        let request = serde_json::json!({
            "remotePort": 4173,
            "preferredLocalPort": 4173,
            "remoteHost": "127.0.0.1",
            "paneId": "manual",
            "process": "Manual",
            "detectedUrl": "http://localhost:4173/",
            "automatic": false
        })
        .to_string();

        let response = raw_request(
            &address.to_string(),
            &format!(
                "POST /v1/forwards HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{request}",
                request.len()
            ),
        );

        assert!(response.starts_with("HTTP/1.1 500"));
        assert!(response.contains("failed to persist forwarding state"));
        assert!(state.registry.lock().unwrap().forwards.is_empty());
        assert!(calls.lock().unwrap().is_empty());
        stop.store(true, Ordering::SeqCst);
        thread.join().unwrap();
    }

    #[test]
    fn restores_paused_manual_forward_without_opening_a_tunnel() {
        let temporary = RuntimeDirectory::create("herdr-rpf-paused-manual-test-").unwrap();
        let path = temporary.path().join("manual.json");
        write_private_json(
            &path,
            &vec![ManualForward {
                remote_port: 4173,
                local_port: 4173,
                remote_host: "localhost".into(),
                enabled: false,
            }],
        )
        .unwrap();
        let ssh = FakeSsh::default();
        let calls = ssh.calls.clone();
        let state = CompanionState {
            session_id: "0123456789abcdef01234567".into(),
            target: "workbox".into(),
            companion_url: "http://127.0.0.1:1".into(),
            token: "ab".repeat(32),
            wrapper_pid: std::process::id(),
            registry: Mutex::new(Registry::new(ssh)),
            state_path: temporary.path().join("session.json"),
            manual_path: path,
            paused_path: temporary.path().join("paused.json"),
            last_heartbeat: Mutex::new(None),
            open_new: false,
            reconnecting: AtomicBool::new(false),
        };

        state.restore_manual().unwrap();

        assert!(calls.lock().unwrap().is_empty());
        assert!(!state.registry.lock().unwrap().forwards["fwd-1"].enabled);
    }

    #[test]
    fn persists_paused_automatic_ports_for_the_next_session() {
        let temporary = RuntimeDirectory::create("herdr-rpf-paused-auto-test-").unwrap();
        let path = temporary.path().join("paused.json");
        let ssh = FakeSsh::default();
        let state = CompanionState {
            session_id: "0123456789abcdef01234567".into(),
            target: "workbox".into(),
            companion_url: "http://127.0.0.1:1".into(),
            token: "ab".repeat(32),
            wrapper_pid: std::process::id(),
            registry: Mutex::new(Registry::new(ssh)),
            state_path: temporary.path().join("session.json"),
            manual_path: temporary.path().join("manual.json"),
            paused_path: path,
            last_heartbeat: Mutex::new(None),
            open_new: false,
            reconnecting: AtomicBool::new(false),
        };
        let request = ForwardRequest {
            remote_port: 4173,
            preferred_local_port: 4173,
            remote_host: "127.0.0.1".into(),
            pane_id: "w1:p1".into(),
            process: "Vite".into(),
            detected_url: "http://localhost:4173/".into(),
            automatic: true,
            server_started_at: None,
            process_id: Some(123),
        };
        let forward = state.registry.lock().unwrap().create(&request).unwrap().0;
        state
            .mutate_and_persist(false, |registry| registry.set_enabled(&forward.id, false))
            .unwrap();

        assert!(state.paused_automatic_ports().unwrap().contains(&4173));
    }
}
#[cfg(test)]
mod regression_tests {
    use super::*;
    use crate::local::support::RuntimeDirectory;
    #[derive(Default)]
    struct NoSsh;
    impl Ssh for NoSsh {
        fn forward(&self, _: u16, _: &str, _: u16) -> Result<(), String> {
            Ok(())
        }
        fn cancel(&self, _: u16, _: &str, _: u16) -> Result<(), String> {
            Ok(())
        }
    }
    fn state(root: &std::path::Path, id: &str) -> CompanionState<NoSsh> {
        CompanionState {
            session_id: id.into(),
            target: "host".into(),
            companion_url: "http://127.0.0.1:1".into(),
            token: "ab".repeat(32),
            wrapper_pid: std::process::id(),
            registry: Mutex::new(Registry::new(NoSsh)),
            state_path: root.join(format!("{id}.json")),
            manual_path: root.join("manual.json"),
            paused_path: root.join("paused.json"),
            last_heartbeat: Mutex::new(None),
            open_new: false,
            reconnecting: AtomicBool::new(false),
        }
    }
    fn request(port: u16, automatic: bool) -> ForwardRequest {
        ForwardRequest {
            remote_port: port,
            preferred_local_port: port,
            remote_host: "127.0.0.1".into(),
            pane_id: "p1".into(),
            process: "test".into(),
            detected_url: format!("http://localhost:{port}/"),
            automatic,
            server_started_at: None,
            process_id: Some(1),
        }
    }
    #[test]
    fn preserve_all_saved_pauses_during_discovery() {
        let dir = RuntimeDirectory::create("audit-pauses-").unwrap();
        let state = state(dir.path(), "one");
        write_private_json(
            &state.paused_path,
            &serde_json::json!([{"remotePort":5173},{"remotePort":6006}]),
        )
        .unwrap();
        state
            .mutate_and_persist(false, |r| r.create_paused(&request(5173, true)))
            .unwrap();
        assert!(
            state.paused_automatic_ports().unwrap().contains(&6006),
            "discovering first paused port erased the saved pause for the second"
        );
    }
    #[test]
    fn peer_mutation_preserves_manual_preferences() {
        let dir = RuntimeDirectory::create("audit-peers-").unwrap();
        let first = state(dir.path(), "one");
        let second = state(dir.path(), "two");
        first
            .mutate_and_persist(false, |r| r.create_paused(&request(5173, false)))
            .unwrap();
        second
            .mutate_and_persist(false, |r| r.create_paused(&request(6006, true)))
            .unwrap();
        let saved: Vec<ManualForward> =
            serde_json::from_slice(&std::fs::read(&first.manual_path).unwrap()).unwrap();
        assert_eq!(
            saved.len(),
            1,
            "unrelated peer discovery erased persisted manual forward"
        );
    }
    #[test]
    fn shutdown_is_bounded_with_incomplete_body() {
        use std::{
            io::Write,
            net::{TcpListener, TcpStream},
            sync::mpsc,
        };
        let dir = RuntimeDirectory::create("audit-body-").unwrap();
        let state = Arc::new(state(dir.path(), "one"));
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = Server::from_listener(listener).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let server = spawn_server(server, state.clone(), stop.clone());
        let mut stream = TcpStream::connect(address).unwrap();
        write!(stream,"POST /v1/forwards HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Length: 10000\r\nConnection: close\r\n\r\n{{",state.token).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(*server.requests.count.lock().unwrap(), 1);
        stop.store(true, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        let join = thread::spawn(move || {
            server.join().unwrap();
            tx.send(()).unwrap();
        });
        let bounded = rx.recv_timeout(Duration::from_secs(1)).is_ok();
        drop(stream);
        join.join().unwrap();
        assert!(
            bounded,
            "shutdown waits on request body instead of cancelling in-flight IO"
        );
    }

    #[test]
    fn concurrent_companions_merge_distinct_manual_changes() {
        let dir = RuntimeDirectory::create("preferences-concurrent-").unwrap();
        let first = Arc::new(state(dir.path(), "one"));
        let second = Arc::new(state(dir.path(), "two"));
        let threads = [first.clone(), second.clone()]
            .into_iter()
            .enumerate()
            .map(|(index, state)| {
                thread::spawn(move || {
                    for port in 5000 + index as u16 * 10..5005 + index as u16 * 10 {
                        state
                            .mutate_and_persist(false, |r| r.create_paused(&request(port, false)))
                            .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in threads {
            worker.join().unwrap();
        }
        assert_eq!(
            read_saved::<ManualForward>(&first.manual_path)
                .unwrap()
                .len(),
            10
        );
    }

    #[test]
    fn pause_survives_process_exit_and_only_resume_clears_it() {
        let dir = RuntimeDirectory::create("preferences-pause-").unwrap();
        let state = state(dir.path(), "one");
        state.registry.lock().unwrap().port_available = |_| true;
        let (forward, _) = state
            .mutate_and_persist(false, |r| r.create_paused(&request(5173, true)))
            .unwrap();
        state
            .mutate_and_persist(false, |r| r.remove(&forward.id))
            .unwrap();
        assert!(state.paused_automatic_ports().unwrap().contains(&5173));
        let (forward, _) = state
            .mutate_and_persist(false, |r| r.create_paused(&request(5173, true)))
            .unwrap();
        state
            .mutate_and_persist(false, |r| r.set_enabled(&forward.id, true))
            .unwrap();
        assert!(!state.paused_automatic_ports().unwrap().contains(&5173));
    }

    #[test]
    fn failed_transaction_restores_preferences_without_erasing_peer_state() {
        let dir = RuntimeDirectory::create("preferences-rollback-").unwrap();
        let first = state(dir.path(), "one");
        let second = state(dir.path(), "two");
        first
            .mutate_and_persist(false, |r| r.create_paused(&request(5173, false)))
            .unwrap();
        std::fs::create_dir(&second.state_path).unwrap();
        assert!(second
            .mutate_and_persist(false, |r| r.create_paused(&request(6006, false)))
            .is_err());
        let saved = read_saved::<ManualForward>(&first.manual_path).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].remote_port, 5173);
        assert!(second.registry.lock().unwrap().forwards.is_empty());
    }
}
