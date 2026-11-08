//! The operations behind the backup sheets (spec phase 3, steps 2, 11 to
//! 13): create, add an existing backup, enter or show the key, remove, save
//! settings. The GUI and the agent operations call them; the key reaches
//! nothing but the key store and restic.

use std::path::PathBuf;

use crate::behavior::Bandwidth;
use crate::bisync::Fault;
use crate::config::{Config, unique_id};
use crate::paths::Paths;
use crate::rclone::Rclone;
use crate::restic::{Repository, Restic, ResticError};
use crate::state::GateKind;
use crate::units::{ApplyEnv, ApplyReport, apply};
use crate::watch::Trigger;

use super::config::{
    BackupBehavior, BackupConfig, Every, Keep, KeepSetting, folder_problem, id_problem,
};
use super::destination::{
    Destination, DestinationError, RepositoryError, create_repository, is_empty, open_repository,
};
use super::effects::{self, BackupEvent, gate_key};
use super::key::{Key, KeyError};
use super::keystore::{KeyBackend, KeyStore};
use super::state::BackupState;

/// The id of the item `test_key_storage` writes. Never a backup id: drafts
/// with it are refused, and `backup_id` never picks it.
pub const TEST_ITEM_ID: &str = "key-storage-test";

pub struct LifecycleEnv<'a> {
    pub apply: ApplyEnv<'a>,
    pub restic: &'a Restic,
    pub keys: &'a dyn KeyStore,
    pub trigger: &'a dyn Trigger,
}

impl std::fmt::Debug for LifecycleEnv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleEnv").finish_non_exhaustive()
    }
}

impl<'a> LifecycleEnv<'a> {
    #[must_use]
    pub fn new(
        apply: ApplyEnv<'a>,
        restic: &'a Restic,
        keys: &'a dyn KeyStore,
        trigger: &'a dyn Trigger,
    ) -> Self {
        Self {
            apply,
            restic,
            keys,
            trigger,
        }
    }

    fn paths(&self) -> &'a Paths {
        self.apply.paths
    }

    fn rclone(&self) -> &'a Rclone {
        self.apply.rclone
    }
}

/// What the New Backup wizard collected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBackup {
    pub name: String,
    pub id: String,
    pub paths: Vec<PathBuf>,
    pub dest: String,
    pub every: Every,
    pub keep: Keep,
    pub presets: Vec<String>,
    pub filters: Vec<String>,
    pub one_file_system: bool,
    pub limit_upload: Bandwidth,
    pub limit_download: Bandwidth,
}

impl NewBackup {
    #[must_use]
    pub fn with_defaults(name: String, id: String, paths: Vec<PathBuf>, dest: String) -> Self {
        let behavior = BackupBehavior::default();
        Self {
            name,
            id,
            paths,
            dest,
            every: behavior.every,
            keep: behavior.keep,
            presets: behavior.presets,
            filters: Vec::new(),
            one_file_system: behavior.one_file_system,
            limit_upload: behavior.limit_upload,
            limit_download: behavior.limit_download,
        }
    }

    #[must_use]
    pub fn to_config(&self) -> BackupConfig {
        BackupConfig {
            id: self.id.clone(),
            name: self.name.clone(),
            paths: self.paths.clone(),
            dest: self.dest.clone(),
            every: self.every.as_str().to_owned(),
            keep: KeepSetting::new(
                i64::from(self.keep.daily),
                i64::from(self.keep.weekly),
                i64::from(self.keep.monthly),
                i64::from(self.keep.yearly),
            ),
            presets: self.presets.clone(),
            filters: self.filters.clone(),
            one_file_system: self.one_file_system,
            limit_upload: self.limit_upload.format(),
            limit_download: self.limit_download.format(),
            enabled: true,
        }
    }
}

/// What the Add Existing Backup sheet collected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingBackup {
    pub name: String,
    pub id: String,
    pub dest: String,
}

impl ExistingBackup {
    /// Disabled, with no folders: the person picks them afterwards.
    #[must_use]
    pub fn to_config(&self) -> BackupConfig {
        let mut config = NewBackup::with_defaults(
            self.name.clone(),
            self.id.clone(),
            Vec::new(),
            self.dest.clone(),
        )
        .to_config();
        config.enabled = false;
        config
    }
}

/// The first problem of each wizard step, `None` where the step is fine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftProblems {
    pub id: Option<String>,
    pub name: Option<String>,
    pub folders: Option<String>,
    pub destination: Option<String>,
    pub schedule: Option<String>,
    pub filters: Option<String>,
}

