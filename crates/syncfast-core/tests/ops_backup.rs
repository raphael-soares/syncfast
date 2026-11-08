//! The backup operations an agent uses, against a real restic and rclone
//! on `type = local` remotes and folders.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::Zoned;
use syncfast_core::backup::config::BackupConfig;
use syncfast_core::backup::key::{Key, KeyError};
use syncfast_core::backup::keystore::{KeyBackend, KeyStore, MemoryKeyStore};
use syncfast_core::backup::restore_job::{RestoreEnv, RestoreStarter, run_restore};
use syncfast_core::backup::run::{BackupRunEnv, run_backup};
use syncfast_core::backup::state::CheckKind;
use syncfast_core::config::{Config, Pair};
use syncfast_core::network::{BoxFuture, Network};
use syncfast_core::notify::{Notice, Notifier};
use syncfast_core::ops::{
    BackupEdit, BackupOptions, KeepPartial, NewBackupRequest, Ops, RestoreArgs,
};
use syncfast_core::paths::Paths;
use syncfast_core::rclone::{Rclone, RcloneError};
use syncfast_core::restic::{Restic, ResticError};
use syncfast_core::settings::Overrides;
use syncfast_core::state::try_lock_pair;
use syncfast_core::units::{Systemctl, SystemdMode};
use syncfast_core::watch::Trigger;

const DEADLINE: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(60);

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

struct Online;

impl Network for Online {
    fn is_online(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
    }
}

struct Silent;

impl Notifier for Silent {
    fn notify(&self, _notice: &Notice) {}
}

struct NoSystemctl;

impl Systemctl for NoSystemctl {
    fn run<'a>(&'a self, _args: &'a [&'a str]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// A key storage that cannot be reached.
struct NoStorage;

impl KeyStore for NoStorage {
    fn get<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Option<Key>, KeyError>> {
        Box::pin(async { Err(unreachable_storage()) })
    }

    fn put<'a>(
        &'a self,
        _id: &'a str,
        _name: &'a str,
        _key: &'a Key,
    ) -> BoxFuture<'a, Result<(), KeyError>> {
        Box::pin(async { Err(unreachable_storage()) })
    }

    fn delete<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<(), KeyError>> {
        Box::pin(async { Err(unreachable_storage()) })
    }

    fn backend(&self) -> Option<KeyBackend> {
        None
    }

    fn probe(&self) -> BoxFuture<'_, Result<KeyBackend, KeyError>> {
        Box::pin(async { Err(unreachable_storage()) })
    }
}

fn unreachable_storage() -> KeyError {
    KeyError::Unavailable("no secret service".to_owned())
}

/// Runs a backup inside `start_backup`, so a run that was asked for has
/// ended when the operation looks.
struct InProcess {
    paths: Paths,
    restic: Restic,
    keys: Arc<dyn KeyStore>,
}

impl Trigger for InProcess {
    fn start<'a>(&'a self, _pair_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("this test starts no syncs".to_owned()) })
    }

    fn start_backup<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let clock = Zoned::now;
            let env = BackupRunEnv {
                paths: &self.paths,
                restic: &self.restic,
                keys: &*self.keys,
                network: &Online,
                notifier: &Silent,
                clock: &clock,
                forward_signals: false,
            };
            run_backup(&env, backup_id)
                .await
                .map(|_| ())
                .map_err(|err| err.to_string())
        })
    }
}

struct InlineRestore {
    paths: Paths,
    restic: Restic,
    keys: Arc<dyn KeyStore>,
}

impl RestoreStarter for InlineRestore {
    fn start<'a>(&'a self, job_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let env = RestoreEnv {
                paths: &self.paths,
                restic: &self.restic,
                keys: &*self.keys,
            };
            run_restore(&env, job_id, false)
                .await
                .map_err(|err| err.to_string())
        })
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    ops: Ops,
    keys: Arc<MemoryKeyStore>,
}

