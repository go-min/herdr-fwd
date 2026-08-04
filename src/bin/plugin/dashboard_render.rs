use std::{collections::HashMap, io::Write};

use crossterm::{
    cursor::MoveTo,
    queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{self, Clear, ClearType},
};
use herdr_fwd::{registry::Forward, RemoteSessionConfig};

use crate::plugin::dashboard_terminal::{
    CustomForwardForm, DashboardForm, DashboardState, IntegrationMenu, LocalPortForm,
    RemoveForwardConfirmation,
};
use crate::plugin::herdr::PaneLocation;
use crate::plugin::sidebar_config::configured_theme_name;

pub(crate) fn render_dashboard(
    config: &RemoteSessionConfig,
    forwards: &[Forward],
    locations: &HashMap<String, PaneLocation>,
    state: &DashboardState,
) -> Result<(), String> {
    let (width, height) = terminal::size().unwrap_or((100, 30));
    let mut stdout = std::io::stdout();
    let (total, active, paused) = forward_counts(forwards);
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All)).map_err(|error| error.to_string())?;

    queue!(
        stdout,
        ResetColor,
        SetAttribute(Attribute::Bold),
        Print("  HERDR  /  PORT FORWARD"),
        SetAttribute(Attribute::Reset),
        Print("\r\n"),
        SetAttribute(Attribute::Dim),
        Print(format!(
            "  Session {}  ·  loopback only  ·  {} total  {} active  {} paused{}\r\n",
            &config.session_id[..config.session_id.len().min(8)],
            total,
            active,
            paused,
            if state.unavailable {
                "  ·  UNAVAILABLE — retrying"
            } else {
                ""
            },
        )),
        SetAttribute(Attribute::Reset),
        Print("\r\n")
    )
    .map_err(|error| error.to_string())?;

    if forwards.is_empty() {
        queue!(
            stdout,
            SetAttribute(Attribute::Dim),
            Print("  ┌──────────────────────────────────────────────┐\r\n"),
            Print("  │  No forwarded ports yet                     │\r\n"),
            Print("  │  Start a dev server or press a to add one.  │\r\n"),
            Print("  └──────────────────────────────────────────────┘\r\n"),
            SetAttribute(Attribute::Reset)
        )
        .map_err(|error| error.to_string())?;
    } else {
        let lines = tree_lines(forwards, locations);
        let palette = DashboardPalette::from_environment();
        let visible_lines = height.saturating_sub(5) as usize;
        let selected_line = lines
            .iter()
            .position(|line| line.forward_index == Some(state.selected))
            .unwrap_or(0);
        let (start, end) = tree_scroll_window(lines.len(), selected_line, visible_lines);
        for line in lines.into_iter().skip(start).take(end - start) {
            render_tree_line(
                &mut stdout,
                width,
                &line,
                line.forward_index == Some(state.selected),
                forwards,
                palette,
            )?;
        }
    }

    let footer_y = height.saturating_sub(2);
    queue!(
        stdout,
        MoveTo(0, footer_y),
        Clear(ClearType::FromCursorDown)
    )
    .map_err(|error| error.to_string())?;
    if let Some(message) = &state.message {
        let message_text = compact_message(&message.text);
        queue!(
            stdout,
            SetAttribute(Attribute::Bold),
            Print(format!(
                "  {} {}\r\n",
                if message.error { "!" } else { "✓" },
                truncate(&message_text, width.saturating_sub(6) as usize)
            )),
            SetAttribute(Attribute::Reset)
        )
        .map_err(|error| error.to_string())?;
    } else {
        queue!(stdout, Print("\r\n")).map_err(|error| error.to_string())?;
    }
    let shortcuts =
        canonical_dashboard_shortcuts(width, forwards.len(), forwards.get(state.selected));
    render_dashboard_shortcuts(&mut stdout, width, &shortcuts)?;

    if state.help {
        render_help(
            &mut stdout,
            width,
            height,
            forwards.len(),
            forwards.get(state.selected),
        )?;
    }
    if let Some(form) = &state.form {
        match form {
            DashboardForm::ConfirmRemoval(form) => {
                render_remove_confirmation(&mut stdout, width, height, form)?
            }
            DashboardForm::Retarget(form) => {
                render_local_port_form(&mut stdout, width, height, form)?
            }
            DashboardForm::Create(form) => render_create_form(&mut stdout, width, height, form)?,
            DashboardForm::Integration(form) => {
                render_integration_menu(&mut stdout, width, height, form)?
            }
        }
    }
    stdout.flush().map_err(|error| error.to_string())
}

fn compact_message(message: &str) -> String {
    message.replace(['\r', '\n'], " · ")
}

fn render_remove_confirmation(
    stdout: &mut impl Write,
    width: u16,
    height: u16,
    form: &RemoveForwardConfirmation,
) -> Result<(), String> {
    let lines = [
        "REMOVE CUSTOM FORWARD".to_string(),
        "".to_string(),
        format!("Forward {} will be removed.", form.id),
        "".to_string(),
        "Press d or Delete to remove · Esc cancel".to_string(),
    ];
    let refs = lines.iter().map(String::as_str).collect::<Vec<_>>();
    render_overlay(stdout, width, height, &refs)
}

fn tree_scroll_window(
    line_count: usize,
    selected_line: usize,
    visible_lines: usize,
) -> (usize, usize) {
    let visible_lines = visible_lines.min(line_count);
    let start = selected_line
        .saturating_add(2)
        .saturating_sub(visible_lines)
        .min(line_count.saturating_sub(visible_lines));
    (start, start + visible_lines)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DashboardShortcut {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    footer: bool,
}

fn canonical_dashboard_shortcuts(
    width: u16,
    forward_count: usize,
    selected: Option<&Forward>,
) -> Vec<DashboardShortcut> {
    let compact = width < 80;
    let mut shortcuts = Vec::new();
    if forward_count > 1 {
        shortcuts.push(if compact {
            DashboardShortcut {
                key: "↑↓",
                label: "move",
                footer: true,
            }
        } else {
            DashboardShortcut {
                key: "↑↓/jk",
                label: "navigate",
                footer: true,
            }
        });
    }
    if let Some(forward) = selected {
        if forward.pane_id != "manual" {
            shortcuts.push(DashboardShortcut {
                key: if compact { "↵" } else { "Enter" },
                label: "pane",
                footer: true,
            });
        }
        if forward.enabled {
            shortcuts.push(DashboardShortcut {
                key: "o",
                label: "open",
                footer: true,
            });
        }
        shortcuts.push(DashboardShortcut {
            key: "Space",
            label: "toggle",
            footer: true,
        });
        shortcuts.push(DashboardShortcut {
            key: "p",
            label: if compact { "port" } else { "change port" },
            footer: true,
        });
        if !forward.automatic {
            shortcuts.push(DashboardShortcut {
                key: "d",
                label: "remove",
                footer: true,
            });
        }
    }
    shortcuts.push(DashboardShortcut {
        key: "a",
        label: "add",
        footer: true,
    });
    shortcuts.push(DashboardShortcut {
        key: "h",
        label: "settings",
        footer: true,
    });
    shortcuts.push(DashboardShortcut {
        key: "?",
        label: if compact { "" } else { "help" },
        footer: true,
    });
    shortcuts.push(DashboardShortcut {
        key: "r",
        label: "refresh",
        footer: false,
    });
    shortcuts.push(DashboardShortcut {
        key: "Esc",
        label: "close",
        footer: false,
    });
    shortcuts
}

fn render_dashboard_shortcuts(
    stdout: &mut impl Write,
    width: u16,
    shortcuts: &[DashboardShortcut],
) -> Result<(), String> {
    if width < 2 {
        return Ok(());
    }
    let separator = if width < 80 { "  " } else { "   " };
    let mut used = 2;
    queue!(stdout, SetAttribute(Attribute::Dim), Print("  ")).map_err(|error| error.to_string())?;
    for (index, shortcut) in shortcuts
        .iter()
        .filter(|shortcut| shortcut.footer)
        .enumerate()
    {
        let separator_width = usize::from(index > 0) * separator.chars().count();
        let label_width = if shortcut.label.is_empty() {
            0
        } else {
            1 + shortcut.label.chars().count()
        };
        let segment_width = separator_width + shortcut.key.chars().count() + label_width;
        if used + segment_width > width as usize {
            break;
        }
        if index > 0 {
            queue!(stdout, SetAttribute(Attribute::Dim), Print(separator))
                .map_err(|error| error.to_string())?;
        }
        queue!(
            stdout,
            SetAttribute(Attribute::Reset),
            SetAttribute(Attribute::Bold),
            Print(shortcut.key),
            SetAttribute(Attribute::Reset)
        )
        .map_err(|error| error.to_string())?;
        if !shortcut.label.is_empty() {
            queue!(
                stdout,
                SetAttribute(Attribute::Dim),
                Print(format!(" {}", shortcut.label))
            )
            .map_err(|error| error.to_string())?;
        }
        used += segment_width;
    }
    queue!(stdout, SetAttribute(Attribute::Reset)).map_err(|error| error.to_string())
}

#[cfg(test)]
fn shortcut_text(shortcuts: &[DashboardShortcut], compact: bool) -> String {
    let separator = if compact { "  " } else { "   " };
    format!(
        "  {}",
        shortcuts
            .iter()
            .filter(|shortcut| shortcut.footer)
            .map(|shortcut| {
                if shortcut.label.is_empty() {
                    shortcut.key.to_string()
                } else {
                    format!("{} {}", shortcut.key, shortcut.label)
                }
            })
            .collect::<Vec<_>>()
            .join(separator)
    )
}

struct TreeLine {
    text: String,
    forward_index: Option<usize>,
}

struct LocationInfo<'a> {
    workspace_id: &'a str,
    workspace: &'a str,
    workspace_number: u64,
    tab_id: &'a str,
    tab: &'a str,
    tab_number: u64,
}

