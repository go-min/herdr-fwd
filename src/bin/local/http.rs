//! The companion uses one Content-Length framed request per connection. Owning
//! the sockets lets shutdown interrupt even incomplete headers and bodies.
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const MAX_HEADER_SIZE: usize = 16 * 1024;
const MAX_BODY_SIZE: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Server(TcpListener);

impl Server {
    pub(crate) fn from_listener(listener: TcpListener) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        Ok(Self(listener))
    }

    pub(crate) fn recv_timeout(&self, timeout: Duration) -> io::Result<Option<TcpStream>> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.0.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                    return Ok(Some(stream));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Post,
    Delete,
    Other,
}

pub(crate) struct Request {
    stream: TcpStream,
    method: Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    pub(crate) fn read(mut stream: TcpStream, stop: &AtomicBool) -> Result<Self, String> {
        let parsed = read_parts(&mut stream, stop, Instant::now() + REQUEST_TIMEOUT);
        match parsed {
            Ok(ParsedRequest {
                method,
                path,
                headers,
                body,
            }) => Ok(Self {
                stream,
                method,
                path,
                headers,
                body,
            }),
            Err(error) => {
                if !stop.load(Ordering::SeqCst) {
                    let _ = respond(
                        &mut stream,
                        400,
                        &serde_json::json!({"error":error}).to_string(),
                    );
                }
                Err(error)
            }
        }
    }

    pub(crate) fn method(&self) -> Method {
        self.method
    }

    pub(crate) fn body(&self) -> &[u8] {
        &self.body
    }

    pub(crate) fn url(&self) -> &str {
        &self.path
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn respond(mut self, status: u16, body: &str) -> io::Result<()> {
        respond(&mut self.stream, status, body)
    }
}

struct ParsedRequest {
    method: Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn read_parts(
    stream: &mut TcpStream,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<ParsedRequest, String> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            if index > MAX_HEADER_SIZE {
                return Err("request headers are too large".into());
            }
            break index + 4;
        }
        if bytes.len() > MAX_HEADER_SIZE {
            return Err("request headers are too large".into());
        }
        read_more(stream, &mut bytes, stop, deadline)?;
    };
    let text = std::str::from_utf8(&bytes[..header_end]).map_err(|_| "invalid request headers")?;
    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .unwrap_or_default()
        .split(' ')
        .collect::<Vec<_>>();
    let [method, path, version] = request_line.as_slice() else {
        return Err("invalid request line".into());
    };
    if !matches!(*version, "HTTP/1.0" | "HTTP/1.1")
        || !path.starts_with('/')
        || path.chars().any(char::is_control)
    {
        return Err("invalid request line".into());
    }
    let method = match *method {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "DELETE" => Method::Delete,
        _ => Method::Other,
    };
    let path = path.to_string();
    let mut headers = Vec::new();
    for line in lines.take_while(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').ok_or("invalid header")?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value.chars().any(|c| c.is_control() && c != '\t')
        {
            return Err("invalid header".into());
        }
        if [
            "Content-Length",
            "Transfer-Encoding",
            "Authorization",
            "Expect",
        ]
        .iter()
        .any(|name| key.eq_ignore_ascii_case(name))
            && headers
                .iter()
                .any(|(name, _): &(String, String)| name.eq_ignore_ascii_case(key))
        {
            return Err("duplicate framing or authorization header".into());
        }
        headers.push((key.to_string(), value.trim().to_string()));
    }
    if headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("Transfer-Encoding"))
    {
        return Err("use Content-Length; transfer encoding is unsupported".into());
    }
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
        .map(|(_, value)| {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("invalid Content-Length");
            }
            value.parse::<usize>().map_err(|_| "invalid Content-Length")
        })
        .transpose()?
        .unwrap_or(0);
    if length > MAX_BODY_SIZE {
        return Err("request body is too large".into());
    }
    if let Some((_, value)) = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Expect"))
    {
        if !value.eq_ignore_ascii_case("100-continue") {
            return Err("unsupported expectation".into());
        }
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .map_err(|e| e.to_string())?;
    }
    while bytes.len() - header_end < length {
        read_more(stream, &mut bytes, stop, deadline)?;
    }
    if stop.load(Ordering::SeqCst) {
        return Err("companion is stopping".into());
    }
    Ok(ParsedRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + length].to_vec(),
    })
}

fn read_more(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<(), String> {
    let mut buffer = [0; 4096];
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err("companion is stopping".into());
        }
        if Instant::now() >= deadline {
            return Err("request timed out".into());
        }
        match stream.read(&mut buffer) {
            Ok(0) => return Err("incomplete request".into()),
            Ok(count) => {
                bytes.extend_from_slice(&buffer[..count]);
                return Ok(());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
}

pub(crate) fn respond(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    write!(stream, "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &[u8], timeout: Duration) -> Result<ParsedRequest, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        client.write_all(raw).unwrap();
        read_parts(
            &mut server,
            &AtomicBool::new(false),
            Instant::now() + timeout,
        )
    }

    #[test]
    fn rejects_ambiguous_and_oversized_framing() {
        for headers in [
            "Content-Length: 1\r\nContent-Length: 2",
            "Transfer-Encoding: chunked",
            "Authorization: Bearer a\r\nAuthorization: Bearer b",
            "Content-Length: 65537",
            "Content-Length: +1",
            " Bad: folded",
        ] {
            assert!(parse(
                format!("POST / HTTP/1.1\r\n{headers}\r\n\r\n").as_bytes(),
                Duration::from_secs(1)
            )
            .is_err());
        }
    }

    #[test]
    fn incomplete_headers_and_body_obey_absolute_deadline() {
        for raw in [
            b"GET / HTTP/1.1\r\nHost:".as_slice(),
            b"POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\n{",
        ] {
            let started = Instant::now();
            let error = parse(raw, Duration::from_millis(80)).err().unwrap();
            assert!(error.contains("timed out"));
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }

    #[test]
    fn parses_only_declared_body_without_accepting_a_second_request() {
        let raw = concat!(
            "POST /v1/heartbeat HTTP/1.1\r\n",
            "authorization: Bearer test\r\n",
            "Content-Length: 2\r\n\r\n",
            "{}GET / HTTP/1.1\r\n\r\n",
        );
        let request = parse(raw.as_bytes(), Duration::from_secs(1)).unwrap();
        assert_eq!(request.method, Method::Post);
        assert_eq!(request.path, "/v1/heartbeat");
        assert_eq!(request.headers[0].1, "Bearer test");
        assert_eq!(request.body, b"{}");
    }
}
