//! What a person or an AI agent can do with syncfast, as functions that
//! return data. The CLI and the MCP server only translate arguments and
//! print what comes back (ADR 0007).

mod accounts;
mod activity;
mod backup_edit;
mod backups;
mod conflicts;
mod deleted;
mod error;
mod explore;
mod pairs;
mod restore;
mod review;
mod rules;
mod settings;
mod sync;
mod theme;
mod transfer;
pub(crate) use transfer::start_detached;
pub(crate) mod views;

use std::path::PathBuf;
use std::sync::Arc;

use crate::backup::keystore::{DefaultKeyStore, KeyStore};
use crate::backup::lifecycle::LifecycleEnv;
use crate::backup::restore_job::{DetachedRestoreStarter, RestoreStarter};
use crate::behavior::Behavior;
use crate::config::{Config, Pair};
use crate::flatpak;
use crate::network::{Network, NetworkManager};
use crate::notify::{DesktopNotifier, Notifier};
use crate::paths::Paths;
use crate::rclone::Rclone;
use crate::restic::{self, Restic};
use crate::units::{ApplyEnv, Systemctl, SystemdMode, UserSystemctl, apply, current_binary};
use crate::watch::{Trigger, trigger_for};

pub use accounts::AccountView;
pub use activity::{RoundLog, RoundView};
pub use backup_edit::{
    BackupCreated, BackupEdit, BackupOptions, BackupSaved, KeepPartial, NewBackupRequest,
};
pub use backups::{
    BackupDetail, BackupGateView, BackupRun, BackupView, BackupsToggled, DEFAULT_FILE_LIMIT,
    KeepView, SnapshotFiles,
};
pub use conflicts::{ConflictView, FileView, Keep, Resolved};
pub use deleted::Restored;
pub use error::OpError;
pub use explore::{
    DEFAULT_DEPTH, DEFAULT_MAX_FOLDERS, Exploration, ExploreOptions, ExtensionStat, FolderList,
    FolderMade, FolderStat, Location,
};
pub use pairs::{
    AddedSync, ApplyFailureView, ApplyView, FIRST_ROUND_WAIT, FirstRound, InvalidBackup,
    InvalidSync, NewSync, RemovedSync, Toggled, first_round_confirmed,
};
pub use restore::RestoreArgs;
pub use review::{ConfirmedReview, ReviewView};
pub use rules::{FilterTestView, FiltersSaved, FolderCount, after_filters_saved};
pub use settings::{SettingChanged, SettingView, SettingsView};
pub use sync::{RoundOutcome, SyncReport, SyncRounds};
pub use theme::{ThemeInfo, ThemeSet};
pub use transfer::{
    DetachedStarter, TransferOptions, TransferReport, TransferStarter, TransferView, run_transfer,
};
pub use views::{GateView, LastRunView, PairDetail, PairView};

/// How long `sync`, `review confirm`, `copy` and `move` wait for the
/// round or transfer to end before they report it as still running.
pub const DEFAULT_WAIT_SECONDS: u64 = 60;

/// Everything an operation needs from the machine. The CLI builds it with
/// [`Ops::from_env`]; tests build it with fakes at the edges.
pub struct Ops {
    pub paths: Paths,
    pub rclone: Rclone,
    pub network: Box<dyn Network>,
    pub notifier: Box<dyn Notifier>,
    pub systemctl: Arc<dyn Systemctl>,
    pub systemd: SystemdMode,
    /// The `syncfast` binary the units and detached rounds call.
    pub bin: PathBuf,
    /// The Flatpak app id when running in the sandbox (ADR 0006).
    pub flatpak_id: Option<String>,
    /// Starts a round without waiting for it.
    pub trigger: Arc<dyn Trigger>,
    /// Starts a `copy` or `move` without waiting for it.
    pub transfers: Arc<dyn TransferStarter>,
    /// `None` when restic is not installed.
    pub restic: Option<Restic>,
    /// Where backup keys live. Operations read it; none returns a key.
    pub keys: Arc<dyn KeyStore>,
    /// Starts a restore without waiting for it.
    pub restores: Arc<dyn RestoreStarter>,
}

