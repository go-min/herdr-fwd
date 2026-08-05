use std::{env, fs, path::PathBuf};

use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::plugin::herdr::herdr_output;

const PORT_FORWARD_STATUS_TOKEN: &str = "$port_forward_status";
const DASHBOARD_POPUP_SHORTCUT: &str = "prefix+shift+f";
const DASHBOARD_POPUP_ACTION: &str = "herdr.fwd.open-dashboard-popup";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DashboardSetupStatus {
    pub(crate) sidebar_ports_enabled: bool,
    pub(crate) toast_delivery: Option<String>,
    pub(crate) popup_shortcut_enabled: bool,
}

pub(crate) fn enable_ports_row() -> Result<PathBuf, String> {
    update_config(add_ports_row_to_config)
}

pub(crate) fn set_ports_row(enabled: bool) -> Result<DashboardSetupStatus, String> {
    update_config(|config| update_ports_row(config, enabled))?;
    dashboard_setup_status()
}

pub(crate) fn dashboard_setup_status() -> Result<DashboardSetupStatus, String> {
    Ok(read_config_document()?
        .as_ref()
        .map(dashboard_setup_status_from_document)
        .unwrap_or_default())
}

pub(crate) fn configured_theme_name() -> Result<Option<String>, String> {
    Ok(read_config_document()?
        .as_ref()
        .and_then(theme_name_from_document))
}

