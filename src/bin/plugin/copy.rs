pub(crate) fn format_actionable_error(summary: &str, reason: &str, next: Option<&str>) -> String {
    let mut message = format!("{summary}\n{reason}");
    if let Some(next) = next {
        message.push_str("\n\n");
        message.push_str(next);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::format_actionable_error;

    #[test]
    fn formats_an_actionable_error_as_summary_reason_and_next_step() {
        assert_eq!(
            format_actionable_error(
                "Could not add the manual forward.",
                "No active session exists for example-host.",
                Some("Start one with:\n  hfwd example-host"),
            ),
            "Could not add the manual forward.\nNo active session exists for example-host.\n\nStart one with:\n  hfwd example-host"
        );
    }

    #[test]
    fn formats_an_actionable_error_without_a_next_step() {
        assert_eq!(
            format_actionable_error(
                "Could not open the forward.",
                "The local browser is unavailable.",
                None,
            ),
            "Could not open the forward.\nThe local browser is unavailable."
        );
    }
}
