pub(crate) fn format_actionable_error(summary: &str, reason: &str, next: Option<&str>) -> String {
    let mut message = format!("{summary}\n{reason}");
    if let Some(next) = next {
        message.push_str("\n\n");
        message.push_str(next);
    }
    message
}