fn tree_lines(forwards: &[Forward], locations: &HashMap<String, PaneLocation>) -> Vec<TreeLine> {
    let mut lines = Vec::new();
    let mut previous = (String::new(), String::new(), String::new());
    let mut manual_section = false;
    let indexes = tree_forward_indexes(forwards, locations);
    for (position, index) in indexes.iter().copied().enumerate() {
        let forward = &forwards[index];
        if !forward.automatic {
            if !manual_section {
                if !lines.is_empty() {
                    lines.push(TreeLine {
                        text: String::new(),
                        forward_index: None,
                    });
                }
                lines.push(TreeLine {
                    text: "  󰖟 MANUAL FORWARDS".into(),
                    forward_index: None,
                });
                manual_section = true;
            }
            let has_next_manual = indexes[position + 1..]
                .iter()
                .any(|next_index| !forwards[*next_index].automatic);
            let branch = if has_next_manual { "├─" } else { "└─" };
            let continuation = if has_next_manual {
                "  │  "
            } else {
                "       "
            };
            lines.push(TreeLine {
                text: format!(
                    "  {branch} {status} {state} {host}:{remote_port}  →  localhost:{local_port}  ·  {tunnel}",
                    status = status(forward),
                    state = forward_state_label(forward),
                    host = forward.remote_host_display(),
                    remote_port = forward.remote_port,
                    local_port = forward.local_port,
                    tunnel = tunnel_status(forward),
                ),
                forward_index: Some(index),
            });
            lines.push(TreeLine {
                text: format!("{continuation}tunnel {}", tunnel_time(forward)),
                forward_index: Some(index),
            });
            continue;
        }
        let location = location_info(forward, locations);
        let workspace_id = location.workspace_id;
        let workspace = location.workspace;
        let tab_id = location.tab_id;
        let tab = location.tab;
        let pane_id = forward.pane_id.as_str();
        let pane = locations
            .get(pane_id)
            .map(|location| pane_heading(forward, location))
            .unwrap_or_else(|| forward.process.clone());
        if workspace_id != previous.0 {
            lines.push(TreeLine {
                text: format!("  󰉋 {workspace}"),
                forward_index: None,
            });
            previous.0 = workspace_id.into();
            previous.1.clear();
            previous.2.clear();
        }
        if tab_id != previous.1 {
            let last_tab = !indexes[position + 1..].iter().any(|&next| {
                let candidate = &forwards[next];
                candidate.automatic && {
                    let candidate_location = location_info(candidate, locations);
                    candidate_location.workspace_id == workspace_id
                        && candidate_location.tab_id != tab_id
                }
            });
            lines.push(TreeLine {
                text: format!(
                    "  {} {} {tab}",
                    if last_tab { "└─" } else { "├─" },
                    tab_icon()
                ),
                forward_index: None,
            });
            previous.1 = tab_id.into();
            previous.2.clear();
        }
        let tab_has_future = indexes[position + 1..].iter().any(|&next| {
            let candidate = &forwards[next];
            candidate.automatic && {
                let candidate_location = location_info(candidate, locations);
                candidate_location.workspace_id == workspace_id
                    && candidate_location.tab_id != tab_id
            }
        });
        let pane_has_future = indexes[position + 1..].iter().any(|&next| {
            let candidate = &forwards[next];
            candidate.automatic && {
                let candidate_location = location_info(candidate, locations);
                candidate_location.workspace_id == workspace_id
                    && candidate_location.tab_id == tab_id
                    && candidate.pane_id != forward.pane_id
            }
        });
        let tab_continuation = if tab_has_future { "  │  " } else { "     " };
        let pane_continuation = if pane_has_future {
            format!("{tab_continuation}│  ")
        } else {
            format!("{tab_continuation}   ")
        };
        if pane_id != previous.2 {
            lines.push(TreeLine {
                text: format!(
                    "{tab_continuation}{} {} {pane}  │  {}",
                    if pane_has_future { "├─" } else { "└─" },
                    pane_icon(),
                    pane_metadata(forward),
                ),
                forward_index: None,
            });
            previous.2 = pane_id.into();
        }
        lines.push(TreeLine {
            text: format!(
                "{}{} {} {}:{}  →  localhost:{}  ·  {}",
                pane_continuation,
                status(forward),
                forward_state_label(forward),
                forward.remote_host_display(),
                forward.remote_port,
                forward.local_port,
                tunnel_status(forward),
            ),
            forward_index: Some(index),
        });
        lines.push(TreeLine {
            text: format!(
                "{}  server {}  ·  tunnel {}",
                pane_continuation,
                forward
                    .server_started_at
                    .map(clock)
                    .unwrap_or_else(|| "—".into()),
                tunnel_time(forward),
            ),
            forward_index: Some(index),
        });
    }
    lines
}

