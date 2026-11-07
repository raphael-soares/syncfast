//! Adding, removing, pausing and resuming syncs, and `apply`.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::sync::start_round;
use super::{OpError, Ops, PairView};
use crate::behavior::{Behavior, ReviewLevel};
use crate::config::{Config, Pair, expand_tilde, unique_id};
use crate::filters::PRESETS;
use crate::paths::Paths;
use crate::rclone::RcloneError;
use crate::state::{ConfirmMarker, GateKind, State, is_running};
use crate::watch::Trigger;

/// How long `add` waits for the round that checks the first review.
const FIRST_ROUND_WAIT: Duration = Duration::from_secs(10);

fn waits_for_review(id: &str) -> String {
    format!("Run `review show` for \"{id}\": the first sync waits for a review.")
}

/// What `add` needs to create a sync. Everything the config validates is
/// checked by the config, with its own messages.
#[derive(Debug, Clone, Default)]
pub struct NewSync {
    pub name: String,
    /// A path on this computer; `~` stands for the home folder.
    pub local: String,
    /// `account:folder`, for example `gdrive:Notes` or `gdrive:`.
    pub remote: String,
    /// Taken from the name when absent.
    pub id: Option<String>,
    /// All presets when absent.
    pub presets: Option<Vec<String>>,
    pub filters: Vec<String>,
    /// Create the local folder when it does not exist.
    pub create_local: bool,
    /// Create the remote folder when it does not exist.
    pub create_remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddedSync {
    pub sync: PairView,
    /// What `apply` could not do. The config is saved regardless.
    pub warnings: Vec<String>,
    pub next_step: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemovedSync {
    pub id: String,
    pub versions_deleted: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Toggled {
    pub syncs: Vec<PairView>,
    pub warnings: Vec<String>,
}

/// What `apply` found, for `syncfast apply --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyView {
    pub ok: bool,
    pub exit_code: u8,
    /// The config has a global problem; nothing was changed.
    pub config_error: Option<String>,
    /// Syncs that got no units because their config is invalid.
    pub invalid_syncs: Vec<InvalidSync>,
    pub failures: Vec<ApplyFailureView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvalidSync {
    pub id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyFailureView {
    pub id: Option<String>,
    pub command: String,
    pub detail: String,
}

impl Ops {
    /// Reconciles the systemd units with `config.toml`, also deleting the
    /// versions of the removed syncs in `delete_versions`.
    pub async fn apply(&self, delete_versions: &[String]) -> ApplyView {
        let report = self.apply_report(delete_versions).await;
        ApplyView {
            ok: report.is_empty(),
            exit_code: report.exit_code(),
            config_error: report.config_error,
            invalid_syncs: report
                .invalid_pairs
                .into_iter()
                .map(|error| InvalidSync {
                    id: error.pair_id,
                    message: error.message,
                })
                .collect(),
            failures: report
                .failures
                .into_iter()
                .map(|failure| ApplyFailureView {
                    id: failure.pair_id,
                    command: failure.command,
                    detail: failure.detail,
                })
                .collect(),
        }
    }

    /// Creates a sync. It starts held for the review of its first round:
    /// nothing is copied until the review is confirmed.
    ///
    /// # Errors
    /// The config rejects the sync (its own message), the local folder is
    /// missing and `create_local` is off, the remote folder is missing and
    /// `create_remote` is off, or rclone or the config file fails.
    pub async fn add(&self, new: NewSync) -> Result<AddedSync, OpError> {
        let mut config = self.load_config().await?;
        let name = new.name.trim().to_owned();
        let id = new.id.unwrap_or_else(|| {
            unique_id(&name, config.pairs().iter().map(|pair| pair.id.as_str()))
        });
        let local = expand_tilde(Path::new(new.local.trim()), &self.paths.home);
        let presets = new.presets.unwrap_or_else(|| {
            PRESETS
                .iter()
                .map(|preset| preset.name.to_owned())
                .collect()
        });
        let pair = Pair {
            id: id.clone(),
            name,
            local: local.clone(),
            remote: new.remote.trim().to_owned(),
            enabled: true,
            presets,
            filters: new.filters,
            overrides: crate::settings::Overrides::default(),
        };
        config.add_pair(pair.clone())?;
        let accounts = self.rclone.listremotes().await?;
        let validation = config.validate(&self.paths, &accounts);
        if let Some(error) = validation.errors.iter().find(|error| error.pair_id == id) {
            return Err(OpError::Invalid(error.message.clone()));
        }
        prepare_local(&local, new.create_local).await?;
        self.prepare_remote(new.remote.trim(), new.create_remote)
            .await?;
        let level = Behavior::for_pair(&config.defaults(), &pair).review;
        Config::edit(&self.paths, |config| config.add_pair(pair)).await??;
        let mut warnings = self.reapply(&[]).await;
        let next_step = if level == ReviewLevel::Automatic {
            match first_round_confirmed(&self.paths, &*self.trigger, &id).await {
                Ok(true) => "Syncing started: review confirmed automatically.".to_owned(),
                Ok(false) => waits_for_review(&id),
                Err(error) => {
                    warnings.push(format!("The first round did not start: {error}"));
                    waits_for_review(&id)
                }
            }
        } else {
            waits_for_review(&id)
        };
        Ok(AddedSync {
            sync: self.pair_view(&id).await?,
            warnings,
            next_step,
        })
    }

    async fn prepare_remote(&self, remote: &str, create: bool) -> Result<(), OpError> {
        if create {
            return Ok(self.rclone.mkdir(remote).await?);
        }
        match self.rclone.list_dirs(remote).await {
            Ok(_) => Ok(()),
            Err(RcloneError::Failed { detail, .. }) if detail.contains("not found") => {
                Err(OpError::Invalid(format!(
                    "the remote folder \"{remote}\" does not exist; create it first or ask to create it"
                )))
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Takes a sync out of the config; its units, state and filters go
    /// with `apply`. Files on both sides stay.
    ///
    /// # Errors
    /// No such sync, or the config cannot be saved.
    pub async fn remove(&self, id: &str, delete_versions: bool) -> Result<RemovedSync, OpError> {
        if !Config::edit(&self.paths, |config| config.remove_pair(id)).await? {
            return Err(OpError::NotFound(id.to_owned()));
        }
        let versions = if delete_versions {
            vec![id.to_owned()]
        } else {
            Vec::new()
        };
        Ok(RemovedSync {
            id: id.to_owned(),
            versions_deleted: delete_versions,
            warnings: self.reapply(&versions).await,
        })
    }

    /// Pauses (`enabled = false`) or resumes the sync `id`, or every sync
    /// when `id` is `None`.
    ///
    /// # Errors
    /// No such sync, or the config cannot be saved.
    pub async fn set_enabled(&self, id: Option<&str>, enabled: bool) -> Result<Toggled, OpError> {
        let ids = Config::edit(&self.paths, |config| {
            let ids: Vec<String> = match id {
                Some(id) => vec![id.to_owned()],
                None => config.pairs().iter().map(|pair| pair.id.clone()).collect(),
            };
            if let Some(missing) = ids
                .iter()
                .find(|id| !config.pairs().iter().any(|pair| &pair.id == *id))
            {
                return Err(OpError::NotFound(missing.clone()));
            }
            for id in &ids {
                config.set_enabled(id, enabled);
            }
            Ok(ids)
        })
        .await??;
        let warnings = self.reapply(&[]).await;
        let mut syncs = Vec::with_capacity(ids.len());
        for id in &ids {
            syncs.push(self.pair_view(id).await?);
        }
        Ok(Toggled { syncs, warnings })
    }
}

/// Starts the first round of a sync whose review level is `automatic`, waits
/// for its check and says whether it left no review waiting: `false` when
/// the dry run failed or held the first review for the user.
///
/// # Errors
/// The round cannot be started.
pub async fn first_round_confirmed(
    paths: &Paths,
    trigger: &dyn Trigger,
    id: &str,
) -> Result<bool, OpError> {
    start_round(paths, trigger, id, FIRST_ROUND_WAIT).await?;
    let state = State::load_or_config_error(paths, id).await;
    let confirmed = matches!(ConfirmMarker::load(paths, id).await, Ok(Some(_)));
    let waiting = state
        .gate
        .is_some_and(|gate| gate.kind == GateKind::NeedsReviewFirstRun)
        && !is_running(paths, id)
        && !confirmed;
    Ok(!waiting)
}

async fn prepare_local(local: &Path, create: bool) -> Result<(), OpError> {
    match tokio::fs::metadata(local).await {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(OpError::Invalid(format!(
            "{} is not a folder",
            local.display()
        ))),
        Err(_) if create => tokio::fs::create_dir_all(local)
            .await
            .map_err(|err| OpError::Failed(format!("cannot create {}: {err}", local.display()))),
        Err(_) => Err(OpError::Invalid(format!(
            "{} does not exist; create it first or ask to create it",
            local.display()
        ))),
    }
}