impl DraftProblems {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.first().is_none()
    }

    /// In the order of the spec's table.
    #[must_use]
    pub fn first(&self) -> Option<&str> {
        [
            &self.id,
            &self.name,
            &self.folders,
            &self.destination,
            &self.schedule,
            &self.filters,
        ]
        .into_iter()
        .find_map(|problem| problem.as_deref())
    }
}

/// Warnings of the steps that follow the change itself: the change is done
/// and stands even when the list is not empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedBackup {
    pub id: String,
    /// What the process that created the backup tells the person.
    pub events: Vec<BackupEvent>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Key(KeyError),
    #[error("{0}")]
    Repository(RepositoryError),
    #[error(
        "The backup was created at {dest}, but syncfast could not save it. Add it with Add Existing Backup…"
    )]
    NotSaved { dest: String },
}

#[derive(Debug, thiserror::Error)]
pub enum AddExistingError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Open(ResticError),
    #[error("{0}")]
    Key(KeyError),
    #[error("Cannot save the backup: {0}")]
    NotSaved(String),
}

#[derive(Debug, thiserror::Error)]
pub enum EnterKeyError {
    #[error("There is no backup \"{0}\".")]
    Unknown(String),
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Open(ResticError),
    #[error("{0}")]
    Key(KeyError),
    #[error("{0}")]
    State(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ShowKeyError {
    #[error("The key for this backup is missing on this computer.")]
    Missing,
    #[error("{0}")]
    Storage(KeyError),
}

#[derive(Debug, thiserror::Error)]
pub enum RemoveError {
    #[error("There is no backup \"{0}\".")]
    Unknown(String),
    #[error("{0}")]
    Config(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SaveSettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("There is no backup \"{0}\".")]
    Unknown(String),
    #[error("{0}")]
    Config(String),
}

/// The id a backup named `name` gets among the ids in `taken`: its own
/// namespace, apart from the syncs'.
#[must_use]
pub fn backup_id<'a>(name: &str, taken: impl IntoIterator<Item = &'a str>) -> String {
    let base = if name.chars().any(|c| c.is_ascii_alphanumeric()) {
        name
    } else {
        "backup"
    };
    unique_id(base, taken.into_iter().chain([TEST_ITEM_ID]))
}

pub async fn new_backup_id(paths: &Paths, name: &str) -> String {
    let config = Config::load(paths).await.ok();
    let backups = config.as_ref().map_or(&[][..], Config::backups);
    backup_id(name, backups.iter().map(|b| b.id.as_str()))
}

/// The wizard's checks of one draft against the config on disk and the
/// rclone accounts; nothing is written and no folder is looked at except
/// through the accounts list.
///
/// # Errors
/// The config or the account list cannot be read.
pub async fn validate_draft(
    env: &LifecycleEnv<'_>,
    draft: &NewBackup,
) -> Result<DraftProblems, String> {
    let (config, accounts) = surroundings(env).await?;
    Ok(draft_problems(
        env.paths(),
        &config,
        &accounts,
        &draft.to_config(),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationTarget {
    /// The wizard creates the repository: the folder must be empty.
    New,
    /// Add Existing Backup: the contents are checked when the key opens it.
    Existing,
}

/// The destination step: overlaps with syncs, other backups and the folders
/// being backed up, then for a new repository that the folder is empty and
/// can be looked at.
///
/// # Errors
/// The first problem, worded for the person.
pub async fn check_destination(
    env: &LifecycleEnv<'_>,
    dest: &str,
    folders: &[PathBuf],
    target: DestinationTarget,
) -> Result<(), String> {
    let (config, accounts) = surroundings(env).await?;
    if let Some(problem) = destination_problem(env.paths(), &config, &accounts, dest, folders) {
        return Err(problem);
    }
    if target == DestinationTarget::New {
        let destination = Destination::parse(dest).map_err(|err| err.to_string())?;
        match is_empty(env.rclone(), &destination).await {
            Ok(true) => {}
            Ok(false) => return Err(RepositoryError::NotEmpty.to_string()),
            Err(err) => return Err(err.to_string()),
        }
    }
    Ok(())
}

/// Spec step 12, item 7.
///
/// # Errors
/// See [`CreateError`]. After `NotSaved` the key and the repository stay.
pub async fn create_backup(
    env: &LifecycleEnv<'_>,
    draft: NewBackup,
    key: Key,
    key_unsaved: bool,
) -> Result<CreatedBackup, CreateError> {
    let backup = draft.to_config();
    let (config, accounts) = surroundings(env).await.map_err(CreateError::Invalid)?;
    if let Some(problem) = draft_problems(env.paths(), &config, &accounts, &backup).first() {
        return Err(CreateError::Invalid(problem.to_owned()));
    }
    let repository =
        repository_of(env.paths(), &backup).map_err(|err| CreateError::Invalid(err.to_string()))?;

    env.keys
        .put(&backup.id, &backup.name, &key)
        .await
        .map_err(CreateError::Key)?;
    if let Err(err) = create_repository(env.restic, &repository, &key).await {
        if let Err(delete) = env.keys.delete(&backup.id).await {
            log::warn!("cannot delete the key of backup {}: {delete}", backup.id);
        }
        return Err(CreateError::Repository(err));
    }

    if let Err(reason) = add_to_config(env.paths(), &backup).await {
        log::warn!("backup {} was created but not saved: {reason}", backup.id);
        return Err(CreateError::NotSaved { dest: backup.dest });
    }

    let mut warnings = Vec::new();
    let mut events = Vec::new();
    if key_unsaved {
        match update_state(env.paths(), &backup.id, |state| {
            events = effects::key_created_by_agent(state);
        })
        .await
        {
            Ok(()) => {}
            Err(reason) => warnings.push(reason),
        }
    }
    warnings.extend(apply_warnings(&apply(&env.apply, &[]).await));
    if let Err(reason) = env.trigger.start_backup(&backup.id).await {
        warnings.push(reason);
    }
    Ok(CreatedBackup {
        id: backup.id,
        events,
        warnings,
    })
}

/// Spec step 12, item 8: the repository exists; the key opens it.
///
/// # Errors
/// See [`AddExistingError`].
pub async fn add_existing_backup(
    env: &LifecycleEnv<'_>,
    draft: &ExistingBackup,
    key: Key,
) -> Result<Applied, AddExistingError> {
    let backup = draft.to_config();
    let (config, accounts) = surroundings(env).await.map_err(AddExistingError::Invalid)?;
    if let Some(problem) = draft_problems(env.paths(), &config, &accounts, &backup).first() {
        return Err(AddExistingError::Invalid(problem.to_owned()));
    }
    let repository = repository_of(env.paths(), &backup)
        .map_err(|err| AddExistingError::Invalid(err.to_string()))?;
    open_repository(env.restic, &repository, &key)
        .await
        .map_err(AddExistingError::Open)?;
    env.keys
        .put(&backup.id, &backup.name, &key)
        .await
        .map_err(AddExistingError::Key)?;
    if let Err(reason) = add_to_config(env.paths(), &backup).await {
        if let Err(delete) = env.keys.delete(&backup.id).await {
            log::warn!("cannot delete the key of backup {}: {delete}", backup.id);
        }
        return Err(AddExistingError::NotSaved(reason));
    }
    Ok(Applied {
        warnings: apply_warnings(&apply(&env.apply, &[]).await),
    })
}

/// Spec step 13, `Enter Key…`: the key is checked against the repository,
/// kept, the gate it lifts is cleared, and a run starts.
///
/// # Errors
/// See [`EnterKeyError`]; nothing changes when the key does not open the
/// repository.
pub async fn enter_key(
    env: &LifecycleEnv<'_>,
    backup_id: &str,
    key: Key,
) -> Result<Applied, EnterKeyError> {
    let config = Config::load(env.paths())
        .await
        .map_err(|err| EnterKeyError::Config(err.to_string()))?;
    let backup = config
        .backups()
        .iter()
        .find(|backup| backup.id == backup_id)
        .ok_or_else(|| EnterKeyError::Unknown(backup_id.to_owned()))?;
    let repository =
        repository_of(env.paths(), backup).map_err(|err| EnterKeyError::Config(err.to_string()))?;
    open_repository(env.restic, &repository, &key)
        .await
        .map_err(EnterKeyError::Open)?;
    env.keys
        .put(&backup.id, &backup.name, &key)
        .await
        .map_err(EnterKeyError::Key)?;
    update_state(env.paths(), backup_id, |state| {
        let key_gate = state.gate.as_ref().is_some_and(|gate| {
            gate.kind == GateKind::NeedsAttention
                && matches!(gate.fault, Some(Fault::KeyMissing | Fault::WrongKey))
        });
        if key_gate {
            effects::clear_gate(state);
        }
        for fault in [Fault::KeyMissing, Fault::WrongKey] {
            let notified = gate_key(GateKind::NeedsAttention, Some(fault));
            state.notified.retain(|key| *key != notified);
        }
    })
    .await
    .map_err(EnterKeyError::State)?;
    let mut warnings = Vec::new();
    if let Err(reason) = env.trigger.start_backup(backup_id).await {
        warnings.push(reason);
    }
    Ok(Applied { warnings })
}

/// The person saved the key: the warning goes away.
///
/// # Errors
/// The state file cannot be read or written.
pub async fn mark_key_saved(paths: &Paths, backup_id: &str) -> Result<(), String> {
    update_state(paths, backup_id, effects::key_saved).await
}

/// The key for `Show Key`. The only way to read a key back; the caller
/// shows it and drops it.
///
/// # Errors
/// There is no key on this computer, or the key storage cannot be reached.
pub async fn show_key(keys: &dyn KeyStore, backup_id: &str) -> Result<Key, ShowKeyError> {
    keys.get(backup_id)
        .await
        .map_err(ShowKeyError::Storage)?
        .ok_or(ShowKeyError::Missing)
}

/// Takes the backup out of the config and its units away. The repository
/// stays; the key too unless `delete_key`.
///
/// # Errors
/// See [`RemoveError`].
pub async fn remove_backup(
    env: &LifecycleEnv<'_>,
    backup_id: &str,
    delete_key: bool,
) -> Result<Applied, RemoveError> {
    let removed = Config::edit(env.paths(), |config| config.remove_backup(backup_id))
        .await
        .map_err(|err| RemoveError::Config(err.to_string()))?;
    if !removed {
        return Err(RemoveError::Unknown(backup_id.to_owned()));
    }
    let mut warnings = apply_warnings(&apply(&env.apply, &[]).await);
    if delete_key && let Err(err) = env.keys.delete(backup_id).await {
        warnings.push(err.to_string());
    }
    Ok(Applied { warnings })
}

/// Writes the settings of a backup that exists; its id and destination
/// never change.
///
/// # Errors
/// See [`SaveSettingsError`].
pub async fn save_backup_settings(
    env: &LifecycleEnv<'_>,
    replacement: &BackupConfig,
) -> Result<Applied, SaveSettingsError> {
    let config = Config::load(env.paths())
        .await
        .map_err(|err| SaveSettingsError::Config(err.to_string()))?;
    let stored = config
        .backups()
        .iter()
        .find(|backup| backup.id == replacement.id)
        .ok_or_else(|| SaveSettingsError::Unknown(replacement.id.clone()))?;
    let candidate = BackupConfig {
        dest: stored.dest.clone(),
        ..replacement.clone()
    };
    if let Some(problem) = content_problems(&candidate).first() {
        return Err(SaveSettingsError::Invalid(problem.to_owned()));
    }
    let replaced = Config::edit(env.paths(), |config| config.replace_backup(&candidate))
        .await
        .map_err(|err| SaveSettingsError::Config(err.to_string()))?;
    if !replaced {
        return Err(SaveSettingsError::Unknown(candidate.id));
    }
    Ok(Applied {
        warnings: apply_warnings(&apply(&env.apply, &[]).await),
    })
}

/// Writes, reads and deletes a test item and tells which storage took it.
///
/// # Errors
/// The storage cannot be reached or does not give the item back.
pub async fn test_key_storage(keys: &dyn KeyStore) -> Result<KeyBackend, KeyError> {
    let item = Key::generate()?;
    keys.put(TEST_ITEM_ID, "key storage test", &item).await?;
    let read = keys.get(TEST_ITEM_ID).await;
    let deleted = keys.delete(TEST_ITEM_ID).await;
    if read? != Some(item) {
        return Err(KeyError::Unavailable(
            "the test item could not be read back".to_owned(),
        ));
    }
    deleted?;
    match keys.backend() {
        Some(backend) => Ok(backend),
        None => keys.probe().await,
    }
}

async fn surroundings(env: &LifecycleEnv<'_>) -> Result<(Config, Vec<String>), String> {
    let config = Config::load(env.paths())
        .await
        .map_err(|err| err.to_string())?;
    let accounts = env
        .rclone()
        .listremotes()
        .await
        .map_err(|err| err.to_string())?;
    Ok((config, accounts))
}

pub(crate) fn repository_of(
    paths: &Paths,
    backup: &BackupConfig,
) -> Result<Repository, DestinationError> {
    let destination = Destination::parse(&backup.dest)?;
    let (upload, download) = backup
        .behavior()
        .map_or((Bandwidth::Off, Bandwidth::Off), |behavior| {
            (behavior.limit_upload, behavior.limit_download)
        });
    Ok(
        Repository::new(destination, paths.backup_state_dir(&backup.id))
            .with_limits(upload, download),
    )
}

fn content_problems(backup: &BackupConfig) -> DraftProblems {
    DraftProblems {
        name: backup.name_problem(),
        folders: folder_problem(backup),
        schedule: backup.behavior().err(),
        filters: backup.filters_problem(),
        ..DraftProblems::default()
    }
}

fn draft_problems(
    paths: &Paths,
    config: &Config,
    accounts: &[String],
    draft: &BackupConfig,
) -> DraftProblems {
    let taken = config.backups().iter().any(|backup| backup.id == draft.id);
    let id = id_problem(&draft.id).or_else(|| {
        if draft.id == TEST_ITEM_ID {
            Some(format!("backup id \"{}\" is reserved", draft.id))
        } else {
            taken.then(|| format!("duplicate backup id \"{}\"", draft.id))
        }
    });
    let mut problems = content_problems(draft);
    problems.id = id;
    let folders: &[PathBuf] = if problems.folders.is_none() {
        &draft.paths
    } else {
        &[]
    };
    problems.destination = destination_problem(paths, config, accounts, &draft.dest, folders);
    problems
}

fn destination_problem(
    paths: &Paths,
    config: &Config,
    accounts: &[String],
    dest: &str,
    folders: &[PathBuf],
) -> Option<String> {
    let mut neutral = NewBackup::with_defaults(
        "draft".to_owned(),
        TEST_ITEM_ID.to_owned(),
        folders.to_vec(),
        dest.to_owned(),
    )
    .to_config();
    neutral.enabled = false;
    config.validate_backup_draft(&neutral, paths, accounts)
}

async fn add_to_config(paths: &Paths, backup: &BackupConfig) -> Result<(), String> {
    Config::edit(paths, |config| {
        if config.backups().iter().any(|other| other.id == backup.id) {
            return Err(format!("duplicate backup id \"{}\"", backup.id));
        }
        config.add_backup(backup).map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| err.to_string())?
}

async fn update_state(
    paths: &Paths,
    backup_id: &str,
    change: impl FnOnce(&mut BackupState),
) -> Result<(), String> {
    let mut state = BackupState::load(paths, backup_id)
        .await
        .map_err(|err| err.to_string())?;
    change(&mut state);
    state
        .save(paths, backup_id)
        .await
        .map_err(|err| err.to_string())
}

fn apply_warnings(report: &ApplyReport) -> Vec<String> {
    report.to_string().lines().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_without_letters_gets_a_backup_id_and_never_the_reserved_one() {
        assert_eq!(backup_id("***", []), "backup");
        assert_eq!(backup_id("***", ["backup"]), "backup-2");
        assert_eq!(backup_id("Key storage test", []), "key-storage-test-2");
    }

    #[test]
    fn a_taken_id_gets_a_number_and_the_name_is_slugged() {
        assert_eq!(backup_id("Docs & Photos", []), "docs-photos");
        assert_eq!(backup_id("Docs & Photos", ["docs-photos"]), "docs-photos-2");
    }

    #[tokio::test]
    async fn a_new_id_avoids_the_ids_in_the_config_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        );
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            file,
            "version = 3\n\n[[backup]]\nid = \"docs\"\nname = \"Docs\"\npaths = [\"/work/docs\"]\ndest = \"/backups/docs\"\n",
        )
        .unwrap();

        assert_eq!(new_backup_id(&paths, "Docs").await, "docs-2");
        assert_eq!(new_backup_id(&paths, "Photos").await, "photos");
    }

    #[test]
    fn a_draft_with_no_problem_is_empty_and_one_with_any_is_not() {
        assert!(DraftProblems::default().is_empty());
        let problems = DraftProblems {
            filters: Some("bad".to_owned()),
            ..DraftProblems::default()
        };

        assert!(!problems.is_empty());
        assert_eq!(problems.first(), Some("bad"));
    }
}
