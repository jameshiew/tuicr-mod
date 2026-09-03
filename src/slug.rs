//! Small helpers shared by the export and summary-popup renderers.

const SHORT_SHA_LEN: usize = 7;

/// Take the first `SHORT_SHA_LEN` chars of a SHA. Shorter inputs pass through.
pub fn short_sha(sha: &str) -> String {
    sha.chars().take(SHORT_SHA_LEN).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_truncate_to_short_sha_length() {
        assert_eq!(short_sha("abcdef1234567890"), "abcdef1");
        assert_eq!(short_sha("abc"), "abc");
    }
}
