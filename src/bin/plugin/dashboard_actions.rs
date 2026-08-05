use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use herdr_fwd::{registry::Forward, ForwardRequest, RemoteSessionConfig, MAX_PROCESS_TREE_DEPTH};
use serde_json::Value;

use crate::plugin::{
    dashboard_render::forward_index_at,
    dashboard_terminal::{
        manual_remote_host, CustomForwardForm, DashboardForm, DashboardState, IntegrationMenu,
        LocalPortForm, RemoveForwardConfirmation,
    },
    herdr::{focus_pane, herdr_output},
    preferences::{load_preferences, set_after_forward, set_process_tree_depth, AfterForward},
    rpc::api_request,
    sidebar_config::{
        dashboard_setup_status, enable_dashboard_popup_shortcut, enable_herdr_notifications,
        set_ports_row,
    },
};

#[path = "copy.rs"]
mod copy;

#[cfg(test)]
pub(crate) fn handle_dashboard_key(
    key: KeyEvent,
    config: &RemoteSessionConfig,
    forwards: &[Forward],
    state: &mut DashboardState,
) {
    handle_dashboard_key_with_session_path(key, config, forwards, state, None);
}

pub(crate) fn handle_dashboard_key_with_session_path(
    key: KeyEvent,
    config: &RemoteSessionConfig,
    forwards: &[Forward],
    state: &mut DashboardState,
    session_path: Option<&Path>,
) {
    if state.form.is_some() {
        handle_form_key(key, config, forwards, state, session_path);
        return;
    }
    match key.code {
        KeyCode::Up => {
            state.select(state.selected.saturating_sub(1));
        }
        KeyCode::Down => {
            if state.selected + 1 < forwards.len() {
                state.select(state.selected + 1);
            }
        }
        KeyCode::Home => state.select(0),
        KeyCode::End => {
            state.select(forwards.len().saturating_sub(1));
        }
        KeyCode::Enter => {
            if let Some(forward) = forwards.get(state.selected) {
                set_message(state, focus_pane(&forward.pane_id), "Focused source pane");
            }
        }
        KeyCode::Char(_) if is_shortcut(key, 'k') => {
            state.select(state.selected.saturating_sub(1));
        }
        KeyCode::Char(_) if is_shortcut(key, 'j') => {
            if state.selected + 1 < forwards.len() {
                state.select(state.selected + 1);
            }
        }
        KeyCode::Char(_) if is_shortcut(key, 'g') => state.select(0),
        KeyCode::Char(_) if is_shortcut(key, 'o') => {
            if let Some(forward) = forwards.get(state.selected) {
                if !forward.enabled {
                    state.show_message("Enable the forward before opening it", true);
                } else {
                    let result = api_request::<Value>(
                        config,
                        "POST",
                        &format!("/v1/forwards/{}/open", forward.id),
                        None,
                    )
                    .map(|_| ());
                    set_message(state, result, "Opened in local browser");
                }
            }
        }
        KeyCode::Char(' ') => {
            toggle_selected_forward(config, forwards, state);
        }
        KeyCode::Char(_) if is_shortcut(key, 'e') => {
            toggle_selected_forward(config, forwards, state);
        }
        KeyCode::Char(_) if is_shortcut(key, 'p') => {
            let Some(forward) = forwards.get(state.selected) else {
                state.show_message("Press a to add the first forward", true);
                return;
            };
            state.form = Some(DashboardForm::Retarget(LocalPortForm {
                id: forward.id.clone(),
                remote_port: forward.remote_port,
                local_port: forward.local_port.to_string(),
                replace_on_type: true,
            }));
            state.message = None;
        }
        KeyCode::Char(_) if is_shortcut(key, 'a') || is_shortcut(key, 'n') => {
            state.form = Some(DashboardForm::Create(CustomForwardForm::default()));
            state.message = None;
        }
        KeyCode::Char(_) if is_shortcut(key, 'h') => {
            match (load_preferences(), dashboard_setup_status()) {
                (Ok(preferences), Ok(status)) => {
                    state.form = Some(DashboardForm::Integration(IntegrationMenu {
                        selected: 0,
                        after_forward: preferences.after_forward,
                        sidebar_ports_enabled: status.sidebar_ports_enabled,
                        toast_delivery: status.toast_delivery,
                        popup_shortcut_enabled: status.popup_shortcut_enabled,
                        process_tree_depth: preferences.process_tree_depth,
                    }));
                    state.message = None;
                }
                (Err(error), _) | (_, Err(error)) => state.show_message(error, true),
            }
        }
        KeyCode::Delete => {
            if let Some(forward) = forwards.get(state.selected) {
                if forward.automatic {
                    state.show_message("Auto-detected forwards can be paused with Space", true);
                } else {
                    state.form = Some(DashboardForm::ConfirmRemoval(RemoveForwardConfirmation {
                        id: forward.id.clone(),
                    }));
                    state.show_message(
                        "Press d or Delete again to remove this custom forward. Esc cancels.",
                        false,
                    );
                }
            }
        }
        KeyCode::Char(_) if is_shortcut(key, 'd') => {
            if let Some(forward) = forwards.get(state.selected) {
                if forward.automatic {
                    state.show_message("Auto-detected forwards can be paused with Space", true);
                } else {
                    state.form = Some(DashboardForm::ConfirmRemoval(RemoveForwardConfirmation {
                        id: forward.id.clone(),
                    }));
                    state.show_message(
                        "Press d or Delete again to remove this custom forward. Esc cancels.",
                        false,
                    );
                }
            }
        }
        KeyCode::Char(_) if is_help_shortcut(key) => state.help = !state.help,
        KeyCode::Esc => {
            state.help = false;
            state.message = None;
        }
        KeyCode::Char(_) if is_shortcut(key, 'r') => state.show_message("Refreshed", false),
        _ => {}
    }
}