fn read_config_document() -> Result<Option<DocumentMut>, String> {
    let path = herdr_config_path()?;
    let config = match fs::read_to_string(&path) {
        Ok(config) => config,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    parse_config(&config).map(Some)
}

pub(crate) fn enable_herdr_notifications() -> Result<DashboardSetupStatus, String> {
    update_config(update_herdr_notifications)?;
    dashboard_setup_status()
}

pub(crate) fn enable_dashboard_popup_shortcut() -> Result<DashboardSetupStatus, String> {
    update_config(update_dashboard_popup_shortcut)?;
    dashboard_setup_status()
}

fn update_config(
    transform: impl FnOnce(&str) -> Result<String, String>,
) -> Result<PathBuf, String> {
    let path = herdr_config_path()?;
    let existing = match fs::read_to_string(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    let updated = transform(&existing)?;
    if updated == existing {
        return Ok(path);
    }
    if !existing.is_empty() {
        let backup = path.with_extension("toml.herdr-fwd.bak");
        fs::copy(&path, &backup).map_err(|error| {
            format!(
                "failed to back up {} to {}: {error}",
                path.display(),
                backup.display()
            )
        })?;
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("config path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    herdr_fwd::atomic::write_file(&path, updated.as_bytes(), 0o600)?;
    Ok(path)
}

fn herdr_config_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("HERDR_CONFIG_PATH") {
        return Ok(PathBuf::from(path));
    }
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| "HOME is not set".to_string())?;
    Ok(base.join("herdr/config.toml"))
}

fn add_ports_row_to_config(config: &str) -> Result<String, String> {
    update_ports_row(config, true)
}

fn update_ports_row(config: &str, enabled: bool) -> Result<String, String> {
    let original = parse_config(config)?;
    let default_rows = if enabled && sidebar_rows_or_error(&original)?.is_none() {
        Some(herdr_default_space_rows()?)
    } else {
        None
    };
    update_ports_row_with_default_rows(config, enabled, default_rows.as_ref())
}

fn update_ports_row_with_default_rows(
    config: &str,
    enabled: bool,
    default_rows: Option<&Array>,
) -> Result<String, String> {
    let original = parse_config(config)?;
    let mut document = original.clone();
    let changed = if enabled {
        add_ports_row_to_document(&mut document, default_rows)?
    } else {
        remove_ports_row_from_document(&mut document)?
    };
    if changed {
        verify_ports_row_change(&original, &document, enabled, default_rows)?;
    }
    Ok(render_config(config, document, changed))
}

fn update_herdr_notifications(config: &str) -> Result<String, String> {
    let mut document = parse_config(config)?;
    let ui = ensure_table(document.as_table_mut(), "ui", "[ui]")?;
    let toast = ensure_table(ui, "toast", "[ui.toast]")?;
    let changed = toast.get("delivery").and_then(Item::as_str) != Some("herdr");
    if changed {
        toast["delivery"] = value("herdr");
    }
    Ok(render_config(config, document, changed))
}

fn update_dashboard_popup_shortcut(config: &str) -> Result<String, String> {
    let mut document = parse_config(config)?;
    let keys = ensure_table(document.as_table_mut(), "keys", "[keys]")?;
    if !keys.contains_key("command") {
        keys.insert("command", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let commands = keys["command"].as_array_of_tables_mut().ok_or_else(|| {
        "refusing to update: keys.command exists but is not an array of tables".to_string()
    })?;
    for binding in commands.iter() {
        if binding.get("key").and_then(Item::as_str) == Some(DASHBOARD_POPUP_SHORTCUT) {
            if binding.get("command").and_then(Item::as_str) == Some(DASHBOARD_POPUP_ACTION) {
                return Ok(config.into());
            }
            return Err(format!(
                "refusing to replace existing {DASHBOARD_POPUP_SHORTCUT} keybinding"
            ));
        }
    }
    let mut binding = Table::new();
    binding["key"] = value(DASHBOARD_POPUP_SHORTCUT);
    binding["type"] = value("plugin_action");
    binding["command"] = value(DASHBOARD_POPUP_ACTION);
    binding["description"] = value("Open port-forward dashboard");
    commands.push(binding);
    Ok(document.to_string())
}

fn parse_config(config: &str) -> Result<DocumentMut, String> {
    config.parse::<DocumentMut>().map_err(|error| {
        format!(
            "refusing to update invalid TOML: {error}. Fix the config first; it was not changed"
        )
    })
}

fn render_config(original: &str, document: DocumentMut, changed: bool) -> String {
    if changed {
        document.to_string()
    } else {
        original.into()
    }
}

fn add_ports_row_to_document(
    document: &mut DocumentMut,
    default_rows: Option<&Array>,
) -> Result<bool, String> {
    if sidebar_rows_or_error(document)?.is_none() {
        let defaults = default_rows.ok_or_else(|| {
            "refusing to add $port_forward_status: could not read Herdr's default ui.sidebar.spaces.rows".to_string()
        })?;
        let ui = ensure_table(document.as_table_mut(), "ui", "[ui]")?;
        let sidebar = ensure_table(ui, "sidebar", "[ui.sidebar]")?;
        let spaces = ensure_table(sidebar, "spaces", "[ui.sidebar.spaces]")?;
        spaces["rows"] = Item::Value(Value::Array(defaults.clone()));
    }
    let spaces = table_at_mut(document.as_table_mut(), &["ui", "sidebar", "spaces"])
        .ok_or_else(|| "ui.sidebar.spaces disappeared while updating config".to_string())?;
    let rows = spaces
        .get_mut("rows")
        .and_then(Item::as_array_mut)
        .ok_or_else(|| {
            "refusing to update: ui.sidebar.spaces.rows exists but is not an array".to_string()
        })?;
    if rows.iter().any(is_ports_row) {
        return Ok(false);
    }
    let mut ports_row = Array::new();
    ports_row.push(PORT_FORWARD_STATUS_TOKEN);
    rows.push(Value::Array(ports_row));
    Ok(true)
}

fn remove_ports_row_from_document(document: &mut DocumentMut) -> Result<bool, String> {
    let Some(spaces) = table_at_mut(document.as_table_mut(), &["ui", "sidebar", "spaces"]) else {
        return Ok(false);
    };
    let Some(rows) = spaces.get_mut("rows") else {
        return Ok(false);
    };
    let rows = rows.as_array_mut().ok_or_else(|| {
        "refusing to update: ui.sidebar.spaces.rows exists but is not an array".to_string()
    })?;
    let Some(index) = rows.iter().position(is_ports_row) else {
        return Ok(false);
    };
    rows.remove(index);
    Ok(true)
}

fn verify_ports_row_change(
    original: &DocumentMut,
    updated: &DocumentMut,
    enabled: bool,
    default_rows: Option<&Array>,
) -> Result<(), String> {
    if config_fingerprint(original) != config_fingerprint(updated) {
        return Err("refusing to update: sidebar change would modify configuration outside ui.sidebar.spaces.rows".into());
    }
    let original_rows = sidebar_rows_or_error(original)?
        .cloned()
        .or_else(|| default_rows.cloned())
        .ok_or_else(|| {
            "refusing to update: no effective default rows were available".to_string()
        })?;
    let updated_rows = sidebar_rows_or_error(updated)?
        .ok_or_else(|| "refusing to update: ui.sidebar.spaces.rows disappeared".to_string())?;
    let original_other = original_rows
        .iter()
        .filter(|row| !is_ports_row(row))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let updated_other = updated_rows
        .iter()
        .filter(|row| !is_ports_row(row))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let original_ports = original_rows.iter().filter(|row| is_ports_row(row)).count();
    let updated_ports = updated_rows.iter().filter(|row| is_ports_row(row)).count();
    let expected_counts = if enabled { (0, 1) } else { (1, 0) };
    if original_other != updated_other || (original_ports, updated_ports) != expected_counts {
        return Err(
            "refusing to update: only one standalone [\"$port_forward_status\"] row may change"
                .into(),
        );
    }
    Ok(())
}

fn sidebar_rows_or_error(document: &DocumentMut) -> Result<Option<&Array>, String> {
    let Some(spaces) = table_at(document.as_table(), &["ui", "sidebar", "spaces"]) else {
        return Ok(None);
    };
    let Some(rows) = spaces.get("rows") else {
        return Ok(None);
    };
    rows.as_array().map(Some).ok_or_else(|| {
        "refusing to update: ui.sidebar.spaces.rows exists but is not an array".to_string()
    })
}

fn herdr_default_space_rows() -> Result<Array, String> {
    let output = herdr_output(&["--default-config"])?;
    let default_config = String::from_utf8(output.stdout)
        .map_err(|error| format!("Herdr --default-config returned invalid UTF-8: {error}"))?;
    let rows = commented_default_assignment(&default_config, "[ui.sidebar.spaces]", "rows")?;
    let document = parse_config(&format!("[ui.sidebar.spaces]\n{rows}\n"))?;
    sidebar_rows_or_error(&document)?
        .cloned()
        .ok_or_else(|| "Herdr --default-config did not contain ui.sidebar.spaces.rows".to_string())
}

fn commented_default_assignment(config: &str, section: &str, key: &str) -> Result<String, String> {
    let mut in_section = false;
    let prefix = format!("# {key} =");
    for line in config.lines() {
        let line = line.trim_start();
        if line == format!("# {section}") {
            in_section = true;
            continue;
        }
        if in_section && line.starts_with("# [") {
            break;
        }
        if in_section && line.starts_with(&prefix) {
            return Ok(line.trim_start_matches("# ").to_string());
        }
    }
    Err(format!(
        "Herdr --default-config did not contain a default {section}.{key} assignment"
    ))
}

fn config_fingerprint(document: &DocumentMut) -> Vec<(String, String)> {
    let mut values = Vec::new();
    collect_config_fingerprint(document.as_table(), &mut Vec::new(), &mut values);
    values.sort();
    values
}

fn collect_config_fingerprint(
    table: &Table,
    path: &mut Vec<String>,
    values: &mut Vec<(String, String)>,
) {
    for (key, item) in table.iter() {
        path.push(key.to_string());
        if path.as_slice() == ["ui", "sidebar", "spaces", "rows"] {
            path.pop();
            continue;
        }
        if let Some(table) = item.as_table() {
            collect_config_fingerprint(table, path, values);
        } else if let Some(tables) = item.as_array_of_tables() {
            for (index, table) in tables.iter().enumerate() {
                path.push(index.to_string());
                collect_config_fingerprint(table, path, values);
                path.pop();
            }
        } else {
            values.push((path.join("."), item.to_string()));
        }
        path.pop();
    }
}

fn dashboard_setup_status_from_document(document: &DocumentMut) -> DashboardSetupStatus {
    let root = document.as_table();
    let sidebar_ports_enabled = table_at(root, &["ui", "sidebar", "spaces"])
        .and_then(|spaces| spaces.get("rows"))
        .and_then(Item::as_array)
        .is_some_and(|rows| rows.iter().any(is_ports_row));
    DashboardSetupStatus {
        sidebar_ports_enabled,
        toast_delivery: table_at(document.as_table(), &["ui", "toast"])
            .and_then(|toast| toast.get("delivery"))
            .and_then(Item::as_str)
            .map(str::to_string),
        popup_shortcut_enabled: table_at(root, &["keys"])
            .and_then(|keys| keys.get("command"))
            .and_then(Item::as_array_of_tables)
            .is_some_and(|commands| {
                commands.iter().any(|binding| {
                    binding.get("key").and_then(Item::as_str) == Some(DASHBOARD_POPUP_SHORTCUT)
                        && binding.get("type").and_then(Item::as_str) == Some("plugin_action")
                        && binding.get("command").and_then(Item::as_str)
                            == Some(DASHBOARD_POPUP_ACTION)
                })
            }),
    }
}

fn theme_name_from_document(document: &DocumentMut) -> Option<String> {
    document
        .get("theme")
        .and_then(Item::as_table_like)
        .and_then(|theme| theme.get("name"))
        .and_then(Item::as_str)
        .map(str::to_owned)
}

fn table_at<'a>(table: &'a Table, path: &[&str]) -> Option<&'a Table> {
    path.iter()
        .try_fold(table, |table, key| table.get(key)?.as_table())
}

fn table_at_mut<'a>(table: &'a mut Table, path: &[&str]) -> Option<&'a mut Table> {
    path.iter()
        .try_fold(table, |table, key| table.get_mut(key)?.as_table_mut())
}