pub(crate) fn forward_index_at(
    row: u16,
    height: u16,
    selected: usize,
    forwards: &[Forward],
    locations: &HashMap<String, PaneLocation>,
) -> Option<usize> {
    const TREE_START_ROW: u16 = 3;
    if row < TREE_START_ROW || row >= height.saturating_sub(2) || forwards.is_empty() {
        return None;
    }
    let lines = tree_lines(forwards, locations);
    let visible_lines = height.saturating_sub(5) as usize;
    if visible_lines == 0 {
        return None;
    }
    let selected_line = lines
        .iter()
        .position(|line| line.forward_index == Some(selected))
        .unwrap_or(0);
    let (start, end) = tree_scroll_window(lines.len(), selected_line, visible_lines);
    let line_index = start + usize::from(row - TREE_START_ROW);
    if line_index >= end {
        return None;
    }
    lines[line_index].forward_index
}

fn location_info<'a>(
    forward: &'a Forward,
    locations: &'a HashMap<String, PaneLocation>,
) -> LocationInfo<'a> {
    locations
        .get(&forward.pane_id)
        .map(|location| LocationInfo {
            workspace_id: &location.workspace_id,
            workspace: &location.workspace,
            workspace_number: location.workspace_number,
            tab_id: &location.tab_id,
            tab: &location.tab,
            tab_number: location.tab_number,
        })
        .unwrap_or_else(|| LocationInfo {
            workspace_id: &forward.pane_id,
            workspace: "Unknown Space",
            workspace_number: u64::MAX,
            tab_id: &forward.pane_id,
            tab: "Unknown tab",
            tab_number: u64::MAX,
        })
}

fn tab_icon() -> &'static str {
    "󰓩"
}

fn pane_icon() -> &'static str {
    "󰆍"
}

fn pane_heading(forward: &Forward, location: &PaneLocation) -> String {
    location
        .pane_label
        .as_deref()
        .unwrap_or(&forward.process)
        .into()
}

fn pane_metadata(forward: &Forward) -> String {
    format!(
        "{}  ·  {} {}{}",
        forward.pane_id,
        process_icon(&forward.process),
        forward.process,
        forward
            .process_id
            .map(|process_id| format!("  ·  pid {process_id}"))
            .unwrap_or_default()
    )
}

fn process_icon(process: &str) -> &'static str {
    let process = process.to_ascii_lowercase();
    const DEDICATED_TOOL_ICONS: &[(&str, &str)] = &[
        ("storybook", ""),
        ("vite", ""),
        ("next", ""),
        ("astro", ""),
        ("nuxt", ""),
        ("vue", ""),
        ("svelte", ""),
        ("angular", ""),
        ("react", ""),
        ("webpack", ""),
        ("express", ""),
        ("fastify", ""),
        ("nest", ""),
        ("node", ""),
        ("django", ""),
        ("fastapi", ""),
        ("flask", ""),
        ("python", ""),
        ("rails", ""),
        ("ruby", ""),
        ("cargo", ""),
        ("rust", ""),
        ("spring", ""),
        ("java", ""),
        ("laravel", ""),
        ("php", ""),
        ("docker", ""),
        ("nginx", ""),
        ("apache", ""),
        ("bun", ""),
        ("deno", ""),
    ];

    if let Some((_, icon)) = DEDICATED_TOOL_ICONS
        .iter()
        .find(|(name, _)| process.contains(name))
    {
        icon
    } else if process == "go" || process.contains("golang") || process.starts_with("go ") {
        ""
    } else {
        "󰒋"
    }
}

pub(crate) fn sort_for_display(
    forwards: &mut Vec<Forward>,
    locations: &HashMap<String, PaneLocation>,
) {
    let indexes = tree_forward_indexes(forwards, locations);
    let current = forwards.clone();
    *forwards = indexes
        .into_iter()
        .map(|index| current[index].clone())
        .collect();
}

fn tree_forward_indexes(
    forwards: &[Forward],
    locations: &HashMap<String, PaneLocation>,
) -> Vec<usize> {
    let mut indexes = (0..forwards.len()).collect::<Vec<_>>();
    indexes.sort_by_key(|&index| {
        let forward = &forwards[index];
        if forward.automatic {
            let location = location_info(forward, locations);
            (
                0,
                location.workspace_number,
                location.workspace_id.into(),
                location.tab_number,
                location.tab_id.into(),
                forward.pane_id.clone(),
            )
        } else {
            (
                1,
                u64::MAX,
                String::new(),
                u64::MAX,
                String::new(),
                forward.id.clone(),
            )
        }
    });
    indexes
}

fn clock(timestamp: u64) -> String {
    if timestamp == 0 {
        return "—".into();
    }
    let Ok(timestamp) = timestamp.try_into() else {
        return "—".into();
    };
    let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
    if unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null() {
        return "—".into();
    }
    format!("{:02}:{:02}", local.tm_hour, local.tm_min)
}

fn status(forward: &Forward) -> &'static str {
    if forward.enabled {
        "●"
    } else {
        "○"
    }
}

fn forward_state_label(forward: &Forward) -> &'static str {
    if forward.enabled {
        "ACTIVE"
    } else {
        "PAUSED"
    }
}

fn forward_counts(forwards: &[Forward]) -> (usize, usize, usize) {
    let active = forwards.iter().filter(|forward| forward.enabled).count();
    (
        forwards.len(),
        active,
        forwards.len().saturating_sub(active),
    )
}

