use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::ForwardRequest;

pub const MAX_FORWARDS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Forward {
    pub id: String,
    pub remote_port: u16,
    pub local_port: u16,
    pub remote_host: String,
    pub pane_id: String,
    pub process: String,
    pub detected_url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub automatic: bool,
    #[serde(default)]
    pub server_started_at: Option<u64>,
    #[serde(default)]
    pub process_id: Option<u32>,
    #[serde(default)]
    pub tunnel_opened_at: u64,
}

fn default_true() -> bool {
    true
}

impl Forward {
    pub fn remote_host_display(&self) -> &str {
        if self.remote_host == "::1" || self.remote_host == "[::1]" {
            "[::1]"
        } else {
            &self.remote_host
        }
    }

    pub fn local_url(&self) -> String {
        let scheme = if self.detected_url.starts_with("https://") {
            "https"
        } else {
            "http"
        };
        let suffix = self
            .detected_url
            .split_once("://")
            .and_then(|(_, rest)| rest.find(['/', '?', '#']).map(|index| &rest[index..]))
            .unwrap_or("");
        format!("{scheme}://localhost:{}{suffix}", self.local_port)
    }
}

pub trait Ssh: Send + Sync + 'static {
    fn forward(&self, local: u16, host: &str, remote: u16) -> Result<(), String>;
    fn cancel(&self, local: u16, host: &str, remote: u16) -> Result<(), String>;
}

pub struct Registry<S: Ssh> {
    pub forwards: BTreeMap<String, Forward>,
    next: u64,
    pub ssh: S,
    pub port_available: fn(u16) -> bool,
    pub now: fn() -> u64,
}

#[derive(Clone)]
pub struct RegistrySnapshot {
    forwards: BTreeMap<String, Forward>,
    next: u64,
}

impl<S: Ssh> Registry<S> {
    pub fn new(ssh: S) -> Self {
        Self {
            forwards: BTreeMap::new(),
            next: 0,
            ssh,
            port_available: |port| std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
            now: unix_time_now,
        }
    }

    pub fn snapshot(&self) -> RegistrySnapshot {
        RegistrySnapshot {
            forwards: self.forwards.clone(),
            next: self.next,
        }
    }

