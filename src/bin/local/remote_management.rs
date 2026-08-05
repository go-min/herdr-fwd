use std::env;

use crate::local::{
    release::{release_bundle, ReleaseBundle},
    ssh::{OwnedMaster, SshClient, SSH_DEPLOYMENT_TIMEOUT},
    support::{require_herdr_compatibility, secure_random_hex, RuntimeDirectory},
};
use serde::{Deserialize, Serialize};

use crate::local::support::require_ssh;

const REMOTE_PLUGIN_ID: &str = "herdr.fwd";
const REMOTE_PLUGIN_SOURCE: &str = "go-min/herdr-fwd";

pub(crate) fn manage_remote(arguments: &[String]) -> Result<(), String> {
    let operation = arguments
        .first()
        .map(String::as_str)
        .ok_or_else(remote_management_usage)?;
    let target = arguments.get(1).ok_or_else(remote_management_usage)?;
    if arguments.len() != 2 {
        return Err(remote_management_usage());
    }
    validate_ssh_target(target)?;
    require_ssh()?;

    let runtime = RuntimeDirectory::create("herdr-fwd-remote-")?;
    let client = SshClient {
        target: target.clone(),
        control_path: runtime.path().join("c"),
    };
    let master = OwnedMaster::start(client.clone())?;
    let remote_version = client.remote_command(&remote_prelude("herdr --version"))?;
    require_herdr_compatibility(&remote_version, "remote")?;

    match operation {
        "install" | "update" => ensure_remote_plugin(&client)?,
        "status" => print!(
            "{}",
            client.remote_command(&remote_plugin_status_command())?
        ),
        "uninstall" => uninstall_remote_plugin(&client)?,
        _ => return Err(remote_management_usage()),
    }
    drop(master);
    drop(runtime);
    Ok(())
}

/// Ensures the server that will host the plugin has the matching managed
/// plugin before attaching. The caller's private ControlMaster is reused, so
/// this preflight does not create another SSH authentication exchange.
pub(crate) fn ensure_remote_plugin(client: &SshClient) -> Result<(), String> {
    let current = remote_plugin_status(client)?;
    if matches!(current, Some(ref status) if status.enabled && status.version == env!("CARGO_PKG_VERSION"))
    {
        return Ok(());
    }

    println!("Preparing remote port forwarding on {}…", client.target);
    client.remote_command(&remote_prelude(&reject_active_sessions_command()))?;
    let created_by_hfwd = current.is_none();
    let previous_origin = (!created_by_hfwd)
        .then(|| remote_plugin_origin(client))
        .transpose()?
        .flatten();
    let update_owned_by_hfwd = current
        .as_ref()
        .zip(previous_origin.as_ref())
        .is_some_and(|(status, origin)| is_managed_remote_origin(status, origin));
    if let Err(remote_error) = client.remote_command_with_timeout(
        &install_remote_plugin_command(current.is_some()),
        SSH_DEPLOYMENT_TIMEOUT,
    ) {
        restore_install_failure_state(client, current.as_ref())?;
        let platform = remote_platform(client)?;
        if remote_release_access(client, &platform).is_ok() {
            let managed_root = managed_bundle_root(client)?;
            match current
                .as_ref()
                .filter(|status| status.root == managed_root)
            {
                Some(previous_bundle) => {
                    replace_linked_bundle_with_native_install(client, previous_bundle).map_err(
                        |retry_error| {
                            format!(
                                "remote plugin install failed ({remote_error}); linked-bundle migration failed: {retry_error}"
                            )
                        },
                    )?;
                }
                None => {
                    return Err(format!(
                        "remote plugin install failed even though the exact release is reachable; refusing local fallback: {remote_error}"
                    ));
                }
            }
        } else {
            let bundle = release_bundle(&platform).map_err(|local_error| format!(
                "remote plugin install failed ({remote_error}); local verified release fallback is unavailable: {local_error}"
            ))?;
            deploy_release_bundle(client, &bundle, current.as_ref())?;
        }
    }
    verify_remote_plugin(client)?;
    if created_by_hfwd || update_owned_by_hfwd {
        let installed = required_remote_plugin_status(client)?;
        if let Err(origin_error) = persist_remote_origin(client, &installed) {
            if created_by_hfwd {
                let rollback = remove_remote_plugin(client, &installed)
                    .and_then(|_| verify_remote_plugin_absent(client))
                    .and_then(|_| clear_remote_origin(client))
                    .and_then(|_| clear_managed_bundles(client));
                return Err(match rollback {
                    Ok(()) => origin_error,
                    Err(rollback_error) => format!(
                        "{origin_error}; newly installed plugin rollback failed: {rollback_error}"
                    ),
                });
            }
            return Err(origin_error);
        }
    }
    Ok(())
}