impl std::fmt::Debug for Ops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ops")
            .field("paths", &self.paths)
            .field("rclone", &self.rclone)
            .field("systemd", &self.systemd)
            .field("bin", &self.bin)
            .finish_non_exhaustive()
    }
}

impl Ops {
    /// The real machine: XDG paths, `rclone` on `PATH`, `NetworkManager`, the
    /// user's systemd and the running window for notifications.
    ///
    /// # Errors
    /// The user directories or the `syncfast` binary cannot be located, or
    /// rclone is not installed.
    pub fn from_env() -> Result<Self, OpError> {
        let paths = Paths::from_env().map_err(|err| OpError::Failed(err.to_string()))?;
        let bin = current_binary()
            .map_err(|err| OpError::Failed(format!("cannot locate the syncfast binary: {err}")))?;
        let systemctl: Arc<dyn Systemctl> = Arc::new(UserSystemctl::detect());
        let systemd = SystemdMode::from_env();
        let rclone = Rclone::from_env()?;
        Ok(Self {
            restic: restic::locate_binary().map(|bin| Restic::new(bin, rclone.clone(), &paths)),
            keys: Arc::new(DefaultKeyStore::from_env(&paths)),
            restores: Arc::new(DetachedRestoreStarter::new(bin.clone())),
            paths,
            rclone,
            network: Box::new(NetworkManager),
            notifier: Box::new(DesktopNotifier::new()),
            trigger: trigger_for(systemd, bin.clone(), Arc::clone(&systemctl)),
            transfers: Arc::new(DetachedStarter::new(bin.clone())),
            systemctl,
            systemd,
            bin,
            flatpak_id: flatpak::app_id(),
        })
    }

    async fn load_config(&self) -> Result<Config, OpError> {
        Ok(Config::load(&self.paths).await?)
    }

    /// The sync `id` as written in the config.
    async fn find_pair(&self, id: &str) -> Result<Pair, OpError> {
        Ok(self.find_pair_and_behavior(id).await?.0)
    }

    /// The sync `id` as written in the config, with its settings resolved.
    async fn find_pair_and_behavior(&self, id: &str) -> Result<(Pair, Behavior), OpError> {
        let config = self.load_config().await?;
        let pair = config
            .pairs()
            .iter()
            .find(|pair| pair.id == id)
            .cloned()
            .ok_or_else(|| OpError::NotFound(id.to_owned()))?;
        let behavior = config.effective(&pair).behavior;
        Ok((pair, behavior))
    }

    /// `apply`, as the GUI runs it after it changes the config. Invalid
    /// syncs are not a failure here: their own status says so. What did
    /// not work is returned as warnings, since the config is already saved.
    async fn reapply(&self, delete_versions: &[String]) -> Vec<String> {
        let report = self.apply_report(delete_versions).await;
        let mut warnings = Vec::new();
        if let Some(error) = report.config_error {
            warnings.push(error);
        }
        for failure in report.failures {
            warnings.push(match failure.pair_id {
                Some(id) => format!("{id}: {}: {}", failure.command, failure.detail),
                None => format!("{}: {}", failure.command, failure.detail),
            });
        }
        warnings
    }

    async fn apply_report(&self, delete_versions: &[String]) -> crate::units::ApplyReport {
        apply(&self.apply_env(), delete_versions).await
    }

    fn apply_env(&self) -> ApplyEnv<'_> {
        ApplyEnv::new(
            &self.paths,
            &self.rclone,
            &self.bin,
            self.systemd,
            self.systemctl.as_ref(),
        )
        .with_flatpak_id(self.flatpak_id.as_deref())
    }

    fn lifecycle_env<'a>(&'a self, restic: &'a Restic) -> LifecycleEnv<'a> {
        LifecycleEnv::new(self.apply_env(), restic, &*self.keys, &*self.trigger)
    }

    /// restic, checked once it is going to be used. The Flatpak ships the
    /// version it was built for, so only a host restic is looked at.
    async fn restic(&self) -> Result<&Restic, OpError> {
        let restic = self.restic.as_ref().ok_or_else(|| {
            OpError::Failed("restic was not found; install it or reinstall syncfast".to_owned())
        })?;
        if self.flatpak_id.is_none() {
            restic
                .ensure_supported()
                .await
                .map_err(|err| OpError::Failed(err.to_string()))?;
        }
        Ok(restic)
    }
}