    pub fn restore(&mut self, snapshot: RegistrySnapshot) -> Result<(), String> {
        let current = self.forwards.clone();
        let mut errors = Vec::new();

        for forward in current.values().filter(|forward| forward.enabled) {
            let keep = snapshot
                .forwards
                .values()
                .any(|candidate| candidate.enabled && same_tunnel(candidate, forward));
            if keep {
                continue;
            }
            match self.ssh.cancel(
                forward.local_port,
                &forward.remote_host,
                forward.remote_port,
            ) {
                Ok(()) => {
                    if let Some(current) = self.forwards.get_mut(&forward.id) {
                        current.enabled = false;
                        current.tunnel_opened_at = 0;
                    }
                }
                Err(error) => errors.push(format!("cancel {}: {error}", forward.id)),
            }
        }

        for forward in snapshot.forwards.values().filter(|forward| forward.enabled) {
            let present = current
                .values()
                .any(|candidate| candidate.enabled && same_tunnel(candidate, forward));
            if present {
                continue;
            }
            match self.ssh.forward(
                forward.local_port,
                &forward.remote_host,
                forward.remote_port,
            ) {
                Ok(()) => {
                    self.forwards.insert(forward.id.clone(), forward.clone());
                }
                Err(error) => errors.push(format!("restore {}: {error}", forward.id)),
            }
        }

        if errors.is_empty() {
            self.forwards = snapshot.forwards;
            self.next = self.next.max(snapshot.next);
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub fn create(&mut self, request: &ForwardRequest) -> Result<(Forward, bool), String> {
        self.create_with_enabled(request, true)
    }

    pub fn create_paused(&mut self, request: &ForwardRequest) -> Result<(Forward, bool), String> {
        self.create_with_enabled(request, false)
    }

    fn create_with_enabled(
        &mut self,
        request: &ForwardRequest,
        enabled: bool,
    ) -> Result<(Forward, bool), String> {
        request.validate()?;
        let remote_host = match request.remote_host.as_str() {
            "::1" | "[::1]" => "::1",
            _ => "127.0.0.1",
        };
        if let Some(id) = self
            .forwards
            .values()
            .find(|forward| {
                forward.remote_port == request.remote_port && forward.remote_host == remote_host
            })
            .map(|forward| forward.id.clone())
        {
            let forward = self
                .forwards
                .get_mut(&id)
                .expect("forward id came from registry");
            if request.process_id.is_some() && request.process_id != forward.process_id {
                forward.server_started_at = request.server_started_at;
                forward.process_id = request.process_id;
            } else {
                forward.server_started_at = forward.server_started_at.or(request.server_started_at);
                forward.process_id = forward.process_id.or(request.process_id);
            }
            if forward.enabled && forward.tunnel_opened_at == 0 {
                forward.tunnel_opened_at = (self.now)();
            }
            return Ok((forward.clone(), false));
        }
        if self.forwards.len() >= MAX_FORWARDS {
            return Err(format!("forward limit reached ({MAX_FORWARDS})"));
        }

        let start = if request.preferred_local_port == 0 {
            request.remote_port
        } else {
            request.preferred_local_port
        };
        let local_port = if enabled {
            self.available_port(start, None)?
        } else {
            start
        };

        if enabled {
            self.ssh
                .forward(local_port, remote_host, request.remote_port)?;
        }
        self.next = self.next.saturating_add(1);
        let forward = Forward {
            id: format!("fwd-{}", self.next),
            remote_port: request.remote_port,
            local_port,
            remote_host: remote_host.into(),
            pane_id: request.pane_id.clone(),
            process: request.process.clone(),
            detected_url: request.detected_url.clone(),
            enabled,
            automatic: request.automatic,
            server_started_at: request.server_started_at,
            process_id: request.process_id,
            tunnel_opened_at: if enabled { (self.now)() } else { 0 },
        };
        self.forwards.insert(forward.id.clone(), forward.clone());
        Ok((forward, true))
    }

    pub fn remove(&mut self, id: &str) -> Result<Option<Forward>, String> {
        let Some(forward) = self.forwards.get(id).cloned() else {
            return Ok(None);
        };
        if forward.enabled {
            self.ssh.cancel(
                forward.local_port,
                &forward.remote_host,
                forward.remote_port,
            )?;
        }
        self.forwards.remove(id);
        Ok(Some(forward))
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<Option<Forward>, String> {
        let Some(current) = self.forwards.get(id).cloned() else {
            return Ok(None);
        };
        if current.enabled == enabled {
            return Ok(Some(current));
        }
        if enabled {
            let local_port = self.available_port(current.local_port, Some(id))?;
            self.ssh
                .forward(local_port, &current.remote_host, current.remote_port)?;
            let forward = self
                .forwards
                .get_mut(id)
                .ok_or_else(|| "forward disappeared during enable".to_string())?;
            forward.local_port = local_port;
            forward.enabled = true;
            forward.tunnel_opened_at = (self.now)();
            Ok(Some(forward.clone()))
        } else {
            self.ssh.cancel(
                current.local_port,
                &current.remote_host,
                current.remote_port,
            )?;
            let forward = self
                .forwards
                .get_mut(id)
                .ok_or_else(|| "forward disappeared during disable".to_string())?;
            forward.enabled = false;
            forward.tunnel_opened_at = 0;
            Ok(Some(forward.clone()))
        }
    }

    pub fn set_local_port(&mut self, id: &str, local_port: u16) -> Result<Option<Forward>, String> {
        if local_port == 0 {
            return Err("localPort must be in 1..=65535".into());
        }
        let Some(current) = self.forwards.get(id).cloned() else {
            return Ok(None);
        };
        if current.local_port == local_port {
            return Ok(Some(current));
        }
        if self.forwards.iter().any(|(other_id, forward)| {
            other_id != id && forward.enabled && forward.local_port == local_port
        }) || (current.enabled && !(self.port_available)(local_port))
        {
            return Err(format!("local port {local_port} is unavailable"));
        }

        if current.enabled {
            self.ssh.cancel(
                current.local_port,
                &current.remote_host,
                current.remote_port,
            )?;
            if let Err(error) =
                self.ssh
                    .forward(local_port, &current.remote_host, current.remote_port)
            {
                return match self.ssh.forward(
                    current.local_port,
                    &current.remote_host,
                    current.remote_port,
                ) {
                    Ok(()) => Err(format!("failed to change local port: {error}")),
                    Err(rollback) => {
                        // The old forward was cancelled and neither replacement
                        // succeeded. Keep the registry truthful so the UI does
                        // not claim that an unreachable mapping is active.
                        if let Some(forward) = self.forwards.get_mut(id) {
                            forward.enabled = false;
                            forward.tunnel_opened_at = 0;
                        }
                        Err(format!(
                            "failed to change local port: {error}; rollback failed: {rollback}; forward is now disabled"
                        ))
                    }
                };
            }
        }

        let forward = self
            .forwards
            .get_mut(id)
            .ok_or_else(|| "forward disappeared while changing local port".to_string())?;
        forward.local_port = local_port;
        if forward.enabled {
            forward.tunnel_opened_at = (self.now)();
        }
        Ok(Some(forward.clone()))
    }

    fn available_port(&self, start: u16, excluding_id: Option<&str>) -> Result<u16, String> {
        (start..=u16::MAX)
            .find(|port| {
                *port != 0
                    && (self.port_available)(*port)
                    && !self.forwards.iter().any(|(id, forward)| {
                        Some(id.as_str()) != excluding_id
                            && forward.enabled
                            && forward.local_port == *port
                    })
            })
            .ok_or_else(|| "no available local port".to_string())
    }

    pub fn close_all(&mut self) -> Vec<String> {
        let ids = self.forwards.keys().cloned().collect::<Vec<_>>();
        ids.into_iter()
            .filter_map(|id| self.remove(&id).err().map(|error| format!("{id}: {error}")))
            .collect()
    }
}

fn same_tunnel(left: &Forward, right: &Forward) -> bool {
    left.local_port == right.local_port
        && left.remote_port == right.remote_port
        && left.remote_host == right.remote_host
}

fn unix_time_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{Registry, Ssh};
    use crate::ForwardRequest;

    #[derive(Clone, Default)]
    struct FakeSsh {
        calls: Arc<Mutex<Vec<String>>>,
        fail_cancel: bool,
        fail_forward_on: Option<u16>,
    }

    impl Ssh for FakeSsh {
        fn forward(&self, local: u16, host: &str, remote: u16) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("forward:{local}:{host}:{remote}"));
            if self.fail_forward_on == Some(local) {
                Err("forward failed".into())
            } else {
                Ok(())
            }
        }

        fn cancel(&self, local: u16, host: &str, remote: u16) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("cancel:{local}:{host}:{remote}"));
            if self.fail_cancel {
                Err("cancel failed".into())
            } else {
                Ok(())
            }
        }
    }

    #[derive(Clone, Default)]
    struct FailedRollbackSsh {
        forward_calls: Arc<Mutex<u8>>,
    }

    impl Ssh for FailedRollbackSsh {
        fn forward(&self, local: u16, _host: &str, _remote: u16) -> Result<(), String> {
            let mut calls = self.forward_calls.lock().unwrap();
            *calls += 1;
            if *calls == 1 {
                Ok(())
            } else {
                Err(format!("forward {local} failed"))
            }
        }

        fn cancel(&self, _local: u16, _host: &str, _remote: u16) -> Result<(), String> {
            Ok(())
        }
    }

    fn request() -> ForwardRequest {
        ForwardRequest {
            remote_port: 5173,
            preferred_local_port: 5173,
            remote_host: "127.0.0.1".into(),
            pane_id: "w1:p1".into(),
            process: "vite".into(),
            detected_url: "http://localhost:5173/".into(),
            automatic: true,
            server_started_at: None,
            process_id: None,
        }
    }

    #[test]
    fn deduplicates_and_enriches_an_existing_forward() {
        let mut registry = Registry::new(FakeSsh::default());
        registry.port_available = |_| true;
        registry.now = || 1_722_000_120;
        let (first, created) = registry.create(&request()).unwrap();
        assert!(created);
        assert_eq!(first.tunnel_opened_at, 1_722_000_120);

        let mut enriched_request = request();
        enriched_request.process_id = Some(10);
        enriched_request.server_started_at = Some(1_722_000_000);
        let (duplicate, created) = registry.create(&enriched_request).unwrap();

        assert!(!created);
        assert_eq!(first.id, duplicate.id);
        assert_eq!(duplicate.process_id, Some(10));
        assert_eq!(duplicate.server_started_at, Some(1_722_000_000));
        assert!(registry.close_all().is_empty());
        assert!(registry.forwards.is_empty());
    }

    #[test]
    fn normalizes_a_valid_localhost_request_to_loopback() {
        let ssh = FakeSsh::default();
        let calls = ssh.calls.clone();
        let mut registry = Registry::new(ssh);
        registry.port_available = |_| true;
        let mut request = request();
        request.remote_host = "localhost".into();

        let (forward, created) = registry.create(&request).unwrap();

        assert!(created);
        assert_eq!(forward.remote_host, "127.0.0.1");
        assert_eq!(*calls.lock().unwrap(), ["forward:5173:127.0.0.1:5173"]);
    }

    #[test]
    fn paused_forwards_have_no_live_tunnel_timestamp() {
        let mut registry = Registry::new(FakeSsh::default());
        registry.port_available = |_| true;
        registry.now = || 1_722_000_120;

        let (forward, _) = registry.create_paused(&request()).unwrap();

        assert!(!forward.enabled);
        assert_eq!(forward.tunnel_opened_at, 0);
    }

    #[test]
    fn refreshes_server_time_when_the_process_restarts() {
        let mut registry = Registry::new(FakeSsh::default());
        registry.port_available = |_| true;
        let mut first_request = request();
        first_request.process_id = Some(10);
        first_request.server_started_at = Some(1_722_000_000);
        let (first, _) = registry.create(&first_request).unwrap();

        let mut restarted_request = request();
        restarted_request.process_id = Some(11);
        restarted_request.server_started_at = Some(1_722_000_100);
        let (updated, created) = registry.create(&restarted_request).unwrap();

        assert!(!created);
        assert_eq!(updated.id, first.id);
        assert_eq!(updated.process_id, Some(11));
        assert_eq!(updated.server_started_at, Some(1_722_000_100));
    }

    #[test]
    fn enforces_the_per_session_forward_limit() {
        let mut registry = Registry::new(FakeSsh::default());
        registry.port_available = |_| true;
        for index in 1..=super::MAX_FORWARDS {
            let mut request = request();
            request.remote_port = index as u16;
            request.preferred_local_port = index as u16;
            request.detected_url = format!("http://localhost:{index}/");
            registry.create(&request).unwrap();
        }
        let mut overflow = request();
        overflow.remote_port = 30_000;
        overflow.preferred_local_port = 30_000;
        overflow.detected_url = "http://localhost:30000/".into();
        assert!(registry.create(&overflow).is_err());
    }

    #[test]
    fn failed_cancel_keeps_registry_entry_for_retry() {
        let ssh = FakeSsh {
            fail_cancel: true,
            ..FakeSsh::default()
        };
        let mut registry = Registry::new(ssh);
        registry.port_available = |_| true;
        let (forward, _) = registry.create(&request()).unwrap();
        assert!(registry.remove(&forward.id).is_err());
        assert!(registry.forwards.contains_key(&forward.id));
        assert!(registry.set_enabled(&forward.id, false).is_err());
        assert!(registry.forwards[&forward.id].enabled);
    }

    #[test]
    fn maps_detected_scheme_and_path_to_local_port() {
        let mut registry = Registry::new(FakeSsh::default());
        registry.port_available = |_| true;
        let mut request = request();
        request.detected_url = "https://localhost:5173/docs?dev=1".into();
        let (forward, _) = registry.create(&request).unwrap();
        assert_eq!(forward.local_url(), "https://localhost:5173/docs?dev=1");
    }

    #[test]
    fn disabling_is_idempotent_and_enabling_restores_the_forward() {
        let ssh = FakeSsh::default();
        let calls = ssh.calls.clone();
        let mut registry = Registry::new(ssh);
        registry.port_available = |_| true;
        registry.now = || 1_722_000_120;
        let (forward, _) = registry.create(&request()).unwrap();
        assert_eq!(forward.tunnel_opened_at, 1_722_000_120);

        let disabled = registry.set_enabled(&forward.id, false).unwrap().unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.tunnel_opened_at, 0);
        assert!(
            !registry
                .set_enabled(&forward.id, false)
                .unwrap()
                .unwrap()
                .enabled
        );
        registry.now = || 1_722_000_240;
        let enabled = registry.set_enabled(&forward.id, true).unwrap().unwrap();
        assert!(enabled.enabled);
        assert_eq!(enabled.tunnel_opened_at, 1_722_000_240);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "forward:5173:127.0.0.1:5173",
                "cancel:5173:127.0.0.1:5173",
                "forward:5173:127.0.0.1:5173"
            ]
        );
    }

    #[test]
    fn remaps_the_local_port_and_keeps_the_remote_endpoint() {
        let ssh = FakeSsh::default();
        let calls = ssh.calls.clone();
        let mut registry = Registry::new(ssh);
        registry.port_available = |_| true;
        let (forward, _) = registry.create(&request()).unwrap();

        let remapped = registry.set_local_port(&forward.id, 5180).unwrap().unwrap();
        assert_eq!(remapped.remote_port, 5173);
        assert_eq!(remapped.local_port, 5180);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "forward:5173:127.0.0.1:5173",
                "cancel:5173:127.0.0.1:5173",
                "forward:5180:127.0.0.1:5173"
            ]
        );
    }

    #[test]
    fn restores_the_original_mapping_when_remap_fails() {
        let ssh = FakeSsh {
            fail_forward_on: Some(5180),
            ..FakeSsh::default()
        };
        let calls = ssh.calls.clone();
        let mut registry = Registry::new(ssh);
        registry.port_available = |_| true;
        let (forward, _) = registry.create(&request()).unwrap();

        assert!(registry.set_local_port(&forward.id, 5180).is_err());
        assert_eq!(registry.forwards[&forward.id].local_port, 5173);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "forward:5173:127.0.0.1:5173",
                "cancel:5173:127.0.0.1:5173",
                "forward:5180:127.0.0.1:5173",
                "forward:5173:127.0.0.1:5173"
            ]
        );
    }

    #[test]
    fn disables_the_entry_when_remap_and_rollback_both_fail() {
        let mut registry = Registry::new(FailedRollbackSsh::default());
        registry.port_available = |_| true;
        let (forward, _) = registry.create(&request()).unwrap();

        let error = registry.set_local_port(&forward.id, 5180).unwrap_err();
        assert!(error.contains("forward is now disabled"));
        assert!(!registry.forwards[&forward.id].enabled);
        assert_eq!(registry.forwards[&forward.id].local_port, 5173);
        assert_eq!(registry.forwards[&forward.id].tunnel_opened_at, 0);
    }
}