fn ensure_table<'a>(parent: &'a mut Table, key: &str, name: &str) -> Result<&'a mut Table, String> {
    if !parent.contains_key(key) {
        parent.insert(key, Item::Table(Table::new()));
    }
    parent[key]
        .as_table_mut()
        .ok_or_else(|| format!("refusing to update: {name} exists but is not a table"))
}

fn is_ports_row(value: &Value) -> bool {
    value.as_array().is_some_and(|row| {
        row.len() == 1 && row.get(0).and_then(Value::as_str) == Some(PORT_FORWARD_STATUS_TOKEN)
    })
}

#[cfg(test)]
mod sidebar_config_tests {
    use super::{
        add_ports_row_to_config, commented_default_assignment,
        dashboard_setup_status_from_document, parse_config, theme_name_from_document,
        update_dashboard_popup_shortcut, update_herdr_notifications,
        update_ports_row_with_default_rows,
    };

    #[test]
    fn reads_the_configured_herdr_theme_name() {
        let document = parse_config("[theme]\nname = \"catppuccin-latte\"\n").unwrap();
        assert_eq!(
            theme_name_from_document(&document).as_deref(),
            Some("catppuccin-latte")
        );
    }

    #[test]
    fn adds_the_ports_row_idempotently_without_losing_existing_configuration() {
        let config = "# retain this comment\n[ui.sidebar.spaces]\nrows = [[\"state_icon\", \"workspace\"], [\"branch\", \"git_status\"]]\n";
        let updated = add_ports_row_to_config(config).unwrap();
        assert!(updated.contains("# retain this comment"));
        assert!(updated.contains("[\"state_icon\", \"workspace\"]"));
        assert!(updated.contains("[\"branch\", \"git_status\"]"));
        assert!(updated.contains("[\"$port_forward_status\"]"));
        assert!(updated.parse::<toml_edit::DocumentMut>().is_ok());
        assert_eq!(add_ports_row_to_config(&updated).unwrap(), updated);
    }

