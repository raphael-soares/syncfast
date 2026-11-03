//! The dry run behind the Review sheet: what a resync or a forced round
//! would change, without changing anything.

use std::path::PathBuf;

use serde::Serialize;

use crate::behavior::Behavior;
use crate::bisync::{Mode, ReviewKind, bisync_args, log_file};
use crate::bisync_log::{Change, ChangeKind, RunSummary, summarize};
use crate::filters;
use crate::notify::last_error_message;
use crate::run::{
    PairLookup, RunEnv, RunError, execute, fresh_start, lookup_pair, read_log_or_empty,
};
use crate::state::try_lock_pair;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Hash of the filters the preview ran with; the confirmation marker
    /// carries it so a later filter change voids the confirmation.
    pub filters_hash: String,
    pub summary: RunSummary,
}

impl Preview {
    /// What `syncfast preview` prints.
    #[must_use]
    pub fn report(&self) -> PreviewReport<'_> {
        PreviewReport {
            filters_hash: &self.filters_hash,
            changes: &self.summary.changes,
            counts: ChangeCounts::of(&self.summary.changes),
        }
    }
}

/// How many changes of each kind a preview found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ChangeCounts {
    pub upload: u32,
    pub download: u32,
    pub overwrite_local: u32,
    pub overwrite_remote: u32,
    pub delete_local: u32,
    pub delete_remote: u32,
    pub other: u32,
}

impl ChangeCounts {
    #[must_use]
    pub fn of(changes: &[Change]) -> Self {
        let mut counts = Self::default();
        for change in changes {
            let slot = match change.kind {
                ChangeKind::Upload => &mut counts.upload,
                ChangeKind::Download => &mut counts.download,
                ChangeKind::OverwriteLocal => &mut counts.overwrite_local,
                ChangeKind::OverwriteRemote => &mut counts.overwrite_remote,
                ChangeKind::DeleteLocal => &mut counts.delete_local,
                ChangeKind::DeleteRemote => &mut counts.delete_remote,
                ChangeKind::Other => &mut counts.other,
            };
            *slot = slot.saturating_add(1);
        }
        counts
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewReport<'a> {
    pub filters_hash: &'a str,
    pub changes: &'a [Change],
    pub counts: ChangeCounts,
}

#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    /// A round of this sync is running; try again when it finishes.
    #[error("this sync is running; try again when it finishes")]
    Busy,
    #[error("{0}")]
    InvalidPair(String),
    /// rclone ended the dry run with an error; `message` is its last error
    /// line.
    #[error("{message}")]
    Failed { exit_code: i32, message: String },
    #[error(transparent)]
    Run(#[from] RunError),
}

impl PreviewError {
    /// 3 when a round is running, 2 for a config the user must fix, 1 for
    /// anything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Busy => 3,
            Self::InvalidPair(_) => 2,
            Self::Failed { .. } => 1,
            Self::Run(err) => err.exit_code(),
        }
    }
}

/// The log of a dry run. It goes away with the guard, also when the caller
/// gives up on the preview and the future is dropped.
struct PreviewLog(PathBuf);

impl Drop for PreviewLog {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.0)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("cannot delete {}: {err}", self.0.display());
        }
    }
}