fn toggle_selected_forward(
    config: &RemoteSessionConfig,
    forwards: &[Forward],
    state: &mut DashboardState,
) {
    if let Some(forward) = forwards.get(state.selected) {
        let body = serde_json::json!({"enabled": !forward.enabled});
        let result = api_request::<Forward>(
            config,
            "POST",
            &format!("/v1/forwards/{}/toggle", forward.id),
            Some(body),
        )
        .map(|_| ());
        set_message(
            state,
            result,
            if forward.enabled {
                "Forward paused"
            } else {
                "Forward enabled"
            },
        );
    }
}

pub(crate) fn handle_dashboard_mouse(
    mouse: MouseEvent,
    forwards: &[Forward],
    locations: &std::collections::HashMap<String, crate::plugin::herdr::PaneLocation>,
    state: &mut DashboardState,
) {
    match mouse.kind {
        MouseEventKind::ScrollUp => state.select(state.selected.saturating_sub(1)),
        MouseEventKind::ScrollDown => {
            if state.selected + 1 < forwards.len() {
                state.select(state.selected + 1);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let height = crossterm::terminal::size()
                .map(|(_, height)| height)
                .unwrap_or(30);
            if let Some(index) =
                forward_index_at(mouse.row, height, state.selected, forwards, locations)
            {
                state.select(index);
            }
        }
        _ => {}
    }
}

pub(crate) fn is_shortcut(key: KeyEvent, expected: char) -> bool {
    let KeyCode::Char(character) = key.code else {
        return false;
    };
    character.eq_ignore_ascii_case(&expected) || ukrainian_shortcut(character) == Some(expected)
}

fn is_help_shortcut(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('?') | KeyCode::Char(','))
        || (key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Char('7') | KeyCode::Char('/')))
}

fn ukrainian_shortcut(character: char) -> Option<char> {
    Some(match character {
        'ф' | 'Ф' => 'a',
        'и' | 'И' => 'b',
        'с' | 'С' => 'c',
        'в' | 'В' => 'd',
        'у' | 'У' => 'e',
        'а' | 'А' => 'f',
        'п' | 'П' => 'g',
        'р' | 'Р' => 'h',
        'ш' | 'Ш' => 'i',
        'о' | 'О' => 'j',
        'л' | 'Л' => 'k',
        'д' | 'Д' => 'l',
        'ь' | 'Ь' => 'm',
        'т' | 'Т' => 'n',
        'щ' | 'Щ' => 'o',
        'з' | 'З' => 'p',
        'й' | 'Й' => 'q',
        'к' | 'К' => 'r',
        'і' | 'І' => 's',
        'е' | 'Е' => 't',
        'г' | 'Г' => 'u',
        'м' | 'М' => 'v',
        'ц' | 'Ц' => 'w',
        'ч' | 'Ч' => 'x',
        'н' | 'Н' => 'y',
        'я' | 'Я' => 'z',
        _ => return None,
    })
}

