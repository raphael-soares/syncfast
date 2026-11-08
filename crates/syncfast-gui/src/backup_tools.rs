//! What the backend does for the backup screens: the tools the core's
//! lifecycle needs, the repository's snapshots, the filter test, the check
//! request and the restic version.

use std::path::PathBuf;
use std::sync::Arc;

use syncfast_core::backup::config::BackupConfig;
use syncfast_core::backup::destination::Destination;
use syncfast_core::backup::excludes::render_excludes_live;
use syncfast_core::backup::files::{self, Listing};
use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::{DefaultKeyStore, KeyBackend, KeyStore as _};
use syncfast_core::backup::lifecycle::{self, LifecycleEnv};
use syncfast_core::backup::restore_job::{
    self, DetachedRestoreStarter, RestoreEnv, RestoreError, RestoreReport, RestoreRequest,
};
use syncfast_core::backup::run_decision::{BackupRequest, save_request};
use syncfast_core::backup::snapshots::SnapshotView;
use syncfast_core::backup::state::CheckKind;
use syncfast_core::backup::test_filters::{FilterTest, run_filter_test};
use syncfast_core::config::Config;
use syncfast_core::flatpak;
use syncfast_core::paths::Paths;
use syncfast_core::rclone::Rclone;
use syncfast_core::restic::{self, Repository, Restic};
use syncfast_core::units::{ApplyEnv, SystemdMode, UserSystemctl};
use syncfast_core::watch::Trigger;

const NO_KEY: &str = "The key for this backup is missing on this computer.";

/// Everything a lifecycle operation borrows from, owned in one place.
#[expect(
    missing_debug_implementations,
    reason = "holds a trait object and the key store"
)]
pub struct Tools {
    paths: Arc<Paths>,
    rclone: Rclone,
    restic: Restic,
    keys: DefaultKeyStore,
    trigger: Arc<dyn Trigger>,
    bin: PathBuf,
    systemctl: UserSystemctl,
    flatpak_id: Option<String>,
}

impl Tools {
    /// # Errors
    /// rclone or restic is missing or unsupported: the reason, in words.
    pub async fn load(
        paths: Arc<Paths>,
        bin: PathBuf,
        trigger: Arc<dyn Trigger>,
    ) -> Result<Self, String> {
        let rclone = Rclone::from_env().map_err(|err| err.to_string())?;
        let restic = Restic::from_env(rclone.clone(), &paths)
            .await
            .map_err(|err| err.to_string())?;
        Ok(Self {
            keys: DefaultKeyStore::from_env(&paths),
            paths,
            rclone,
            restic,
            trigger,
            bin,
            systemctl: UserSystemctl::detect(),
            flatpak_id: flatpak::app_id(),
        })
    }

    #[must_use]
    pub fn env(&self) -> LifecycleEnv<'_> {
        let apply = ApplyEnv::new(
            &self.paths,
            &self.rclone,
            &self.bin,
            SystemdMode::from_env(),
            &self.systemctl,
        )
        .with_flatpak_id(self.flatpak_id.as_deref());
        LifecycleEnv::new(apply, &self.restic, &self.keys, &*self.trigger)
    }

    #[must_use]
    pub fn keys(&self) -> &DefaultKeyStore {
        &self.keys
    }

    /// The snapshots of the backup's repository, newest first.
    ///
    /// # Errors
    /// The backup, its key or its repository cannot be read: the reason.
    pub async fn snapshots(&self, backup_id: &str) -> Result<Arc<[SnapshotView]>, String> {
        let backup = stored_backup(&self.paths, backup_id).await?;
        let key = self.key_of(backup_id).await?;
        let repository = repository_of(&self.paths, &backup)?;
        let mut snapshots = self
            .restic
            .snapshots(&repository, &key)
            .await
            .map_err(|err| err.to_string())?;
        snapshots.sort_unstable_by_key(|snapshot| std::cmp::Reverse(snapshot.time));
        Ok(snapshots.into())
    }

    /// One folder of a snapshot.
    ///
    /// # Errors
    /// The backup, its key or the folder cannot be read: the reason.
    pub async fn list_files(
        &self,
        backup_id: &str,
        snapshot: &SnapshotView,
        dir: &str,
    ) -> Result<Listing, String> {
        let backup = stored_backup(&self.paths, backup_id).await?;
        let key = self.key_of(backup_id).await?;
        let repository = repository_of(&self.paths, &backup)?;
        files::list_files(&self.restic, &repository, &key, snapshot, dir)
            .await
            .map_err(|err| err.to_string())
    }

    /// Starts a restore that goes on when the window closes.
    ///
    /// # Errors
    /// The restore is refused or cannot start.
    pub async fn start_restore(
        &self,
        request: &RestoreRequest,
    ) -> Result<RestoreReport, RestoreError> {
        let env = RestoreEnv {
            paths: &self.paths,
            restic: &self.restic,
            keys: &self.keys,
        };
        let starter = DetachedRestoreStarter::new(self.bin.clone());
        restore_job::start_restore(&env, &starter, request).await
    }

    async fn key_of(&self, backup_id: &str) -> Result<Key, String> {
        self.keys
            .get(backup_id)
            .await
            .map_err(|err| err.to_string())?
            .ok_or_else(|| NO_KEY.to_owned())
    }

    /// How many files and bytes the rules of `backup` leave out of its
    /// folders.
    ///
    /// # Errors
    /// The test repository or restic fails: the reason.
    pub async fn test_filters(&self, backup: &BackupConfig) -> Result<FilterTest, String> {
        let excludes = render_excludes_live(backup).await;
        run_filter_test(&self.restic, backup, &excludes, &self.paths.backups_dir())
            .await
            .map_err(|err| err.to_string())
    }
}

