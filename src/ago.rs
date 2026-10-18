//! Relative "10 min ago" labels.

use chrono::{DateTime, Utc};

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
}