fn handle_form_key(
    key: KeyEvent,
    config: &RemoteSessionConfig,
    forwards: &[Forward],
    state: &mut DashboardState,
    session_path: Option<&Path>,
) {
    let Some(mut form) = state.form.take() else {
        return;
    };
    let keep_open = match &mut form {
        DashboardForm::ConfirmRemoval(form) => {
            handle_remove_confirmation_key(key, config, form, state)
        }
        DashboardForm::Retarget(form) => handle_local_port_key(key, config, form, state),
        DashboardForm::Create(form) => handle_create_key(key, config, forwards, form, state),
        DashboardForm::Integration(form) => {
            handle_integration_key(key, config, form, state, session_path)
        }
    };
    if keep_open {
        state.form = Some(form);
    }
}

fn handle_remove_confirmation_key(
    key: KeyEvent,
    config: &RemoteSessionConfig,
    form: &RemoveForwardConfirmation,
    state: &mut DashboardState,
) -> bool {
    match key.code {
        KeyCode::Esc => false,
        KeyCode::Delete => {
            let result =
                api_request::<Value>(config, "DELETE", &format!("/v1/forwards/{}", form.id), None)
                    .map(|_| ());
            set_message(state, result, "Custom forward removed");
            false
        }
        KeyCode::Char(_) if is_shortcut(key, 'd') => {
            let result =
                api_request::<Value>(config, "DELETE", &format!("/v1/forwards/{}", form.id), None)
                    .map(|_| ());
            set_message(state, result, "Custom forward removed");
            false
        }
        _ => true,
    }
}

fn handle_integration_key(
    key: KeyEvent,
    config: &RemoteSessionConfig,
    form: &mut IntegrationMenu,
    state: &mut DashboardState,
    session_path: Option<&Path>,
) -> bool {
    match key.code {
        KeyCode::Esc => return false,
        KeyCode::Char(_) if is_shortcut(key, 'h') => return false,
        KeyCode::Up => form.selected = form.selected.saturating_sub(1),
        KeyCode::Down => form.selected = (form.selected + 1).min(4),
        KeyCode::Char(_) if is_shortcut(key, 'k') => {
            form.selected = form.selected.saturating_sub(1)
        }
        KeyCode::Char(_) if is_shortcut(key, 'j') => form.selected = (form.selected + 1).min(4),
        KeyCode::Left if form.selected == 0 => {
            update_after_forward(config, form, state, session_path, -1)
        }
        KeyCode::Right if form.selected == 0 => {
            update_after_forward(config, form, state, session_path, 1)
        }
        KeyCode::Left if form.selected == 4 => {
            update_process_tree_depth(config, form, state, session_path, -1)
        }
        KeyCode::Right if form.selected == 4 => {
            update_process_tree_depth(config, form, state, session_path, 1)
        }
        KeyCode::Enter | KeyCode::Char(' ') => match form.selected {
            0 => update_after_forward(config, form, state, session_path, 1),
            1 => match set_ports_row(!form.sidebar_ports_enabled) {
                Ok(status) => match herdr_output(&["server", "reload-config"]) {
                    Ok(_) => {
                        form.sidebar_ports_enabled = status.sidebar_ports_enabled;
                        state.show_message(
                            if form.sidebar_ports_enabled {
                                "Sidebar port status added"
                            } else {
                                "Sidebar port status removed"
                            },
                            false,
                        );
                    }
                    Err(error) => state.show_message(error, true),
                },
                Err(error) => state.show_message(error, true),
            },
            2 => match enable_herdr_notifications() {
                Ok(status) => match herdr_output(&["server", "reload-config"]) {
                    Ok(_) => {
                        form.toast_delivery = status.toast_delivery.clone();
                        state.show_message("Notifications inside Herdr configured", false);
                    }
                    Err(error) => state.show_message(error, true),
                },
                Err(error) => state.show_message(error, true),
            },
            3 => match enable_dashboard_popup_shortcut() {
                Ok(status) => match herdr_output(&["server", "reload-config"]) {
                    Ok(_) => {
                        form.popup_shortcut_enabled = status.popup_shortcut_enabled;
                        state.show_message(
                            "Popup shortcut ready for --remote-keybindings server",
                            false,
                        );
                    }
                    Err(error) => state.show_message(error, true),
                },
                Err(error) => state.show_message(error, true),
            },
            _ => update_process_tree_depth(config, form, state, session_path, 1),
        },
        _ => {}
    }
    true
}

