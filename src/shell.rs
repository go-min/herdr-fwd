/// Quotes one value for the remote POSIX shell commands this project emits.
///
/// The call sites still pass the result as a single SSH argument; this helper
/// only protects values interpolated into that remote command string.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn quotes_single_quotes_without_opening_the_shell() {
        assert_eq!(quote("a' b"), "'a'\\'' b'");
    }
}
