//! Numbers and times as the detail shows them.

use jiff::{Timestamp, Zoned};
use syncfast_core::bisync_log::Progress;

const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

/// A size in decimal units: `512 B`, `1.8 GB`.
#[must_use]
#[allow(clippy::cast_precision_loss)] // sizes shown with one decimal
pub fn bytes(count: u64) -> String {
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A span of seconds, truncated: `4 s`, `3 min`, `1 h 5 min`.
#[must_use]
pub fn duration(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    if seconds < MINUTE {
        format!("{seconds} s")
    } else if seconds < HOUR {
        format!("{} min", seconds / MINUTE)
    } else {
        let minutes = (seconds % HOUR) / MINUTE;
        if minutes == 0 {
            format!("{} h", seconds / HOUR)
        } else {
            format!("{} h {minutes} min", seconds / HOUR)
        }
    }
}

/// Time left, rounded up to the minute so a wait never reads shorter than it is.
#[must_use]
pub fn eta(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    if seconds < MINUTE {
        duration(seconds)
    } else {
        duration(seconds.div_ceil(MINUTE) * MINUTE)
    }
}

/// `<n> of <total> files · <bytes>/<total bytes> · <speed>/s · <eta> left`,
/// without the parts rclone has not reported.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a speed is never negative or huge
pub fn progress_line(progress: &Progress) -> String {
    let line = format!(
        "{} of {} files · {}/{} · {}/s",
        progress.transfers,
        progress.total_transfers,
        bytes(progress.bytes),
        bytes(progress.total_bytes),
        bytes(progress.speed.max(0.0) as u64),
    );
    match progress.eta_seconds {
        Some(left) => format!("{line} · {} left", eta(left)),
        None => line,
    }
}

/// The fraction of files done, `None` when the total is not known yet.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // a ratio for a bar
pub fn progress_fraction(progress: &Progress) -> Option<f32> {
    (progress.total_transfers > 0)
        .then(|| (progress.transfers as f64 / progress.total_transfers as f64).min(1.0) as f32)
}

/// When a round started: `Today 14:02`, `Yesterday 13:55`, `3 Nov 13:40`,
/// and with the year when it is not this one.
#[must_use]
pub fn started(now: Timestamp, at: &Zoned) -> String {
    let today = now.to_zoned(at.time_zone().clone());
    let time = at.strftime("%H:%M");
    if at.date() == today.date() {
        format!("Today {time}")
    } else if Some(at.date()) == today.date().yesterday().ok() {
        format!("Yesterday {time}")
    } else if at.year() == today.year() {
        at.strftime("%-d %b %H:%M").to_string()
    } else {
        at.strftime("%-d %b %Y %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;

    use super::*;

    fn progress() -> Progress {
        Progress {
            transfers: 412,
            total_transfers: 980,
            bytes: 1_800_000_000,
            total_bytes: 4_300_000_000,
            speed: 12_400_000.0,
            eta_seconds: Some(170),
            current: None,
        }
    }

    #[test]
    fn sizes_use_decimal_units_with_one_decimal_above_a_byte() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1_000), "1.0 KB");
        assert_eq!(bytes(12_400_000), "12.4 MB");
        assert_eq!(bytes(4_300_000_000), "4.3 GB");
    }

    #[test]
    fn durations_are_truncated_and_only_the_eta_rounds_up() {
        assert_eq!(duration(4), "4 s");
        assert_eq!(duration(60), "1 min");
        assert_eq!(duration(119), "1 min");
        assert_eq!(duration(3_600), "1 h");
        assert_eq!(duration(3_900), "1 h 5 min");
        assert_eq!(eta(4), "4 s");
        assert_eq!(eta(61), "2 min");
        assert_eq!(eta(170), "3 min");
    }

    #[test]
    fn the_progress_line_matches_the_mock() {
        assert_eq!(
            progress_line(&progress()),
            "412 of 980 files · 1.8 GB/4.3 GB · 12.4 MB/s · 3 min left"
        );
    }

    #[test]
    fn the_progress_line_drops_the_eta_when_rclone_has_none() {
        let mut p = progress();
        p.eta_seconds = None;

        assert_eq!(
            progress_line(&p),
            "412 of 980 files · 1.8 GB/4.3 GB · 12.4 MB/s"
        );
    }

    #[test]
    fn the_fraction_is_unknown_until_there_is_a_total_and_never_above_one() {
        let mut p = progress();
        assert!((progress_fraction(&p).unwrap() - 0.42).abs() < 0.01);
        p.transfers = 2_000;
        assert_eq!(progress_fraction(&p), Some(1.0));
        p.total_transfers = 0;
        assert_eq!(progress_fraction(&p), None);
    }

    fn at(text: &str) -> Zoned {
        text.parse::<Timestamp>().unwrap().to_zoned(TimeZone::UTC)
    }

    #[test]
    fn a_start_reads_today_yesterday_a_date_or_a_date_with_the_year() {
        let now: Timestamp = "2026-11-03T14:30:00Z".parse().unwrap();

        assert_eq!(started(now, &at("2026-11-03T14:02:00Z")), "Today 14:02");
        assert_eq!(started(now, &at("2026-11-02T13:55:00Z")), "Yesterday 13:55");
        assert_eq!(started(now, &at("2026-10-30T13:40:00Z")), "30 Oct 13:40");
        assert_eq!(
            started(now, &at("2025-12-30T13:40:00Z")),
            "30 Dec 2025 13:40"
        );
    }
}