    #[test]
    fn adds_to_the_effective_herdr_default_when_rows_are_unset() {
        let defaults = parse_config(
            "[ui.sidebar.spaces]\nrows = [[\"state_icon\", \"workspace\"], [\"branch\", \"git_status\"]]\n",
        )
        .unwrap();
        let default_rows = super::sidebar_rows_or_error(&defaults).unwrap().unwrap();
        let updated = update_ports_row_with_default_rows(
            "[ui]\npane_gaps = true\n",
            true,
            Some(default_rows),
        )
        .unwrap();
        assert!(updated.contains("[\"state_icon\", \"workspace\"]"));
        assert!(updated.contains("[\"branch\", \"git_status\"]"));
        assert!(updated.contains("[\"$port_forward_status\"]"));
    }

    #[test]
    fn removes_only_the_standalone_ports_row() {
        let config = "[ui.sidebar.spaces]\nrows = [[\"state_icon\", \"workspace\"], [\"$port_forward_status\"], [\"branch\", \"git_status\"]]\n";
        let updated = update_ports_row_with_default_rows(config, false, None).unwrap();
        assert!(updated.contains("[\"state_icon\", \"workspace\"]"));
        assert!(updated.contains("[\"branch\", \"git_status\"]"));
        assert!(!updated.contains("[\"$port_forward_status\"]"));
    }

