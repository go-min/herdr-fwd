use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub host: String,
    pub port: u16,
    pub url: String,
}

pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            let character = input[index..].chars().next().expect("valid UTF-8 boundary");
            out.push(character);
            index += character.len_utf8();
            continue;
        }

        index += 1;
        if index >= bytes.len() {
            break;
        }
        match bytes[index] {
            b'[' => {
                index += 1;
                while index < bytes.len() && !(0x40..=0x7e).contains(&bytes[index]) {
                    index += 1;
                }
                index = index.saturating_add(1).min(bytes.len());
            }
            b']' | b'P' | b'^' | b'_' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 7 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'\\'
                    {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    out
}

pub fn sanitize_display_text(input: &str) -> String {
    strip_ansi(input)
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

pub fn find_urls(input: &str) -> Vec<Candidate> {
    let clean = strip_ansi(input);
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for token in clean.split_whitespace() {
        let token = token.trim_matches(|character: char| ".,;!()[]{}<>\"'`".contains(character));
        let Some(scheme_end) = token.find("://") else {
            continue;
        };
        let scheme = &token[..scheme_end];
        if !matches!(scheme, "http" | "https") {
            continue;
        }
        let rest = &token[scheme_end + 3..];
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let Some((host, raw_port)) = split_authority(authority) else {
            continue;
        };
        if !matches!(host, "localhost" | "127.0.0.1" | "::1") {
            continue;
        }
        let Ok(port) = raw_port.parse::<u16>() else {
            continue;
        };
        if port == 0 || !seen.insert((host.to_string(), port)) {
            continue;
        }
        candidates.push(Candidate {
            host: host.to_string(),
            port,
            url: token.to_string(),
        });
    }
    candidates.sort_by_key(|candidate| candidate.port);
    candidates
}

fn split_authority(authority: &str) -> Option<(&str, &str)> {
    if let Some(authority) = authority.strip_prefix('[') {
        authority.split_once("]:")
    } else {
        authority.rsplit_once(':')
    }
}

#[cfg(test)]
mod tests {
    use super::{find_urls, sanitize_display_text, strip_ansi};

    #[test]
    fn strips_ansi_and_extracts_only_explicit_loopback_ports() {
        let output = "\x1b[32mLocal: http://localhost:5173/\x1b[0m https://127.0.0.1:8443 http://[::1]:3000\nhttp://example.test:4 http://localhost";
        assert_eq!(strip_ansi("\x1b[31mX\x1b[0m"), "X");
        let candidates = find_urls(output);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].port, 3000);
        assert_eq!(candidates[1].port, 5173);
        assert_eq!(candidates[2].port, 8443);
    }

    #[test]
    fn removes_terminal_control_sequences_from_display_text() {
        assert_eq!(
            sanitize_display_text("\x1b]8;;https://evil.test\x07Node\x1b]8;;\x07\r\n\x08"),
            "Node"
        );
    }
}
