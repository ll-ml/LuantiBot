pub(super) fn clip_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}
