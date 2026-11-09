//! The backup lifecycle operations (create, add existing, key, remove,
//! settings) against a real restic and rclone on `type = local` remotes,
//! with the key store in memory and systemd off.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use jiff::Timestamp;
use syncfast_core::backup::config::{BackupConfig, filter_line_problem, keep_reduced};
use syncfast_core::backup::destination::{Destination, create_repository};
use syncfast_core::backup::effects::BackupEvent;
use syncfast_core::backup::key::{Key, KeyError};
use syncfast_core::backup::keystore::{KeyBackend, KeyStore, MemoryKeyStore};
use syncfast_core::backup::lifecycle::{
    AddExistingError, CreateError, DestinationTarget, EnterKeyError, ExistingBackup, LifecycleEnv,
    NewBackup, RemoveError, SaveSettingsError, ShowKeyError, TEST_ITEM_ID, add_existing_backup,
    check_destination, create_backup, enter_key, mark_key_saved, remove_backup,
    save_backup_settings, show_key, test_key_storage, validate_draft,
};
use syncfast_core::backup::state::BackupState;
use syncfast_core::bisync::Fault;
use syncfast_core::config::Config;
use syncfast_core::network::BoxFuture;
use syncfast_core::paths::Paths;
use syncfast_core::rclone::{Rclone, RcloneError};
use syncfast_core::restic::{Repository, Restic, ResticError};
use syncfast_core::state::{Gate, GateKind};
use syncfast_core::units::{ApplyEnv, Systemctl, SystemdMode};
use syncfast_core::watch::Trigger;

const BIN: &str = "/opt/syncfast/syncfast";
const DEADLINE: Duration = Duration::from_secs(90);

fn paths_under(root: &Path) -> Paths {
    Paths::new(
        root.join("home"),
        root.join("config"),
        root.join("state"),
        root.join("data"),
        root.join("run"),
    )
}

struct NoSystemctl;

impl Systemctl for NoSystemctl {
    fn run<'a>(&'a self, _args: &'a [&'a str]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// Records the backups it was asked to start and, at that moment, whether
/// their state already said the key is unsaved.
struct FakeTrigger {
    paths: Paths,
    started: Mutex<Vec<(String, bool)>>,
}

impl Trigger for FakeTrigger {
    fn start<'a>(&'a self, _pair_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn start_backup<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let unsaved = BackupState::load(&self.paths, backup_id)
                .await
                .unwrap()
                .key_unsaved;
            self.started
                .lock()
                .unwrap()
                .push((backup_id.to_owned(), unsaved));
            Ok(())
        })
    }
}

/// A key storage that cannot keep anything.
#[derive(Debug, Default)]
struct RefusingKeyStore {
    inner: MemoryKeyStore,
}

impl KeyStore for RefusingKeyStore {
    fn get<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Key>, KeyError>> {
        self.inner.get(id)
    }

    fn put<'a>(
        &'a self,
        _id: &'a str,
        _name: &'a str,
        _key: &'a Key,
    ) -> BoxFuture<'a, Result<(), KeyError>> {
        Box::pin(async { Err(KeyError::Unavailable("locked".to_owned())) })
    }

    fn delete<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), KeyError>> {
        self.inner.delete(id)
    }

    fn backend(&self) -> Option<KeyBackend> {
        None
    }

    fn probe(&self) -> BoxFuture<'_, Result<KeyBackend, KeyError>> {
        Box::pin(async { Err(KeyError::Unavailable("locked".to_owned())) })
    }
}

struct Bed {
    _dir: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
    restic: Restic,
    bin: PathBuf,
    trigger: FakeTrigger,
    keys: MemoryKeyStore,
}

