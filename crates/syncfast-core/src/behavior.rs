//! Every tunable of a sync, resolved in one place: the built-in value, then
//! `[defaults]`, then the sync's own override.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Pair;
use crate::notify::Policy;

macro_rules! named {
    ($ty:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $ty {
            /// The spellings `config.toml` and the CLI accept, in display order.
            pub const NAMES: &'static [&'static str] = &[$($name),+];

            #[must_use]
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }

            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictResolve {
    None,
    Newer,
    Older,
    Larger,
    Smaller,
    Local,
    Remote,
}

named!(ConflictResolve {
    None => "none",
    Newer => "newer",
    Older => "older",
    Larger => "larger",
    Smaller => "smaller",
    Local => "local",
    Remote => "remote",
});

impl ConflictResolve {
    /// Value for `--conflict-resolve`. Local is Path1, remote is Path2.
    #[must_use]
    pub fn rclone_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Newer => "newer",
            Self::Older => "older",
            Self::Larger => "larger",
            Self::Smaller => "smaller",
            Self::Local => "path1",
            Self::Remote => "path2",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictLoser {
    Num,
    Pathname,
    Delete,
}

named!(ConflictLoser {
    Num => "num",
    Pathname => "pathname",
    Delete => "delete",
});

impl ConflictLoser {
    /// Value for `--conflict-loser`.
    #[must_use]
    pub fn rclone_value(self) -> &'static str {
        match self {
            Self::Num => "num",
            Self::Pathname => "pathname",
            Self::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleDocs {
    Skip,
    Export,
    Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncMode {
    Newer,
    Older,
    Larger,
    Smaller,
    Local,
    Remote,
}

named!(ResyncMode {
    Newer => "newer",
    Older => "older",
    Larger => "larger",
    Smaller => "smaller",
    Local => "local",
    Remote => "remote",
});

impl ResyncMode {
    /// Value for `--resync-mode`. Local is Path1, remote is Path2.
    #[must_use]
    pub fn rclone_value(self) -> &'static str {
        match self {
            Self::Newer => "newer",
            Self::Older => "older",
            Self::Larger => "larger",
            Self::Smaller => "smaller",
            Self::Local => "path1",
            Self::Remote => "path2",
        }
    }
}

/// How much of a review syncfast confirms by itself, safest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewLevel {
    Manual,
    Deletions,
    Automatic,
}

named!(ReviewLevel {
    Manual => "manual",
    Deletions => "deletions",
    Automatic => "automatic",
});

impl ReviewLevel {
    /// Whether `self` asks the user less often than `other`.
    #[must_use]
    pub fn is_less_safe_than(self, other: Self) -> bool {
        self.safety() < other.safety()
    }

    fn safety(self) -> u8 {
        match self {
            Self::Manual => 2,
            Self::Deletions => 1,
            Self::Automatic => 0,
        }
    }
}

/// A speed limit in one direction of a sync's rounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    Off,
    KiBPerSec(u64),
}

impl Bandwidth {
    /// `off`, or a whole positive number followed by `K`, `M` or `G` (KiB,
    /// MiB, GiB per second; lowercase is accepted).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text == "off" {
            return Some(Self::Off);
        }
        let unit = text.chars().next_back()?;
        let shift = match unit.to_ascii_lowercase() {
            'k' => 0,
            'm' => 10,
            'g' => 20,
            _ => return None,
        };
        let digits = &text[..text.len() - unit.len_utf8()];
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let amount: u64 = digits.parse().ok().filter(|amount| *amount > 0)?;
        amount.checked_mul(1 << shift).map(Self::KiBPerSec)
    }

    /// The form `config.toml` stores and `--bwlimit` takes: the largest unit
    /// that divides the speed exactly, so `1024K` is `1M`.
    #[must_use]
    pub fn format(self) -> String {
        match self {
            Self::Off => "off".to_owned(),
            Self::KiBPerSec(kib) if kib.is_multiple_of(1 << 20) => format!("{}G", kib >> 20),
            Self::KiBPerSec(kib) if kib.is_multiple_of(1 << 10) => format!("{}M", kib >> 10),
            Self::KiBPerSec(kib) => format!("{kib}K"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behavior {
    pub watch: bool,
    pub debounce: Duration,
    pub max_wait: Duration,
    pub interval: Duration,
    pub limit_upload: Bandwidth,
    pub limit_download: Bandwidth,
    pub conflict_resolve: ConflictResolve,
    pub conflict_loser: ConflictLoser,
    pub versions: bool,
    pub versions_keep_days: u32,
    pub google_docs: GoogleDocs,
    /// Percentage for `--max-delete`.
    pub max_delete: u8,
    pub resync_mode: ResyncMode,
    pub empty_dirs: bool,
    pub review: ReviewLevel,
    /// App-wide: which notifications a round may send.
    pub notifications: Policy,
    /// App-wide: what the window and its autostart use.
    pub app: AppOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppOptions {
    pub autostart: bool,
    /// `system`, `light`, `dark` or a file of the user's themes folder.
    pub theme: String,
}

impl Default for AppOptions {
    fn default() -> Self {
        Self {
            autostart: false,
            theme: "system".to_owned(),
        }
    }
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            watch: true,
            debounce: Duration::from_secs(30),
            max_wait: Duration::from_mins(5),
            interval: Duration::from_mins(5),
            limit_upload: Bandwidth::Off,
            limit_download: Bandwidth::Off,
            conflict_resolve: ConflictResolve::Newer,
            conflict_loser: ConflictLoser::Num,
            versions: true,
            versions_keep_days: 30,
            google_docs: GoogleDocs::Skip,
            max_delete: 50,
            resync_mode: ResyncMode::Newer,
            empty_dirs: true,
            review: ReviewLevel::Deletions,
            notifications: Policy::default(),
            app: AppOptions::default(),
        }
    }
}

impl Behavior {
    /// `defaults` with the sync's own overrides on top.
    #[must_use]
    pub fn for_pair(defaults: &Self, pair: &Pair) -> Self {
        let mut behavior = defaults.clone();
        pair.overrides.apply_to(&mut behavior);
        behavior
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid duration \"{0}\": use a number followed by s, m or h, for example 30s, 5m or 1h30m"
)]
pub struct InvalidDuration(pub String);

/// One or more `<integer><s|m|h>` pairs with no spaces: `30s`, `5m`,
/// `1h30m`.
///
/// # Errors
/// The text has any other form, or the total does not fit.
pub fn parse_duration(text: &str) -> Result<Duration, InvalidDuration> {
    let invalid = || InvalidDuration(text.to_owned());
    if text.is_empty() {
        return Err(invalid());
    }
    let mut seconds: u64 = 0;
    let mut rest = text;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let (number, tail) = rest.split_at(digits);
        let mut chars = tail.chars();
        let unit: u64 = match chars.next() {
            Some('s') => 1,
            Some('m') => 60,
            Some('h') => 3600,
            _ => return Err(invalid()),
        };
        let amount: u64 = number.parse().map_err(|_| invalid())?;
        seconds = amount
            .checked_mul(unit)
            .and_then(|part| seconds.checked_add(part))
            .ok_or_else(invalid)?;
        rest = chars.as_str();
    }
    Ok(Duration::from_secs(seconds))
}

/// The form `config.toml` stores: the largest unit that divides the
/// duration exactly, so `300s` is `5m`.
#[must_use]
pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds != 0 && seconds.is_multiple_of(3600) {
        format!("{}h", seconds / 3600)
    } else if seconds != 0 && seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_the_phase_one_values() {
        let behavior = Behavior::default();

        assert!(behavior.watch);
        assert_eq!(behavior.debounce, Duration::from_secs(30));
        assert_eq!(behavior.max_wait, Duration::from_secs(300));
        assert_eq!(behavior.interval, Duration::from_secs(300));
        assert_eq!(behavior.limit_upload, Bandwidth::Off);
        assert_eq!(behavior.limit_download, Bandwidth::Off);
        assert_eq!(behavior.conflict_resolve, ConflictResolve::Newer);
        assert_eq!(behavior.conflict_loser, ConflictLoser::Num);
        assert!(behavior.versions);
        assert_eq!(behavior.versions_keep_days, 30);
        assert_eq!(behavior.google_docs, GoogleDocs::Skip);
        assert_eq!(behavior.max_delete, 50);
        assert_eq!(behavior.resync_mode, ResyncMode::Newer);
        assert!(behavior.empty_dirs);
    }

    fn config(text: &str) -> crate::config::Config {
        crate::config::Config::parse(text, std::path::Path::new("/home/ada")).unwrap()
    }

    const TWO_SYNCS: &str = r#"
version = 2

[defaults]
debounce = "10s"
max_delete = 40

[[pair]]
id = "docs"
name = "Docs"
local = "/home/ada/Docs"
remote = "gdrive:Docs"
debounce = "1m"
versions = false

[[pair]]
id = "music"
name = "Music"
local = "/home/ada/Music"
remote = "gdrive:Music"
"#;

    #[test]
    fn for_pair_uses_the_sync_value_then_defaults_then_builtin() {
        let config = config(TWO_SYNCS);
        let defaults = config.defaults();

        let docs = Behavior::for_pair(&defaults, &config.pairs()[0]);
        let music = Behavior::for_pair(&defaults, &config.pairs()[1]);

        assert_eq!(docs.debounce, Duration::from_secs(60));
        assert_eq!(docs.max_delete, 40);
        assert!(!docs.versions);
        assert_eq!(music.debounce, Duration::from_secs(10));
        assert_eq!(music.max_delete, 40);
        assert!(music.versions);
        assert_eq!(music.interval, Behavior::default().interval);
    }

    #[test]
    fn origins_name_where_each_value_came_from() {
        use crate::settings::Origin;
        let config = config(TWO_SYNCS);

        let docs = config.effective(&config.pairs()[0]);

        assert_eq!(docs.origin("debounce"), Origin::Sync);
        assert_eq!(docs.origin("versions"), Origin::Sync);
        assert_eq!(docs.origin("max_delete"), Origin::Defaults);
        assert_eq!(docs.origin("interval"), Origin::Builtin);
    }

    #[test]
    fn durations_parse_one_or_more_number_and_unit_pairs() {
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Ok(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_duration("1h30m"), Ok(Duration::from_mins(90)));
        assert_eq!(parse_duration("0s"), Ok(Duration::ZERO));
    }

    #[test]
    fn durations_with_any_other_shape_are_rejected_by_name() {
        for text in ["", "90", "5 min", "m", "5x", "-5s", "1.5h", "5m 30s"] {
            let err = parse_duration(text).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "invalid duration \"{text}\": use a number followed by s, m or h, for example 30s, 5m or 1h30m"
                )
            );
        }
    }

    #[test]
    fn durations_are_written_in_the_largest_exact_unit() {
        assert_eq!(format_duration(Duration::from_secs(300)), "5m");
        assert_eq!(format_duration(Duration::from_secs(3600)), "1h");
        assert_eq!(format_duration(Duration::from_mins(90)), "90m");
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
        assert_eq!(format_duration(Duration::ZERO), "0s");
    }

    #[test]
    fn conflict_resolve_maps_local_and_remote_to_path1_and_path2() {
        let values: Vec<_> = [
            ConflictResolve::None,
            ConflictResolve::Newer,
            ConflictResolve::Older,
            ConflictResolve::Larger,
            ConflictResolve::Smaller,
            ConflictResolve::Local,
            ConflictResolve::Remote,
        ]
        .map(ConflictResolve::rclone_value)
        .to_vec();

        assert_eq!(
            values,
            [
                "none", "newer", "older", "larger", "smaller", "path1", "path2"
            ]
        );
    }

    #[test]
    fn resync_mode_maps_local_and_remote_to_path1_and_path2() {
        let values: Vec<_> = [
            ResyncMode::Newer,
            ResyncMode::Older,
            ResyncMode::Larger,
            ResyncMode::Smaller,
            ResyncMode::Local,
            ResyncMode::Remote,
        ]
        .map(ResyncMode::rclone_value)
        .to_vec();

        assert_eq!(
            values,
            ["newer", "older", "larger", "smaller", "path1", "path2"]
        );
    }

    #[test]
    fn conflict_loser_maps_to_the_rclone_names() {
        let values: Vec<_> = [
            ConflictLoser::Num,
            ConflictLoser::Pathname,
            ConflictLoser::Delete,
        ]
        .map(ConflictLoser::rclone_value)
        .to_vec();

        assert_eq!(values, ["num", "pathname", "delete"]);
    }

    #[test]
    fn a_sync_off_overrides_a_defaults_limit() {
        let config = config(
            r#"
version = 2

[defaults]
limit_upload = "2M"
limit_download = "500K"

[[pair]]
id = "docs"
name = "Docs"
local = "/home/ada/Docs"
remote = "gdrive:Docs"
limit_upload = "off"

[[pair]]
id = "music"
name = "Music"
local = "/home/ada/Music"
remote = "gdrive:Music"
"#,
        );
        let defaults = config.defaults();

        let docs = Behavior::for_pair(&defaults, &config.pairs()[0]);
        let music = Behavior::for_pair(&defaults, &config.pairs()[1]);

        assert_eq!(docs.limit_upload, Bandwidth::Off);
        assert_eq!(docs.limit_download, Bandwidth::KiBPerSec(500));
        assert_eq!(music.limit_upload, Bandwidth::KiBPerSec(2048));
        assert_eq!(music.limit_download, Bandwidth::KiBPerSec(500));
    }

    #[test]
    fn a_speed_is_off_or_a_whole_number_with_k_m_or_g() {
        for (text, expected) in [
            ("off", Bandwidth::Off),
            ("500K", Bandwidth::KiBPerSec(500)),
            ("2M", Bandwidth::KiBPerSec(2048)),
            ("1G", Bandwidth::KiBPerSec(1_048_576)),
            ("2m", Bandwidth::KiBPerSec(2048)),
            ("1g", Bandwidth::KiBPerSec(1_048_576)),
            ("5k", Bandwidth::KiBPerSec(5)),
        ] {
            assert_eq!(Bandwidth::parse(text), Some(expected), "{text}");
        }
    }

    #[test]
    fn anything_else_is_not_a_speed() {
        for text in [
            "",
            "0",
            "0K",
            "00M",
            "1.5M",
            "10",
            "-1K",
            "+1K",
            "5T",
            "5P",
            "5B",
            "K",
            "M5",
            "1 M",
            " 1M",
            "Off",
            "OFF",
            "1MB",
            "99999999999999999999K",
            "17592186044416G",
        ] {
            assert_eq!(Bandwidth::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_canonical_text_uses_the_largest_exact_unit() {
        for (speed, text) in [
            (Bandwidth::Off, "off"),
            (Bandwidth::KiBPerSec(1), "1K"),
            (Bandwidth::KiBPerSec(1023), "1023K"),
            (Bandwidth::KiBPerSec(1024), "1M"),
            (Bandwidth::KiBPerSec(1536), "1536K"),
            (Bandwidth::KiBPerSec(1_048_576), "1G"),
            (Bandwidth::KiBPerSec(2_097_152), "2G"),
            (Bandwidth::KiBPerSec(1_048_577), "1048577K"),
        ] {
            assert_eq!(speed.format(), text);
        }
    }
}