fn update_process_tree_depth(
    _config: &RemoteSessionConfig,
    form: &mut IntegrationMenu,
    state: &mut DashboardState,
    _session_path: Option<&Path>,
    delta: i8,
) {
    let depth = adjust_process_tree_depth(form.process_tree_depth, delta);
    if depth == form.process_tree_depth {
        return;
    }
    match set_process_tree_depth(depth) {
        Ok(_) => {
            form.process_tree_depth = depth;
            state.show_message(format!("Process tree depth: {depth}"), false);
        }
        Err(error) => {
            state.show_message(format!("Could not save process tree depth: {error}"), true)
        }
    }
}

pub(crate) fn adjust_process_tree_depth(current: u8, delta: i8) -> u8 {
    let maximum = i16::from(MAX_PROCESS_TREE_DEPTH);
    let next = i16::from(current) + i16::from(delta);
    if next < 0 {
        MAX_PROCESS_TREE_DEPTH
    } else if next > maximum {
        0
    } else {
        next as u8
    }
}

fn update_after_forward(
    _config: &RemoteSessionConfig,
    form: &mut IntegrationMenu,
    state: &mut DashboardState,
    _session_path: Option<&Path>,
    delta: i8,
) {
    let after_forward = cycle_after_forward(form.after_forward, delta);
    match set_after_forward(after_forward) {
        Ok(preferences) => {
            form.after_forward = preferences.after_forward;
            state.show_message(
                match form.after_forward {
                    AfterForward::Space => "After forwarding: open Space",
                    AfterForward::Popup => "After forwarding: open popup",
                    AfterForward::Nothing => "After forwarding: do nothing",
                },
                false,
            );
        }
        Err(error) => state.show_message(error, true),
    }
}

fn cycle_after_forward(current: AfterForward, delta: i8) -> AfterForward {
    match delta.signum() {
        -1 => match current {
            AfterForward::Space => AfterForward::Nothing,
            AfterForward::Popup => AfterForward::Space,
            AfterForward::Nothing => AfterForward::Popup,
        },
        1 => current.next(),
        _ => current,
    }
}

fn handle_local_port_key(
    key: KeyEvent,
    config: &RemoteSessionConfig,
    form: &mut LocalPortForm,
    state: &mut DashboardState,
) -> bool {
    match key.code {
        KeyCode::Esc => return false,
        KeyCode::Backspace => {
            if form.replace_on_type {
                form.local_port.clear();
                form.replace_on_type = false;
            } else {
                form.local_port.pop();
            }
        }
        KeyCode::Char(character)
            if character.is_ascii_digit() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            if form.replace_on_type {
                form.local_port.clear();
                form.replace_on_type = false;
            }
            if form.local_port.len() < 5 {
                form.local_port.push(character);
            }
        }
        KeyCode::Enter => {
            let Some(local_port) = form.local_port.parse::<u16>().ok().filter(|port| *port > 0)
            else {
                state.show_message("Local port must be between 1 and 65535", true);
                return true;
            };
            let result = api_request::<Forward>(
                config,
                "POST",
                &format!("/v1/forwards/{}/local-port", form.id),
                Some(serde_json::json!({"localPort": local_port})),
            )
            .map(|_| ());
            set_message(
                state,
                result,
                &format!("Forward remapped: {} → {local_port}", form.remote_port),
            );
            return false;
        }
        _ => {}
    }
    true
}