async fn stored_backup(paths: &Paths, backup_id: &str) -> Result<BackupConfig, String> {
    let config = Config::load(paths).await.map_err(|err| err.to_string())?;
    config
        .backups()
        .iter()
        .find(|backup| backup.id == backup_id)
        .cloned()
        .ok_or_else(|| format!("There is no backup \"{backup_id}\"."))
}

fn repository_of(paths: &Paths, backup: &BackupConfig) -> Result<Repository, String> {
    let destination = Destination::parse(&backup.dest).map_err(|err| err.to_string())?;
    let behavior = backup.behavior()?;
    Ok(
        Repository::new(destination, paths.backup_state_dir(&backup.id))
            .with_limits(behavior.limit_upload, behavior.limit_download),
    )
}

/// Asks the next round of a backup for `kind` of check.
///
/// # Errors
/// The request file cannot be written.
pub async fn request_check(paths: &Paths, backup_id: &str, kind: CheckKind) -> Result<(), String> {
    save_request(paths, backup_id, BackupRequest::Check(kind))
        .await
        .map_err(|err| err.to_string())
}

/// The backups that are not paused.
///
/// # Errors
/// The config cannot be read.
pub async fn enabled_backups(paths: &Paths) -> Result<Vec<String>, String> {
    let config = Config::load(paths).await.map_err(|err| err.to_string())?;
    Ok(config
        .backups()
        .iter()
        .filter(|backup| backup.enabled)
        .map(|backup| backup.id.clone())
        .collect())
}

/// Pauses or resumes a backup in the config; `false` when there is none.
///
/// # Errors
/// The config cannot be written.
pub async fn set_enabled(paths: &Paths, backup_id: &str, enabled: bool) -> Result<bool, String> {
    Config::edit(paths, |config| {
        config.set_backup_enabled(backup_id, enabled)
    })
    .await
    .map_err(|err| err.to_string())
}

/// Which storage holds the backup keys; with `test`, after writing and
/// reading back a test item.
///
/// # Errors
/// No storage answered: the reason.
pub async fn key_storage(paths: &Paths, test: bool) -> Result<KeyBackend, String> {
    let keys = DefaultKeyStore::from_env(paths);
    let found = if test {
        lifecycle::test_key_storage(&keys).await
    } else {
        keys.probe().await
    };
    found.map_err(|err| err.to_string())
}

/// The folder a backup writes to, when it is on this computer.
///
/// # Errors
/// The backup is not in the config.
pub async fn local_destination(paths: &Paths, backup_id: &str) -> Result<Option<PathBuf>, String> {
    let backup = stored_backup(paths, backup_id).await?;
    Ok(match Destination::parse(&backup.dest) {
        Ok(Destination::Local(folder)) => Some(folder),
        _ => None,
    })
}

/// What `restic version` says: `0.18.1`. `None` when restic is missing or
/// answers something else.
pub async fn restic_version() -> Option<String> {
    let bin = restic::locate_binary()?;
    let output = tokio::process::Command::new(bin)
        .arg("version")
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), output)
        .await
        .ok()?
        .ok()?;
    version_of(&String::from_utf8_lossy(&output.stdout))
}

fn version_of(text: &str) -> Option<String> {
    let mut words = text.split_whitespace();
    (words.next()? == "restic").then_some(())?;
    words.next().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_the_second_word_of_the_first_line() {
        let text = "restic 0.18.1 compiled with go1.24.1 on linux/amd64\n";

        assert_eq!(version_of(text).as_deref(), Some("0.18.1"));
    }

    #[test]
    fn an_answer_that_is_not_restic_has_no_version() {
        assert_eq!(version_of("rclone v1.70.0"), None);
        assert_eq!(version_of(""), None);
    }
}