#[allow(clippy::print_stderr)]
async fn bed() -> Option<Bed> {
    let found = match Rclone::from_env() {
        Err(RcloneError::RcloneNotFound) => {
            eprintln!("skipping: rclone is not installed");
            return None;
        }
        other => other.unwrap(),
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let conf = root.join("rclone.conf");
    fs::write(&conf, "[testremote]\ntype = local\n").unwrap();
    let paths = paths_under(&root);
    let rclone = Rclone::new(found.bin().to_owned()).with_config(conf);
    let restic = match Restic::from_env(rclone, &paths).await {
        Err(ResticError::NotFound) => {
            eprintln!("skipping: restic is not installed");
            return None;
        }
        other => other.unwrap(),
    };
    Some(Bed {
        trigger: FakeTrigger {
            paths: paths_under(&root),
            started: Mutex::new(Vec::new()),
        },
        _dir: dir,
        root,
        paths,
        restic,
        bin: PathBuf::from(BIN),
        keys: MemoryKeyStore::default(),
    })
}

impl Bed {
    fn env<'a>(&'a self, keys: &'a dyn KeyStore) -> LifecycleEnv<'a> {
        let apply = ApplyEnv::new(
            &self.paths,
            self.restic.rclone(),
            &self.bin,
            SystemdMode::Off,
            &NoSystemctl,
        );
        LifecycleEnv::new(apply, &self.restic, keys, &self.trigger)
    }

    fn repo_dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn account_dest(&self, name: &str) -> String {
        format!("testremote:{}", self.repo_dir(name).display())
    }

    fn folder(&self, name: &str) -> PathBuf {
        let dir = self.root.join("data").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("note.txt"), "hello").unwrap();
        dir
    }

    fn draft(&self, id: &str) -> NewBackup {
        NewBackup::with_defaults(
            format!("Backup {id}"),
            id.to_owned(),
            vec![self.folder(id)],
            self.account_dest(&format!("repo-{id}")),
        )
    }

    fn repository(&self, id: &str, dest: &str) -> Repository {
        Repository::new(
            Destination::parse(dest).unwrap(),
            self.paths.backup_state_dir(id),
        )
    }

    fn started(&self) -> Vec<(String, bool)> {
        self.trigger.started.lock().unwrap().clone()
    }

    async fn config(&self) -> Config {
        Config::load(&self.paths).await.unwrap()
    }

    fn preview_units(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.paths.units_preview_dir())
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with("syncfast-backup_"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    async fn create(&self, id: &str) -> Key {
        let key = Key::generate().unwrap();
        create_backup(&self.env(&self.keys), self.draft(id), key.clone(), false)
            .await
            .unwrap();
        key
    }
}

#[allow(clippy::print_stderr)]
fn skipping(why: &str) {
    eprintln!("skipping: {why}");
}

macro_rules! deadline_tests {
    ($(async fn $name:ident() $body:block)*) => {
        $(
            #[tokio::test]
            async fn $name() {
                tokio::time::timeout(DEADLINE, async $body)
                    .await
                    .expect("restic or rclone hung past the deadline");
            }
        )*
    };
}

deadline_tests! {
async fn create_writes_config_v3_and_the_units_and_asks_for_the_first_run() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();

    let created = create_backup(&bed.env(&bed.keys), bed.draft("docs"), key.clone(), false)
        .await
        .unwrap();

    assert_eq!(created.id, "docs");
    assert!(created.warnings.is_empty(), "{:?}", created.warnings);
    assert!(created.events.is_empty());
    let text = fs::read_to_string(bed.paths.config_file()).unwrap();
    assert!(text.contains("version = 3"), "{text}");
    let config = bed.config().await;
    assert_eq!(config.backups().len(), 1);
    assert!(config.backups()[0].enabled);
    assert_eq!(
        bed.preview_units(),
        ["syncfast-backup_docs.service", "syncfast-backup_docs.timer"]
    );
    assert!(bed.repo_dir("repo-docs").join("config").is_file());
    assert_eq!(bed.keys.get("docs").await.unwrap(), Some(key));
    assert_eq!(bed.started(), [("docs".to_owned(), false)]);
}

async fn an_unsaved_key_is_in_the_state_before_the_first_run_starts() {
    let Some(bed) = bed().await else { return };

    let created = create_backup(
        &bed.env(&bed.keys),
        bed.draft("docs"),
        Key::generate().unwrap(),
        true,
    )
    .await
    .unwrap();

    assert_eq!(bed.started(), [("docs".to_owned(), true)]);
    assert_eq!(created.events, [BackupEvent::KeyUnsaved]);
    let state = BackupState::load(&bed.paths, "docs").await.unwrap();
    assert!(state.key_unsaved);
    assert!(state.notified.contains(&"key_unsaved".to_owned()));
}

async fn a_key_storage_that_refuses_the_key_leaves_no_repository_and_no_backup() {
    let Some(bed) = bed().await else { return };
    let keys = RefusingKeyStore::default();

    let err = create_backup(&bed.env(&keys), bed.draft("docs"), Key::generate().unwrap(), false)
        .await
        .unwrap_err();

    assert!(matches!(err, CreateError::Key(_)), "{err}");
    assert!(!bed.repo_dir("repo-docs").exists());
    assert!(bed.config().await.backups().is_empty());
    assert!(bed.started().is_empty());
}

async fn a_repository_that_cannot_be_created_deletes_the_key_and_writes_no_config() {
    let Some(bed) = bed().await else { return };
    let repo = bed.repo_dir("repo-docs");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("something"), "x").unwrap();

    let err = create_backup(&bed.env(&bed.keys), bed.draft("docs"), Key::generate().unwrap(), false)
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "The destination folder is not empty.");
    assert!(bed.keys.get("docs").await.unwrap().is_none());
    assert!(bed.config().await.backups().is_empty());
    assert!(bed.started().is_empty());
}

