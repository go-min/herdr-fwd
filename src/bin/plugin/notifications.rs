use herdr_fwd::registry::Forward;

use crate::plugin::herdr::herdr_output;

pub(crate) fn notify_changes(previous: &[Forward], current: &[Forward]) {
    for forward in current {
        if !previous.iter().any(|old| old.id == forward.id) {
            let _ = herdr_output(&[
                "notification",
                "show",
                "Port forwarded",
                "--body",
                &format!(
                    "remote {}:{} → localhost:{}",
                    forward.remote_host_display(),
                    forward.remote_port,
                    forward.local_port
                ),
            ]);
        }
    }
    for forward in previous {
        if !current.iter().any(|new| new.id == forward.id) {
            let _ = herdr_output(&[
                "notification",
                "show",
                "Port forward removed",
                "--body",
                &format!(
                    "localhost:{} → remote {}",
                    forward.local_port, forward.remote_port
                ),
            ]);
        }
    }
    for forward in current {
        if let Some(old) = previous.iter().find(|old| old.id == forward.id) {
            if old.enabled != forward.enabled {
                let _ = herdr_output(&[
                    "notification",
                    "show",
                    if forward.enabled {
                        "Port forwarding enabled"
                    } else {
                        "Port forwarding paused"
                    },
                    "--body",
                    &format!(
                        "remote {}:{} → localhost:{}",
                        forward.remote_host_display(),
                        forward.remote_port,
                        forward.local_port
                    ),
                ]);
            }
        }
    }
}