#[allow(clippy::print_stderr)]
async fn rig_with(keys: Option<Arc<dyn KeyStore>>) -> Option<Rig> {
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
    let paths = Paths::new(
        root.join("home"),
        root.join("config"),
        root.join("state"),
        root.join("data"),
        root.join("run"),
    );
    fs::create_dir_all(&paths.home).unwrap();
    let rclone = Rclone::new(found.bin().to_owned()).with_config(conf);
    let restic = match Restic::from_env(rclone.clone(), &paths).await {
        Err(ResticError::NotFound) => {
            eprintln!("skipping: restic is not installed");
            return None;
        }
        other => other.unwrap(),
    };
    let memory = Arc::new(MemoryKeyStore::default());
    let store: Arc<dyn KeyStore> = keys.unwrap_or_else(|| memory.clone());
    let ops = Ops {
        trigger: Arc::new(InProcess {
            paths: paths.clone(),
            restic: restic.clone(),
            keys: store.clone(),
        }),
        transfers: Arc::new(NoTransfers),
        restores: Arc::new(InlineRestore {
            paths: paths.clone(),
            restic: restic.clone(),
            keys: store.clone(),
        }),
        keys: store,
        restic: Some(restic),
        paths,
        rclone,
        network: Box::new(Online),
        notifier: Box::new(Silent),
        systemctl: Arc::new(NoSystemctl),
        systemd: SystemdMode::Off,
        bin: PathBuf::from("/bin/true"),
        flatpak_id: None,
    };
    Some(Rig {
        _dir: dir,
        root,
        ops,
        keys: memory,
    })
}

struct NoTransfers;

impl syncfast_core::ops::TransferStarter for NoTransfers {
    fn start<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("this test starts no transfers".to_owned()) })
    }
}

