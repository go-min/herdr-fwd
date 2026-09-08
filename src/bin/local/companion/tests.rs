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
    let duplicate = http_request(&base, &token, "POST", "/v1/forwards", Some(&payload)).unwrap();
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

fn state(root: &std::path::Path, id: &str) -> CompanionState<FakeSsh> {
    CompanionState {
        session_id: id.into(),
        target: "host".into(),
        companion_url: "http://127.0.0.1:1".into(),
        token: "ab".repeat(32),
        wrapper_pid: std::process::id(),
        registry: Mutex::new(Registry::new(FakeSsh::default())),
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
    let dir = RuntimeDirectory::create("hfwd-saved-pauses-").unwrap();
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
    let dir = RuntimeDirectory::create("hfwd-shared-preferences-").unwrap();
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
    let dir = RuntimeDirectory::create("hfwd-incomplete-request-").unwrap();
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
