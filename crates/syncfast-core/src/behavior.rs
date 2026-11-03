//! Every tunable of a sync, resolved in one place. Phase 1 returns the
//! defaults for every pair; phase 2 only has to make them configurable.

use std::time::Duration;

use crate::config::Pair;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behavior {
    pub watch: bool,
    pub debounce: Duration,
    pub max_wait: Duration,
    pub interval: Duration,
    pub conflict_resolve: ConflictResolve,
    pub conflict_loser: ConflictLoser,
    pub versions: bool,
    pub versions_keep_days: u32,
    pub google_docs: GoogleDocs,
    /// Percentage for `--max-delete`.
    pub max_delete: u8,
    pub resync_mode: ResyncMode,
    pub empty_dirs: bool,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            watch: true,
            debounce: Duration::from_secs(30),
            max_wait: Duration::from_mins(5),
            interval: Duration::from_mins(5),
            conflict_resolve: ConflictResolve::Newer,
            conflict_loser: ConflictLoser::Num,
            versions: true,
            versions_keep_days: 30,
            google_docs: GoogleDocs::Skip,
            max_delete: 50,
            resync_mode: ResyncMode::Newer,
            empty_dirs: true,
        }
    }
}

impl Behavior {
    #[must_use]
    pub fn for_pair(_pair: &Pair) -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn default_has_the_phase_one_values() {
        let behavior = Behavior::default();

        assert!(behavior.watch);
        assert_eq!(behavior.debounce, Duration::from_secs(30));
        assert_eq!(behavior.max_wait, Duration::from_secs(300));
        assert_eq!(behavior.interval, Duration::from_secs(300));
        assert_eq!(behavior.conflict_resolve, ConflictResolve::Newer);
        assert_eq!(behavior.conflict_loser, ConflictLoser::Num);
        assert!(behavior.versions);
        assert_eq!(behavior.versions_keep_days, 30);
        assert_eq!(behavior.google_docs, GoogleDocs::Skip);
        assert_eq!(behavior.max_delete, 50);
        assert_eq!(behavior.resync_mode, ResyncMode::Newer);
        assert!(behavior.empty_dirs);
    }

    #[test]
    fn for_pair_returns_the_defaults() {
        let pair = Pair {
            id: "drive".into(),
            name: "Drive".into(),
            local: PathBuf::from("/home/ada/Drive"),
            remote: "gdrive:".into(),
            enabled: false,
            presets: vec![],
            filters: vec!["- x".into()],
        };

        assert_eq!(Behavior::for_pair(&pair), Behavior::default());
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
}
