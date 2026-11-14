//! The dry run behind the Review sheet: what a resync or a forced round
//! would change, without changing anything.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::behavior::{Behavior, SyncMode};
use crate::bisync::{ReviewKind, RoundMode, bisync_args, log_file};
use crate::bisync_log::{Change, ChangeKind, RunSummary, summarize};
use crate::config::Pair;
use crate::filters::{self, FiltersFile};
use crate::listing::{LastSync, last_sync};
use crate::notify::last_error_message;
use crate::oneway::{self, oneway_args, summarize_oneway};
use crate::run::{
    PairLookup, RunEnv, RunError, execute, fresh_start, lookup_pair, read_log_or_empty,
    take_changes,
};
use crate::state::try_lock_pair;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Hash of the filters the preview ran with; the confirmation marker
    /// carries it so a later filter change voids the confirmation.
    pub filters_hash: String,
    /// The mode the preview ran in; the confirmation is only good for it.
    pub mode: SyncMode,
    /// Folders the `git_repos` preset left out of the dry run.
    pub excluded_repos: Vec<String>,
    pub summary: RunSummary,
    /// The changes of a resync that bring back files deleted or renamed
    /// since the last good sync; also listed in `summary`.
    pub returning: Vec<Change>,
}

impl Preview {
    /// What `syncfast preview` prints.
    #[must_use]
    pub fn report(&self) -> PreviewReport<'_> {
        PreviewReport {
            filters_hash: &self.filters_hash,
            changes: &self.summary.changes,
            excluded_repos: &self.excluded_repos,
            counts: ChangeCounts::of(&self.summary.changes),
            returning: &self.returning,
        }
    }
}

/// The resync changes that copy back a file one side had at the last good
/// sync and no longer has: a download of a file the local listing had, or
/// an upload of a file the remote listing had.
#[must_use]
pub fn coming_back(changes: &[Change], last: &LastSync) -> Vec<Change> {
    changes
        .iter()
        .filter(|change| match change.kind {
            ChangeKind::Download => last.local.contains(&change.path),
            ChangeKind::Upload => last.remote.contains(&change.path),
            _ => false,
        })
        .cloned()
        .collect()
}

/// How many changes of each kind a preview found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeCounts {
    pub upload: u32,
    pub download: u32,
    pub overwrite_local: u32,
    pub overwrite_remote: u32,
    pub delete_local: u32,
    pub delete_remote: u32,
    pub other: u32,
    #[serde(default)]
    pub renamed: u32,
}

impl ChangeCounts {
    /// Files the review would delete, on either side.
    #[must_use]
    pub fn deleted(&self) -> u32 {
        self.delete_local.saturating_add(self.delete_remote)
    }

