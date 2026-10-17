//! Relative "10 min ago" labels.

use chrono::{DateTime, Utc};

/// Label for how long ago `ts` was, relative to `now`.
/// Future timestamps (clock skew) read as "just now".
pub fn label(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - ts).num_seconds();
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins} min ago");
    }
    let hrs = mins / 60;
    if hrs < 24 {
        return format!("{hrs} hr ago");
    }
    if hrs < 48 {
        return "yesterday".to_string();
    }
    format!("{} days ago", hrs / 24)
}

/// Compact age: "18s", "14m", "3h", "2d". Future timestamps read "0s".
pub fn short(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - ts).num_seconds().max(0);
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hrs = mins / 60;
    if hrs < 24 {
        return format!("{hrs}h");
    }
    format!("{}d", hrs / 24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn at(secs_ago: i64) -> String {
        let now = DateTime::parse_from_rfc3339("2026-10-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        label(now - Duration::seconds(secs_ago), now)
    }

    #[test]
    fn boundaries() {
        assert_eq!(at(0), "just now");
        assert_eq!(at(59), "just now");
        assert_eq!(at(60), "1 min ago");
        assert_eq!(at(59 * 60 + 59), "59 min ago");
        assert_eq!(at(60 * 60), "1 hr ago");
        assert_eq!(at(23 * 3600 + 3599), "23 hr ago");
        assert_eq!(at(24 * 3600), "yesterday");
        assert_eq!(at(47 * 3600 + 3599), "yesterday");
        assert_eq!(at(48 * 3600), "2 days ago");
        assert_eq!(at(10 * 24 * 3600), "10 days ago");
    }

    #[test]
    fn short_boundaries() {
        let s = |secs: i64| {
            let now = DateTime::parse_from_rfc3339("2026-10-14T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc);
            short(now - Duration::seconds(secs), now)
        };
        assert_eq!(s(0), "0s");
        assert_eq!(s(59), "59s");
        assert_eq!(s(60), "1m");
        assert_eq!(s(59 * 60 + 59), "59m");
        assert_eq!(s(3600), "1h");
        assert_eq!(s(23 * 3600 + 59 * 60), "23h");
        assert_eq!(s(24 * 3600), "1d");
        assert_eq!(s(-300), "0s");
    }

    #[test]
    fn future_is_just_now() {
        assert_eq!(at(-300), "just now");
    }
}