async fn rig() -> Option<Rig> {
    rig_with(None).await
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

impl Rig {
    fn src(&self) -> PathBuf {
        self.root.join("src")
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn request(&self) -> NewBackupRequest {
        write(&self.src().join("docs/a.txt"), "alpha");
        write(&self.src().join("docs/sub/b.txt"), "beta");
        NewBackupRequest {
            name: "Agent Docs".to_owned(),
            paths: vec![self.src().display().to_string()],
            dest: self.repo().display().to_string(),
            options: BackupOptions::default(),
        }
    }

    async fn create(&self) -> String {
        self.ops.create_backup(self.request()).await.unwrap().id
    }

    async fn config(&self) -> BackupConfig {
        let config = Config::load(&self.ops.paths).await.unwrap();
        config.backups()[0].clone()
    }

    /// The key as the text restic reads, to look for it in outputs.
    async fn key_text(&self, id: &str) -> String {
        self.keys
            .get(id)
            .await
            .unwrap()
            .unwrap()
            .display_groups()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect()
    }
}

fn keep(daily: i64) -> BackupEdit {
    BackupEdit {
        options: BackupOptions {
            keep: KeepPartial {
                daily: Some(daily),
                ..KeepPartial::default()
            },
            ..BackupOptions::default()
        },
        ..BackupEdit::default()
    }
}

deadline_tests! {
async fn create_stores_the_key_marks_it_unsaved_and_makes_the_first_snapshot() {
    let Some(rig) = rig().await else { return };

    let created = rig.ops.create_backup(rig.request()).await.unwrap();

    assert_eq!(created.key, "stored");
    assert!(created.key_unsaved);
    assert!(rig.keys.get(&created.id).await.unwrap().is_some());
    let view = rig.ops.backups().await.unwrap().pop().unwrap();
    assert_eq!(view.id, created.id);
    assert_eq!((view.key, view.key_unsaved), ("stored", true));
    assert!(view.last_snapshot.is_some());
    assert_eq!(rig.ops.snapshots(&created.id).await.unwrap().len(), 1);
}

async fn create_into_a_destination_that_is_not_empty_is_refused_and_leaves_nothing() {
    let Some(rig) = rig().await else { return };
    write(&rig.repo().join("mine.txt"), "keep me");

    let err = rig.ops.create_backup(rig.request()).await.unwrap_err();

    assert_eq!(err.exit_code(), 2);
    assert_eq!(err.code(), "invalid");
    assert!(Config::load(&rig.ops.paths).await.unwrap().backups().is_empty());
    assert_eq!(fs::read_to_string(rig.repo().join("mine.txt")).unwrap(), "keep me");
    assert_eq!(fs::read_dir(rig.repo()).unwrap().count(), 1);
}

async fn create_with_no_key_storage_fails_and_creates_nothing() {
    let Some(rig) = rig_with(Some(Arc::new(NoStorage))).await else { return };

    let err = rig.ops.create_backup(rig.request()).await.unwrap_err();

    assert_eq!(err.exit_code(), 1);
    assert!(err.to_string().contains("Cannot reach the key storage"), "{err}");
    assert!(Config::load(&rig.ops.paths).await.unwrap().backups().is_empty());
    assert!(!rig.repo().exists());
}

async fn create_with_overlapping_folders_is_refused() {
    let Some(rig) = rig().await else { return };
    let mut request = rig.request();
    request.dest = rig.src().join("repo").display().to_string();

    let err = rig.ops.create_backup(request).await.unwrap_err();

    assert_eq!(err.exit_code(), 2);
    assert!(Config::load(&rig.ops.paths).await.unwrap().backups().is_empty());
}

async fn no_output_holds_the_key() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let key = rig.key_text(&id).await;
    assert_eq!(key.len(), 52);

    let mut outputs = vec![
        serde_json::to_string(&rig.ops.backups().await.unwrap()).unwrap(),
        serde_json::to_string(&rig.ops.show_backup(&id).await.unwrap()).unwrap(),
        serde_json::to_string(&rig.ops.snapshots(&id).await.unwrap()).unwrap(),
        serde_json::to_string(
            &rig.ops.snapshot_files(&id, "latest", "", 50).await.unwrap(),
        )
        .unwrap(),
        serde_json::to_string(&rig.ops.backup_now(&id, WAIT).await.unwrap()).unwrap(),
        serde_json::to_string(
            &rig.ops.check_backup(&id, CheckKind::Quick, WAIT).await.unwrap(),
        )
        .unwrap(),
        serde_json::to_string(&rig.ops.pause_backup(Some(&id)).await.unwrap()).unwrap(),
        serde_json::to_string(&rig.ops.resume_backup(None).await.unwrap()).unwrap(),
        serde_json::to_string(&rig.ops.edit_backup(&id, &keep(30)).await.unwrap()).unwrap(),
        serde_json::to_string(
            &rig.ops
                .restore(restore_args(&id, &rig.root.join("out"), false), WAIT)
                .await
                .unwrap(),
        )
        .unwrap(),
        rig.ops.edit_backup(&id, &keep(1)).await.unwrap_err().to_string(),
        Ops::remove_backup(&id).unwrap_err().to_string(),
    ];
    outputs.push(fs::read_to_string(rig.ops.paths.config_file()).unwrap());

    for output in &outputs {
        assert!(!output.contains(&key), "the key is in: {output}");
    }
}

async fn a_backup_run_by_request_makes_a_new_snapshot() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    write(&rig.src().join("docs/new.txt"), "fresh");

    let run = rig.ops.backup_now(&id, WAIT).await.unwrap();

    assert!(run.finished);
    let snapshots = rig.ops.snapshots(&id).await.unwrap();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(
        run.backup.last_snapshot.unwrap().id,
        snapshots[0].id,
        "newest first"
    );
    assert!(snapshots[0].time >= snapshots[1].time);
}

async fn a_check_by_request_records_its_result() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;

    let run = rig.ops.check_backup(&id, CheckKind::Quick, WAIT).await.unwrap();

    assert!(run.finished);
    let detail = rig.ops.show_backup(&id).await.unwrap();
    let check = detail.last_check.unwrap();
    assert!(check.ok, "{}", check.detail);
    assert_eq!(check.kind, CheckKind::Quick);
}

