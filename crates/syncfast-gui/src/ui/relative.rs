//! Relative times such as "5 min ago", for the sync list.

use jiff::Timestamp;

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
/// Anything under this is "just now": rounds end within seconds of each other.
const JUST_NOW: i64 = 45;

/// How long ago `then` was, as of `now`. A time in the future (the clock
/// moved) counts as just now.
#[must_use]
pub fn ago(now: Timestamp, then: Timestamp) -> String {
    let seconds = now.as_second() - then.as_second();
    if seconds < JUST_NOW {
        "just now".to_owned()
    } else if seconds < HOUR {
        format!("{} min ago", (seconds / MINUTE).max(1))
    } else if seconds < DAY {
        format!("{} h ago", seconds / HOUR)
    } else if seconds < 2 * DAY {
        "1 day ago".to_owned()
    } else {
        format!("{} days ago", seconds / DAY)
    }
}

/// The line under a sync's name: `Synced <time>` or `Never synced`.
#[must_use]
pub fn synced(now: Timestamp, last_success: Option<Timestamp>) -> String {
    match last_success {
        Some(at) => format!("Synced {}", ago(now, at)),
        None => "Never synced".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    const NOW: &str = "2026-11-03T12:00:00Z";

    #[test]
    fn seconds_old_is_just_now_and_so_is_the_future() {
        assert_eq!(ago(at(NOW), at("2026-11-03T11:59:30Z")), "just now");
        assert_eq!(ago(at(NOW), at("2026-11-03T12:05:00Z")), "just now");
    }

    #[test]
    fn minutes_are_whole_and_at_least_one() {
        assert_eq!(ago(at(NOW), at("2026-11-03T11:59:00Z")), "1 min ago");
        assert_eq!(ago(at(NOW), at("2026-11-03T11:55:10Z")), "4 min ago");
        assert_eq!(ago(at(NOW), at("2026-11-03T11:00:01Z")), "59 min ago");
    }

    #[test]
    fn hours_and_days_are_whole_units() {
        assert_eq!(ago(at(NOW), at("2026-11-03T10:00:00Z")), "2 h ago");
        assert_eq!(ago(at(NOW), at("2026-11-02T11:00:00Z")), "1 day ago");
        assert_eq!(ago(at(NOW), at("2026-10-31T12:00:00Z")), "3 days ago");
    }

    #[test]
    fn line_under_the_name_says_never_when_there_is_no_success() {
        assert_eq!(synced(at(NOW), None), "Never synced");
        assert_eq!(
            synced(at(NOW), Some(at("2026-11-03T10:00:00Z"))),
            "Synced 2 h ago"
        );
    }
}