async fn a_config_that_cannot_be_saved_keeps_the_key_and_the_repository() {
    let Some(bed) = bed().await else { return };
    let config_dir = bed.paths.config_dir();
    fs::create_dir_all(&config_dir).unwrap();
    fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(config_dir.join("probe"), "").is_ok() {
        fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let key = Key::generate().unwrap();
    let draft = bed.draft("docs");
    let dest = draft.dest.clone();

    let err = create_backup(&bed.env(&bed.keys), draft, key.clone(), false)
        .await
        .unwrap_err();
    fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(
        err.to_string(),
        format!(
            "The backup was created at {dest}, but syncfast could not save it. Add it with Add Existing Backup…"
        )
    );
    assert_eq!(bed.keys.get("docs").await.unwrap(), Some(key));
    assert!(bed.repo_dir("repo-docs").join("config").is_file());
    assert!(bed.started().is_empty());
}

async fn an_invalid_draft_changes_nothing() {
    let Some(bed) = bed().await else { return };
    let mut draft = bed.draft("docs");
    draft.paths.clear();

    let err = create_backup(&bed.env(&bed.keys), draft, Key::generate().unwrap(), false)
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "a backup needs at least one folder");
    assert!(bed.keys.get("docs").await.unwrap().is_none());
    assert!(!bed.repo_dir("repo-docs").exists());
}

async fn the_draft_is_checked_against_the_other_backups_and_the_filters() {
    let Some(bed) = bed().await else { return };
    bed.create("docs").await;
    let env = bed.env(&bed.keys);

    let mut again = bed.draft("docs");
    again.dest = bed.account_dest("elsewhere");
    let problems = validate_draft(&env, &again).await.unwrap();
    assert_eq!(problems.id.as_deref(), Some("duplicate backup id \"docs\""));
    assert_eq!(problems.destination, None);

    let mut shared = bed.draft("photos");
    shared.dest = bed.account_dest("repo-docs");
    let problems = validate_draft(&env, &shared).await.unwrap();
    assert_eq!(
        problems.destination.as_deref(),
        Some("backup destination is used by \"Backup docs\"")
    );
    assert_eq!(problems.first(), problems.destination.as_deref());

    let mut inside = bed.draft("photos");
    inside.dest = inside.paths[0].join("repo").to_string_lossy().into_owned();
    let problems = validate_draft(&env, &inside).await.unwrap();
    assert_eq!(
        problems.destination.as_deref(),
        Some("backup destination is inside a folder being backed up")
    );

    let mut filtered = bed.draft("photos");
    filtered.filters = vec!["+ keep".to_owned()];
    let problems = validate_draft(&env, &filtered).await.unwrap();
    assert_eq!(problems.filters, filter_line_problem(1, "+ keep"));
    assert!(problems.filters.is_some());

    assert!(validate_draft(&env, &bed.draft("photos")).await.unwrap().is_empty());
}

async fn the_destination_step_refuses_a_folder_with_files_and_accepts_an_empty_one() {
    let Some(bed) = bed().await else { return };
    let env = bed.env(&bed.keys);
    let folders = vec![bed.folder("docs")];
    let full = bed.repo_dir("full");
    fs::create_dir_all(&full).unwrap();
    fs::write(full.join("file"), "x").unwrap();

    let refused = check_destination(
        &env,
        &bed.account_dest("full"),
        &folders,
        DestinationTarget::New,
    )
    .await;
    let accepted = check_destination(
        &env,
        &bed.account_dest("fresh"),
        &folders,
        DestinationTarget::New,
    )
    .await;
    let existing = check_destination(
        &env,
        &bed.account_dest("full"),
        &[],
        DestinationTarget::Existing,
    )
    .await;

    assert_eq!(refused.unwrap_err(), "The destination folder is not empty.");
    accepted.unwrap();
    existing.unwrap();
}