async fn the_files_of_a_snapshot_come_as_names_sizes_and_dates_only() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let sub = format!("{}/docs/sub", rig.src().display());

    let top = rig.ops.snapshot_files(&id, "latest", "", 50).await.unwrap();
    let inside = rig.ops.snapshot_files(&id, "latest", &sub, 50).await.unwrap();

    assert!(!top.files.is_empty());
    assert_eq!(inside.files.len(), 1);
    let file = &inside.files[0];
    assert_eq!((file.name.as_str(), file.size), ("b.txt", Some(4)));
    assert!(file.mtime.is_some());
    let json = serde_json::to_string(&inside).unwrap();
    assert!(!json.contains("beta"), "{json}");
}

async fn a_long_folder_is_cut_at_the_limit_and_says_so() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let docs = format!("{}/docs", rig.src().display());

    let cut = rig.ops.snapshot_files(&id, "latest", &docs, 1).await.unwrap();
    let whole = rig.ops.snapshot_files(&id, "latest", &docs, 50).await.unwrap();

    assert_eq!(cut.files.len(), 1);
    assert!(cut.truncated);
    assert_eq!(whole.files.len(), 2);
    assert!(!whole.truncated);
}

async fn a_snapshot_that_does_not_exist_is_refused() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;

    let err = rig.ops.snapshot_files(&id, "ffffffff", "", 50).await.unwrap_err();

    assert_eq!(err.exit_code(), 2);
}

async fn a_backup_that_does_not_exist_is_refused_everywhere_with_exit_2() {
    let Some(rig) = rig().await else { return };
    let ops = &rig.ops;

    let errors = [
        ops.show_backup("nope").await.map(|_| ()),
        ops.snapshots("nope").await.map(|_| ()),
        ops.snapshot_files("nope", "latest", "", 5).await.map(|_| ()),
        ops.backup_now("nope", WAIT).await.map(|_| ()),
        ops.check_backup("nope", CheckKind::Quick, WAIT).await.map(|_| ()),
        ops.pause_backup(Some("nope")).await.map(|_| ()),
        ops.edit_backup("nope", &keep(9)).await.map(|_| ()),
        ops.restore(restore_args("nope", &rig.root.join("o"), true), WAIT)
            .await
            .map(|_| ()),
    ];

    for result in errors {
        assert_eq!(result.unwrap_err().exit_code(), 2);
    }
}

async fn a_backup_whose_key_is_not_stored_cannot_be_opened() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    rig.keys.delete(&id).await.unwrap();

    let err = rig.ops.snapshots(&id).await.unwrap_err();

    assert_eq!(err.exit_code(), 1);
    assert!(err.to_string().contains("Ask the user"), "{err}");
    assert_eq!(rig.ops.backups().await.unwrap()[0].key, "missing");
}

async fn pausing_and_resuming_change_whether_the_backup_is_enabled() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;

    let paused = rig.ops.pause_backup(Some(&id)).await.unwrap();
    assert!(!paused.backups[0].enabled);
    assert!(!rig.config().await.enabled);

    let resumed = rig.ops.resume_backup(None).await.unwrap();
    assert!(resumed.backups[0].enabled);
    assert!(rig.config().await.enabled);
}