fn handle_create_key(
    key: KeyEvent,
    config: &RemoteSessionConfig,
    forwards: &[Forward],
    form: &mut CustomForwardForm,
    state: &mut DashboardState,
) -> bool {
    match key.code {
        KeyCode::Esc => return false,
        KeyCode::Tab | KeyCode::Down => {
            form.field = (form.field + 1) % 3;
        }
        KeyCode::Up | KeyCode::BackTab => {
            form.field = (form.field + 2) % 3;
        }
        KeyCode::Left if form.field == 2 => {
            form.remote_host = (form.remote_host + 2) % 3;
        }
        KeyCode::Right if form.field == 2 => {
            form.remote_host = (form.remote_host + 1) % 3;
        }
        KeyCode::Backspace => {
            if form.field == 0 {
                form.remote_port.pop();
            } else if form.field == 1 {
                form.local_port.pop();
            }
        }
        KeyCode::Char(character)
            if character.is_ascii_digit() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            if form.field == 0 {
                if form.remote_port.len() < 5 {
                    form.remote_port.push(character);
                }
            } else if form.field == 1 && form.local_port.len() < 5 {
                form.local_port.push(character);
            }
        }
        KeyCode::Enter => {
            let remote_port = if form.remote_port.is_empty() {
                Some(3000)
            } else {
                form.remote_port
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port > 0)
            };
            let local_port = if form.local_port.is_empty() {
                remote_port
            } else {
                form.local_port.parse::<u16>().ok().filter(|port| *port > 0)
            };
            let (Some(remote_port), Some(local_port)) = (remote_port, local_port) else {
                state.show_message("Ports must be between 1 and 65535", true);
                return true;
            };
            let pane_id = forwards
                .get(state.selected)
                .map(|forward| forward.pane_id.clone())
                .unwrap_or_else(|| "manual".into());
            let request = ForwardRequest {
                remote_port,
                preferred_local_port: local_port,
                remote_host: manual_remote_host(form.remote_host).into(),
                pane_id,
                process: "Custom".into(),
                detected_url: format!(
                    "http://{}:{remote_port}/",
                    if manual_remote_host(form.remote_host) == "::1" {
                        "[::1]"
                    } else {
                        manual_remote_host(form.remote_host)
                    }
                ),
                automatic: false,
                server_started_at: None,
                process_id: None,
            };
            let result = api_request::<Forward>(
                config,
                "POST",
                "/v1/forwards",
                serde_json::to_value(request).ok(),
            )
            .map(|_| ());
            set_message(state, result, "New forward created");
            return false;
        }
        _ => {}
    }
    true
}

fn set_message(state: &mut DashboardState, result: Result<(), String>, success: &str) {
    match result {
        Ok(()) => state.show_message(success, false),
        Err(error) => state.show_message(
            copy::format_actionable_error(
                "Action failed.",
                &error,
                Some("Press r to retry or h for settings."),
            ),
            true,
        ),
    }
}