async fn adding_an_existing_backup_needs_a_repository_and_its_key() {
    let Some(bed) = bed().await else { return };
    let dest = bed.account_dest("old");
    let key = Key::generate().unwrap();
    let draft = ExistingBackup {
        name: "Old".to_owned(),
        id: "old".to_owned(),
        dest: dest.clone(),
    };
    let env = bed.env(&bed.keys);

    let nothing = add_existing_backup(&env, &draft, key.clone()).await.unwrap_err();
    create_repository(&bed.restic, &bed.repository("old", &dest), &key)
        .await
        .unwrap();
    let wrong = add_existing_backup(&env, &draft, Key::generate().unwrap())
        .await
        .unwrap_err();

    assert!(matches!(nothing, AddExistingError::Open(_)));
    assert_eq!(nothing.to_string(), "No backup was found in that folder.");
    assert_eq!(wrong.to_string(), "That key does not open this backup.");
    assert!(bed.keys.get("old").await.unwrap().is_none());
    assert!(bed.config().await.backups().is_empty());

    let added = add_existing_backup(&env, &draft, key.clone()).await.unwrap();

    assert!(added.warnings.is_empty(), "{:?}", added.warnings);
    assert_eq!(bed.keys.get("old").await.unwrap(), Some(key));
    let config = bed.config().await;
    let backup = &config.backups()[0];
    assert_eq!(backup.id, "old");
    assert!(!backup.enabled);
    assert!(backup.paths.is_empty());
    assert!(bed.started().is_empty());
}

async fn an_existing_backup_whose_config_cannot_be_saved_does_not_keep_the_key() {
    let Some(bed) = bed().await else { return };
    let dest = bed.account_dest("old");
    let key = Key::generate().unwrap();
    create_repository(&bed.restic, &bed.repository("old", &dest), &key)
        .await
        .unwrap();
    let draft = ExistingBackup {
        name: "Old".to_owned(),
        id: "old".to_owned(),
        dest,
    };
    let config_dir = bed.paths.config_dir();
    fs::create_dir_all(&config_dir).unwrap();
    fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(config_dir.join("probe"), "").is_ok() {
        fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o755)).unwrap();
        skipping("the config folder stays writable, as for root");
        return;
    }

    let err = add_existing_backup(&bed.env(&bed.keys), &draft, key)
        .await
        .unwrap_err();
    fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(matches!(err, AddExistingError::NotSaved(_)), "{err:?}");
    assert!(err.to_string().starts_with("Cannot save the backup: "));
    assert!(bed.keys.get("old").await.unwrap().is_none());
    assert!(bed.config().await.backups().is_empty());
    assert!(bed.started().is_empty());
}

async fn entering_the_key_clears_the_key_gate_and_starts_a_run() {
    let Some(bed) = bed().await else { return };
    let key = bed.create("docs").await;
    bed.keys.delete("docs").await.unwrap();
    let state = BackupState {
        gate: Some(Gate {
            kind: GateKind::NeedsAttention,
            since: Timestamp::now(),
            detail: "no key".to_owned(),
            fault: Some(Fault::KeyMissing),
            auto_checked: None,
        }),
        notified: vec![
            "gate:needs_attention:key_missing".to_owned(),
            "key_unsaved".to_owned(),
        ],
        ..BackupState::default()
    };
    state.save(&bed.paths, "docs").await.unwrap();
    let env = bed.env(&bed.keys);

    let wrong = enter_key(&env, "docs", Key::generate().unwrap()).await.unwrap_err();
    let state_after_wrong = BackupState::load(&bed.paths, "docs").await.unwrap();
    enter_key(&env, "docs", key.clone()).await.unwrap();

    assert!(matches!(wrong, EnterKeyError::Open(_)));
    assert_eq!(wrong.to_string(), "That key does not open this backup.");
    assert!(state_after_wrong.gate.is_some());
    let state = BackupState::load(&bed.paths, "docs").await.unwrap();
    assert!(state.gate.is_none());
    assert_eq!(state.notified, ["key_unsaved"]);
    assert_eq!(bed.keys.get("docs").await.unwrap(), Some(key));
    assert_eq!(bed.started().len(), 2);
}