fn live_age(timestamp: u64) -> String {
    if timestamp == 0 {
        return "—".into();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{}m", now.saturating_sub(timestamp) / 60)
}

fn tunnel_status(forward: &Forward) -> String {
    if forward.enabled {
        format!("live {}", live_age(forward.tunnel_opened_at))
    } else {
        "paused".into()
    }
}

fn tunnel_time(forward: &Forward) -> String {
    if forward.enabled {
        clock(forward.tunnel_opened_at)
    } else {
        "—".into()
    }
}

#[cfg(test)]
fn tree_text(forwards: &[Forward], locations: &HashMap<String, PaneLocation>) -> Vec<String> {
    tree_lines(forwards, locations)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

fn render_tree_line(
    stdout: &mut impl Write,
    width: u16,
    line: &TreeLine,
    selected: bool,
    forwards: &[Forward],
    palette: DashboardPalette,
) -> Result<(), String> {
    let color = palette.color_for(line, forwards);
    if selected {
        queue!(
            stdout,
            SetAttribute(Attribute::Bold),
            SetBackgroundColor(palette.selection)
        )
        .map_err(|error| error.to_string())?;
    } else if palette.dim_paused
        && line
            .forward_index
            .is_some_and(|index| !forwards[index].enabled)
    {
        queue!(stdout, SetAttribute(Attribute::Dim)).map_err(|error| error.to_string())?;
    } else if line.forward_index.is_none() {
        queue!(stdout, SetAttribute(Attribute::Bold)).map_err(|error| error.to_string())?;
    }
    let display = padded_line(&line.text, width);
    let prefix_length = tree_prefix_length(&display);
    let (prefix, content) = display.split_at(prefix_length);
    if selected {
        queue!(stdout, SetForegroundColor(Color::Reset)).map_err(|error| error.to_string())?;
    } else {
        queue!(stdout, ResetColor).map_err(|error| error.to_string())?;
    }
    let (primary, metadata) = split_tree_metadata(content);
    queue!(
        stdout,
        Print(prefix),
        SetForegroundColor(color),
        Print(primary),
    )
    .map_err(|error| error.to_string())?;
    if !metadata.is_empty() {
        if palette.dim_metadata {
            queue!(stdout, SetAttribute(Attribute::Dim)).map_err(|error| error.to_string())?;
        }
        queue!(stdout, Print(metadata)).map_err(|error| error.to_string())?;
    }
    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("\r\n")
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn split_tree_metadata(value: &str) -> (&str, &str) {
    value
        .find("  │  ")
        .map(|index| value.split_at(index))
        .unwrap_or((value, ""))
}

#[cfg(test)]
fn tree_line_color(line: &TreeLine, _selected: bool, forwards: &[Forward]) -> Color {
    DashboardPalette::dark().color_for(line, forwards)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DashboardPalette {
    active: Color,
    paused: Color,
    manual: Color,
    section: Color,
    tree: Color,
    selection: Color,
    dim_metadata: bool,
    dim_paused: bool,
}

impl DashboardPalette {
    fn from_environment() -> Self {
        let theme_name = configured_theme_name().ok().flatten();
        Self::from_theme_and_colorfgbg(
            theme_name.as_deref(),
            std::env::var("COLORFGBG").ok().as_deref(),
        )
    }

    fn from_theme_and_colorfgbg(theme_name: Option<&str>, colorfgbg: Option<&str>) -> Self {
        if uses_light_palette(theme_name, colorfgbg) {
            Self::light()
        } else {
            Self::dark()
        }
    }

    fn dark() -> Self {
        Self {
            active: Color::Green,
            paused: Color::Yellow,
            manual: Color::Magenta,
            section: Color::Cyan,
            tree: Color::Blue,
            selection: Color::DarkGrey,
            dim_metadata: true,
            dim_paused: true,
        }
    }

    fn light() -> Self {
        Self {
            active: Color::DarkGreen,
            paused: Color::AnsiValue(136),
            manual: Color::DarkMagenta,
            section: Color::DarkCyan,
            tree: Color::DarkBlue,
            selection: Color::DarkGrey,
            dim_metadata: false,
            dim_paused: false,
        }
    }

    fn color_for(self, line: &TreeLine, forwards: &[Forward]) -> Color {
        if let Some(index) = line.forward_index {
            return if forwards[index].enabled {
                self.active
            } else {
                self.paused
            };
        }
        if line.text.contains("MANUAL FORWARDS") {
            self.manual
        } else if !line.text.trim().is_empty()
            && !line.text.contains("├─")
            && !line.text.contains("└─")
        {
            self.section
        } else if line.forward_index.is_none() {
            self.tree
        } else {
            Color::Reset
        }
    }
}

fn uses_light_palette(theme_name: Option<&str>, colorfgbg: Option<&str>) -> bool {
    const LIGHT_THEMES: &[&str] = &[
        "catppuccin-latte",
        "tokyo-night-day",
        "gruvbox-light",
        "one-light",
        "solarized-light",
        "kanagawa-lotus",
        "rose-pine-dawn",
    ];
    const DARK_THEMES: &[&str] = &[
        "catppuccin",
        "tokyo-night",
        "dracula",
        "nord",
        "gruvbox",
        "one-dark",
        "solarized",
        "kanagawa",
        "rose-pine",
        "vesper",
    ];

    if let Some(theme_name) = theme_name.map(str::trim) {
        if LIGHT_THEMES.contains(&theme_name) {
            return true;
        }
        if DARK_THEMES.contains(&theme_name) {
            return false;
        }
    }
    colorfgbg
        .and_then(|value| value.split(';').next_back())
        .and_then(|background| background.parse::<u8>().ok())
        .is_some_and(|background| matches!(background, 7 | 15))
}

fn tree_prefix_length(value: &str) -> usize {
    value
        .char_indices()
        .take_while(|(_, character)| matches!(character, ' ' | '│' | '├' | '└' | '─'))
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0)
}

fn padded_line(value: &str, width: u16) -> String {
    format!(
        "{:<width$}",
        truncate(value, width as usize),
        width = width as usize
    )
}

fn help_shortcut_lines(forward_count: usize, selected: Option<&Forward>) -> Vec<String> {
    let shortcuts = canonical_dashboard_shortcuts(u16::MAX, forward_count, selected);
    let key_width = shortcuts
        .iter()
        .map(|shortcut| shortcut.key.chars().count())
        .max()
        .unwrap_or(0);
    std::iter::once("KEYBOARD SHORTCUTS".to_string())
        .chain(shortcuts.into_iter().map(|shortcut| {
            let description = help_shortcut_description(&shortcut);
            if description.is_empty() {
                shortcut.key.to_string()
            } else {
                format!("{:<key_width$}  {description}", shortcut.key)
            }
        }))
        .collect()
}

fn help_shortcut_description(shortcut: &DashboardShortcut) -> &'static str {
    match shortcut.key {
        "↑↓" | "↑↓/jk" => "Move between forwarded ports",
        "Enter" | "↵" => "Focus the source pane",
        "o" => "Open the forwarded URL",
        "Space" => "Pause or resume the selected forward",
        "p" => "Choose a different local port",
        "d" => "Remove the manual forward",
        "a" => "Add a manual forward",
        "h" => "Open integration settings",
        "?" => "Show keyboard shortcuts",
        "r" => "Refresh forwarding status",
        "Esc" => "Close the current view",
        _ => shortcut.label,
    }
}

fn render_help(
    stdout: &mut impl Write,
    width: u16,
    height: u16,
    forward_count: usize,
    selected: Option<&Forward>,
) -> Result<(), String> {
    let lines = help_shortcut_lines(forward_count, selected);
    let lines = lines.iter().map(String::as_str).collect::<Vec<_>>();
    render_overlay(stdout, width, height, &lines)
}

fn render_integration_menu(
    stdout: &mut impl Write,
    width: u16,
    height: u16,
    menu: &IntegrationMenu,
) -> Result<(), String> {
    let rows = [
        format!(
            "{} After forwarding            {}",
            if menu.selected == 0 { "›" } else { " " },
            match menu.after_forward {
                crate::plugin::preferences::AfterForward::Space => "SPACE",
                crate::plugin::preferences::AfterForward::Popup => "POPUP",
                crate::plugin::preferences::AfterForward::Nothing => "NOTHING",
            }
        ),
        format!(
            "{} Port-forward status         {}",
            if menu.selected == 1 { "›" } else { " " },
            if menu.sidebar_ports_enabled {
                "ON"
            } else {
                "OFF"
            }
        ),
        format!(
            "{} Notifications inside Herdr  {}",
            if menu.selected == 2 { "›" } else { " " },
            if menu.toast_delivery.as_deref() == Some("herdr") {
                "READY"
            } else {
                "SET UP"
            }
        ),
        format!(
            "{} Popup shortcut              {}",
            if menu.selected == 3 { "›" } else { " " },
            if menu.popup_shortcut_enabled {
                "READY"
            } else {
                "SET UP"
            }
        ),
        format!(
            "{} Process tree depth          {}",
            if menu.selected == 4 { "›" } else { " " },
            menu.process_tree_depth
        ),
    ];
    let lines = [
        "INTEGRATION SETTINGS".to_string(),
        "".to_string(),
        "Recommended settings for remote forwarding".to_string(),
        "".to_string(),
        rows[0].clone(),
        "    ↳ Default destination after an approved detected port".to_string(),
        rows[1].clone(),
        "    ↳ Shows compact mappings; only its own row is changed".to_string(),
        rows[2].clone(),
        "    ↳ Shows forwarding notices in the Herdr interface".to_string(),
        rows[3].clone(),
        "    ↳ Uses prefix+shift+f with --remote-keybindings server".to_string(),
        rows[4].clone(),
        "    ↳ Searches child processes for loopback listeners".to_string(),
        "".to_string(),
        integration_footer(menu),
    ];
    let styles = [
        OverlayLineStyle::Bold,
        OverlayLineStyle::Normal,
        OverlayLineStyle::Dim,
        OverlayLineStyle::Normal,
        OverlayLineStyle::Bold,
        OverlayLineStyle::Dim,
        OverlayLineStyle::Bold,
        OverlayLineStyle::Dim,
        OverlayLineStyle::Bold,
        OverlayLineStyle::Dim,
        OverlayLineStyle::Bold,
        OverlayLineStyle::Dim,
        OverlayLineStyle::Bold,
        OverlayLineStyle::Dim,
        OverlayLineStyle::Normal,
        OverlayLineStyle::Dim,
    ];
    let styled = lines
        .iter()
        .zip(styles)
        .map(|(text, style)| OverlayLine { text, style })
        .collect::<Vec<_>>();
    render_styled_overlay(stdout, width, height, &styled)
}

fn integration_footer(menu: &IntegrationMenu) -> String {
    match menu.selected {
        0 | 4 => "↑/↓ select · ←/→ adjust · Enter/Space next · Esc close".to_string(),
        1 => "↑/↓ select · Enter/Space toggle · Esc close".to_string(),
        _ => "↑/↓ select · Enter/Space setup · Esc close".to_string(),
    }
}

fn render_local_port_form(
    stdout: &mut impl Write,
    width: u16,
    height: u16,
    form: &LocalPortForm,
) -> Result<(), String> {
    let mapping = format!(
        "Remote 127.0.0.1:{}  →  local 127.0.0.1:{}",
        form.remote_port,
        if form.local_port.is_empty() {
            "_"
        } else {
            &form.local_port
        }
    );
    let lines = [
        "CHANGE LOCAL PORT".to_string(),
        mapping,
        "Type the port on your local machine".to_string(),
        "Enter apply · Esc cancel".to_string(),
    ];
    let refs = lines.iter().map(String::as_str).collect::<Vec<_>>();
    render_overlay(stdout, width, height, &refs)
}

fn render_create_form(
    stdout: &mut impl Write,
    width: u16,
    height: u16,
    form: &CustomForwardForm,
) -> Result<(), String> {
    let remote = format!(
        "{} Remote port: {}",
        if form.field == 0 { "›" } else { " " },
        if form.remote_port.is_empty() {
            "3000 (default)"
        } else {
            &form.remote_port
        }
    );
    let local = format!(
        "{} Local port:  {}",
        if form.field == 1 { "›" } else { " " },
        if form.local_port.is_empty() {
            "same as remote"
        } else {
            &form.local_port
        }
    );
    let host = format!(
        "{} Remote host:  {}",
        if form.field == 2 { "›" } else { " " },
        crate::plugin::dashboard_terminal::manual_remote_host(form.remote_host)
    );
    let lines = [
        "ADD NEW FORWARD".to_string(),
        "Both endpoints are loopback-only".to_string(),
        remote,
        local,
        host,
        "Tab switch field · ←/→ host · Enter create · Esc cancel".to_string(),
    ];
    let refs = lines.iter().map(String::as_str).collect::<Vec<_>>();
    render_overlay(stdout, width, height, &refs)
}

#[derive(Clone, Copy)]
enum OverlayLineStyle {
    Normal,
    Bold,
    Dim,
}

struct OverlayLine<'a> {
    text: &'a str,
    style: OverlayLineStyle,
}

fn render_overlay(
    stdout: &mut impl Write,
    width: u16,
    height: u16,
    lines: &[&str],
) -> Result<(), String> {
    let styled = lines
        .iter()
        .enumerate()
        .map(|(index, text)| OverlayLine {
            text,
            style: if index == 0 {
                OverlayLineStyle::Bold
            } else {
                OverlayLineStyle::Normal
            },
        })
        .collect::<Vec<_>>();
    render_styled_overlay(stdout, width, height, &styled)
}

fn render_styled_overlay(
    stdout: &mut impl Write,
    width: u16,
    height: u16,
    lines: &[OverlayLine<'_>],
) -> Result<(), String> {
    if width < 12 || height < lines.len() as u16 + 4 {
        return Ok(());
    }
    let box_width = lines
        .iter()
        .map(|line| line.text.chars().count())
        .max()
        .unwrap_or(20)
        .saturating_add(4)
        .min(width.saturating_sub(4) as usize)
        .max(8);
    let x = width.saturating_sub(box_width as u16) / 2;
    let y = height.saturating_sub(lines.len() as u16 + 2) / 2;
    queue!(stdout, ResetColor).map_err(|error| error.to_string())?;
    queue!(
        stdout,
        MoveTo(x, y),
        Print(format!("╭{}╮", "─".repeat(box_width.saturating_sub(2))))
    )
    .map_err(|error| error.to_string())?;
    for (index, line) in lines.iter().enumerate() {
        let attribute = match line.style {
            OverlayLineStyle::Normal => Attribute::Reset,
            OverlayLineStyle::Bold => Attribute::Bold,
            OverlayLineStyle::Dim => Attribute::Dim,
        };
        queue!(
            stdout,
            MoveTo(x, y + index as u16 + 1),
            SetAttribute(Attribute::Reset),
            Print("│ "),
            SetAttribute(attribute),
            Print(format!(
                "{:<content_width$}",
                truncate(line.text, box_width.saturating_sub(4)),
                content_width = box_width.saturating_sub(4)
            )),
            SetAttribute(Attribute::Reset),
            Print(" │"),
        )
        .map_err(|error| error.to_string())?;
    }
    queue!(
        stdout,
        MoveTo(x, y + lines.len() as u16 + 1),
        Print(format!("╰{}╯", "─".repeat(box_width.saturating_sub(2)))),
        ResetColor
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn truncate(value: &str, width: usize) -> String {
    let sanitized = herdr_fwd::detect::sanitize_display_text(value);
    let value = sanitized.as_str();
    if value.chars().count() <= width {
        return value.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut output = value.chars().take(width - 1).collect::<String>();
    output.push('…');
    output
}

#[cfg(test)]
mod dashboard_render_tests {
    use std::collections::HashMap;

    use crossterm::style::Color;
    use herdr_fwd::registry::Forward;

    use crate::plugin::dashboard_terminal::IntegrationMenu;
    use crate::plugin::herdr::PaneLocation;
    use crate::plugin::preferences::AfterForward;

    use super::{
        canonical_dashboard_shortcuts, compact_message, forward_counts, forward_index_at,
        forward_state_label, help_shortcut_lines, integration_footer, pane_heading, pane_icon,
        pane_metadata, process_icon, render_dashboard_shortcuts, shortcut_text,
        split_tree_metadata, status, tab_icon, tree_forward_indexes, tree_line_color,
        tree_prefix_length, tree_scroll_window, tree_text, truncate, tunnel_status, tunnel_time,
        uses_light_palette, DashboardPalette, TreeLine,
    };

    fn forward(automatic: bool, enabled: bool, pane_id: &str) -> Forward {
        Forward {
            id: "fwd-1".into(),
            remote_port: 5173,
            local_port: 5173,
            remote_host: "127.0.0.1".into(),
            pane_id: pane_id.into(),
            process: "Vite".into(),
            detected_url: "http://localhost:5173/".into(),
            enabled,
            automatic,
            server_started_at: None,
            process_id: None,
            tunnel_opened_at: 0,
        }
    }

    #[test]
    fn truncates_dashboard_text_on_character_boundaries() {
        assert_eq!(truncate("Vite", 8), "Vite");
        assert_eq!(truncate("порт-форвард", 6), "порт-…");
        assert_eq!(truncate("abc", 1), "…");
    }

    #[test]
    fn prioritizes_an_explicit_pane_label_and_separates_metadata() {
        let location = PaneLocation {
            pane_label: Some("▲ Next.js".into()),
            ..PaneLocation::default()
        };
        let mut next = forward(true, true, "wT:p1");
        next.process = "Next.js".into();
        next.process_id = Some(700_590);

        assert_eq!(pane_heading(&next, &location), "▲ Next.js");
        assert_eq!(pane_metadata(&next), "wT:p1  ·   Next.js  ·  pid 700590");
        assert_eq!(
            split_tree_metadata("󰆍 ▲ Next.js  │  wT:p1  ·   Next.js  ·  pid 700590"),
            ("󰆍 ▲ Next.js", "  │  wT:p1  ·   Next.js  ·  pid 700590")
        );
    }

    #[test]
    fn integration_footer_matches_the_selected_control_type() {
        let mut menu = IntegrationMenu {
            selected: 0,
            after_forward: AfterForward::Space,
            sidebar_ports_enabled: false,
            toast_delivery: None,
            popup_shortcut_enabled: false,
            process_tree_depth: 2,
        };
        assert!(integration_footer(&menu).contains("←/→ adjust"));
        menu.selected = 1;
        assert!(integration_footer(&menu).contains("toggle"));
        assert!(!integration_footer(&menu).contains("←/→"));
        menu.selected = 2;
        assert!(integration_footer(&menu).contains("setup"));
        assert!(!integration_footer(&menu).contains("←/→"));
    }

    #[test]
    fn compacts_multiline_messages_for_the_footer() {
        assert_eq!(
            compact_message("Action failed.\nRPC unavailable\n\nPress r to retry."),
            "Action failed. · RPC unavailable ·  · Press r to retry."
        );
    }

    #[test]
    fn keeps_the_selected_forward_second_line_inside_the_scroll_window() {
        assert_eq!(tree_scroll_window(12, 10, 10), (2, 12));
    }

    #[test]
    fn maps_a_mouse_row_to_the_visible_forward() {
        let forwards = vec![forward(false, true, "manual")];
        assert_eq!(
            forward_index_at(4, 24, 0, &forwards, &HashMap::new()),
            Some(0)
        );
        assert_eq!(forward_index_at(3, 24, 0, &forwards, &HashMap::new()), None);
    }

    #[test]
    fn uses_dedicated_nerd_font_icons_without_ecosystem_grouping() {
        assert_eq!(tab_icon(), "󰓩");
        assert_eq!(pane_icon(), "󰆍");
        assert_eq!(process_icon("Vite"), "");
        assert_eq!(process_icon("Storybook"), "");
        assert_eq!(process_icon("Next.js dev server"), "");
        assert_eq!(process_icon("Astro"), "");
        assert_eq!(process_icon("Nuxt"), "");
        assert_eq!(process_icon("Vue"), "");
        assert_eq!(process_icon("Svelte"), "");
        assert_eq!(process_icon("Angular"), "");
        assert_eq!(process_icon("React"), "");
        assert_eq!(process_icon("Webpack"), "");
        assert_eq!(process_icon("Express"), "");
        assert_eq!(process_icon("Fastify"), "");
        assert_eq!(process_icon("NestJS"), "");
        assert_eq!(process_icon("Django runserver"), "");
        assert_eq!(process_icon("Flask"), "");
        assert_eq!(process_icon("FastAPI"), "");
        assert_eq!(process_icon("Rails"), "");
        assert_eq!(process_icon("Ruby"), "");
        assert_eq!(process_icon("Go HTTP server"), "");
        assert_eq!(process_icon("cargo watch"), "");
        assert_eq!(process_icon("Spring Boot"), "");
        assert_eq!(process_icon("Laravel"), "");
        assert_eq!(process_icon("PHP"), "");
        assert_eq!(process_icon("Docker Compose"), "");
        assert_eq!(process_icon("Nginx"), "");
        assert_eq!(process_icon("Apache"), "");
        assert_eq!(process_icon("Bun"), "");
        assert_eq!(process_icon("Deno"), "");
        assert_eq!(process_icon("Caddy"), "󰒋");
        assert_eq!(process_icon("Parcel"), "󰒋");
        assert_eq!(process_icon("Uvicorn"), "󰒋");
        assert_eq!(process_icon("Gunicorn"), "󰒋");
        assert_eq!(process_icon("unknown-server"), "󰒋");
    }

    #[test]
    fn keeps_tree_connectors_neutral_and_uses_status_bullets() {
        let live = forward(true, true, "w1:p1");
        let mut paused = forward(true, false, "w1:p1");
        paused.tunnel_opened_at = 1_722_000_120;
        assert_eq!(status(&live), "●");
        assert_eq!(status(&paused), "○");
        assert_eq!(tunnel_status(&paused), "paused");
        assert_eq!(tunnel_time(&paused), "—");
        assert_eq!(forward_counts(&[live, paused]), (2, 1, 1));
        assert_eq!(tree_prefix_length("  │  └─ 󰆍 w1:p1"), "  │  └─ ".len());
    }

    #[test]
    fn labels_active_and_paused_forwards_in_text() {
        assert_eq!(forward_state_label(&forward(true, true, "w1:p1")), "ACTIVE");
        assert_eq!(
            forward_state_label(&forward(true, false, "w1:p1")),
            "PAUSED"
        );
    }

    #[test]
    fn colors_every_line_of_an_active_or_paused_forward() {
        let line = TreeLine {
            text: "          server —  ·  tunnel —".into(),
            forward_index: Some(0),
        };
        assert_eq!(
            tree_line_color(&line, false, &[forward(true, true, "w1:p1")]),
            Color::Green
        );
        assert_eq!(
            tree_line_color(&line, false, &[forward(true, false, "w1:p1")]),
            Color::Yellow
        );
        assert_eq!(
            tree_line_color(&line, true, &[forward(true, true, "w1:p1")]),
            Color::Green
        );
        assert_eq!(
            tree_line_color(&line, true, &[forward(true, false, "w1:p1")]),
            Color::Yellow
        );
    }

    #[test]
    fn detects_light_terminal_backgrounds_from_colorfgbg() {
        assert!(uses_light_palette(Some("catppuccin-latte"), None));
        assert!(uses_light_palette(Some("tokyo-night-day"), Some("15;0")));
        assert!(!uses_light_palette(Some("catppuccin"), Some("0;15")));
        assert!(uses_light_palette(None, Some("0;15")));
        assert!(uses_light_palette(None, Some("0;7")));
        assert!(!uses_light_palette(None, Some("15;0")));
        assert!(!uses_light_palette(None, Some("default;default")));
        assert!(!uses_light_palette(None, None));
    }

    #[test]
    fn defines_light_contrast_in_one_semantic_palette() {
        let palette = DashboardPalette::from_theme_and_colorfgbg(None, Some("0;15"));

        assert_eq!(palette.active, Color::DarkGreen);
        assert_eq!(palette.paused, Color::AnsiValue(136));
        assert_eq!(palette.section, Color::DarkCyan);
        assert_eq!(palette.tree, Color::DarkBlue);
        assert!(!palette.dim_metadata);
        assert!(!palette.dim_paused);
    }

    #[test]
    fn uses_darker_semantic_colours_on_light_backgrounds() {
        let active_line = TreeLine {
            text: "  ● ACTIVE".into(),
            forward_index: Some(0),
        };
        let paused_line = TreeLine {
            text: "  ○ PAUSED".into(),
            forward_index: Some(0),
        };
        let header_line = TreeLine {
            text: "  󰉋 Product".into(),
            forward_index: None,
        };

        assert_eq!(
            DashboardPalette::light().color_for(&active_line, &[forward(true, true, "w1:p1")]),
            Color::DarkGreen
        );
        assert_eq!(
            DashboardPalette::light().color_for(&paused_line, &[forward(true, false, "w1:p1")]),
            Color::AnsiValue(136)
        );
        assert_eq!(
            DashboardPalette::light().color_for(&header_line, &[]),
            Color::DarkCyan
        );
    }

    #[test]
    fn keeps_paused_forwards_undimmed_on_light_backgrounds() {
        let paused_line = TreeLine {
            text: "  ○ PAUSED".into(),
            forward_index: Some(0),
        };
        let forwards = [forward(true, false, "w1:p1")];

        assert!(!DashboardPalette::light().dim_paused);
        assert!(DashboardPalette::dark().dim_paused);
        assert_eq!(
            DashboardPalette::light().color_for(&paused_line, &forwards),
            Color::AnsiValue(136)
        );
    }

    #[test]
    fn uses_one_space_before_the_remote_address() {
        let forwards = vec![forward(true, true, "w1:p1"), forward(false, true, "manual")];
        let locations = HashMap::from([(
            "w1:p1".into(),
            PaneLocation {
                workspace_id: "w1".into(),
                workspace: "Apps".into(),
                tab_id: "w1:t1".into(),
                tab: "API".into(),
                ..Default::default()
            },
        )]);

        let text = tree_text(&forwards, &locations);
        assert!(text
            .iter()
            .any(|line| line.contains("● ACTIVE 127.0.0.1:5173")));
        assert!(!text
            .iter()
            .any(|line| line.contains("● ACTIVE  127.0.0.1:5173")));
    }

    #[test]
    fn shows_only_actions_available_for_the_selected_forward() {
        let automatic_paused = forward(true, false, "w1:p1");
        let shortcuts = canonical_dashboard_shortcuts(120, 1, Some(&automatic_paused));
        let shortcuts = shortcut_text(&shortcuts, false);
        assert!(shortcuts.contains("Enter pane"));
        assert!(shortcuts.contains("Space toggle"));
        assert!(!shortcuts.contains("o open"));
        assert!(!shortcuts.contains("d remove"));
        assert!(!shortcuts.contains("navigate"));

        let compact = canonical_dashboard_shortcuts(79, 1, Some(&automatic_paused));
        assert!(compact
            .iter()
            .any(|shortcut| shortcut.key == "Space" && shortcut.label == "toggle"));
        assert!(!compact.iter().any(|shortcut| shortcut.key == "󱁐"));

        let custom = forward(false, true, "manual");
        let shortcuts = canonical_dashboard_shortcuts(120, 2, Some(&custom));
        let shortcuts = shortcut_text(&shortcuts, false);
        assert!(shortcuts.contains("navigate"));
        assert!(shortcuts.contains("o open"));
        assert!(shortcuts.contains("d remove"));
        assert!(!shortcuts.contains("Enter pane"));
    }

    #[test]
    fn empty_dashboard_shows_only_global_actions() {
        let shortcuts = canonical_dashboard_shortcuts(120, 0, None);
        assert_eq!(
            shortcut_text(&shortcuts, false),
            "  a add   h settings   ? help"
        );
    }

    #[test]
    fn renders_shortcut_keys_in_bold_and_labels_dimmed() {
        let custom = forward(false, true, "w1:p1");
        let shortcuts = canonical_dashboard_shortcuts(120, 2, Some(&custom));
        let mut output = Vec::new();
        render_dashboard_shortcuts(&mut output, 120, &shortcuts).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("\x1b[1m↑↓/jk"));
        assert!(output.contains("\x1b[2m navigate"));
        assert!(output.contains("\x1b[1md"));
        assert!(output.contains("\x1b[2m remove"));
    }

    #[test]
    fn derives_contextual_help_from_canonical_shortcuts() {
        let paused = forward(true, false, "w1:p1");
        assert_eq!(
            help_shortcut_lines(1, Some(&paused)),
            vec![
                "KEYBOARD SHORTCUTS".to_string(),
                "Enter  Focus the source pane".to_string(),
                "Space  Pause or resume the selected forward".to_string(),
                "p      Choose a different local port".to_string(),
                "a      Add a manual forward".to_string(),
                "h      Open integration settings".to_string(),
                "?      Show keyboard shortcuts".to_string(),
                "r      Refresh forwarding status".to_string(),
                "Esc    Close the current view".to_string(),
            ]
        );
    }

    #[test]
    fn groups_forwards_by_space_tab_and_pane() {
        let forwards = vec![
            Forward {
                process_id: Some(9132),
                ..forward(true, true, "w1:p1")
            },
            Forward {
                id: "fwd-2".into(),
                pane_id: "w1:p2".into(),
                ..forward(true, true, "w1:p1")
            },
            Forward {
                id: "fwd-3".into(),
                pane_id: "w2:p1".into(),
                ..forward(true, true, "w1:p1")
            },
        ];
        let locations = HashMap::from([
            (
                "w1:p1".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Apps".into(),
                    tab_id: "w1:t1".into(),
                    tab: "Frontend".into(),
                    ..Default::default()
                },
            ),
            (
                "w1:p2".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Apps".into(),
                    tab_id: "w1:t1".into(),
                    tab: "Frontend".into(),
                    ..Default::default()
                },
            ),
            (
                "w2:p1".into(),
                PaneLocation {
                    workspace_id: "w2".into(),
                    workspace: "Services".into(),
                    tab_id: "w2:t1".into(),
                    tab: "API".into(),
                    ..Default::default()
                },
            ),
        ]);

        let rendered = tree_text(&forwards, &locations);
        assert_eq!(
            rendered,
            vec![
                "  󰉋 Apps",
                "  └─ 󰓩 Frontend",
                "     ├─ 󰆍 Vite  │  w1:p1  ·   Vite  ·  pid 9132",
                "     │  ● ACTIVE 127.0.0.1:5173  →  localhost:5173  ·  live —",
                "     │    server —  ·  tunnel —",
                "     └─ 󰆍 Vite  │  w1:p2  ·   Vite",
                "        ● ACTIVE 127.0.0.1:5173  →  localhost:5173  ·  live —",
                "          server —  ·  tunnel —",
                "  󰉋 Services",
                "  └─ 󰓩 API",
                "     └─ 󰆍 Vite  │  w2:p1  ·   Vite",
                "        ● ACTIVE 127.0.0.1:5173  →  localhost:5173  ·  live —",
                "          server —  ·  tunnel —",
            ]
        );
    }

    #[test]
    fn keeps_same_named_spaces_separate_by_id() {
        let forwards = vec![
            forward(true, true, "w1:p1"),
            Forward {
                id: "fwd-2".into(),
                pane_id: "w2:p1".into(),
                ..forward(true, true, "w1:p1")
            },
        ];
        let locations = HashMap::from([
            (
                "w1:p1".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Apps".into(),
                    tab_id: "w1:t1".into(),
                    tab: "API".into(),
                    ..Default::default()
                },
            ),
            (
                "w2:p1".into(),
                PaneLocation {
                    workspace_id: "w2".into(),
                    workspace: "Apps".into(),
                    tab_id: "w2:t1".into(),
                    tab: "API".into(),
                    ..Default::default()
                },
            ),
        ]);

        let text = tree_text(&forwards, &locations);
        assert_eq!(
            text,
            vec![
                "  󰉋 Apps",
                "  └─ 󰓩 API",
                "     └─ 󰆍 Vite  │  w1:p1  ·   Vite",
                "        ● ACTIVE 127.0.0.1:5173  →  localhost:5173  ·  live —",
                "          server —  ·  tunnel —",
                "  󰉋 Apps",
                "  └─ 󰓩 API",
                "     └─ 󰆍 Vite  │  w2:p1  ·   Vite",
                "        ● ACTIVE 127.0.0.1:5173  →  localhost:5173  ·  live —",
                "          server —  ·  tunnel —",
            ]
        );
    }

    #[test]
    fn keeps_same_named_spaces_contiguous_across_tabs() {
        let forwards = vec![
            forward(true, true, "w1:p1"),
            Forward {
                id: "fwd-2".into(),
                pane_id: "w2:p1".into(),
                ..forward(true, true, "w1:p1")
            },
            Forward {
                id: "fwd-3".into(),
                pane_id: "w1:p2".into(),
                ..forward(true, true, "w1:p1")
            },
            Forward {
                id: "fwd-4".into(),
                pane_id: "w2:p2".into(),
                ..forward(true, true, "w1:p1")
            },
        ];
        let locations = HashMap::from([
            (
                "w1:p1".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Apps".into(),
                    tab_id: "w1:t1".into(),
                    tab: "One".into(),
                    ..Default::default()
                },
            ),
            (
                "w2:p1".into(),
                PaneLocation {
                    workspace_id: "w2".into(),
                    workspace: "Apps".into(),
                    tab_id: "w2:t1".into(),
                    tab: "One".into(),
                    ..Default::default()
                },
            ),
            (
                "w1:p2".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Apps".into(),
                    tab_id: "w1:t2".into(),
                    tab: "Two".into(),
                    ..Default::default()
                },
            ),
            (
                "w2:p2".into(),
                PaneLocation {
                    workspace_id: "w2".into(),
                    workspace: "Apps".into(),
                    tab_id: "w2:t2".into(),
                    tab: "Two".into(),
                    ..Default::default()
                },
            ),
        ]);

        assert_eq!(
            tree_forward_indexes(&forwards, &locations),
            vec![0, 2, 1, 3]
        );
        assert_eq!(
            tree_text(&forwards, &locations)
                .iter()
                .filter(|line| *line == "  󰉋 Apps")
                .count(),
            2
        );
    }

    #[test]
    fn follows_herdr_space_and_tab_numbers() {
        let forwards = vec![
            forward(true, true, "w2:p2"),
            Forward {
                id: "fwd-2".into(),
                pane_id: "w1:p2".into(),
                ..forward(true, true, "w2:p2")
            },
            Forward {
                id: "fwd-3".into(),
                pane_id: "w1:p1".into(),
                ..forward(true, true, "w2:p2")
            },
            Forward {
                id: "fwd-4".into(),
                pane_id: "w2:p1".into(),
                ..forward(true, true, "w2:p2")
            },
        ];
        let locations = HashMap::from([
            (
                "w2:p2".into(),
                PaneLocation {
                    workspace_id: "w2".into(),
                    workspace: "Alpha".into(),
                    workspace_number: 2,
                    tab_id: "w2:t2".into(),
                    tab: "Alpha".into(),
                    tab_number: 2,
                    pane_label: None,
                },
            ),
            (
                "w1:p2".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Zeta".into(),
                    workspace_number: 1,
                    tab_id: "w1:t2".into(),
                    tab: "Alpha".into(),
                    tab_number: 2,
                    pane_label: None,
                },
            ),
            (
                "w1:p1".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Zeta".into(),
                    workspace_number: 1,
                    tab_id: "w1:t1".into(),
                    tab: "Zeta".into(),
                    tab_number: 1,
                    pane_label: None,
                },
            ),
            (
                "w2:p1".into(),
                PaneLocation {
                    workspace_id: "w2".into(),
                    workspace: "Alpha".into(),
                    workspace_number: 2,
                    tab_id: "w2:t1".into(),
                    tab: "Zeta".into(),
                    tab_number: 1,
                    pane_label: None,
                },
            ),
        ]);

        assert_eq!(
            tree_forward_indexes(&forwards, &locations),
            vec![2, 1, 3, 0]
        );
    }

    #[test]
    fn navigates_in_visible_tree_order_and_keeps_manual_last() {
        let forwards = vec![
            Forward {
                id: "manual".into(),
                automatic: false,
                ..forward(false, true, "manual")
            },
            Forward {
                id: "services".into(),
                pane_id: "w2:p1".into(),
                ..forward(true, true, "w1:p1")
            },
            Forward {
                id: "apps".into(),
                pane_id: "w1:p1".into(),
                ..forward(true, true, "w1:p1")
            },
        ];
        let locations = HashMap::from([
            (
                "w1:p1".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Apps".into(),
                    tab_id: "w1:t1".into(),
                    tab: "Frontend".into(),
                    ..Default::default()
                },
            ),
            (
                "w2:p1".into(),
                PaneLocation {
                    workspace_id: "w2".into(),
                    workspace: "Services".into(),
                    tab_id: "w2:t1".into(),
                    tab: "API".into(),
                    ..Default::default()
                },
            ),
        ]);

        assert_eq!(tree_forward_indexes(&forwards, &locations), vec![2, 1, 0]);
        let text = tree_text(&forwards, &locations);
        let manual_header = text
            .iter()
            .position(|line| line == "  󰖟 MANUAL FORWARDS")
            .unwrap();
        assert_eq!(text[manual_header - 1], "");
        assert_eq!(text.iter().filter(|line| line.is_empty()).count(), 1);
        assert!(text.contains(&"  └─ ● ACTIVE 127.0.0.1:5173  →  localhost:5173  ·  live —".into()));
        assert!(text.contains(&"       tunnel —".into()));
        assert!(!text.iter().any(|line| line.contains('◆')));
    }

    #[test]
    fn keeps_manual_forward_metadata_under_its_tree_branch() {
        let forwards = vec![
            Forward {
                id: "manual-one".into(),
                automatic: false,
                ..forward(false, true, "manual-one")
            },
            Forward {
                id: "manual-two".into(),
                automatic: false,
                enabled: false,
                remote_port: 3000,
                local_port: 3000,
                ..forward(false, true, "manual-two")
            },
        ];

        assert_eq!(
            tree_text(&forwards, &HashMap::new()),
            vec![
                "  󰖟 MANUAL FORWARDS",
                "  ├─ ● ACTIVE 127.0.0.1:5173  →  localhost:5173  ·  live —",
                "  │  tunnel —",
                "  └─ ○ PAUSED 127.0.0.1:3000  →  localhost:3000  ·  paused",
                "       tunnel —",
            ]
        );
    }

    #[test]
    fn keeps_vertical_connectors_for_sibling_tabs() {
        let forwards = vec![
            forward(true, true, "w1:p1"),
            Forward {
                id: "fwd-2".into(),
                pane_id: "w1:p2".into(),
                ..forward(true, true, "w1:p1")
            },
        ];
        let locations = HashMap::from([
            (
                "w1:p1".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Apps".into(),
                    tab_id: "w1:t1".into(),
                    tab: "One".into(),
                    ..Default::default()
                },
            ),
            (
                "w1:p2".into(),
                PaneLocation {
                    workspace_id: "w1".into(),
                    workspace: "Apps".into(),
                    tab_id: "w1:t2".into(),
                    tab: "Two".into(),
                    ..Default::default()
                },
            ),
        ]);

        let text = tree_text(&forwards, &locations);
        assert!(text.contains(&"  ├─ 󰓩 One".into()));
        assert!(text.iter().any(|line| line.starts_with("  │  └─ 󰆍 Vite")));
        assert!(text.contains(&"  └─ 󰓩 Two".into()));
    }
}
