/// Format token count with short suffixes (e.g., 1.2M, 45.3K).
pub fn format_tokens_short(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format token count with comma separators (e.g., 1,245,000).
pub fn format_tokens_comma(n: u64) -> String {
    if n >= 1_000_000 {
        format!(
            "{},{:03},{:03}",
            n / 1_000_000,
            (n / 1_000) % 1_000,
            n % 1_000
        )
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        n.to_string()
    }
}

/// Format a duration in seconds as a short human-readable string.
pub fn format_duration_short(secs: i64) -> String {
    if secs < 0 {
        return "0s".to_string();
    }
    let mins = secs / 60;
    let s = secs % 60;
    if mins > 0 {
        format!("{}m {}s", mins, s)
    } else {
        format!("{}s", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens_short() {
        assert_eq!(format_tokens_short(0), "0");
        assert_eq!(format_tokens_short(999), "999");
        assert_eq!(format_tokens_short(1_000), "1.0K");
        assert_eq!(format_tokens_short(45_300), "45.3K");
        assert_eq!(format_tokens_short(1_000_000), "1.0M");
        assert_eq!(format_tokens_short(2_500_000), "2.5M");
    }

    #[test]
    fn test_format_tokens_comma() {
        assert_eq!(format_tokens_comma(0), "0");
        assert_eq!(format_tokens_comma(999), "999");
        assert_eq!(format_tokens_comma(1_000), "1,000");
        assert_eq!(format_tokens_comma(1_245_000), "1,245,000");
    }

    #[test]
    fn test_format_duration_short() {
        assert_eq!(format_duration_short(-5), "0s");
        assert_eq!(format_duration_short(0), "0s");
        assert_eq!(format_duration_short(30), "30s");
        assert_eq!(format_duration_short(90), "1m 30s");
        assert_eq!(format_duration_short(3600), "60m 0s");
    }
}