async fn entering_the_key_leaves_a_gate_of_another_kind_alone() {
    let Some(bed) = bed().await else { return };
    let key = bed.create("docs").await;
    let state = BackupState {
        gate: Some(Gate {
            kind: GateKind::NeedsAttention,
            since: Timestamp::now(),
            detail: "repository damaged".to_owned(),
            fault: Some(Fault::RepoDamaged),
            auto_checked: None,
        }),
        ..BackupState::default()
    };
    state.save(&bed.paths, "docs").await.unwrap();

    enter_key(&bed.env(&bed.keys), "docs", key).await.unwrap();

    let state = BackupState::load(&bed.paths, "docs").await.unwrap();
    assert_eq!(state.gate.unwrap().fault, Some(Fault::RepoDamaged));
}

async fn entering_a_key_for_an_unknown_backup_says_so() {
    let Some(bed) = bed().await else { return };

    let err = enter_key(&bed.env(&bed.keys), "nope", Key::generate().unwrap())
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "There is no backup \"nope\".");
}

async fn marking_the_key_saved_clears_the_flag_and_its_notice() {
    let Some(bed) = bed().await else { return };
    let state = BackupState {
        key_unsaved: true,
        notified: vec!["key_unsaved".to_owned(), "gate:needs_attention".to_owned()],
        ..BackupState::default()
    };
    state.save(&bed.paths, "docs").await.unwrap();

    mark_key_saved(&bed.paths, "docs").await.unwrap();

    let state = BackupState::load(&bed.paths, "docs").await.unwrap();
    assert!(!state.key_unsaved);
    assert_eq!(state.notified, ["gate:needs_attention"]);
}

async fn show_key_returns_the_stored_key_or_says_it_is_missing() {
    let Some(bed) = bed().await else { return };
    let key = bed.create("docs").await;

    let shown = show_key(&bed.keys, "docs").await.unwrap();
    let missing = show_key(&bed.keys, "other").await.unwrap_err();

    assert_eq!(shown, key);
    assert!(matches!(missing, ShowKeyError::Missing));
    assert_eq!(
        missing.to_string(),
        "The key for this backup is missing on this computer."
    );
}

async fn removing_a_backup_keeps_the_repository_and_the_key_unless_asked() {
    let Some(bed) = bed().await else { return };
    bed.create("docs").await;
    bed.create("photos").await;
    assert_eq!(bed.preview_units().len(), 4);
    assert!(bed.paths.backup_state_dir("docs").is_dir());
    let env = bed.env(&bed.keys);

    let removed = remove_backup(&env, "docs", false).await.unwrap();

    assert!(removed.warnings.is_empty(), "{:?}", removed.warnings);
    let config = bed.config().await;
    assert_eq!(config.backups().len(), 1);
    assert_eq!(config.backups()[0].id, "photos");
    assert_eq!(
        bed.preview_units(),
        ["syncfast-backup_photos.service", "syncfast-backup_photos.timer"]
    );
    assert!(!bed.paths.backup_state_dir("docs").exists());
    assert!(bed.repo_dir("repo-docs").join("config").is_file());
    assert!(bed.keys.get("docs").await.unwrap().is_some());

    remove_backup(&env, "photos", true).await.unwrap();

    assert!(bed.keys.get("photos").await.unwrap().is_none());
    assert!(bed.repo_dir("repo-photos").join("config").is_file());
    assert!(bed.preview_units().is_empty());
    let unknown = remove_backup(&env, "photos", false).await.unwrap_err();
    assert!(matches!(unknown, RemoveError::Unknown(_)));
}

async fn saving_settings_replaces_everything_but_the_destination() {
    let Some(bed) = bed().await else { return };
    bed.create("docs").await;
    let stored = bed.config().await.backups()[0].clone();
    let env = bed.env(&bed.keys);

    let mut replacement = stored.clone();
    replacement.every = "weekly".to_owned();
    replacement.dest = "testremote:/somewhere/else".to_owned();
    replacement.name = "Renamed".to_owned();
    let saved = save_backup_settings(&env, &replacement).await.unwrap();

    assert!(saved.warnings.is_empty(), "{:?}", saved.warnings);
    let after = bed.config().await.backups()[0].clone();
    assert_eq!(after.every, "weekly");
    assert_eq!(after.name, "Renamed");
    assert_eq!(after.dest, stored.dest);
    let timer = fs::read_to_string(
        bed.paths.units_preview_dir().join("syncfast-backup_docs.timer"),
    )
    .unwrap();
    assert!(timer.contains("OnCalendar=Mon "), "{timer}");

    let mut broken = after.clone();
    broken.every = "monthly".to_owned();
    let refused = save_backup_settings(&env, &broken).await.unwrap_err();
    assert!(matches!(refused, SaveSettingsError::Invalid(_)), "{refused}");
    assert_eq!(bed.config().await.backups()[0].every, "weekly");

    let mut unknown = after;
    unknown.id = "nope".to_owned();
    let missing = save_backup_settings(&env, &unknown).await.unwrap_err();
    assert!(matches!(missing, SaveSettingsError::Unknown(_)));
}