/// Dry-runs `kind` for `pair_id`. Takes the pair's lock for the duration,
/// so a running round makes it return [`PreviewError::Busy`].
///
/// # Errors
/// See [`PreviewError`].
pub async fn preview(
    env: &RunEnv<'_>,
    pair_id: &str,
    kind: ReviewKind,
) -> Result<Preview, PreviewError> {
    let pair = match lookup_pair(env, pair_id).await? {
        PairLookup::Valid(pair) => pair,
        PairLookup::Invalid(_, error) => return Err(PreviewError::InvalidPair(error.message)),
    };
    let Some(_lock) = try_lock_pair(env.paths, pair_id).map_err(RunError::from)? else {
        return Err(PreviewError::Busy);
    };
    let filters_file = filters::write_if_changed(env.paths, &pair)
        .await
        .map_err(RunError::from)?;
    tokio::fs::create_dir_all(env.paths.pair_state_dir(pair_id))
        .await
        .map_err(|source| RunError::Io {
            context: "creating the sync's state folder",
            source,
        })?;
    let mode = Mode::DryRun(kind);
    let now = fresh_start(env.paths, pair_id, mode, env.now());
    let log_path = log_file(env.paths, pair_id, mode, &now);
    let args = bisync_args(&pair, &Behavior::for_pair(&pair), env.paths, mode, &now);
    let log = PreviewLog(log_path);
    let exit_code = execute(env.rclone, args, env.forward_signals).await?;
    let lines = read_log_or_empty(&log.0).await;
    drop(log);
    if exit_code != 0 {
        let message = last_error_message(&lines).map_or_else(
            || format!("rclone exited with code {exit_code}"),
            str::to_owned,
        );
        return Err(PreviewError::Failed { exit_code, message });
    }
    Ok(Preview {
        filters_hash: filters_file.hash,
        summary: summarize(&lines),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(kind: ChangeKind, path: &str) -> Change {
        Change {
            kind,
            path: path.to_owned(),
        }
    }

    #[test]
    fn the_dry_run_log_is_deleted_when_the_guard_goes_away() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-1.jsonl");
        std::fs::write(&path, "x").unwrap();

        drop(PreviewLog(path.clone()));

        assert!(!path.exists());
    }

    #[test]
    fn a_log_that_is_already_gone_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();

        drop(PreviewLog(dir.path().join("absent.jsonl")));
    }

    #[tokio::test]
    async fn the_dry_run_log_is_deleted_when_the_preview_is_aborted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-2.jsonl");
        std::fs::write(&path, "x").unwrap();
        let log = PreviewLog(path.clone());
        let job = tokio::spawn(async move {
            let _log = log;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;

        job.abort();
        let _ = job.await;

        assert!(!path.exists());
    }

    #[test]
    fn counts_tally_each_kind_and_leave_the_others_at_zero() {
        let changes = [
            change(ChangeKind::Upload, "a"),
            change(ChangeKind::Upload, "b"),
            change(ChangeKind::DeleteRemote, "c"),
            change(ChangeKind::Other, "d"),
        ];

        let counts = ChangeCounts::of(&changes);

        assert_eq!(
            counts,
            ChangeCounts {
                upload: 2,
                delete_remote: 1,
                other: 1,
                ..ChangeCounts::default()
            }
        );
    }

    #[test]
    fn report_serializes_hash_changes_and_counts() {
        let preview = Preview {
            filters_hash: "abc".to_owned(),
            summary: RunSummary {
                changes: vec![change(ChangeKind::Download, "x.txt")],
                ..RunSummary::default()
            },
        };

        let json = serde_json::to_value(preview.report()).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "filters_hash": "abc",
                "changes": [{"kind": "download", "path": "x.txt"}],
                "counts": {
                    "upload": 0,
                    "download": 1,
                    "overwrite_local": 0,
                    "overwrite_remote": 0,
                    "delete_local": 0,
                    "delete_remote": 0,
                    "other": 0
                }
            })
        );
    }

    #[test]
    fn busy_preview_exits_with_three_and_the_documented_message() {
        let err = PreviewError::Busy;

        assert_eq!(err.exit_code(), 3);
        assert_eq!(
            err.to_string(),
            "this sync is running; try again when it finishes"
        );
    }

    #[test]
    fn invalid_pair_and_failed_dry_run_exit_with_two_and_one() {
        let invalid = PreviewError::InvalidPair("bad".to_owned());
        let failed = PreviewError::Failed {
            exit_code: 1,
            message: "boom".to_owned(),
        };

        assert_eq!([invalid.exit_code(), failed.exit_code()], [2, 1]);
    }
}