fn replace_linked_bundle_with_native_install(
    client: &SshClient,
    previous: &RemotePluginStatus,
) -> Result<(), String> {
    if let Err(error) = client.remote_command_with_timeout(
        &remote_prelude(&format!("herdr plugin unlink {REMOTE_PLUGIN_ID}")),
        SSH_DEPLOYMENT_TIMEOUT,
    ) {
        return Err(restore_linked_bundle(client, previous, error));
    }
    let install = client
        .remote_command_with_timeout(&install_remote_plugin_command(true), SSH_DEPLOYMENT_TIMEOUT);
    match install.and_then(|_| verify_remote_plugin(client)) {
        Ok(()) => Ok(()),
        Err(error) => Err(restore_linked_bundle(client, previous, error)),
    }
}

fn restore_linked_bundle(
    client: &SshClient,
    previous: &RemotePluginStatus,
    install_error: String,
) -> String {
    let enabled = if previous.enabled {
        "--enabled"
    } else {
        "--disabled"
    };
    let restore = remote_prelude(&format!(
        "herdr plugin disable {REMOTE_PLUGIN_ID} >/dev/null 2>&1 || true; \
         herdr plugin uninstall {REMOTE_PLUGIN_ID} >/dev/null 2>&1 || \
         herdr plugin unlink {REMOTE_PLUGIN_ID} >/dev/null 2>&1 || true; \
         herdr plugin link {} {enabled}",
        herdr_fwd::shell::quote(&previous.root)
    ));
    match client
        .remote_command_with_timeout(&restore, SSH_DEPLOYMENT_TIMEOUT)
        .and_then(|_| {
            let restored = remote_plugin_status(client)?;
            (restored.as_ref() == Some(previous))
                .then_some(())
                .ok_or_else(|| "restored plugin status does not match the previous bundle".into())
        }) {
        Ok(()) => install_error,
        Err(rollback_error) => {
            format!("{install_error}; previous linked bundle rollback failed: {rollback_error}")
        }
    }
}

fn restore_install_failure_state(
    client: &SshClient,
    previous: Option<&RemotePluginStatus>,
) -> Result<(), String> {
    let current = remote_plugin_status(client)?;
    if current.as_ref() == previous {
        return Ok(());
    }
    match previous {
        Some(previous) => {
            let enabled = if previous.enabled {
                "--enabled"
            } else {
                "--disabled"
            };
            client.remote_command_with_timeout(
                &remote_prelude(&format!(
                    "herdr plugin link {} {enabled}",
                    herdr_fwd::shell::quote(&previous.root)
                )),
                SSH_DEPLOYMENT_TIMEOUT,
            )?;
            let restored = remote_plugin_status(client)?;
            if restored.as_ref() != Some(previous) {
                return Err(
                    "remote install failed and the previous plugin state could not be restored"
                        .into(),
                );
            }
        }
        None => {
            client.remote_command_with_timeout(
                &remote_prelude(&format!(
                    "herdr plugin disable {REMOTE_PLUGIN_ID} >/dev/null 2>&1 || true; \
                     herdr plugin uninstall {REMOTE_PLUGIN_ID} >/dev/null 2>&1 || \
                     herdr plugin unlink {REMOTE_PLUGIN_ID} >/dev/null 2>&1 || true"
                )),
                SSH_DEPLOYMENT_TIMEOUT,
            )?;
            if remote_plugin_status(client)?.is_some() {
                return Err(
                    "remote install failed and its partial plugin registration remains".into(),
                );
            }
        }
    }
    Ok(())
}

