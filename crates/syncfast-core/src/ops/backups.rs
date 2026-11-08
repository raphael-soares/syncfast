//! Backups as a reader that is not the GUI sees them: the list, one
//! backup, its snapshots and files, runs and checks, pause and resume.
//! Nothing here holds the key.

use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use serde::Serialize;
use tokio::time::Instant;

use super::sync::await_run;
use super::{OpError, Ops};
use crate::backup::config::BackupConfig;
use crate::backup::files::{ListError, NodeView, list_files};
use crate::backup::key::Key;
use crate::backup::lifecycle::repository_of;
use crate::backup::run_decision::{BackupRequest, save_request};
use crate::backup::snapshots::{SnapshotView, pick_snapshot};
use crate::backup::state::{BackupState, CheckKind, LastCheck, LastRun, LastSnapshot, is_running};
use crate::backup::status::{display_backup_status, running_step};
use crate::bisync::Fault;
use crate::restic::{Repository, ResticError};
use crate::state::GateKind;

/// How many files `snapshot_files` returns unless told otherwise.
pub const DEFAULT_FILE_LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeepView {
    pub daily: i64,
    pub weekly: i64,
    pub monthly: i64,
    pub yearly: i64,
}

/// Why runs of a backup are held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupGateView {
    pub kind: GateKind,
    pub fault: Option<Fault>,
    pub detail: String,
}

/// A backup and where it stands. `status` is the name `syncfast backups`
/// prints; `key` says only whether the key storage has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools, reason = "the shape of the JSON")]
pub struct BackupView {
    pub id: String,
    pub name: String,
    pub status: &'static str,
    /// What the GUI says for `status`.
    pub status_text: &'static str,
    pub enabled: bool,
    pub running: bool,
    pub dest: String,
    pub paths: Vec<PathBuf>,
    pub every: String,
    pub keep: KeepView,
    pub presets: Vec<String>,
    pub filters: Vec<String>,
    pub one_file_system: bool,
    pub limit_upload: String,
    pub limit_download: String,
    pub last_success: Option<Timestamp>,
    pub last_snapshot: Option<LastSnapshot>,
    pub gate: Option<BackupGateView>,
    /// `stored` or `missing`; never the key.
    pub key: &'static str,
    /// An agent created the backup and the person has not yet saved the
    /// key in syncfast.
    pub key_unsaved: bool,
    /// Why the config rejects this backup, when it does.
    pub config_error: Option<String>,
}

/// One backup with the details of its last run and check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupDetail {
    #[serde(flatten)]
    pub backup: BackupView,
    pub last_run: Option<LastRun>,
    pub last_check: Option<LastCheck>,
    /// Why the last `forget` failed, until one succeeds.
    pub prune_error: Option<String>,
}

/// A run or check that was started, and whether it ended within the wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupRun {
    pub backup: BackupView,
    /// `false` means it is still going: read `backups` again later.
    pub finished: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupsToggled {
    pub backups: Vec<BackupView>,
    pub warnings: Vec<String>,
}

/// The files of a folder of a snapshot: names, sizes and dates only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotFiles {
    pub snapshot: String,
    /// The folder listed, as the snapshot knows it; empty for the roots.
    pub dir: String,
    pub files: Vec<NodeView>,
    /// There are more entries than the list holds.
    pub truncated: bool,
}

impl Ops {
    /// Every backup.
    ///
    /// # Errors
    /// The config or the state cannot be read, or rclone cannot list the
    /// accounts.
    pub async fn backups(&self) -> Result<Vec<BackupView>, OpError> {
        self.views_of(None).await
    }

    /// The backup `id` with its last run and check.
    ///
    /// # Errors
    /// [`OpError::NoBackup`], or the config or state cannot be read.
    pub async fn show_backup(&self, id: &str) -> Result<BackupDetail, OpError> {
        let backup = self.backup_view(id).await?;
        let state = BackupState::load(&self.paths, id).await?;
        Ok(BackupDetail {
            backup,
            last_run: state.last_run,
            last_check: state.last_check,
            prune_error: state.prune_error,
        })
    }

    pub(super) async fn backup_view(&self, id: &str) -> Result<BackupView, OpError> {
        self.views_of(Some(id))
            .await?
            .pop()
            .ok_or_else(|| OpError::NoBackup(id.to_owned()))
    }

    async fn views_of(&self, only: Option<&str>) -> Result<Vec<BackupView>, OpError> {
        let config = self.load_config().await?;
        if let Some(id) = only
            && config.backups().iter().all(|backup| backup.id != id)
        {
            return Err(OpError::NoBackup(id.to_owned()));
        }
        let accounts = self.rclone.listremotes().await?;
        let validation = config.validate_backups(&self.paths, &accounts);
        let mut views = Vec::new();
        for backup in config
            .backups()
            .iter()
            .filter(|backup| only.is_none_or(|id| backup.id == id))
        {
            let problem = validation
                .errors
                .iter()
                .find(|error| error.backup_id == backup.id)
                .map(|error| error.message.clone());
            views.push(self.make_view(backup, problem).await?);
        }
        Ok(views)
    }

