use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    thread,
    time::Duration,
};

use herdr_fwd::{registry::Forward, ForwardRequest};

use crate::local::{companion::LocalSessionState, support::state_directory};

const MAX_RESPONSE_SIZE: u64 = 1024 * 1024;

pub(crate) const MANUAL_FORWARD_USAGE: &str = "hfwd forward <target> <remote-port> [local-port]";

fn session_states() -> Result<Vec<(PathBuf, LocalSessionState)>, String> {
    let directory = state_directory()?;
    let mut sessions = Vec::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .flatten()
    {
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let path = entry.path();
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(mut state) = serde_json::from_slice::<LocalSessionState>(&bytes) {
                let current = http_request(
                    &state.companion_url,
                    &state.token,
                    "GET",
                    "/v1/forwards",
                    None,
                )
                .ok()
                .and_then(|response| response.split_once("\r\n\r\n").map(|(_, body)| body.into()))
                .and_then(|body: String| serde_json::from_str::<Vec<Forward>>(&body).ok());
                if let Some(forwards) = current {
                    state.forwards = forwards;
                    sessions.push((path, state));
                } else if process_is_alive(state.wrapper_pid) {
                    eprintln!(
                        "warning: companion for session {} ({}) is temporarily unreachable; state retained",
                        state.session_id, state.target
                    );
                } else {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
    Ok(sessions)
}

fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // Signal 0 performs permission/existence validation without sending a
        // signal. EPERM still proves that a process with this PID exists.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        // The package currently supports only macOS and Linux. Keep unknown
        // platforms conservative: retain state instead of deleting it.
        true
    }
}

pub(crate) fn list_forwards() -> Result<(), String> {
    let mut found = false;
    for (_, session) in session_states()? {
        for forward in session.forwards {
            found = true;
            println!(
                "{}  {:<6} {:<6} remote {}:{} → local 127.0.0.1:{}  pane={}",
                forward.id,
                if forward.enabled { "active" } else { "paused" },
                if forward.automatic { "auto" } else { "custom" },
                forward.remote_host_display(),
                forward.remote_port,
                forward.local_port,
                forward.pane_id
            );
        }
    }
    if !found {
        println!("No active remote port forwards.");
    }
    Ok(())
}

pub(crate) fn watch_forwards() -> Result<(), String> {
    loop {
        print!("\x1b[2J\x1b[H");
        list_forwards()?;
        thread::sleep(Duration::from_secs(1));
    }
}

pub(crate) fn close_forward(id_or_port: Option<&String>) -> Result<(), String> {
    let Some(id_or_port) = id_or_port else {
        return Err("usage: hfwd close <id-or-local-port>".into());
    };
    let sessions = session_states()?;
    let (session, forward) = resolve_close_forward(&sessions, id_or_port)?;
    http_request(
        &session.companion_url,
        &session.token,
        "DELETE",
        &format!("/v1/forwards/{}", forward.id),
        None,
    )?;
    Ok(())
}

fn resolve_close_forward<'a>(
    sessions: &'a [(PathBuf, LocalSessionState)],
    selector: &str,
) -> Result<(&'a LocalSessionState, &'a Forward), String> {
    let mut matches = sessions.iter().flat_map(|(_, session)| {
        session.forwards.iter().filter_map(move |forward| {
            (forward.id == selector || forward.local_port.to_string() == selector)
                .then_some((session, forward))
        })
    });
    let selected = matches
        .next()
        .ok_or_else(|| format!("forward not found: {selector}"))?;
    if matches.next().is_some() {
        return Err(format!("ambiguous forward {selector}; use a unique local port or the intended session's dashboard"));
    }
    Ok(selected)
}

