use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use herdr_fwd::{registry::Forward, RemoteSessionConfig};
use serde::Deserialize;
use serde_json::Value;

const RPC_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RPC_RESPONSE_SIZE: u64 = 1024 * 1024;

pub(crate) fn list_forwards(config: &RemoteSessionConfig) -> Result<Vec<Forward>, String> {
    api_request(config, "GET", "/v1/forwards", None)
}

pub(crate) fn api_request<T: for<'de> Deserialize<'de>>(
    config: &RemoteSessionConfig,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<T, String> {
    let authority = config
        .rpc_url
        .strip_prefix("http://")
        .ok_or_else(|| "RPC URL must use http://".to_string())?;
    let address: SocketAddr = authority
        .parse()
        .map_err(|_| "invalid RPC address".to_string())?;
    if !address.ip().is_loopback() {
        return Err("RPC address must be loopback".into());
    }
    let mut stream = TcpStream::connect_timeout(&address, RPC_TIMEOUT)
        .map_err(|error| format!("RPC connect failed: {error}"))?;
    stream.set_read_timeout(Some(RPC_TIMEOUT)).ok();
    stream.set_write_timeout(Some(RPC_TIMEOUT)).ok();
    let body = body
        .map(|body| serde_json::to_vec(&body).map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        config.token,
        body.len()
    )
    .and_then(|_| stream.write_all(&body))
    .map_err(|error| format!("RPC write failed: {error}"))?;
    let mut response = Vec::new();
    stream
        .take(MAX_RPC_RESPONSE_SIZE + 1)
        .read_to_end(&mut response)
        .map_err(|error| format!("RPC read failed: {error}"))?;
    if response.len() as u64 > MAX_RPC_RESPONSE_SIZE {
        return Err("RPC response is too large".into());
    }
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "invalid RPC response".to_string())?;
    let headers = String::from_utf8_lossy(&response[..separator]);
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| "invalid RPC status".to_string())?;
    let response_body = &response[separator + 4..];
    if !(200..300).contains(&status) {
        return Err(format!(
            "RPC returned {status}: {}",
            String::from_utf8_lossy(response_body)
        ));
    }
    serde_json::from_slice(response_body).map_err(|error| format!("invalid RPC JSON: {error}"))
}
