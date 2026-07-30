use std::{
    env,
    path::Path,
    time::{Duration, Instant},
};

use crossterm::{
    cursor::{Hide, Show},
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    style::ResetColor,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use herdr_fwd::RemoteSessionConfig;

use crate::plugin::preferences::AfterForward;
use crate::plugin::{
    dashboard_actions::handle_dashboard_key_with_session_path,
    dashboard_actions::handle_dashboard_mouse,
    dashboard_render::{render_dashboard, sort_for_display},
    herdr::{close_popup, herdr_json, pane_locations},
    rpc::list_forwards,
    session::read_json_file,
};

struct DashboardTerminal;

impl DashboardTerminal {
    fn enter() -> Result<Self, String> {
        terminal::enable_raw_mode().map_err(|error| error.to_string())?;
        if let Err(error) = execute!(
            std::io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            Hide
        ) {
            let _ = terminal::disable_raw_mode();
            return Err(error.to_string());
        }
        Ok(Self)
    }
}

impl Drop for DashboardTerminal {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            Show,
            DisableMouseCapture,
            LeaveAlternateScreen,
            ResetColor
        );
    }
}

const DASHBOARD_MESSAGE_TTL: Duration = Duration::from_secs(3);

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct DashboardState {
    pub(crate) selected: usize,
    pub(crate) help: bool,
    pub(crate) form: Option<DashboardForm>,
    pub(crate) message: Option<DashboardMessage>,
    pub(crate) unavailable: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DashboardMessage {
    pub(crate) text: String,
    pub(crate) error: bool,
    pub(crate) shown_at: Instant,
}

impl DashboardState {
    pub(crate) fn select(&mut self, selected: usize) {
        if self.selected != selected {
            self.selected = selected;
            if !self.unavailable {
                self.message = None;
            }
        }
    }

    pub(crate) fn show_message(&mut self, text: impl Into<String>, error: bool) {
        self.unavailable = false;
        self.message = Some(DashboardMessage {
            text: text.into(),
            error,
            shown_at: Instant::now(),
        });
    }

    pub(crate) fn show_unavailable(&mut self, details: impl AsRef<str>) {
        self.unavailable = true;
        self.message = Some(DashboardMessage {
            text: format!("UNAVAILABLE · {} · Press r to retry", details.as_ref()),
            error: true,
            shown_at: Instant::now(),
        });
    }

    pub(crate) fn mark_available(&mut self) {
        if self.unavailable {
            self.unavailable = false;
            self.message = None;
        }
    }

    pub(crate) fn expire_message(&mut self, now: Instant) {
        if self.unavailable {
            return;
        }
        if self
            .message
            .as_ref()
            .is_some_and(|message| now.duration_since(message.shown_at) >= DASHBOARD_MESSAGE_TTL)
        {
            self.message = None;
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum DashboardForm {
    ConfirmRemoval(RemoveForwardConfirmation),
    Retarget(LocalPortForm),
    Create(CustomForwardForm),
    Integration(IntegrationMenu),
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RemoveForwardConfirmation {
    pub(crate) id: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LocalPortForm {
    pub(crate) id: String,
    pub(crate) remote_port: u16,
    pub(crate) local_port: String,
    pub(crate) replace_on_type: bool,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct CustomForwardForm {
    pub(crate) remote_port: String,
    pub(crate) local_port: String,
    pub(crate) remote_host: usize,
    pub(crate) field: usize,
}

pub(crate) fn manual_remote_host(index: usize) -> &'static str {
    ["localhost", "127.0.0.1", "::1"][index.min(2)]
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct IntegrationMenu {
    pub(crate) selected: usize,
    pub(crate) after_forward: AfterForward,
    pub(crate) sidebar_ports_enabled: bool,
    pub(crate) toast_delivery: Option<String>,
    pub(crate) popup_shortcut_enabled: bool,
    pub(crate) process_tree_depth: u8,
}

#[derive(Clone, PartialEq, Eq)]
struct DashboardFrame {
    session_id: String,
    forwards: Vec<herdr_fwd::registry::Forward>,
    locations: std::collections::HashMap<String, crate::plugin::herdr::PaneLocation>,
    state: DashboardState,
    terminal_size: (u16, u16),
}

impl DashboardFrame {
    fn new(
        config: &RemoteSessionConfig,
        forwards: &[herdr_fwd::registry::Forward],
        locations: &std::collections::HashMap<String, crate::plugin::herdr::PaneLocation>,
        state: &DashboardState,
    ) -> Self {
        Self {
            session_id: config.session_id.clone(),
            forwards: forwards.to_vec(),
            locations: locations.clone(),
            state: state.clone(),
            terminal_size: terminal::size().unwrap_or((100, 30)),
        }
    }
}

pub(crate) fn dashboard(session_path: &Path) -> Result<(), String> {
    let _terminal = DashboardTerminal::enter()?;
    let mut state = DashboardState::default();
    let mut config: RemoteSessionConfig = read_json_file(session_path)?;
    config.validate()?;
    let mut forwards = match list_forwards(&config) {
        Ok(forwards) => forwards,
        Err(error) => {
            state.show_unavailable(error);
            Vec::new()
        }
    };
    let mut locations = herdr_json(&["api", "snapshot"])
        .map(|snapshot| pane_locations(&snapshot))
        .unwrap_or_default();
    sort_for_display(&mut forwards, &locations);
    let mut last_refresh = Instant::now();
    let mut last_rendered: Option<DashboardFrame> = None;

    loop {
        if !session_path.exists() {
            return Ok(());
        }
        state.select(state.selected.min(forwards.len().saturating_sub(1)));
        state.expire_message(Instant::now());
        let frame = DashboardFrame::new(&config, &forwards, &locations, &state);
        if last_rendered.as_ref() != Some(&frame) {
            render_dashboard(&config, &forwards, &locations, &state)?;
            last_rendered = Some(frame);
        }

        if event::poll(Duration::from_millis(200)).map_err(|error| error.to_string())? {
            match event::read().map_err(|error| error.to_string())? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if should_close_popup(key, state.form.is_some(), is_popup_dashboard()) {
                        let _ = close_popup();
                        return Ok(());
                    }
                    handle_dashboard_key_with_session_path(
                        key,
                        &config,
                        &forwards,
                        &mut state,
                        Some(session_path),
                    );
                    last_refresh = Instant::now() - Duration::from_millis(750);
                }
                Event::Mouse(mouse) if state.form.is_none() && !state.help => {
                    handle_dashboard_mouse(mouse, &forwards, &locations, &mut state);
                    last_refresh = Instant::now() - Duration::from_millis(750);
                }
                _ => {}
            }
        }

        if last_refresh.elapsed() >= Duration::from_millis(750) {
            config = match read_json_file(session_path) {
                Ok(config) => config,
                Err(error) => {
                    state.show_unavailable(error);
                    last_refresh = Instant::now();
                    continue;
                }
            };
            match list_forwards(&config) {
                Ok(mut current) => {
                    locations = herdr_json(&["api", "snapshot"])
                        .map(|snapshot| pane_locations(&snapshot))
                        .unwrap_or_default();
                    sort_for_display(&mut current, &locations);
                    preserve_selected_forward(&mut state, &forwards, &current);
                    forwards = current;
                    state.mark_available();
                }
                Err(error) => state.show_unavailable(error),
            }
            last_refresh = Instant::now();
        }
    }
}

fn is_popup_dashboard() -> bool {
    env::var_os("HERDR_PANE_ID").is_none()
}

fn should_close_popup(key: event::KeyEvent, form_open: bool, is_popup: bool) -> bool {
    is_popup
        && !form_open
        && (matches!(key.code, event::KeyCode::Esc)
            || crate::plugin::dashboard_actions::is_shortcut(key, 'q'))
}

fn preserve_selected_forward(
    state: &mut DashboardState,
    previous: &[herdr_fwd::registry::Forward],
    refreshed: &[herdr_fwd::registry::Forward],
) {
    let selected_id = previous
        .get(state.selected)
        .map(|forward| forward.id.as_str());
    state.selected = selected_id
        .and_then(|id| refreshed.iter().position(|forward| forward.id == id))
        .unwrap_or_else(|| state.selected.min(refreshed.len().saturating_sub(1)));
}

#[cfg(test)]
mod dashboard_terminal_tests {
    use std::{collections::HashMap, time::Duration};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use herdr_fwd::{registry::Forward, RemoteSessionConfig};

    use super::{
        preserve_selected_forward, should_close_popup, DashboardFrame, DashboardState,
        DASHBOARD_MESSAGE_TTL,
    };

    fn forward(id: &str) -> Forward {
        Forward {
            id: id.into(),
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

    #[test]
    fn expires_status_messages_after_the_ttl() {
        let mut state = DashboardState::default();
        state.show_message("Forward paused", false);
        let shown_at = state.message.as_ref().unwrap().shown_at;

        state.expire_message(shown_at + DASHBOARD_MESSAGE_TTL - Duration::from_millis(1));
        assert!(state.message.is_some());
        state.expire_message(shown_at + DASHBOARD_MESSAGE_TTL);
        assert!(state.message.is_none());
    }

    #[test]
    fn clears_status_when_selection_changes() {
        let mut state = DashboardState::default();
        state.show_message("Forward paused", false);
        state.select(1);
        assert!(state.message.is_none());

        state.show_message("Forward enabled", false);
        state.select(1);
        assert!(state.message.is_some());
    }

    #[test]
    fn preserves_selected_forward_when_refresh_reorders_the_tree() {
        let previous = vec![forward("other"), forward("selected")];
        let refreshed = vec![forward("selected"), forward("new"), forward("other")];
        let mut state = DashboardState::default();
        state.select(1);
        state.show_message("Forward paused", false);

        preserve_selected_forward(&mut state, &previous, &refreshed);

        assert_eq!(state.selected, 0);
        assert!(state.message.is_some());
    }

    #[test]
    fn popup_close_keys_require_a_popup_without_a_form() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(should_close_popup(key(KeyCode::Esc), false, true));
        assert!(should_close_popup(key(KeyCode::Char('q')), false, true));
        assert!(!should_close_popup(key(KeyCode::Esc), true, true));
        assert!(!should_close_popup(key(KeyCode::Char('x')), false, true));
        assert!(!should_close_popup(key(KeyCode::Esc), false, false));
    }

    #[test]
    fn unavailable_state_is_persistent_until_refresh_recovers() {
        let mut state = DashboardState::default();
        state.show_unavailable("RPC unavailable");
        let shown_at = state.message.as_ref().unwrap().shown_at;

        state.expire_message(shown_at + DASHBOARD_MESSAGE_TTL);
        assert!(state.unavailable);
        assert_eq!(
            state.message.as_ref().unwrap().text,
            "UNAVAILABLE · RPC unavailable · Press r to retry"
        );

        state.mark_available();
        assert!(!state.unavailable);
        assert!(state.message.is_none());
    }

    #[test]
    fn render_frame_changes_only_when_visible_dashboard_state_changes() {
        let config = RemoteSessionConfig {
            protocol_version: 1,
            session_id: "0123456789abcdef01234567".into(),
            token: "ab".repeat(32),
            rpc_url: "http://127.0.0.1:23000".into(),
            auto_detect: true,
        };
        let forwards = Vec::<Forward>::new();
        let locations = HashMap::new();
        let mut state = DashboardState::default();
        let first = DashboardFrame::new(&config, &forwards, &locations, &state);
        let same = DashboardFrame::new(&config, &forwards, &locations, &state);
        assert!(first == same);

        state.help = true;
        let changed = DashboardFrame::new(&config, &forwards, &locations, &state);
        assert!(first != changed);
    }
}
