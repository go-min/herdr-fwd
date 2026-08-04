use std::{env, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use herdr_fwd::DEFAULT_PROCESS_TREE_DEPTH;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AfterForward {
    #[default]
    Space,
    Popup,
    Nothing,
}

impl AfterForward {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Space => Self::Popup,
            Self::Popup => Self::Nothing,
            Self::Nothing => Self::Space,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub(crate) onboarding: bool,
    pub(crate) after_forward: AfterForward,
    pub(crate) process_tree_depth: u8,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            onboarding: true,
            after_forward: AfterForward::Space,
            process_tree_depth: DEFAULT_PROCESS_TREE_DEPTH,
        }
    }
}

pub(crate) fn set_onboarding(onboarding: bool) -> Result<Preferences, String> {
    let mut preferences = load_preferences()?;
    preferences.onboarding = onboarding;
    write_preferences(&preferences)
}

pub(crate) fn load_preferences() -> Result<Preferences, String> {
    let path = preferences_path()?;
    match fs::read(&path) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes)
                .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
            toml_edit::de::from_str(&text)
                .map_err(|error| format!("failed to parse {}: {error}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
        Err(error) => Err(format!("failed to read {}: {error}", path.display())),
    }
}

pub(crate) fn set_after_forward(after_forward: AfterForward) -> Result<Preferences, String> {
    let mut preferences = load_preferences()?;
    preferences.after_forward = after_forward;
    write_preferences(&preferences)
}

pub(crate) fn set_process_tree_depth(process_tree_depth: u8) -> Result<Preferences, String> {
    let mut preferences = load_preferences()?;
    preferences.process_tree_depth = process_tree_depth;
    write_preferences(&preferences)
}

fn write_preferences(preferences: &Preferences) -> Result<Preferences, String> {
    let path = preferences_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("preferences path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let bytes = toml_edit::ser::to_string_pretty(preferences).map_err(|error| error.to_string())?;
    herdr_fwd::atomic::write_file(&path, bytes.as_bytes(), 0o600)?;
    Ok(preferences.clone())
}

fn preferences_path() -> Result<PathBuf, String> {
    preferences_path_from(
        env::var_os("HERDR_PLUGIN_CONFIG_DIR"),
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
    )
}

fn preferences_path_from(
    plugin_config: Option<std::ffi::OsString>,
    xdg_config: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    if let Some(directory) = plugin_config {
        return Ok(PathBuf::from(directory).join("config.toml"));
    }
    let base = xdg_config
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| "HOME is not set".to_string())?;
    Ok(base.join("herdr-fwd/config.toml"))
}

#[cfg(test)]
mod preferences_tests {
    use herdr_fwd::DEFAULT_PROCESS_TREE_DEPTH;

    use super::{preferences_path_from, AfterForward, Preferences};

    #[test]
    fn defaults_to_opening_the_dashboard_space_after_forwarding() {
        let preferences = Preferences::default();
        assert!(preferences.onboarding);
        assert_eq!(preferences.after_forward, AfterForward::Space);
        assert_eq!(preferences.process_tree_depth, DEFAULT_PROCESS_TREE_DEPTH);
    }

    #[test]
    fn keeps_onboarding_as_a_boolean_without_installation_metadata() {
        let mut preferences = Preferences::default();
        let serialized = toml_edit::ser::to_string_pretty(&preferences).unwrap();
        assert_eq!(
            serialized
                .lines()
                .find(|line| line.starts_with("onboarding")),
            Some("onboarding = true")
        );
        assert!(!serialized.contains("installation_source"));
        preferences.onboarding = false;
        assert!(
            !toml_edit::de::from_str::<Preferences>(
                &toml_edit::ser::to_string_pretty(&preferences).unwrap()
            )
            .unwrap()
            .onboarding
        );
    }

    #[test]
    fn stores_preferences_in_config_toml() {
        let directory = std::env::temp_dir().join("herdr-fwd-preferences-test");
        assert_eq!(
            preferences_path_from(Some(directory.clone().into_os_string()), None, None).unwrap(),
            directory.join("config.toml")
        );
    }
}