async fn every_change_that_lowers_safety_is_refused_with_the_config_untouched() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let before = fs::read_to_string(rig.ops.paths.config_file()).unwrap();
    let other = rig.root.join("other");
    let cases: Vec<(BackupEdit, &str)> = vec![
        (keep(1), "keep.daily"),
        (keep(0), "keep.daily"),
        (
            BackupEdit {
                paths: Some(vec![other.display().to_string()]),
                ..BackupEdit::default()
            },
            "paths",
        ),
        (
            BackupEdit {
                paths: Some(Vec::new()),
                ..BackupEdit::default()
            },
            "paths",
        ),
        (
            BackupEdit {
                one_file_system: Some(false),
                ..BackupEdit::default()
            },
            "one_file_system",
        ),
    ];

    for (edit, key) in cases {
        let err = rig.ops.edit_backup(&id, &edit).await.unwrap_err();

        assert_eq!(err.exit_code(), 4, "{key}");
        assert_eq!(
            err.to_string(),
            format!("This setting lowers safety ({key}). Change it in syncfast.")
        );
        assert_eq!(fs::read_to_string(rig.ops.paths.config_file()).unwrap(), before, "{key}");
    }
}

async fn keeping_no_yearly_snapshots_after_keeping_some_is_refused() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let yearly = |years| BackupEdit {
        options: BackupOptions {
            keep: KeepPartial {
                yearly: Some(years),
                ..KeepPartial::default()
            },
            ..BackupOptions::default()
        },
        ..BackupEdit::default()
    };
    rig.ops.edit_backup(&id, &yearly(2)).await.unwrap();

    let err = rig.ops.edit_backup(&id, &yearly(0)).await.unwrap_err();

    assert_eq!(err.to_string(), "This setting lowers safety (keep.yearly). Change it in syncfast.");
    assert_eq!(rig.config().await.keep.yearly, 2);
}

async fn a_changed_destination_is_refused_for_good_and_the_config_is_untouched() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let before = fs::read_to_string(rig.ops.paths.config_file()).unwrap();
    let edit = BackupEdit {
        dest: Some(rig.root.join("elsewhere").display().to_string()),
        ..BackupEdit::default()
    };

    let err = rig.ops.edit_backup(&id, &edit).await.unwrap_err();

    assert_eq!(err.exit_code(), 4);
    assert!(err.to_string().contains("destination is fixed"), "{err}");
    assert_eq!(fs::read_to_string(rig.ops.paths.config_file()).unwrap(), before);
}

async fn changes_that_keep_more_are_applied() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let extra = rig.root.join("extra");
    fs::create_dir_all(&extra).unwrap();
    let edit = BackupEdit {
        name: Some("Renamed".to_owned()),
        paths: Some(vec![
            rig.src().display().to_string(),
            extra.display().to_string(),
        ]),
        options: BackupOptions {
            every: Some("hourly".to_owned()),
            keep: KeepPartial {
                daily: Some(30),
                yearly: Some(3),
                ..KeepPartial::default()
            },
            presets: Some(Vec::new()),
            filters: Some(vec!["- *.tmp".to_owned()]),
            limit_upload: Some("1M".to_owned()),
            limit_download: Some("2M".to_owned()),
        },
        ..BackupEdit::default()
    };

    let saved = rig.ops.edit_backup(&id, &edit).await.unwrap();

    let stored = rig.config().await;
    assert_eq!(stored.name, "Renamed");
    assert_eq!(stored.paths, vec![rig.src(), extra]);
    assert_eq!(stored.every, "hourly");
    assert_eq!((stored.keep.daily, stored.keep.yearly), (30, 3));
    assert_eq!(stored.filters, ["- *.tmp"]);
    assert_eq!((stored.limit_upload.as_str(), stored.limit_download.as_str()), ("1M", "2M"));
    assert_eq!(saved.backup.name, "Renamed");
    assert_eq!(saved.backup.keep.daily, 30);
}

async fn turning_one_file_system_on_is_allowed_and_an_invalid_value_changes_nothing() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let before = fs::read_to_string(rig.ops.paths.config_file()).unwrap();
    let bad = BackupEdit {
        options: BackupOptions {
            every: Some("fortnightly".to_owned()),
            ..BackupOptions::default()
        },
        ..BackupEdit::default()
    };

    let err = rig.ops.edit_backup(&id, &bad).await.unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert_eq!(fs::read_to_string(rig.ops.paths.config_file()).unwrap(), before);

    let empty = rig.ops.edit_backup(&id, &BackupEdit::default()).await.unwrap_err();
    assert_eq!(empty.exit_code(), 2);
}

