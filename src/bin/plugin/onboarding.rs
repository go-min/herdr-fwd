use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde::Deserialize;

use crate::plugin::{
    herdr::{close_popup, herdr_output, run_command_with_timeout},
    preferences::{load_preferences, set_onboarding},
    session::{installation_state_directory, is_session_file, session_directory},
    sidebar_config::{enable_dashboard_popup_shortcut, enable_ports_row},
};

const PLUGIN_ORIGIN_FILE: &str = "plugin-origin.toml";

#[derive(Deserialize)]
struct PluginOrigin {
    origin: String,
    plugin_root: String,
    version: String,
}

struct WelcomeTerminal;

struct WelcomeView<'a> {
    role: OnboardingRole,
    wrapper_installed: bool,
    actions: &'a [WelcomeAction],
    selected: usize,
    message: Option<&'a (String, bool)>,
}

impl WelcomeTerminal {
    fn enter() -> Result<Self, String> {
        terminal::enable_raw_mode().map_err(|error| error.to_string())?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(error.to_string());
        }
        Ok(Self)
    }
}

impl Drop for WelcomeTerminal {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen, ResetColor);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OnboardingDecision {
    Skip,
    Show {
        wrapper_installed: bool,
        installed_by_wrapper: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OnboardingRole {
    Connect,
    Host,
    Both,
}

impl OnboardingRole {
    const ALL: [Self; 3] = [Self::Connect, Self::Host, Self::Both];

    fn label(self) -> &'static str {
        match self {
            Self::Connect => "Connect to a remote host",
            Self::Host => "Host remote sessions",
            Self::Both => "Connect and host",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WelcomeAction {
    EnableSidebarStatus,
    EnablePopupShortcut,
    InstallWrapper,
    Skip,
    Done,
    UninstallPlugin,
}

impl WelcomeAction {
    fn label(self) -> &'static str {
        match self {
            Self::EnableSidebarStatus => "Enable port status (recommended)",
            Self::EnablePopupShortcut => "Enable dashboard shortcut (recommended)",
            Self::InstallWrapper => "Install hfwd",
            Self::Skip => "Skip",
            Self::Done => "Got it",
            Self::UninstallPlugin => "Uninstall plugin",
        }
    }
}

pub(crate) fn welcome_actions(
    role: OnboardingRole,
    wrapper_installed: bool,
    installed_by_wrapper: bool,
) -> Vec<WelcomeAction> {
    let mut actions = if wrapper_installed {
        vec![WelcomeAction::Done]
    } else if matches!(role, OnboardingRole::Connect | OnboardingRole::Both) {
        vec![WelcomeAction::InstallWrapper]
    } else {
        Vec::new()
    };
    actions.extend([
        WelcomeAction::EnableSidebarStatus,
        WelcomeAction::EnablePopupShortcut,
    ]);
    if !wrapper_installed {
        actions.push(WelcomeAction::Skip);
    }
    if installed_by_wrapper {
        actions.push(WelcomeAction::UninstallPlugin);
    }
    actions
}

pub(crate) fn welcome_copy(
    role: OnboardingRole,
    wrapper_installed: bool,
    remote_install: bool,
) -> String {
    let mut copy = String::from("Herdr Fwd is ready.\n\n");
    if remote_install {
        copy.push_str(
            "This plugin was installed on this machine from a remote machine during a remote connection by hfwd.\n\n",
        );
    }
    match role {
        OnboardingRole::Connect => {
            copy.push_str("Connect from this machine to a remote Herdr host.\n\n");
        }
        OnboardingRole::Host => {
            copy.push_str(
                "Host remote Herdr sessions and development servers on this machine. hfwd is optional here; install it only if this machine will also connect to another host.",
            );
        }
        OnboardingRole::Both => {
            copy.push_str("Connect from this machine and host remote Herdr sessions here.\n\n");
        }
    }
    if matches!(role, OnboardingRole::Connect | OnboardingRole::Both) {
        if wrapper_installed {
            copy.push_str("hfwd is installed. Start a remote session with:\n  hfwd <target>\n\n");
        } else {
            copy.push_str("Install hfwd to start remote sessions with:\n  hfwd <target>\n\n");
        }
        copy.push_str(
            "Replace <target> with the SSH host or SSH config alias for the remote machine.\n\nTo keep using herdr --remote, install the shell integration:\n  hfwd hook install",
        );
    }
    copy
}

pub(crate) fn wrap_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut output = Vec::new();
    for paragraph in value.split('\n') {
        if paragraph.is_empty() {
            output.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.len() + 1 + word.len() > width {
                output.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        output.push(line);
    }
    output
}

pub(crate) fn maybe_open() -> Result<(), String> {
    let onboarding_enabled = load_preferences()?.onboarding;
    let plugin_root = required_directory("HERDR_PLUGIN_ROOT")?;
    let sessions = session_directory()?;
    let wrapper_installed = wrapper_is_installed_in(&wrapper_search_paths());
    let installed_by_wrapper =
        installation_state_is_remote(&installation_state_directory()?, &plugin_root);
    match decide_onboarding(
        onboarding_enabled,
        has_wrapper_session(&sessions)?,
        wrapper_installed,
        installed_by_wrapper,
    ) {
        OnboardingDecision::Skip => Ok(()),
        OnboardingDecision::Show { .. } => herdr_output(&[
            "plugin",
            "pane",
            "open",
            "--plugin",
            "herdr.fwd",
            "--entrypoint",
            "welcome",
        ])
        .map(|_| ()),
    }
}

pub(crate) fn welcome() -> Result<(), String> {
    let plugin_root = required_directory("HERDR_PLUGIN_ROOT")?;
    let installed_by_wrapper =
        installation_state_is_remote(&installation_state_directory()?, &plugin_root);
    let remote_install = installed_by_wrapper;
    let mut wrapper_installed = wrapper_is_installed_in(&wrapper_search_paths());
    let mut role: Option<OnboardingRole> = None;
    let mut selected = 0usize;
    let mut message: Option<(String, bool)> = None;
    let _terminal = WelcomeTerminal::enter()?;

    loop {
        if let Some(role) = role {
            let actions = welcome_actions(role, wrapper_installed, installed_by_wrapper);
            selected = selected.min(actions.len().saturating_sub(1));
            render_welcome(
                role,
                wrapper_installed,
                &actions,
                selected,
                message.as_ref(),
            )?;
        } else {
            selected = selected.min(role_selection_item_count(remote_install).saturating_sub(1));
            render_role_selection(selected, remote_install, message.as_ref())?;
        }
        let Event::Key(key) = event::read().map_err(|error| error.to_string())? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                let items = role.map_or(role_selection_item_count(remote_install), |role| {
                    welcome_actions(role, wrapper_installed, installed_by_wrapper).len()
                });
                selected = (selected + 1).min(items.saturating_sub(1));
            }
            KeyCode::Esc | KeyCode::Backspace if role.is_some() => {
                role = None;
                selected = 0;
                message = None;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                set_onboarding(false)?;
                let _ = close_popup();
                return Ok(());
            }
            KeyCode::Enter => {
                if let Some(role) = role {
                    match welcome_actions(role, wrapper_installed, installed_by_wrapper)[selected] {
                        WelcomeAction::EnableSidebarStatus => match enable_ports_row()
                            .and_then(|_| herdr_output(&["server", "reload-config"]).map(|_| ()))
                        {
                            Ok(()) => {
                                message =
                                    Some(("Port-forward status enabled in Herdr.".into(), false));
                            }
                            Err(error) => message = Some((error, true)),
                        },
                        WelcomeAction::EnablePopupShortcut => {
                            match enable_dashboard_popup_shortcut().and_then(|_| {
                                herdr_output(&["server", "reload-config"]).map(|_| ())
                            }) {
                                Ok(()) => {
                                    message = Some((
                                        "Dashboard shortcut enabled; use --remote-keybindings server."
                                            .into(),
                                        false,
                                    ));
                                }
                                Err(error) => message = Some((error, true)),
                            }
                        }
                        WelcomeAction::InstallWrapper => {
                            match install_wrapper_from(&plugin_root, env!("CARGO_PKG_VERSION")) {
                                Ok(()) => {
                                    wrapper_installed = true;
                                    selected = 0;
                                    message = Some(("hfwd installed successfully.".into(), false));
                                }
                                Err(error) => message = Some((error, true)),
                            }
                        }
                        WelcomeAction::Skip | WelcomeAction::Done => {
                            set_onboarding(false)?;
                            let _ = close_popup();
                            return Ok(());
                        }
                        WelcomeAction::UninstallPlugin => {
                            match uninstall_plugin_with(&herdr_binary_path()) {
                                Ok(()) => {
                                    clear_remote_origin()?;
                                    set_onboarding(false)?;
                                    let _ = close_popup();
                                    return Ok(());
                                }
                                Err(error) => message = Some((error, true)),
                            }
                        }
                    }
                } else {
                    if remote_install && selected == OnboardingRole::ALL.len() {
                        match uninstall_plugin_with(&herdr_binary_path()) {
                            Ok(()) => {
                                clear_remote_origin()?;
                                set_onboarding(false)?;
                                let _ = close_popup();
                                return Ok(());
                            }
                            Err(error) => message = Some((error, true)),
                        }
                    } else {
                        role = Some(OnboardingRole::ALL[selected]);
                        selected = 0;
                    }
                }
            }
            _ => {}
        }
    }
}

fn render_welcome(
    role: OnboardingRole,
    wrapper_installed: bool,
    actions: &[WelcomeAction],
    selected: usize,
    message: Option<&(String, bool)>,
) -> Result<(), String> {
    let (columns, _) = terminal::size().unwrap_or((76, 22));
    let mut stdout = io::stdout();
    let view = WelcomeView {
        role,
        wrapper_installed,
        actions,
        selected,
        message,
    };
    render_welcome_to(&mut stdout, columns, &view)?;
    stdout.flush().map_err(|error| error.to_string())
}

fn render_welcome_to<W: Write>(
    stdout: &mut W,
    columns: u16,
    view: &WelcomeView<'_>,
) -> Result<(), String> {
    let content_width = usize::from(columns.saturating_sub(8)).clamp(24, 68);
    queue!(
        stdout,
        Clear(ClearType::All),
        MoveTo(0, 0),
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        Print("\r\n   ◆ Herdr Fwd\r\n"),
        SetAttribute(Attribute::Reset),
        SetForegroundColor(Color::DarkGrey),
        Print("   Port forwarding for remote Herdr sessions\r\n"),
        Print("   ────────────────────────────────────────────────────────────────\r\n\r\n"),
        ResetColor
    )
    .map_err(|error| error.to_string())?;

    for line in wrap_text(
        &welcome_copy(view.role, view.wrapper_installed, false),
        content_width,
    ) {
        queue!(stdout, Print("   "), Print(line), Print("\r\n"))
            .map_err(|error| error.to_string())?;
    }
    queue!(stdout, Print("\r\n")).map_err(|error| error.to_string())?;
    for (index, action) in view.actions.iter().enumerate() {
        if index == view.selected {
            queue!(
                stdout,
                SetForegroundColor(Color::Cyan),
                SetAttribute(Attribute::Bold),
                Print("   › "),
                Print(action.label()),
                SetAttribute(Attribute::Reset),
                ResetColor,
                Print("\r\n")
            )
        } else {
            queue!(stdout, Print("     "), Print(action.label()), Print("\r\n"))
        }
        .map_err(|error| error.to_string())?;
    }
    if let Some((text, error)) = view.message {
        queue!(
            stdout,
            Print("\r\n   "),
            SetForegroundColor(if *error { Color::Red } else { Color::Green }),
            Print(text),
            ResetColor,
            Print("\r\n")
        )
        .map_err(|error| error.to_string())?;
    }
    queue!(
        stdout,
        Print("\r\n   "),
        SetForegroundColor(Color::DarkGrey),
        Print("↑↓ navigate   Enter select   Esc/Backspace back   q close"),
        ResetColor
    )
    .map_err(|error| error.to_string())
}

fn render_role_selection(
    selected: usize,
    remote_install: bool,
    message: Option<&(String, bool)>,
) -> Result<(), String> {
    let (columns, _) = terminal::size().unwrap_or((76, 22));
    let mut stdout = io::stdout();
    render_role_selection_to(&mut stdout, columns, selected, remote_install, message)?;
    stdout.flush().map_err(|error| error.to_string())
}

fn render_role_selection_to<W: Write>(
    stdout: &mut W,
    _columns: u16,
    selected: usize,
    remote_install: bool,
    message: Option<&(String, bool)>,
) -> Result<(), String> {
    queue!(
        stdout,
        Clear(ClearType::All),
        MoveTo(0, 0),
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        Print("\r\n   ◆ Herdr Fwd\r\n"),
        SetAttribute(Attribute::Reset),
        SetForegroundColor(Color::DarkGrey),
        Print("   Port forwarding for remote Herdr sessions\r\n"),
        Print("   ────────────────────────────────────────────────────────────────\r\n\r\n"),
        ResetColor
    )
    .map_err(|error| error.to_string())?;
    if remote_install {
        queue!(
            stdout,
            SetForegroundColor(Color::Yellow),
            SetAttribute(Attribute::Bold),
            Print("   ⚠ REMOTE INSTALLATION\r\n"),
            SetAttribute(Attribute::Reset),
            Print("   This plugin was installed on this machine from a remote machine\r\n"),
            Print("   during a remote connection by hfwd.\r\n"),
            ResetColor,
            Print("\r\n")
        )
        .map_err(|error| error.to_string())?;
    }
    queue!(stdout, Print("   What will this machine do?\r\n\r\n"))
        .map_err(|error| error.to_string())?;
    for (index, role) in OnboardingRole::ALL.iter().enumerate() {
        if index == selected {
            queue!(
                stdout,
                SetForegroundColor(Color::Cyan),
                SetAttribute(Attribute::Bold),
                Print("   › "),
                Print(role.label()),
                SetAttribute(Attribute::Reset),
                ResetColor,
                Print("\r\n")
            )
        } else {
            queue!(stdout, Print("     "), Print(role.label()), Print("\r\n"))
        }
        .map_err(|error| error.to_string())?;
    }
    if remote_install {
        let selected = selected == OnboardingRole::ALL.len();
        if selected {
            queue!(
                stdout,
                SetForegroundColor(Color::Yellow),
                SetAttribute(Attribute::Bold),
                Print("   › Uninstall plugin"),
                SetAttribute(Attribute::Reset),
                ResetColor,
                Print("\r\n")
            )
        } else {
            queue!(stdout, Print("     Uninstall plugin\r\n"))
        }
        .map_err(|error| error.to_string())?;
    }
    if let Some((text, error)) = message {
        queue!(
            stdout,
            Print("\r\n   "),
            SetForegroundColor(if *error { Color::Red } else { Color::Green }),
            Print(text),
            ResetColor,
            Print("\r\n")
        )
        .map_err(|error| error.to_string())?;
    }
    queue!(
        stdout,
        Print("\r\n   "),
        SetForegroundColor(Color::DarkGrey),
        Print("↑↓ navigate   Enter select   Esc close"),
        ResetColor
    )
    .map_err(|error| error.to_string())
}

fn role_selection_item_count(remote_install: bool) -> usize {
    OnboardingRole::ALL.len() + usize::from(remote_install)
}

fn required_directory(name: &str) -> Result<PathBuf, String> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} is not set"))
}