pub(crate) fn close_all_forwards() -> Result<(), String> {
    let mut errors = Vec::new();
    for (_, session) in session_states()? {
        for forward in &session.forwards {
            if let Err(error) = http_request(
                &session.companion_url,
                &session.token,
                "DELETE",
                &format!("/v1/forwards/{}", forward.id),
                None,
            ) {
                errors.push(format!("{}: {error}", forward.id));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub(crate) fn create_manual_forward(arguments: &[String]) -> Result<(), String> {
    let [target, remote_port, rest @ ..] = arguments else {
        return Err(format!("usage: {MANUAL_FORWARD_USAGE}"));
    };
    let local_port = match rest {
        [] => remote_port,
        [local_port] => local_port,
        _ => return Err(format!("usage: {MANUAL_FORWARD_USAGE}")),
    };
    let remote_port = parse_port(
        remote_port,
        "remote port",
        &format!("hfwd forward {target} <remote-port> [local-port]"),
    )?;
    let local_port = parse_port(
        local_port,
        "local port",
        &format!("hfwd forward {target} {remote_port} <local-port>"),
    )?;
    let sessions = session_states()?
        .into_iter()
        .map(|(_, session)| session)
        .collect::<Vec<_>>();
    let session = resolve_manual_session(sessions, target, remote_port)?;
    let request = manual_forward_request(remote_port, local_port);
    let body = serde_json::to_string(&request).map_err(|error| error.to_string())?;
    let response = http_request(
        &session.companion_url,
        &session.token,
        "POST",
        "/v1/forwards",
        Some(&body),
    )?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .ok_or_else(|| "invalid companion response".to_string())?;
    let forward: Forward = serde_json::from_str(body)
        .map_err(|error| format!("invalid companion response: {error}"))?;
    println!(
        "Manual forward: {}:{} → 127.0.0.1:{} ({})",
        forward.remote_host_display(),
        forward.remote_port,
        forward.local_port,
        forward.id
    );
    Ok(())
}

fn resolve_manual_session(
    sessions: Vec<LocalSessionState>,
    target: &str,
    remote_port: u16,
) -> Result<LocalSessionState, String> {
    let mut matches = sessions
        .into_iter()
        .filter(|session| session.target == target);
    let Some(session) = matches.next() else {
        return Err(format!(
            "no active hfwd session for {target}; start one with hfwd {target}"
        ));
    };
    if matches.next().is_some() {
        return Err(format!(
            "multiple active hfwd sessions for {target}; end one session, then retry hfwd forward {target} {remote_port}"
        ));
    }
    Ok(session)
}

fn parse_port(value: &str, name: &str, retry_command: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("invalid {name} {value}; use 1..=65535, then run {retry_command}"))
}

fn manual_forward_request(remote_port: u16, local_port: u16) -> ForwardRequest {
    ForwardRequest {
        remote_port,
        preferred_local_port: local_port,
        remote_host: "127.0.0.1".into(),
        pane_id: "manual".into(),
        process: "Manual".into(),
        detected_url: format!("http://localhost:{remote_port}/"),
        automatic: false,
        server_started_at: None,
        process_id: None,
    }
}

pub(crate) fn http_request(
    base: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    let authority = base
        .strip_prefix("http://")
        .ok_or_else(|| "unsupported companion URL".to_string())?;
    let mut stream = TcpStream::connect_timeout(
        &authority
            .parse()
            .map_err(|_| "invalid companion address".to_string())?,
        Duration::from_secs(2),
    )
    .map_err(|error| format!("companion unavailable: {error}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    let body = body.unwrap_or("");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    stream
        .take(MAX_RESPONSE_SIZE + 1)
        .read_to_end(&mut response)
        .map_err(|error| error.to_string())?;
    if response.len() as u64 > MAX_RESPONSE_SIZE {
        return Err("companion response is too large".into());
    }
    let response = String::from_utf8_lossy(&response);
    if !response.starts_with("HTTP/1.1 2") {
        return Err(response
            .lines()
            .next()
            .unwrap_or("companion request failed")
            .into());
    }
    Ok(response.into_owned())
}

#[cfg(test)]
mod management_tests {
    use crate::local::companion::LocalSessionState;

    use super::{process_is_alive, resolve_manual_session};

    #[test]
    fn distinguishes_live_and_stale_session_owners() {
        assert!(process_is_alive(std::process::id()));
        assert!(!process_is_alive(0));
    }

    #[test]
    fn rejects_an_ambiguous_manual_forward_session() {
        let session = |id: &str| LocalSessionState {
            session_id: id.into(),
            target: "workbox".into(),
            companion_url: "http://127.0.0.1:1".into(),
            token: "test-token".into(),
            wrapper_pid: std::process::id(),
            forwards: Vec::new(),
        };
        assert!(
            resolve_manual_session(vec![session("first"), session("second")], "workbox", 4173)
                .is_err()
        );
    }
}

#[test]
fn close_selector_requires_exactly_one_mapping_across_sessions() {
    let make = |id: &str, port| {
        let forward: Forward = serde_json::from_value(serde_json::json!({
            "id":"fwd-1", "remotePort":5173, "localPort":port, "remoteHost":"127.0.0.1",
            "paneId":"manual", "process":"Manual", "detectedUrl":"http://localhost:5173/"
        }))
        .unwrap();
        (
            PathBuf::new(),
            LocalSessionState {
                session_id: id.into(),
                target: id.into(),
                companion_url: String::new(),
                token: String::new(),
                wrapper_pid: 0,
                forwards: vec![forward],
            },
        )
    };
    let sessions = vec![make("first", 5173), make("second", 5174)];
    assert!(resolve_close_forward(&sessions, "fwd-1").is_err());
    assert_eq!(
        resolve_close_forward(&sessions, "5174")
            .unwrap()
            .0
            .session_id,
        "second"
    );
    assert!(resolve_close_forward(&sessions, "missing").is_err());
    assert_eq!(
        resolve_close_forward(&sessions[..1], "fwd-1")
            .unwrap()
            .0
            .session_id,
        "first"
    );
    let paused = vec![make("first", 5173), make("second", 5173)];
    assert!(resolve_close_forward(&paused, "5173").is_err());
}
