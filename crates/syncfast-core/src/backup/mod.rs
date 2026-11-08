//! Backups of local folders into a restic repository (ADR 0009).

pub mod classify;
pub mod config;
pub mod destination;
pub mod effects;
pub mod excludes;
pub mod key;
pub mod keystore;
pub mod run_decision;
pub mod snapshots;
pub mod state;
pub mod status;
pub mod test_filters;

use std::time::Duration;

use state::CheckKind;

/// How often a backup runs a sample `restic check` of its repository.
pub const CHECK_INTERVAL: Duration = Duration::from_hours(30 * 24);
/// The share of the repository's data `restic check` reads each time.
pub const CHECK_SAMPLE: &str = "5%";
/// The check that follows a failed backup: structural, reads no data.
pub const AFTER_FAILURE_CHECK: CheckKind = CheckKind::Quick;
/// Failed prunes in a row before the backup asks for attention.
pub const PRUNE_FAILURES_BEFORE_GATE: u32 = 3;
/// The least time without a snapshot before a backup is called stale.
pub const STALE_MIN: Duration = Duration::from_hours(48);
/// A backup is stale after this many of its own intervals without a snapshot.
pub const STALE_FACTOR: u32 = 2;
/// How long a backup that failed waits before it tries again.
pub const RETRY_AFTER: Duration = Duration::from_mins(15);