fn wrapper_search_paths() -> Vec<PathBuf> {
    let mut paths = env::var_os("PATH")
        .as_deref()
        .map(env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if let Some(home) = env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".local/bin"));
    }
    paths
}

fn herdr_binary_path() -> PathBuf {
    env::var_os("HERDR_BIN_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("herdr"))
}

pub(crate) fn decide_onboarding(
    onboarding_enabled: bool,
    active_wrapper_session: bool,
    wrapper_installed: bool,
    installed_by_wrapper: bool,
) -> OnboardingDecision {
    if !onboarding_enabled || active_wrapper_session {
        OnboardingDecision::Skip
    } else {
        OnboardingDecision::Show {
            wrapper_installed,
            installed_by_wrapper,
        }
    }
}

pub(crate) fn install_wrapper_from(plugin_root: &Path, version: &str) -> Result<(), String> {
    let installer = plugin_root.join("install.sh");
    let output = Command::new(&installer)
        .args(["--version", version])
        .output()
        .map_err(|error| format!("failed to run {}: {error}", installer.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if error.is_empty() {
            format!("wrapper installer exited with {}", output.status)
        } else {
            error
        })
    }
}

pub(crate) fn uninstall_plugin_with(herdr_binary: &Path) -> Result<(), String> {
    uninstall_plugin_with_timeout(herdr_binary, Duration::from_secs(10))
}

fn uninstall_plugin_with_timeout(herdr_binary: &Path, timeout: Duration) -> Result<(), String> {
    let plugin_root = env::var_os("HERDR_PLUGIN_ROOT").map(PathBuf::from);
    let managed_root = managed_release_bundle_root();
    let remove_managed_bundles = plugin_root.as_deref() == Some(managed_root.as_path())
        || plugin_root.as_deref().is_some_and(|plugin_root| {
            installation_state_directory()
                .ok()
                .is_some_and(|state| installation_state_is_remote(&state, plugin_root))
        });
    let arguments = uninstall_plugin_arguments(plugin_root.as_deref(), &managed_root);
    let output = run_command_with_timeout(herdr_binary, &arguments, timeout)
        .map_err(|error| format!("failed to run Herdr CLI: {error}"))?;
    if output.status.success() {
        if remove_managed_bundles {
            let managed_directory = managed_root
                .parent()
                .expect("managed bundle root always has a parent");
            match fs::remove_dir_all(managed_directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "plugin was removed but managed bundles {} could not be removed: {error}",
                        managed_directory.display()
                    ))
                }
            }
        }
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if error.is_empty() {
            format!("plugin uninstall exited with {}", output.status)
        } else {
            error
        })
    }
}