async fn saving_settings_with_a_blank_name_is_refused_and_changes_nothing() {
    let Some(bed) = bed().await else { return };
    bed.create("docs").await;
    let stored = bed.config().await.backups()[0].clone();
    let env = bed.env(&bed.keys);

    let mut blank = stored.clone();
    blank.name = "  ".to_owned();
    let refused = save_backup_settings(&env, &blank).await.unwrap_err();

    assert!(matches!(refused, SaveSettingsError::Invalid(_)), "{refused}");
    assert_eq!(bed.config().await.backups()[0].name, stored.name);
}

async fn saving_settings_reports_what_applying_could_not_do() {
    let Some(bed) = bed().await else { return };
    bed.create("docs").await;
    let stored = bed.config().await.backups()[0].clone();
    let mut other = stored.clone();
    other.id = "other".to_owned();
    other.name = "Other".to_owned();
    other.dest = bed.account_dest("repo-other");
    other.every = "monthly".to_owned();
    Config::edit(&bed.paths, |config| config.add_backup(&other).unwrap())
        .await
        .unwrap();
    let env = bed.env(&bed.keys);

    let saved = save_backup_settings(&env, &stored).await.unwrap();

    assert_eq!(saved.warnings.len(), 1, "{:?}", saved.warnings);
    assert!(
        saved.warnings[0].starts_with("backup other: not applied:"),
        "{:?}",
        saved.warnings
    );
}
}

fn backup(keep: (u16, u16, u16, u16), one_file_system: bool) -> BackupConfig {
    let mut draft = NewBackup::with_defaults(
        "Docs".to_owned(),
        "docs".to_owned(),
        vec![PathBuf::from("/home/me/docs")],
        "/mnt/disk/repo".to_owned(),
    );
    draft.keep.daily = keep.0;
    draft.keep.weekly = keep.1;
    draft.keep.monthly = keep.2;
    draft.keep.yearly = keep.3;
    draft.one_file_system = one_file_system;
    draft.to_config()
}

#[test]
fn keep_reduced_names_each_period_that_went_down() {
    let before = backup((7, 4, 6, 2), true);

    assert_eq!(keep_reduced(&before, &before), Vec::<&str>::new());
    assert_eq!(
        keep_reduced(&before, &backup((8, 5, 7, 3), true)),
        Vec::<&str>::new()
    );
    assert_eq!(
        keep_reduced(&before, &backup((6, 4, 6, 2), true)),
        ["keep.daily"]
    );
    assert_eq!(
        keep_reduced(&before, &backup((7, 3, 6, 2), true)),
        ["keep.weekly"]
    );
    assert_eq!(
        keep_reduced(&before, &backup((7, 4, 5, 2), true)),
        ["keep.monthly"]
    );
    assert_eq!(
        keep_reduced(&before, &backup((7, 4, 6, 1), true)),
        ["keep.yearly"]
    );
    assert_eq!(
        keep_reduced(&before, &backup((1, 1, 1, 0), true)),
        ["keep.daily", "keep.weekly", "keep.monthly", "keep.yearly"]
    );
}

#[tokio::test]
async fn the_key_storage_test_roundtrips_and_leaves_nothing_behind() {
    let keys = MemoryKeyStore::default();

    let backend = test_key_storage(&keys).await.unwrap();

    assert_eq!(backend, KeyBackend::Memory);
    assert!(keys.get(TEST_ITEM_ID).await.unwrap().is_none());
}

#[tokio::test]
async fn a_key_storage_that_refuses_fails_the_test() {
    let err = test_key_storage(&RefusingKeyStore::default())
        .await
        .unwrap_err();

    assert!(matches!(err, KeyError::Unavailable(_)));
}
