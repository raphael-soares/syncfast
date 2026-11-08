//! Restoring a snapshot for an agent: only into a folder that does not
//! exist yet, and only the counts come back, never the contents.

use std::time::Duration;

use tokio::time::Instant;

use super::backup_edit::{current_folder, place};
use super::{OpError, Ops};
use crate::backup::restore_job::{
    RestoreEnv, RestoreReport, RestoreRequest, restore_status, start_restore,
};

const POLL: Duration = Duration::from_millis(200);

/// What `restore` takes: `to` is the folder to create, and `paths` the
/// folders or files of the snapshot to restore (all when empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreArgs {
    pub backup: String,
    pub snapshot: String,
    pub to: String,
    pub paths: Vec<String>,
    pub dry_run: bool,
}

impl Ops {
    /// Restores into the new folder `to` and waits up to `wait` for the
    /// restore to end. A dry run creates nothing and reports what a real
    /// one would restore.
    ///
    /// # Errors
    /// [`OpError::Invalid`] for a folder that exists or a snapshot or path
    /// that does not; [`OpError::Busy`] when the folder is in a sync with a
    /// round running; [`OpError::Failed`] when restic or the key is
    /// missing, or the restore itself failed.
    pub async fn restore(
        &self,
        args: RestoreArgs,
        wait: Duration,
    ) -> Result<RestoreReport, OpError> {
        let destination = place(&args.to, &self.paths.home, &current_folder()?);
        let restic = self.restic().await?;
        let env = RestoreEnv {
            paths: &self.paths,
            restic,
            keys: &*self.keys,
        };
        let request = RestoreRequest {
            backup: args.backup,
            snapshot: args.snapshot,
            paths: args.paths,
            destination,
            dry_run: args.dry_run,
        };
        let report = start_restore(&env, &*self.restores, &request).await?;
        self.await_restore(report, Instant::now() + wait).await
    }

    async fn await_restore(
        &self,
        mut report: RestoreReport,
        deadline: Instant,
    ) -> Result<RestoreReport, OpError> {
        while let Some(id) = report.id.as_deref()
            && !report.finished
            && Instant::now() < deadline
        {
            tokio::time::sleep(POLL).await;
            report = restore_status(&self.paths, id).await?;
        }
        if report.status == "failed" {
            return Err(OpError::Failed(report.error.unwrap_or_else(|| {
                "the restore failed without saying why".to_owned()
            })));
        }
        Ok(report)
    }

    /// How the restore job `id` stands, waiting up to `wait` for it to end.
    ///
    /// # Errors
    /// [`OpError::Invalid`] when no restore has that id; [`OpError::Failed`]
    /// with the reason when the restore failed.
    pub async fn restore_job(&self, id: &str, wait: Duration) -> Result<RestoreReport, OpError> {
        let report = restore_status(&self.paths, id).await?;
        self.await_restore(report, Instant::now() + wait).await
    }
}