fn managed_release_bundle_root() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("herdr-fwd/plugins/current")
}

fn uninstall_plugin_arguments(
    plugin_root: Option<&Path>,
    managed_root: &Path,
) -> [&'static str; 3] {
    if plugin_root == Some(managed_root) {
        ["plugin", "unlink", "herdr.fwd"]
    } else {
        ["plugin", "uninstall", "herdr.fwd"]
    }
}

pub(crate) fn has_wrapper_session(session_directory: &Path) -> Result<bool, String> {
    let entries = match fs::read_dir(session_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "failed to inspect {}: {error}",
                session_directory.display()
            ))
        }
    };
    Ok(entries
        .flatten()
        .map(|entry| entry.path())
        .any(|path| is_session_file(&path)))
}

pub(crate) fn installation_state_is_remote(state_directory: &Path, plugin_root: &Path) -> bool {
    fs::read_to_string(state_directory.join(PLUGIN_ORIGIN_FILE))
        .ok()
        .and_then(|contents| toml_edit::de::from_str::<PluginOrigin>(&contents).ok())
        .is_some_and(|origin| {
            origin.origin == "hfwd_remote"
                && origin.version == env!("CARGO_PKG_VERSION")
                && Path::new(&origin.plugin_root) == plugin_root
        })
}