fn uninstall_remote_plugin(client: &SshClient) -> Result<(), String> {
    client.remote_command(&remote_prelude(&reject_active_sessions_command()))?;
    let Some(current) = remote_plugin_status(client)? else {
        clear_remote_origin(client)?;
        return clear_managed_bundles(client);
    };
    remove_remote_plugin(client, &current)?;
    verify_remote_plugin_absent(client)?;
    clear_remote_origin(client)?;
    clear_managed_bundles(client)
}

fn remove_remote_plugin(client: &SshClient, current: &RemotePluginStatus) -> Result<(), String> {
    let managed_root = managed_bundle_root(client)?;
    client
        .remote_command_with_timeout(
            &remote_plugin_removal_command(current, &managed_root),
            SSH_DEPLOYMENT_TIMEOUT,
        )
        .map(|_| ())
}

fn managed_bundle_root(client: &SshClient) -> Result<String, String> {
    client
        .remote_command(&remote_prelude(
            "printf '%s' \"${XDG_DATA_HOME:-$HOME/.local/share}/herdr-fwd/plugins/current\"",
        ))
        .map(|root| root.trim().to_string())
}

fn remote_plugin_removal_command(current: &RemotePluginStatus, managed_root: &str) -> String {
    if current.root == managed_root {
        remote_prelude(&format!(
            "herdr plugin unlink {REMOTE_PLUGIN_ID}; rm -rf -- {}",
            herdr_fwd::shell::quote(managed_root)
        ))
    } else {
        remote_prelude(&format!(
            "herdr plugin disable {REMOTE_PLUGIN_ID} >/dev/null 2>&1 || true; herdr plugin uninstall {REMOTE_PLUGIN_ID}"
        ))
    }
}

fn verify_remote_plugin_absent(client: &SshClient) -> Result<(), String> {
    if remote_plugin_status(client)?.is_some() {
        Err("remote plugin uninstall completed but herdr.fwd is still registered".into())
    } else {
        Ok(())
    }
}

fn clear_managed_bundles(client: &SshClient) -> Result<(), String> {
    client
        .remote_command(&remote_prelude(
            "rm -rf -- \"${XDG_DATA_HOME:-$HOME/.local/share}/herdr-fwd/plugins\"",
        ))
        .map(|_| ())
}

fn clear_remote_origin(client: &SshClient) -> Result<(), String> {
    client
        .remote_command(&remote_prelude(
            "rm -f -- \"${XDG_STATE_HOME:-$HOME/.local/state}/herdr-fwd/plugin-origin.toml\"",
        ))
        .map(|_| ())
}

fn remote_platform(client: &SshClient) -> Result<String, String> {
    let output = client.remote_command(&remote_prelude("uname -s; uname -m"))?;
    let mut lines = output.lines();
    let os = match lines.next().map(str::trim) {
        Some("Darwin") => "macos",
        Some("Linux") => "linux",
        Some(value) => return Err(format!("unsupported remote platform: {value}")),
        None => return Err("remote platform check returned no operating system".into()),
    };
    let architecture = match lines.next().map(str::trim) {
        Some("x86_64" | "amd64") => "x86_64",
        Some("arm64" | "aarch64") => "aarch64",
        Some(value) => return Err(format!("unsupported remote architecture: {value}")),
        None => return Err("remote platform check returned no architecture".into()),
    };
    Ok(format!("{os}-{architecture}"))
}