async fn a_backup_cannot_be_removed_by_an_agent() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;

    let err = Ops::remove_backup(&id).unwrap_err();

    assert_eq!(err.exit_code(), 4);
    assert_eq!(err.code(), "needs_user");
    assert_eq!(Config::load(&rig.ops.paths).await.unwrap().backups().len(), 1);
}

async fn a_restore_into_a_new_folder_reports_exact_counts_and_no_contents() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let out = rig.root.join("restored");

    let report = rig
        .ops
        .restore(restore_args(&id, &out, false), WAIT)
        .await
        .unwrap();

    assert_eq!(report.status, "done");
    assert!(report.finished);
    assert_eq!(report.bytes_restored, 9);
    assert_eq!(report.files_restored, report.total_files);
    assert!(report.total_files >= 2);
    assert_eq!(
        fs::read_to_string(out.join("docs/a.txt")).unwrap(),
        "alpha"
    );
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("alpha"), "{json}");
    let again = rig.ops.restore_job(report.id.as_deref().unwrap()).await.unwrap();
    assert_eq!(again, report);
}

async fn a_dry_run_counts_and_creates_nothing() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let out = rig.root.join("restored");

    let report = rig.ops.restore(restore_args(&id, &out, true), WAIT).await.unwrap();

    assert_eq!(report.status, "dry_run");
    assert_eq!(report.total_bytes, 9);
    assert!(report.total_files >= 2);
    assert!(!out.exists());
}

async fn a_restore_into_an_existing_folder_is_refused_and_nothing_changes() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let taken = rig.root.join("taken");
    write(&taken.join("mine.txt"), "keep me");

    let err = rig
        .ops
        .restore(restore_args(&id, &taken, false), WAIT)
        .await
        .unwrap_err();

    assert_eq!(err.exit_code(), 2);
    assert_eq!(fs::read_dir(&taken).unwrap().count(), 1);
    assert_eq!(fs::read_to_string(taken.join("mine.txt")).unwrap(), "keep me");
}

async fn a_restore_into_a_syncing_folder_waits_for_its_round() {
    let Some(rig) = rig().await else { return };
    let id = rig.create().await;
    let synced = rig.root.join("synced");
    fs::create_dir(&synced).unwrap();
    Config::edit(&rig.ops.paths, |config| {
        config
            .add_pair(Pair {
                id: "notes".to_owned(),
                name: "Notes".to_owned(),
                local: synced.clone(),
                remote: "testremote:notes".to_owned(),
                enabled: true,
                presets: Vec::new(),
                filters: Vec::new(),
                overrides: Overrides::default(),
            })
            .unwrap();
    })
    .await
    .unwrap();
    let out = synced.join("restored");
    let round = try_lock_pair(&rig.ops.paths, "notes").unwrap();

    let err = rig
        .ops
        .restore(restore_args(&id, &out, false), WAIT)
        .await
        .unwrap_err();

    assert_eq!(err.exit_code(), 3);
    assert!(!out.exists());
    drop(round);
    rig.ops.restore(restore_args(&id, &out, false), WAIT).await.unwrap();
    assert!(out.exists());
}

async fn a_restore_job_that_does_not_exist_is_refused() {
    let Some(rig) = rig().await else { return };

    let err = rig.ops.restore_job("20990101-000000-000").await.unwrap_err();

    assert_eq!(err.exit_code(), 2);
}
}

fn restore_args(backup: &str, to: &Path, dry_run: bool) -> RestoreArgs {
    RestoreArgs {
        backup: backup.to_owned(),
        snapshot: "latest".to_owned(),
        to: to.display().to_string(),
        paths: Vec::new(),
        dry_run,
    }
}