fn clear_remote_origin() -> Result<(), String> {
    let path = installation_state_directory()?.join(PLUGIN_ORIGIN_FILE);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed to remove {}: {error}", path.display())),
    }
}

pub(crate) fn wrapper_is_installed_in(path_directories: &[PathBuf]) -> bool {
    path_directories.iter().any(|directory| {
        let candidate = directory.join("hfwd");
        let Ok(metadata) = fs::metadata(candidate) else {
            return false;
        };
        #[cfg(unix)]
        return metadata.is_file() && metadata.permissions().mode() & 0o111 != 0;
        #[cfg(not(unix))]
        return metadata.is_file();
    })
}

#[cfg(test)]
mod onboarding_tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{
        decide_onboarding, has_wrapper_session, install_wrapper_from, installation_state_is_remote,
        render_welcome_to, uninstall_plugin_arguments, uninstall_plugin_with_timeout,
        welcome_actions, wrapper_is_installed_in, OnboardingDecision, OnboardingRole, WelcomeView,
    };

    static TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            let id = TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "herdr-fwd-onboarding-test-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
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

    #[test]
    fn onboarding_respects_preferences_sessions_and_remote_provenance() {
        assert_eq!(
            decide_onboarding(true, false, true, false),
            OnboardingDecision::Show {
                wrapper_installed: true,
                installed_by_wrapper: false,
            }
        );
        assert_eq!(
            decide_onboarding(true, false, false, true),
            OnboardingDecision::Show {
                wrapper_installed: false,
                installed_by_wrapper: true,
            }
        );
        assert_eq!(
            decide_onboarding(false, false, false, false),
            OnboardingDecision::Skip
        );
        assert_eq!(
            decide_onboarding(true, true, false, true),
            OnboardingDecision::Skip
        );
    }

    #[cfg(unix)]
    #[test]
    fn one_click_install_uses_the_checkout_installer_and_exact_plugin_version() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TestDirectory::create();
        let installer = temporary.path().join("install.sh");
        fs::write(
            &installer,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/arguments\"\n",
        )
        .unwrap();
        fs::set_permissions(&installer, fs::Permissions::from_mode(0o755)).unwrap();

        install_wrapper_from(temporary.path(), "1.2.3").unwrap();

        assert_eq!(
            fs::read_to_string(temporary.path().join("arguments")).unwrap(),
            "--version\n1.2.3\n"
        );
        assert!(Path::new(&installer).exists());

        fs::write(
            &installer,
            "#!/bin/sh\necho 'download failed' >&2\nexit 7\n",
        )
        .unwrap();
        fs::set_permissions(&installer, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(install_wrapper_from(temporary.path(), "1.2.3")
            .unwrap_err()
            .contains("download failed"));
    }

    #[test]
    fn quick_uninstall_unlinks_only_the_hfwd_managed_release_bundle() {
        let managed = Path::new("/home/test/.local/share/herdr-fwd/plugins/current");
        assert_eq!(
            uninstall_plugin_arguments(Some(managed), managed),
            ["plugin", "unlink", "herdr.fwd"]
        );
        assert_eq!(
            uninstall_plugin_arguments(
                Some(Path::new("/home/test/.config/herdr/plugins/github/fwd")),
                managed,
            ),
            ["plugin", "uninstall", "herdr.fwd"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn quick_uninstall_does_not_wait_for_descendants() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TestDirectory::create();
        let herdr = temporary.path().join("herdr");
        fs::write(&herdr, "#!/bin/sh\n(sleep 5)& wait\n").unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();

        let started = std::time::Instant::now();
        let error = uninstall_plugin_with_timeout(&herdr, std::time::Duration::from_millis(50))
            .expect_err("uninstall should time out");

        assert!(error.contains("timed out"));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "onboarding uninstall must remain bounded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn detects_wrapper_sessions_installation_and_provenance_from_real_paths() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TestDirectory::create();
        let sessions = temporary.path().join("sessions");
        let plugin_root = temporary.path().join("plugin");
        let state = temporary.path().join("state");
        let bin = temporary.path().join("bin");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&plugin_root).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::write(sessions.join("session-abc.json"), b"{}").unwrap();
        fs::write(sessions.join("session-abc.dashboard.json"), b"{}").unwrap();
        let wrapper = bin.join("hfwd");
        fs::write(&wrapper, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(has_wrapper_session(&sessions).unwrap());
        fs::write(
            state.join("plugin-origin.toml"),
            format!(
                "origin = \"hfwd_remote\"\nplugin_root = \"{}\"\nversion = \"{}\"\n",
                plugin_root.display(),
                env!("CARGO_PKG_VERSION")
            ),
        )
        .unwrap();
        assert!(installation_state_is_remote(&state, &plugin_root));
        fs::write(
            state.join("plugin-origin.toml"),
            "origin = \"hfwd_remote\"\nplugin_root = \"/other\"\nversion = \"0.1.2\"\n",
        )
        .unwrap();
        assert!(!installation_state_is_remote(&state, &plugin_root));
        assert!(wrapper_is_installed_in(&[bin]));

        fs::remove_file(sessions.join("session-abc.json")).unwrap();
        assert!(!has_wrapper_session(&sessions).unwrap());
    }

    #[test]
    fn rendered_welcome_uses_terminal_safe_line_endings() {
        let actions = welcome_actions(OnboardingRole::Connect, false, false);
        let mut output = Vec::new();
        let view = WelcomeView {
            role: OnboardingRole::Connect,
            wrapper_installed: false,
            actions: &actions,
            selected: 0,
            message: None,
        };

        render_welcome_to(&mut output, 73, &view).unwrap();

        assert!(output.contains(&b'\n'));
        assert!(output
            .iter()
            .enumerate()
            .all(|(index, byte)| *byte != b'\n' || index > 0 && output[index - 1] == b'\r'));
    }
}