    #[must_use]
    pub fn deletes(&self) -> bool {
        self.deleted() > 0
    }

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
                ChangeKind::RenameLocal | ChangeKind::RenameRemote => &mut counts.renamed,
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
    pub excluded_repos: &'a [String],
    pub counts: ChangeCounts,
    pub returning: &'a [Change],
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

/// The files of a dry run. They go away with the guard, also when the
/// caller gives up on the preview and the future is dropped.
struct PreviewLog(Vec<PathBuf>);

impl Drop for PreviewLog {
    fn drop(&mut self) {
        for path in &self.0 {
            if let Err(err) = std::fs::remove_file(path)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                log::warn!("cannot delete {}: {err}", path.display());
            }
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
    let (pair, behavior) = match lookup_pair(env, pair_id).await? {
        PairLookup::Valid(pair, behavior) => (pair, behavior),
        PairLookup::Invalid(_, error, _) => {
            return Err(PreviewError::InvalidPair(error.message));
        }
    };
    let Some(_lock) = try_lock_pair(env.paths, pair_id).map_err(RunError::from)? else {
        return Err(PreviewError::Busy);
    };
    let filters_file = filters::write_if_changed(env.paths, &pair)
        .await
        .map_err(RunError::from)?;
    preview_locked(env, &pair, &behavior, &filters_file, kind).await
}

/// [`preview`] for a caller that already holds the pair's lock.
pub(crate) async fn preview_locked(
    env: &RunEnv<'_>,
    pair: &Pair,
    behavior: &Behavior,
    filters_file: &FiltersFile,
    kind: ReviewKind,
) -> Result<Preview, PreviewError> {
    let pair_id = pair.id.as_str();
    tokio::fs::create_dir_all(env.paths.pair_state_dir(pair_id))
        .await
        .map_err(|source| RunError::Io {
            context: "creating the sync's state folder",
            source,
        })?;
    let mode = RoundMode::DryRun(kind);
    let now = fresh_start(env.paths, pair_id, mode, env.now());
    let log_path = log_file(env.paths, pair_id, mode, &now);
    let one_way = behavior.mode.is_one_way();
    let changes_path = oneway::changes_file(env.paths, pair_id, mode, behavior.mode, &now);
    let args = if one_way {
        oneway_args(pair, behavior, env.paths, mode, 0, &now)
    } else {
        bisync_args(pair, behavior, env.paths, mode, &now)
    };
    let log = PreviewLog(vec![log_path.clone(), changes_path.clone()]);
    let exit_code = execute(env.rclone, args, env.forward_signals).await?;
    let lines = read_log_or_empty(&log_path).await;
    let changes = if one_way {
        take_changes(&changes_path).await
    } else {
        String::new()
    };
    drop(log);
    if exit_code != 0 {
        let message = last_error_message(&lines).map_or_else(
            || format!("rclone exited with code {exit_code}"),
            str::to_owned,
        );
        return Err(PreviewError::Failed { exit_code, message });
    }
    let summary = if one_way {
        summarize_oneway(&changes, &lines, behavior.mode, exit_code)
    } else {
        summarize(&lines)
    };
    let returning = match (one_way, kind) {
        (false, ReviewKind::Resync) => last_sync(&env.paths.bisync_workdir(pair_id))
            .map(|last| coming_back(&summary.changes, &last))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    Ok(Preview {
        mode: behavior.mode,
        filters_hash: filters_file.hash.clone(),
        excluded_repos: filters_file.git_repos.clone(),
        summary,
        returning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(kind: ChangeKind, path: &str) -> Change {
        Change::new(kind, path)
    }

    #[test]
    fn coming_back_is_what_a_resync_copies_to_the_side_that_lost_it() {
        let last = LastSync {
            local: ["gone-here.txt", "both.txt"].map(str::to_owned).into(),
            remote: ["gone-there.txt", "both.txt"].map(str::to_owned).into(),
        };
        let changes = [
            change(ChangeKind::Download, "gone-here.txt"),
            change(ChangeKind::Upload, "gone-there.txt"),
            change(ChangeKind::Download, "new-there.txt"),
            change(ChangeKind::Upload, "new-here.txt"),
            change(ChangeKind::OverwriteLocal, "both.txt"),
        ];

        let back = coming_back(&changes, &last);

        assert_eq!(back, changes[..2]);
    }

    #[test]
    fn renames_are_counted_apart_and_never_as_deletions() {
        let counts = ChangeCounts::of(&[
            change(ChangeKind::RenameLocal, "a"),
            change(ChangeKind::RenameRemote, "b"),
        ]);

        assert_eq!(counts.renamed, 2);
        assert!(!counts.deletes());
    }

    #[test]
    fn only_deleting_on_either_side_counts_as_deleting_files() {
        let kinds = [
            ChangeKind::Upload,
            ChangeKind::Download,
            ChangeKind::OverwriteLocal,
            ChangeKind::OverwriteRemote,
            ChangeKind::Other,
        ];
        let harmless: Vec<Change> = kinds.iter().map(|kind| change(*kind, "f")).collect();
        let local = change(ChangeKind::DeleteLocal, "a");
        let remote = change(ChangeKind::DeleteRemote, "a");

        assert!(!ChangeCounts::of(&harmless).deletes());
        assert!(ChangeCounts::of(std::slice::from_ref(&local)).deletes());
        assert!(ChangeCounts::of(std::slice::from_ref(&remote)).deletes());
        assert_eq!(ChangeCounts::of(&[local, remote]).deleted(), 2);
    }

    #[test]
    fn the_dry_run_log_is_deleted_when_the_guard_goes_away() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-1.jsonl");
        std::fs::write(&path, "x").unwrap();

        drop(PreviewLog(vec![path.clone()]));

        assert!(!path.exists());
    }

    #[test]
    fn a_log_that_is_already_gone_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();

        drop(PreviewLog(vec![dir.path().join("absent.jsonl")]));
    }

    #[tokio::test]
    async fn the_dry_run_log_is_deleted_when_the_preview_is_aborted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-2.jsonl");
        std::fs::write(&path, "x").unwrap();
        let log = PreviewLog(vec![path.clone()]);
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
            mode: SyncMode::Bisync,
            filters_hash: "abc".to_owned(),
            excluded_repos: vec!["a/repo".to_owned()],
            summary: RunSummary {
                changes: vec![change(ChangeKind::Download, "x.txt")],
                ..RunSummary::default()
            },
            returning: vec![],
        };

        let json = serde_json::to_value(preview.report()).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "filters_hash": "abc",
                "changes": [{"kind": "download", "path": "x.txt"}],
                "excluded_repos": ["a/repo"],
                "counts": {
                    "upload": 0,
                    "download": 1,
                    "overwrite_local": 0,
                    "overwrite_remote": 0,
                    "delete_local": 0,
                    "delete_remote": 0,
                    "other": 0,
                    "renamed": 0
                },
                "returning": []
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
