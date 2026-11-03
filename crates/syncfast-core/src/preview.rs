//! The dry run behind the Review sheet: what a resync or a forced round
//! would change, without changing anything.

use crate::behavior::Behavior;
use crate::bisync::{Mode, ReviewKind, bisync_args, log_file};
use crate::bisync_log::{RunSummary, summarize};
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
    let exit_code = execute(env.rclone, args).await?;
    let lines = read_log_or_empty(&log_path).await;
    if let Err(err) = tokio::fs::remove_file(&log_path).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("cannot delete {}: {err}", log_path.display());
    }
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
