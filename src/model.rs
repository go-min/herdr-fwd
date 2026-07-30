use serde::{Deserialize, Serialize};

use crate::detect::find_urls;

pub const PROTOCOL_VERSION: u16 = 1;
pub const DEFAULT_PROCESS_TREE_DEPTH: u8 = 2;
pub const MAX_PROCESS_TREE_DEPTH: u8 = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ForwardRequest {
    pub remote_port: u16,
    #[serde(default)]
    pub preferred_local_port: u16,
    pub remote_host: String,
    pub pane_id: String,
    #[serde(default)]
    pub process: String,
    #[serde(default)]
    pub detected_url: String,
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub server_started_at: Option<u64>,
    #[serde(default)]
    pub process_id: Option<u32>,
}

impl ForwardRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.remote_port == 0 {
            return Err("remotePort must be in 1..=65535".into());
        }
        if !matches!(
            self.remote_host.as_str(),
            "localhost" | "127.0.0.1" | "::1" | "[::1]"
        ) {
            return Err("remoteHost must be localhost, 127.0.0.1, or ::1".into());
        }
        if self.pane_id.trim().is_empty() || self.pane_id.len() > 256 {
            return Err("paneId is required and must be at most 256 bytes".into());
        }
        if self.process.len() > 256 || self.detected_url.len() > 2048 {
            return Err("process or detectedUrl is too long".into());
        }
        let detected_url = self.detected_url.trim();
        let valid_url = find_urls(detected_url)
            .into_iter()
            .any(|candidate| candidate.url == detected_url && candidate.port == self.remote_port);
        if !valid_url {
            return Err("detectedUrl must be an HTTP(S) loopback URL for remotePort".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct RemoteSessionConfig {
    pub protocol_version: u16,
    pub session_id: String,
    pub token: String,
    pub rpc_url: String,
    pub auto_detect: bool,
}

impl RemoteSessionConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(format!(
                "unsupported forwarding protocol version {}; expected {PROTOCOL_VERSION}",
                self.protocol_version
            ));
        }
        if self.session_id.len() < 16
            || self.session_id.len() > 64
            || !self.session_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("sessionId must be 16..=64 hexadecimal characters".into());
        }
        if self.token.len() != 64 || !self.token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("session token must be a 256-bit hexadecimal value".into());
        }
        let authority = self
            .rpc_url
            .strip_prefix("http://")
            .ok_or_else(|| "RPC URL must use http://".to_string())?;
        let address = authority
            .parse::<std::net::SocketAddr>()
            .map_err(|_| "RPC URL must contain a socket address".to_string())?;
        if address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
            return Err("RPC address must be 127.0.0.1".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ForwardRequest, RemoteSessionConfig, PROTOCOL_VERSION};

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
    fn rejects_untrusted_hosts_and_empty_panes() {
        let mut invalid = request();
        invalid.remote_host = "0.0.0.0".into();
        assert!(invalid.validate().is_err());
        invalid.remote_host = "localhost".into();
        invalid.pane_id.clear();
        assert!(invalid.validate().is_err());

        let mut invalid = request();
        invalid.detected_url = "http://example.test:5173/".into();
        assert!(invalid.validate().is_err());
        invalid.detected_url = "http://localhost:3000/".into();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn validates_remote_session_protocol_and_loopback_rpc() {
        let mut config = RemoteSessionConfig {
            protocol_version: PROTOCOL_VERSION,
            session_id: "0123456789abcdef01234567".into(),
            token: "ab".repeat(32),
            rpc_url: "http://127.0.0.1:23000".into(),
            auto_detect: true,
        };
        assert!(config.validate().is_ok());
        config.protocol_version += 1;
        assert!(config.validate().is_err());
        config.protocol_version = PROTOCOL_VERSION;
        config.rpc_url = "http://192.0.2.1:23000".into();
        assert!(config.validate().is_err());
        config.rpc_url = "http://127.0.0.2:23000".into();
        assert!(config.validate().is_err());
    }
}