    #[test]
    fn enables_herdr_notifications_without_touching_other_toast_settings() {
        let config = "[ui.toast]\ndelivery = \"off\"\ndelay_seconds = 2\n";
        let updated = update_herdr_notifications(config).unwrap();
        assert!(updated.contains("delivery = \"herdr\""));
        assert!(updated.contains("delay_seconds = 2"));
        let status = dashboard_setup_status_from_document(&parse_config(&updated).unwrap());
        assert_eq!(status.toast_delivery.as_deref(), Some("herdr"));
        assert_eq!(update_herdr_notifications(&updated).unwrap(), updated);
    }

    #[test]
    fn adds_a_server_side_dashboard_popup_shortcut_without_touching_other_bindings() {
        let config =
            "[[keys.command]]\nkey = \"prefix+g\"\ntype = \"popup\"\ncommand = \"lazygit\"\n";
        let updated = update_dashboard_popup_shortcut(config).unwrap();
        assert!(updated.contains("key = \"prefix+g\""));
        assert!(updated.contains("key = \"prefix+shift+f\""));
        assert!(updated.contains("type = \"plugin_action\""));
        assert!(updated.contains("command = \"herdr.fwd.open-dashboard-popup\""));
        assert!(
            dashboard_setup_status_from_document(&parse_config(&updated).unwrap())
                .popup_shortcut_enabled
        );
        assert_eq!(update_dashboard_popup_shortcut(&updated).unwrap(), updated);
    }

    #[test]
    fn rejects_invalid_or_incompatible_configuration_without_rewriting_it() {
        for (config, expected) in [
            ("[ui\nrows = []\n", "refusing to update invalid TOML"),
            (
                "[ui.sidebar.spaces]\nrows = \"not an array\"\n",
                "ui.sidebar.spaces.rows exists but is not an array",
            ),
        ] {
            assert!(add_ports_row_to_config(config)
                .unwrap_err()
                .contains(expected));
        }
    }

    #[test]
    fn extracts_the_current_default_rows_from_herdr_output() {
        let default_config = "# [ui.sidebar.spaces]\n# rows = [[\"state_icon\", \"workspace\"], [\"branch\", \"git_status\"]]\n";
        assert_eq!(
            commented_default_assignment(default_config, "[ui.sidebar.spaces]", "rows").unwrap(),
            "rows = [[\"state_icon\", \"workspace\"], [\"branch\", \"git_status\"]]"
        );
    }
}
