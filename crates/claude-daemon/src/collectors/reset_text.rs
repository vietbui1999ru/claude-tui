//! Parse "Resets in …" and "Try again in ~…" text from provider error messages.
//!
//! A port of `parseResetDuration` from the Pi side-panel extension
//! (`pi-side-panel/status-pane.mjs`): every `<n> <unit>` pair is summed, where
//! a day is 24 hours, `h`/`hr`/`hour` is an hour, and anything else (`m`,
//! `min`, `minute`) is a minute.

use chrono::Duration;

/// Total duration named in `text`, or `None` when it names none.
pub fn parse_reset_duration(text: &str) -> Option<Duration> {
    let mut total_minutes: i64 = 0;
    let mut found = false;
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();

    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let amount: i64 = match lower[start..i].parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        // Skip whitespace between the number and its unit.
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let unit_start = j;
        while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
            j += 1;
        }
        let unit = &lower[unit_start..j];
        let minutes_per_unit = match unit_minutes(unit) {
            Some(m) => m,
            None => continue,
        };
        total_minutes += amount * minutes_per_unit;
        found = true;
        i = j;
    }

    if found && total_minutes > 0 {
        Some(Duration::minutes(total_minutes))
    } else {
        None
    }
}

/// Minutes in one unit, or `None` when the word is not a duration unit.
/// Matches the JS regex alternatives `days?|hours?|hrs?|hr|minutes?|mins?|min`.
fn unit_minutes(unit: &str) -> Option<i64> {
    match unit {
        "day" | "days" => Some(24 * 60),
        "hour" | "hours" | "hr" | "hrs" => Some(60),
        "minute" | "minutes" | "min" | "mins" => Some(1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_text_parses_hours_and_minutes() {
        assert_eq!(
            parse_reset_duration("2 hours 30 minutes"),
            Some(Duration::minutes(150))
        );
        assert_eq!(
            parse_reset_duration("Resets in 45 min"),
            Some(Duration::minutes(45))
        );
    }

    #[test]
    fn reset_text_day_is_twenty_four_hours() {
        assert_eq!(
            parse_reset_duration("1 day"),
            Some(Duration::minutes(24 * 60))
        );
        assert_eq!(
            parse_reset_duration("2 days 1 hr"),
            Some(Duration::minutes(49 * 60))
        );
    }

    #[test]
    fn reset_text_unparseable_is_none() {
        assert_eq!(parse_reset_duration("soon"), None);
        assert_eq!(parse_reset_duration(""), None);
        assert_eq!(parse_reset_duration("0 minutes"), None);
    }

    #[test]
    fn reset_text_tilde_prefix_still_parses() {
        assert_eq!(parse_reset_duration("~14 min"), Some(Duration::minutes(14)));
    }
}