    async fn make_view(
        &self,
        backup: &BackupConfig,
        config_error: Option<String>,
    ) -> Result<BackupView, OpError> {
        let state = BackupState::load(&self.paths, &backup.id).await?;
        let step = running_step(&self.paths, &backup.id);
        let status = display_backup_status(backup, &state, step);
        let stored = matches!(self.keys.get(&backup.id).await, Ok(Some(_)));
        Ok(BackupView {
            id: backup.id.clone(),
            name: backup.name.clone(),
            status: status.name(),
            status_text: status.label(),
            enabled: backup.enabled,
            running: step.is_some(),
            dest: backup.dest.clone(),
            paths: backup.paths.clone(),
            every: backup.every.clone(),
            keep: KeepView {
                daily: backup.keep.daily,
                weekly: backup.keep.weekly,
                monthly: backup.keep.monthly,
                yearly: backup.keep.yearly,
            },
            presets: backup.presets.clone(),
            filters: backup.filters.clone(),
            one_file_system: backup.one_file_system,
            limit_upload: backup.limit_upload.clone(),
            limit_download: backup.limit_download.clone(),
            last_success: state.last_success,
            last_snapshot: state.last_snapshot,
            gate: state.gate.map(|gate| BackupGateView {
                kind: gate.kind,
                fault: gate.fault,
                detail: gate.detail,
            }),
            key: if stored { "stored" } else { "missing" },
            key_unsaved: state.key_unsaved,
            config_error,
        })
    }

    pub(super) async fn find_backup(&self, id: &str) -> Result<BackupConfig, OpError> {
        self.load_config()
            .await?
            .backups()
            .iter()
            .find(|backup| backup.id == id)
            .cloned()
            .ok_or_else(|| OpError::NoBackup(id.to_owned()))
    }

    /// The backup, its repository and its key, for what has to open the
    /// repository. The key stays inside the operation.
    async fn open_backup(&self, id: &str) -> Result<(BackupConfig, Repository, Key), OpError> {
        let backup = self.find_backup(id).await?;
        let repository =
            repository_of(&self.paths, &backup).map_err(|err| OpError::Invalid(err.to_string()))?;
        let key = self
            .keys
            .get(id)
            .await
            .map_err(|err| OpError::Failed(err.to_string()))?
            .ok_or_else(|| {
                OpError::Failed(
                    "The key of this backup is not in the key storage. Ask the user to enter it in syncfast."
                        .to_owned(),
                )
            })?;
        Ok((backup, repository, key))
    }

