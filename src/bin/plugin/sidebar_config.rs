use std::path::PathBuf;

pub(crate) use herdr_fwd::herdr_config::{enable_dashboard_popup_shortcut, DashboardSetupStatus};
use herdr_fwd::{herdr_config, RemoteSessionConfig};

use crate::plugin::{herdr::herdr_output, rpc::api_request};

pub(crate) fn enable_ports_row() -> Result<PathBuf, String> {
    herdr_config::enable_ports_row(|| {
        String::from_utf8(herdr_output(&["--default-config"])?.stdout)
            .map_err(|error| error.to_string())
    })
}

// Presentation belongs to the connecting client in Herdr 0.9. Only custom
// plugin command bindings still belong to the remote server.
pub(crate) fn dashboard_setup_status(
    config: &RemoteSessionConfig,
) -> Result<DashboardSetupStatus, String> {
    let mut status: DashboardSetupStatus = api_request(config, "GET", "/v1/settings/local", None)?;
    status.popup_shortcut_enabled = herdr_config::dashboard_setup_status()?.popup_shortcut_enabled;
    Ok(status)
}

pub(crate) fn set_ports_row(
    config: &RemoteSessionConfig,
    enabled: bool,
) -> Result<DashboardSetupStatus, String> {
    api_request(
        config,
        "POST",
        "/v1/settings/sidebar",
        Some(serde_json::json!({"enabled": enabled})),
    )
}

pub(crate) fn enable_herdr_notifications(
    config: &RemoteSessionConfig,
) -> Result<DashboardSetupStatus, String> {
    api_request(config, "POST", "/v1/settings/notifications", None)
}