fn remote_release_access(client: &SshClient, platform: &str) -> Result<(), String> {
    client
        .remote_command_with_timeout(
            &remote_release_access_command(platform),
            SSH_DEPLOYMENT_TIMEOUT,
        )
        .map(|_| ())
}

fn remote_release_access_command(platform: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let asset = format!("herdr-fwd-{platform}.tar.gz");
    let base = format!("https://github.com/{REMOTE_PLUGIN_SOURCE}/releases/download/v{version}");
    remote_prelude(&format!(
        "command -v git >/dev/null 2>&1; command -v curl >/dev/null 2>&1; \
         git ls-remote --exit-code https://github.com/{REMOTE_PLUGIN_SOURCE}.git refs/tags/v{version} >/dev/null; \
         curl -fsSL --connect-timeout 10 --max-time 45 -o /dev/null {asset_url}; \
         curl -fsSL --connect-timeout 10 --max-time 45 -o /dev/null {sums_url}",
        asset_url = herdr_fwd::shell::quote(&format!("{base}/{asset}")),
        sums_url = herdr_fwd::shell::quote(&format!("{base}/SHA256SUMS")),
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemotePluginStatus {
    enabled: bool,
    version: String,
    root: String,
}

fn verify_remote_plugin(client: &SshClient) -> Result<(), String> {
    verify_plugin_status(remote_plugin_status(client)?, &client.target)
}

fn verify_plugin_status(status: Option<RemotePluginStatus>, target: &str) -> Result<(), String> {
    match status {
        Some(status) if status.enabled && status.version == env!("CARGO_PKG_VERSION") => Ok(()),
        Some(status) => Err(format!(
            "remote plugin setup on {} did not produce the required enabled version {} (enabled={enabled}, version={version})",
            target,
            env!("CARGO_PKG_VERSION"),
            enabled = status.enabled,
            version = status.version,
        )),
        None => Err(format!(
            "remote plugin setup on {} completed but herdr.fwd is not registered",
            target
        )),
    }
}

fn required_remote_plugin_status(client: &SshClient) -> Result<RemotePluginStatus, String> {
    remote_plugin_status(client)?.ok_or_else(|| "remote plugin is not registered".into())
}

fn deploy_release_bundle(
    client: &SshClient,
    bundle: &ReleaseBundle,
    previous_status: Option<&RemotePluginStatus>,
) -> Result<(), String> {
    let deployment_id = secure_random_hex(12)?;
    let prepare = local_bundle_command(
        &deployment_id,
        "install -d -m 700 \"$stage/target/release\"",
    );
    if let Err(error) = client.remote_command_with_timeout(&prepare, SSH_DEPLOYMENT_TIMEOUT) {
        cleanup_deployment_stage(client, &deployment_id);
        return Err(error);
    }
    if let Err(error) = client.remote_command_with_stdin_timeout(
        &local_bundle_command(&deployment_id, "cat > \"$stage/herdr-plugin.toml\""),
        &bundle.manifest,
        SSH_DEPLOYMENT_TIMEOUT,
    ) {
        cleanup_deployment_stage(client, &deployment_id);
        return Err(error);
    }
    if let Err(error) = client.remote_command_with_stdin_timeout(
        &local_bundle_command(
            &deployment_id,
            "cat > \"$stage/target/release/herdr-fwd-plugin\"",
        ),
        &bundle.binary,
        SSH_DEPLOYMENT_TIMEOUT,
    ) {
        cleanup_deployment_stage(client, &deployment_id);
        return Err(error);
    }
    let activate = format!(
        "chmod 755 \"$stage/target/release/herdr-fwd-plugin\"; current=\"$root/current\"; previous=\"$root/.previous-{deployment_id}\"; if [ -e \"$current\" ]; then mv \"$current\" \"$previous\"; fi; mv \"$stage\" \"$current\"; herdr plugin link \"$current\" --enabled"
    );
    if let Err(error) = client.remote_command_with_timeout(
        &local_bundle_command(&deployment_id, &activate),
        SSH_DEPLOYMENT_TIMEOUT,
    ) {
        return Err(rollback_deployment(
            client,
            &deployment_id,
            previous_status,
            error,
        ));
    }
    if let Err(error) = verify_remote_plugin(client) {
        return Err(rollback_deployment(
            client,
            &deployment_id,
            previous_status,
            error,
        ));
    }
    if let Err(error) = client.remote_command_with_timeout(
        &local_bundle_command(
            &deployment_id,
            &format!("rm -rf \"$root/.previous-{deployment_id}\""),
        ),
        SSH_DEPLOYMENT_TIMEOUT,
    ) {
        eprintln!("warning: remote plugin activated but old bundle cleanup failed: {error}");
    }
    Ok(())
}

fn cleanup_deployment_stage(client: &SshClient, deployment_id: &str) {
    let _ = client.remote_command_with_timeout(
        &local_bundle_command(deployment_id, "rm -rf -- \"$stage\""),
        SSH_DEPLOYMENT_TIMEOUT,
    );
}

fn rollback_deployment(
    client: &SshClient,
    deployment_id: &str,
    previous_status: Option<&RemotePluginStatus>,
    deployment_error: String,
) -> String {
    let restore_registration = previous_status.map_or_else(String::new, |status| {
        let previous_root = herdr_fwd::shell::quote(&status.root);
        let enabled = if status.enabled {
            "--enabled"
        } else {
            "--disabled"
        };
        format!(
            "previous_root={previous_root}; if [ \"$previous_root\" = \"$current\" ]; then previous_root=\"$current\"; fi; herdr plugin link \"$previous_root\" {enabled}"
        )
    });
    let rollback = format!(
        "current=\"$root/current\"; previous=\"$root/.previous-{deployment_id}\"; \
         herdr plugin unlink {REMOTE_PLUGIN_ID} >/dev/null 2>&1 || true; \
         rm -rf -- \"$current\" \"$stage\"; \
         if [ -e \"$previous\" ]; then mv \"$previous\" \"$current\"; fi; \
         {restore_registration}"
    );
    match client.remote_command_with_timeout(
        &local_bundle_command(deployment_id, &rollback),
        SSH_DEPLOYMENT_TIMEOUT,
    ) {
        Ok(_) => deployment_error,
        Err(rollback_error) => {
            format!("{deployment_error}; previous plugin rollback failed: {rollback_error}")
        }
    }
}

fn local_bundle_command(deployment_id: &str, command: &str) -> String {
    remote_prelude(&format!(
        "root=\"${{XDG_DATA_HOME:-$HOME/.local/share}}/herdr-fwd/plugins\"; stage=\"$root/.staging-{deployment_id}\"; install -d -m 700 \"$root\"; {command}"
    ))
}

fn remote_plugin_status_command() -> String {
    remote_prelude(&format!(
        "herdr plugin list --plugin {REMOTE_PLUGIN_ID} --json"
    ))
}

fn remote_plugin_status(client: &SshClient) -> Result<Option<RemotePluginStatus>, String> {
    let status = client.remote_command(&remote_plugin_status_command())?;
    let value = serde_json::from_str::<serde_json::Value>(&status)
        .map_err(|error| format!("invalid remote plugin status: {error}"))?;
    remote_plugin_status_from_json(&value)
}

fn remote_plugin_status_from_json(
    value: &serde_json::Value,
) -> Result<Option<RemotePluginStatus>, String> {
    match value {
        serde_json::Value::Object(object)
            if object.get("plugin_id").and_then(serde_json::Value::as_str)
                == Some(REMOTE_PLUGIN_ID) =>
        {
            let root = object
                .get("plugin_root")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "remote plugin status has no plugin_root".to_string())?;
            Ok(Some(RemotePluginStatus {
                enabled: object
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                version: object
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                root: root.to_string(),
            }))
        }
        serde_json::Value::Object(object) => {
            for value in object.values() {
                if let Some(status) = remote_plugin_status_from_json(value)? {
                    return Ok(Some(status));
                }
            }
            Ok(None)
        }
        serde_json::Value::Array(values) => {
            for value in values {
                if let Some(status) = remote_plugin_status_from_json(value)? {
                    return Ok(Some(status));
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn persist_remote_origin(client: &SshClient, installed: &RemotePluginStatus) -> Result<(), String> {
    let command_error = client
        .remote_command(&persist_remote_origin_command(&installed.root))
        .err();
    let persisted = remote_plugin_origin(client)?;
    if persisted
        .as_ref()
        .is_some_and(|origin| is_managed_remote_origin(installed, origin))
    {
        Ok(())
    } else {
        Err(command_error.unwrap_or_else(|| {
            "remote plugin provenance was not persisted after verified activation".into()
        }))
    }
}

fn persist_remote_origin_command(plugin_root: &str) -> String {
    let contents = herdr_fwd::shell::quote(&remote_origin_contents(plugin_root));
    remote_prelude(&format!(
        "state=\"${{XDG_STATE_HOME:-$HOME/.local/state}}/herdr-fwd\"; install -d -m 700 \"$state\"; temporary=\"$state/.plugin-origin.toml.$$\"; trap 'rm -f -- \"$temporary\"' EXIT HUP INT TERM; umask 077; printf '%s' {contents} > \"$temporary\"; mv -f \"$temporary\" \"$state/plugin-origin.toml\"; trap - EXIT HUP INT TERM"
    ))
}

#[derive(Clone, Deserialize, Serialize)]
struct RemotePluginOrigin {
    origin: String,
    plugin_root: String,
    version: String,
}

fn remote_origin_contents(plugin_root: &str) -> String {
    toml_edit::ser::to_string_pretty(&RemotePluginOrigin {
        origin: "hfwd_remote".into(),
        plugin_root: plugin_root.into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
    .expect("remote plugin origin serialization must succeed")
}

fn remote_plugin_origin(client: &SshClient) -> Result<Option<RemotePluginOrigin>, String> {
    let contents = client.remote_command(&remote_plugin_origin_command())?;
    Ok(toml_edit::de::from_str(&contents).ok())
}

fn remote_plugin_origin_command() -> String {
    remote_prelude(
        "origin=\"${XDG_STATE_HOME:-$HOME/.local/state}/herdr-fwd/plugin-origin.toml\"; if [ -f \"$origin\" ]; then cat \"$origin\"; fi",
    )
}

fn is_managed_remote_origin(status: &RemotePluginStatus, origin: &RemotePluginOrigin) -> bool {
    origin.origin == "hfwd_remote"
        && origin.plugin_root == status.root
        && origin.version == status.version
}

fn install_remote_plugin_command(is_update: bool) -> String {
    let mut steps = vec![
        format!(
            "HERDR_FWD_MANAGED_REMOTE_INSTALL=1 herdr plugin install {REMOTE_PLUGIN_SOURCE} --ref v{} --yes",
            env!("CARGO_PKG_VERSION")
        ),
        format!("herdr plugin enable {REMOTE_PLUGIN_ID}"),
        format!("herdr plugin list --plugin {REMOTE_PLUGIN_ID} --json"),
    ];
    if is_update {
        steps.insert(
            0,
            "echo 'Updating managed remote port-forward plugin…'".into(),
        );
    }
    remote_prelude(&steps.join("; "))
}

fn remote_prelude(command: &str) -> String {
    format!(
        "set -eu; export PATH=\"$HOME/.local/bin:/usr/local/bin:$PATH\"; \
         command -v herdr >/dev/null 2>&1 || {{ echo 'error: remote Herdr is required' >&2; exit 127; }}; \
         {command}"
    )
}

fn reject_active_sessions_command() -> String {
    "if find \"$HOME/.cache/herdr-fwd\" -name 'session-*.json' \
     ! -name '*.dashboard.json' -type f -print -quit 2>/dev/null | grep -q .; then \
     echo 'error: an active remote-forward session exists; disconnect it before update/uninstall' >&2; \
     exit 1; fi"
        .into()
}

pub(crate) fn validate_ssh_target(target: &str) -> Result<(), String> {
    if target.is_empty()
        || target.starts_with('-')
        || target
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        Err("SSH target must be a non-empty host or alias without whitespace or options".into())
    } else {
        Ok(())
    }
}

fn remote_management_usage() -> String {
    "usage: hfwd remote {install|update|status|uninstall} <target>".into()
}

#[cfg(test)]
mod remote_management_tests {
    use crate::local::release::deployment_manifest;

    use super::{
        is_managed_remote_origin, remote_plugin_removal_command, validate_ssh_target,
        RemotePluginOrigin, RemotePluginStatus,
    };
    #[test]
    fn validates_remote_management_targets() {
        assert!(validate_ssh_target("workbox").is_ok());
        assert!(validate_ssh_target("dev@example.test").is_ok());
        assert!(validate_ssh_target("").is_err());
        assert!(validate_ssh_target("-oProxyCommand=bad").is_err());
        assert!(validate_ssh_target("work box").is_err());
    }

    #[test]
    fn parses_the_exact_remote_plugin_root_for_rollback_and_uninstall() {
        let value = serde_json::json!({"result": {"plugins": [{
            "plugin_id": "herdr.fwd",
            "enabled": true,
            "version": env!("CARGO_PKG_VERSION"),
            "plugin_root": "/home/demo/.local/share/herdr-fwd/plugins/current"
        }]}});
        let status = super::remote_plugin_status_from_json(&value)
            .unwrap()
            .unwrap();
        assert!(status.enabled);
        assert_eq!(status.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            status.root,
            "/home/demo/.local/share/herdr-fwd/plugins/current"
        );
    }

    #[test]
    fn recognizes_provenance_only_for_the_exact_pre_update_plugin() {
        let current = RemotePluginStatus {
            enabled: true,
            version: "0.1.3".into(),
            root: "/previous/root".into(),
        };
        let managed = RemotePluginOrigin {
            origin: "hfwd_remote".into(),
            plugin_root: "/previous/root".into(),
            version: "0.1.3".into(),
        };
        let manual = RemotePluginOrigin {
            origin: "manual".into(),
            plugin_root: "/manual/root".into(),
            version: "0.1.3".into(),
        };

        assert!(is_managed_remote_origin(&current, &managed));
        assert!(!is_managed_remote_origin(&current, &manual));

        let mut stale_root = managed.clone();
        stale_root.plugin_root = "/manual/root".into();
        assert!(!is_managed_remote_origin(&current, &stale_root));

        let mut stale_version = managed;
        stale_version.version = "0.1.2".into();
        assert!(!is_managed_remote_origin(&current, &stale_version));
    }

    #[test]
    fn fallback_manifest_never_triggers_a_remote_build() {
        let manifest = deployment_manifest().unwrap();
        assert!(!manifest.contains("[[build]]"));
        assert!(manifest.contains("herdr-fwd-plugin"));
    }

    #[test]
    fn removal_unlinks_only_the_exact_managed_fallback_root() {
        let managed_root = "/home/demo/.local/share/herdr-fwd/plugins/current";
        let mut status = RemotePluginStatus {
            enabled: true,
            version: env!("CARGO_PKG_VERSION").into(),
            root: managed_root.into(),
        };

        let managed = remote_plugin_removal_command(&status, managed_root);
        assert!(managed.contains("plugin unlink"));
        assert!(managed.contains("rm -rf"));
        assert!(!managed.contains("plugin uninstall"));

        status.root = "/home/demo/.config/herdr/plugins/github/herdr.fwd".into();
        let github = remote_plugin_removal_command(&status, managed_root);
        assert!(github.contains("plugin uninstall"));
        assert!(!github.contains("rm -rf"));
    }
}