#[cfg(test)]
mod dashboard_actions_tests {
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpListener},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
        time::{Duration, Instant},
    };

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use herdr_fwd::{registry::Forward, RemoteSessionConfig};

    use crate::plugin::dashboard_terminal::DashboardState;
    use crate::plugin::preferences::AfterForward;

    use super::{handle_dashboard_key, handle_integration_key};
    use crate::plugin::dashboard_terminal::IntegrationMenu;

    #[test]
    fn help_shortcut_accepts_question_mark_positions_on_both_layouts() {
        let config = RemoteSessionConfig {
            protocol_version: herdr_fwd::PROTOCOL_VERSION,
            session_id: "0123456789abcdef01234567".into(),
            herdr_session: "default".into(),
            token: "ab".repeat(32),
            rpc_url: "http://127.0.0.1:23000".into(),
            auto_detect: true,
        };
        for key in [
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char(','), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('7'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::SHIFT),
        ] {
            let mut state = DashboardState::default();
            handle_dashboard_key(key, &config, &[], &mut state);
            assert!(state.help);
        }
    }

    #[test]
    fn integration_settings_persist_process_tree_depth_as_a_preference() {
        let _environment = crate::plugin::TEST_ENV_LOCK.lock().unwrap();
        let directory = std::env::temp_dir().join(format!(
            "herdr-fwd-dashboard-settings-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::env::set_var("HERDR_PLUGIN_CONFIG_DIR", &directory);
        let config = RemoteSessionConfig {
            protocol_version: herdr_fwd::PROTOCOL_VERSION,
            session_id: "test".into(),
            herdr_session: "default".into(),
            token: "test-token".into(),
            rpc_url: "http://127.0.0.1:23000".into(),
            auto_detect: true,
        };
        let mut menu = IntegrationMenu {
            selected: 4,
            after_forward: AfterForward::Space,
            sidebar_ports_enabled: false,
            toast_delivery: None,
            popup_shortcut_enabled: false,
            process_tree_depth: 2,
        };
        let mut state = DashboardState::default();

        handle_integration_key(
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            &config,
            &mut menu,
            &mut state,
            None,
        );

        let saved = crate::plugin::preferences::load_preferences().unwrap();
        assert_eq!(menu.process_tree_depth, 3);
        assert_eq!(saved.process_tree_depth, 3);
        std::env::remove_var("HERDR_PLUGIN_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(directory);
    }

    fn forward() -> Forward {
        Forward {
            id: "fwd-1".into(),
            remote_port: 5173,
            local_port: 5173,
            remote_host: "127.0.0.1".into(),
            pane_id: "w1:p1".into(),
            process: "Vite".into(),
            detected_url: "http://localhost:5173/".into(),
            enabled: true,
            automatic: true,
            server_started_at: None,
            process_id: None,
            tunnel_opened_at: 0,
        }
    }

    fn manual_forward() -> Forward {
        Forward {
            automatic: false,
            pane_id: "manual".into(),
            ..forward()
        }
    }

    fn remote_config(rpc_url: String) -> RemoteSessionConfig {
        RemoteSessionConfig {
            protocol_version: herdr_fwd::PROTOCOL_VERSION,
            session_id: "0123456789abcdef01234567".into(),
            herdr_session: "default".into(),
            token: "ab".repeat(32),
            rpc_url,
            auto_detect: true,
        }
    }

    fn deletion_server() -> (String, Arc<AtomicUsize>, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = requests.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 256];
                let read = stream.read(&mut bytes).unwrap();
                assert_ne!(read, 0, "RPC client closed before completing its request");
                request.extend_from_slice(&bytes[..read]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            request_count.fetch_add(1, Ordering::SeqCst);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        (format!("http://{address}"), requests, server)
    }

    fn no_request_server() -> (String, Arc<AtomicUsize>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = requests.clone();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(100);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        request_count.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
        });
        (format!("http://{address}"), requests, server)
    }

    #[test]
    fn open_failure_has_one_actionable_error_format() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 256];
                let read = stream.read(&mut bytes).unwrap();
                assert_ne!(read, 0, "RPC client closed before completing its request");
                request.extend_from_slice(&bytes[..read]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let body = "browser unavailable";
            let response = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
        });
        let config = remote_config(format!("http://{address}"));
        let mut state = DashboardState::default();

        handle_dashboard_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
            &config,
            &[forward()],
            &mut state,
        );
        server.join().unwrap();

        let message = state.message.unwrap();
        assert!(message.error);
        assert!(message
            .text
            .contains("RPC returned 500: browser unavailable"));
        assert!(message.text.contains("Press r to retry"));
    }

    #[test]
    fn removing_a_manual_forward_requires_a_second_confirmation_key() {
        let (rpc_url, requests, server) = deletion_server();
        let config = remote_config(rpc_url);
        let mut state = DashboardState::default();

        handle_dashboard_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &config,
            &[manual_forward()],
            &mut state,
        );

        assert_eq!(
            state.message.as_ref().map(|message| message.text.as_str()),
            Some("Press d or Delete again to remove this custom forward. Esc cancels.")
        );
        assert_eq!(requests.load(Ordering::SeqCst), 0);

        handle_dashboard_key(
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
            &config,
            &[manual_forward()],
            &mut state,
        );

        let request = server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(request.starts_with("DELETE /v1/forwards/fwd-1 HTTP/1.1\r\n"));
    }

    #[test]
    fn escape_cancels_manual_forward_removal_without_a_request() {
        let (rpc_url, requests, server) = no_request_server();
        let config = remote_config(rpc_url);
        let mut state = DashboardState::default();

        handle_dashboard_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &config,
            &[manual_forward()],
            &mut state,
        );
        handle_dashboard_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &config,
            &[manual_forward()],
            &mut state,
        );
        handle_dashboard_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &config,
            &[manual_forward()],
            &mut state,
        );

        assert_eq!(
            state.message.as_ref().map(|message| message.text.as_str()),
            Some("Press d or Delete again to remove this custom forward. Esc cancels.")
        );
        server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 0);
    }
}