    /// The snapshots of the backup `id`, newest first, at most `limit` of
    /// them when it is given.
    ///
    /// # Errors
    /// [`OpError::NoBackup`]; the key is missing, restic is missing or the
    /// repository cannot be opened ([`OpError::Failed`]).
    pub async fn snapshots(
        &self,
        id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<SnapshotView>, OpError> {
        let restic = self.restic().await?;
        let (_, repository, key) = self.open_backup(id).await?;
        let mut snapshots = restic
            .snapshots(&repository, &key)
            .await
            .map_err(|err| failed(&err))?;
        newest_first(&mut snapshots);
        snapshots.truncate(limit.unwrap_or(usize::MAX));
        Ok(snapshots)
    }

    /// What a snapshot holds in the folder `dir` (empty for its roots):
    /// names, kinds, sizes and dates, at most `limit` of them. `snapshot`
    /// is `latest` or an id, or the start of one.
    ///
    /// # Errors
    /// As [`Ops::snapshots`], and [`OpError::Invalid`] for a snapshot that
    /// does not exist or a folder the snapshot does not have.
    pub async fn snapshot_files(
        &self,
        id: &str,
        snapshot: &str,
        dir: &str,
        limit: usize,
    ) -> Result<SnapshotFiles, OpError> {
        let restic = self.restic().await?;
        let (_, repository, key) = self.open_backup(id).await?;
        let mut snapshots = restic
            .snapshots(&repository, &key)
            .await
            .map_err(|err| failed(&err))?;
        newest_first(&mut snapshots);
        let picked = pick_snapshot(&snapshots, snapshot).map_err(OpError::Invalid)?;
        let listing = list_files(restic, &repository, &key, picked, dir)
            .await
            .map_err(|err| match err {
                ListError::Invalid(message) => OpError::Invalid(message),
                err @ ListError::Restic(_) => OpError::Failed(err.to_string()),
            })?;
        let (files, truncated) = cut(listing.nodes, listing.truncated, limit);
        Ok(SnapshotFiles {
            snapshot: picked.id.clone(),
            dir: dir.to_owned(),
            files,
            truncated,
        })
    }

    /// Runs the backup `id` now and waits up to `wait` for it.
    ///
    /// # Errors
    /// [`OpError::NoBackup`], or the run cannot be started.
    pub async fn backup_now(&self, id: &str, wait: Duration) -> Result<BackupRun, OpError> {
        self.find_backup(id).await?;
        self.start_and_wait(id, wait).await
    }

    /// Checks the repository of the backup `id` and waits up to `wait`.
    ///
    /// # Errors
    /// [`OpError::NoBackup`], or the check cannot be asked for or started.
    pub async fn check_backup(
        &self,
        id: &str,
        kind: CheckKind,
        wait: Duration,
    ) -> Result<BackupRun, OpError> {
        self.find_backup(id).await?;
        save_request(&self.paths, id, BackupRequest::Check(kind)).await?;
        self.start_and_wait(id, wait).await
    }

    async fn start_and_wait(&self, id: &str, wait: Duration) -> Result<BackupRun, OpError> {
        let deadline = Instant::now() + wait;
        let before = BackupState::load(&self.paths, id).await.ok();
        self.trigger
            .start_backup(id)
            .await
            .map_err(OpError::Failed)?;
        let finished = await_run(
            || is_running(&self.paths, id),
            || async { BackupState::load(&self.paths, id).await.ok() != before },
            deadline,
        )
        .await;
        Ok(BackupRun {
            backup: self.backup_view(id).await?,
            finished,
        })
    }

    /// Turns the schedule of one backup, or of all, off.
    ///
    /// # Errors
    /// [`OpError::NoBackup`], or the config cannot be written.
    pub async fn pause_backup(&self, id: Option<&str>) -> Result<BackupsToggled, OpError> {
        self.set_backups_enabled(id, false).await
    }

    /// Turns the schedule of one backup, or of all, on.
    ///
    /// # Errors
    /// [`OpError::NoBackup`], or the config cannot be written.
    pub async fn resume_backup(&self, id: Option<&str>) -> Result<BackupsToggled, OpError> {
        self.set_backups_enabled(id, true).await
    }

    async fn set_backups_enabled(
        &self,
        id: Option<&str>,
        enabled: bool,
    ) -> Result<BackupsToggled, OpError> {
        let ids: Vec<String> = match id {
            Some(id) => vec![self.find_backup(id).await?.id],
            None => self
                .load_config()
                .await?
                .backups()
                .iter()
                .map(|backup| backup.id.clone())
                .collect(),
        };
        crate::config::Config::edit(&self.paths, |config| {
            for id in &ids {
                config.set_backup_enabled(id, enabled);
            }
        })
        .await?;
        let warnings = self.reapply(&[]).await;
        let mut backups = Vec::with_capacity(ids.len());
        for id in &ids {
            backups.push(self.backup_view(id).await?);
        }
        Ok(BackupsToggled { backups, warnings })
    }
}

fn failed(err: &ResticError) -> OpError {
    OpError::Failed(err.to_string())
}

fn newest_first(snapshots: &mut [SnapshotView]) {
    snapshots.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.time));
}

/// At most `limit` of `nodes`, and whether anything was left out.
fn cut(mut nodes: Vec<NodeView>, truncated: bool, limit: usize) -> (Vec<NodeView>, bool) {
    let over = nodes.len() > limit;
    nodes.truncate(limit);
    (nodes, truncated || over)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::files::NodeKind;

    fn snapshot(id: &str, time: &str) -> SnapshotView {
        SnapshotView {
            id: id.to_owned(),
            short_id: id[..8.min(id.len())].to_owned(),
            time: time.parse().unwrap(),
            host: "host".to_owned(),
            paths: vec!["/home/user".to_owned()],
            files: Some(1),
            bytes: Some(1),
            added_bytes: Some(1),
        }
    }

    fn node(name: &str) -> NodeView {
        NodeView {
            name: name.to_owned(),
            kind: NodeKind::File,
            path: format!("/{name}"),
            size: Some(1),
            mtime: None,
        }
    }

    fn three() -> Vec<SnapshotView> {
        let mut all = vec![
            snapshot("aa11bb22cc33", "2026-01-01T00:00:00Z"),
            snapshot("ab99cc00dd11", "2026-03-01T00:00:00Z"),
            snapshot("cc44dd55ee66", "2026-02-01T00:00:00Z"),
        ];
        newest_first(&mut all);
        all
    }

    #[test]
    fn snapshots_are_listed_newest_first() {
        let ids: Vec<String> = three().into_iter().map(|snapshot| snapshot.id).collect();

        assert_eq!(ids, ["ab99cc00dd11", "cc44dd55ee66", "aa11bb22cc33"]);
    }

    #[test]
    fn a_list_is_cut_at_the_limit_and_says_so() {
        let nodes = || vec![node("a"), node("b"), node("c")];

        let (cut_list, truncated) = cut(nodes(), false, 2);
        let (whole, whole_truncated) = cut(nodes(), false, 3);
        let (listed_short, from_restic) = cut(nodes(), true, 3);

        assert_eq!(cut_list.len(), 2);
        assert!(truncated);
        assert_eq!(whole.len(), 3);
        assert!(!whole_truncated);
        assert_eq!(listed_short.len(), 3);
        assert!(from_restic);
    }
}
