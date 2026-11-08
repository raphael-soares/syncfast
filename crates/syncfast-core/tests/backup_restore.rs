//! Restores against a real restic and rclone, on `type = local` remotes
//! and folders: what comes back, where, and what is refused.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use futures_core::future::BoxFuture;
use jiff::Timestamp;
use syncfast_core::backup::config::{BackupConfig, KeepSetting};
use syncfast_core::backup::destination::{Destination, create_repository};
use syncfast_core::backup::files::{ListError, NodeKind, list_files};
use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::{KeyStore, MemoryKeyStore};
use syncfast_core::backup::restore::{DESTINATION_EXISTS, NOTHING_MATCHES};
use syncfast_core::backup::restore_job::{
    RestoreEnv, RestoreError, RestoreReport, RestoreRequest, RestoreStarter, restore_status,
    run_restore, start_restore,
};
use syncfast_core::backup::snapshots::SnapshotView;
use syncfast_core::config::{Config, Pair};
use syncfast_core::settings::Overrides;
use syncfast_core::paths::Paths;
use syncfast_core::rclone::{Rclone, RcloneError};
use syncfast_core::restic::{Repository, Restic, ResticError};
use syncfast_core::state::try_lock_pair;
use tokio::process::Command;

const DEADLINE: Duration = Duration::from_secs(90);

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

/// Runs the restore inside `start`, so the report that `start_restore`
/// returns is the one of a restore that has ended.
struct Inline<'e> {
    paths: &'e Paths,
    restic: &'e Restic,
    keys: &'e MemoryKeyStore,
}

impl RestoreStarter for Inline<'_> {
    fn start<'a>(&'a self, job_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let env = RestoreEnv {
                paths: self.paths,
                restic: self.restic,
                keys: self.keys,
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
    paths: Paths,
    restic: Restic,
    keys: MemoryKeyStore,
    backup: BackupConfig,
    key: Key,
}

#[allow(clippy::print_stderr)]
async fn rig() -> Option<Rig> {
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
    let rclone = Rclone::new(found.bin().to_owned()).with_config(conf);
    let restic = match Restic::from_env(rclone, &paths).await {
        Err(ResticError::NotFound) => {
            eprintln!("skipping: restic is not installed");
            return None;
        }
        other => other.unwrap(),
    };
    let backup = BackupConfig {
        id: "docs".to_owned(),
        name: "Documents".to_owned(),
        paths: vec![root.join("src")],
        dest: root.join("repo").to_string_lossy().into_owned(),
        every: "daily".to_owned(),
        keep: KeepSetting::default(),
        presets: Vec::new(),
        filters: Vec::new(),
        one_file_system: true,
        limit_upload: "off".to_owned(),
        limit_download: "off".to_owned(),
        enabled: true,
    };
    let key = Key::generate().unwrap();
    let keys = MemoryKeyStore::default();
    let rig = Rig {
        _dir: dir,
        root,
        paths,
        restic,
        keys,
        backup,
        key,
    };
    create_repository(&rig.restic, &rig.repository(), &rig.key)
        .await
        .unwrap();
    rig.keys
        .put(&rig.backup.id, &rig.backup.name, &rig.key)
        .await
        .unwrap();
    Config::edit(&rig.paths, |config| {
        config.add_backup(&rig.backup).unwrap();
    })
    .await
    .unwrap();
    Some(rig)
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Every file under `folder` with its content, by path relative to it.
fn tree_of(folder: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, at: &Path, into: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, into);
            } else {
                let name = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                into.insert(name, fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(folder, folder, &mut files);
    files
}

fn expected(files: &[(&str, &str)]) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(name, content)| ((*name).to_owned(), (*content).to_owned()))
        .collect()
}

const SOURCE: [(&str, &str); 4] = [
    ("docs/a.txt", "alpha"),
    ("docs/sub/b.txt", "beta"),
    ("docs/My Notes [1].txt", "bracketed"),
    ("pics/p.png", "pixels"),
];

impl Rig {
    fn repository(&self) -> Repository {
        Repository::new(
            Destination::parse(&self.backup.dest).unwrap(),
            self.paths.backup_state_dir(&self.backup.id),
        )
    }

    fn make_source(&self) {
        for (name, content) in SOURCE {
            write(&self.root.join("src").join(name), content);
        }
    }

    /// Backs up `folders` as a snapshot of this machine.
    async fn snapshot(&self, folders: &[&Path]) {
        let mut args = vec!["backup".to_owned()];
        args.extend(folders.iter().map(|folder| folder.to_string_lossy().into_owned()));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.restic
            .run(&self.repository(), &self.key, &args, false)
            .await
            .unwrap();
    }

    async fn snapshots(&self) -> Vec<SnapshotView> {
        self.restic
            .snapshots(&self.repository(), &self.key)
            .await
            .unwrap()
    }

    fn request(&self, destination: &Path, paths: &[&str]) -> RestoreRequest {
        RestoreRequest {
            backup: self.backup.id.clone(),
            snapshot: "latest".to_owned(),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            destination: destination.to_owned(),
            dry_run: false,
        }
    }

    async fn restore(&self, request: &RestoreRequest) -> Result<RestoreReport, RestoreError> {
        let env = RestoreEnv {
            paths: &self.paths,
            restic: &self.restic,
            keys: &self.keys,
        };
        let starter = Inline {
            paths: &self.paths,
            restic: &self.restic,
            keys: &self.keys,
        };
        start_restore(&env, &starter, request).await
    }

    fn src(&self, name: &str) -> String {
        self.root.join("src").join(name).to_string_lossy().into_owned()
    }
}

deadline_tests! {
async fn the_whole_snapshot_is_restored_into_a_new_folder_equal_to_the_source() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let destination = rig.root.join("out/restored");
    fs::create_dir(rig.root.join("out")).unwrap();

    let report = rig.restore(&rig.request(&destination, &[])).await.unwrap();

    assert_eq!(tree_of(&destination), tree_of(&rig.root.join("src")));
    assert_eq!(report.status, "done");
    assert!(report.finished);
    assert_eq!(report.error, None);
    assert_eq!(report.files_restored, report.total_files);
    assert!(report.total_files >= 4);
    let bytes: u64 = SOURCE.iter().map(|(_, content)| content.len() as u64).sum();
    assert_eq!(report.bytes_restored, bytes);
    assert_eq!(report.total_bytes, bytes);
}

async fn items_from_different_folders_are_restored_under_their_common_parent() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let destination = rig.root.join("picked");

    let report = rig
        .restore(&rig.request(&destination, &[&rig.src("docs/My Notes [1].txt"), &rig.src("pics")]))
        .await
        .unwrap();

    assert_eq!(
        tree_of(&destination),
        expected(&[("docs/My Notes [1].txt", "bracketed"), ("pics/p.png", "pixels")])
    );
    assert_eq!(report.status, "done");
    assert!(report.total_files >= 2);
    assert_eq!(report.files_restored, report.total_files);
}

async fn a_folder_item_brings_everything_inside_it() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let destination = rig.root.join("picked");

    rig.restore(&rig.request(&destination, &[&rig.src("docs")]))
        .await
        .unwrap();

    assert_eq!(
        tree_of(&destination),
        expected(&[
            ("docs/a.txt", "alpha"),
            ("docs/sub/b.txt", "beta"),
            ("docs/My Notes [1].txt", "bracketed"),
        ])
    );
}

async fn a_name_that_restic_reads_as_a_pattern_restores_only_itself() {
    let Some(rig) = rig().await else { return };
    write(&rig.root.join("src/p/x*y.txt"), "star");
    write(&rig.root.join("src/p/xzy.txt"), "other");
    write(&rig.root.join("src/p/a [1].txt"), "one");
    write(&rig.root.join("src/p/a 1.txt"), "plain");
    rig.snapshot(&[&rig.root.join("src")]).await;
    let destination = rig.root.join("picked");

    rig.restore(&rig.request(&destination, &[&rig.src("p/x*y.txt"), &rig.src("p/a [1].txt")]))
        .await
        .unwrap();

    assert_eq!(
        tree_of(&destination),
        expected(&[("x*y.txt", "star"), ("a [1].txt", "one")])
    );
}

async fn a_snapshot_restores_after_its_source_was_deleted() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let before = tree_of(&rig.root.join("src"));
    fs::remove_dir_all(rig.root.join("src")).unwrap();
    let destination = rig.root.join("back");

    rig.restore(&rig.request(&destination, &[])).await.unwrap();

    assert_eq!(tree_of(&destination), before);
}

async fn a_snapshot_of_another_machine_is_listed_and_restored() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    let password = rig.root.join("password");
    let key_text: String = rig
        .key
        .display_groups()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    fs::write(&password, key_text).unwrap();
    let made = Command::new("restic")
        .args(["-r", &rig.backup.dest, "--password-file"])
        .arg(&password)
        .args(["--cache-dir"])
        .arg(rig.root.join("cache"))
        .args(["backup", "--host", "other-machine", "--quiet"])
        .arg(rig.root.join("src"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .unwrap();
    assert!(made.success());
    let snapshots = rig.snapshots().await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].host, "other-machine");
    let destination = rig.root.join("theirs");

    let mut request = rig.request(&destination, &[]);
    request.snapshot = snapshots[0].short_id.clone();
    rig.restore(&request).await.unwrap();

    assert_eq!(tree_of(&destination), tree_of(&rig.root.join("src")));
}

async fn an_existing_destination_is_refused_and_nothing_is_touched() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let destination = rig.root.join("taken");
    write(&destination.join("mine.txt"), "keep me");

    let err = rig.restore(&rig.request(&destination, &[])).await.unwrap_err();

    assert!(matches!(&err, RestoreError::Invalid(text) if text == DESTINATION_EXISTS));
    assert_eq!(err.exit_code(), 2);
    assert_eq!(tree_of(&destination), expected(&[("mine.txt", "keep me")]));
    assert!(!rig.paths.restores_dir().exists());
}

async fn a_destination_inside_a_syncing_folder_is_refused_while_its_round_runs() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let synced = rig.root.join("synced");
    fs::create_dir(&synced).unwrap();
    Config::edit(&rig.paths, |config| {
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
    let destination = synced.join("restored");
    let round = try_lock_pair(&rig.paths, "notes").unwrap();

    let err = rig.restore(&rig.request(&destination, &[])).await.unwrap_err();

    assert!(matches!(err, RestoreError::Busy));
    assert_eq!(err.exit_code(), 3);
    assert_eq!(err.to_string(), "this sync is running; try again when it finishes");
    assert!(!destination.exists());

    drop(round);
    rig.restore(&rig.request(&destination, &[])).await.unwrap();
    assert_eq!(tree_of(&destination), tree_of(&rig.root.join("src")));
}

async fn a_path_that_matches_nothing_fails_and_leaves_no_folder() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let destination = rig.root.join("nothing");

    let report = rig
        .restore(&rig.request(&destination, &[&rig.src("docs/missing.txt")]))
        .await
        .unwrap();

    assert_eq!((report.status, report.finished), ("failed", true));
    assert_eq!(report.error.as_deref(), Some(NOTHING_MATCHES));
    assert!(!destination.exists());
    let again = restore_status(&rig.paths, report.id.as_deref().unwrap()).await.unwrap();
    assert_eq!(again.error.as_deref(), Some(NOTHING_MATCHES));
}

async fn a_dry_run_counts_the_files_and_creates_nothing() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let destination = rig.root.join("dry");
    let mut request = rig.request(&destination, &[]);
    request.dry_run = true;

    let report = rig.restore(&request).await.unwrap();

    assert_eq!(report.status, "dry_run");
    assert_eq!(report.id, None);
    assert!(report.total_files >= 4);
    let bytes: u64 = SOURCE.iter().map(|(_, content)| content.len() as u64).sum();
    assert_eq!(report.total_bytes, bytes);
    assert!(!destination.exists());
    assert!(!rig.paths.restores_dir().exists());
}

async fn a_snapshot_that_does_not_exist_and_a_relative_path_are_refused() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let destination = rig.root.join("never");

    let mut missing = rig.request(&destination, &[]);
    missing.snapshot = "deadbeef".to_owned();
    let relative = rig.request(&destination, &["docs/a.txt"]);

    assert!(matches!(rig.restore(&missing).await, Err(RestoreError::Invalid(_))));
    assert!(matches!(rig.restore(&relative).await, Err(RestoreError::Invalid(_))));
    assert!(!destination.exists());
}

async fn a_backup_without_snapshots_has_nothing_to_restore() {
    let Some(rig) = rig().await else { return };
    let destination = rig.root.join("never");

    let err = rig.restore(&rig.request(&destination, &[])).await.unwrap_err();

    assert!(matches!(err, RestoreError::Invalid(_)), "{err}");
    assert!(!destination.exists());
}

async fn a_backup_that_does_not_exist_is_refused() {
    let Some(rig) = rig().await else { return };
    let mut request = rig.request(&rig.root.join("never"), &[]);
    request.backup = "nope".to_owned();

    assert!(matches!(rig.restore(&request).await, Err(RestoreError::Invalid(_))));
}

async fn the_latest_snapshot_is_the_newest_of_every_machine() {
    let Some(rig) = rig().await else { return };
    write(&rig.root.join("src/one.txt"), "first");
    rig.snapshot(&[&rig.root.join("src")]).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    write(&rig.root.join("src/two.txt"), "second");
    rig.snapshot(&[&rig.root.join("src")]).await;
    let destination = rig.root.join("newest");

    let report = rig.restore(&rig.request(&destination, &[])).await.unwrap();

    assert_eq!(report.status, "done");
    assert_eq!(
        tree_of(&destination),
        expected(&[("one.txt", "first"), ("two.txt", "second")])
    );
}

async fn files_are_listed_one_folder_at_a_time() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let snapshot = rig.snapshots().await.remove(0);
    let root = rig.root.join("src").to_string_lossy().into_owned();

    let top = list_files(&rig.restic, &rig.repository(), &rig.key, &snapshot, "")
        .await
        .unwrap();
    let docs = list_files(&rig.restic, &rig.repository(), &rig.key, &snapshot, &format!("{root}/docs"))
        .await
        .unwrap();

    assert_eq!(top.nodes.len(), 1);
    assert_eq!(top.nodes[0].path, root);
    assert_eq!(top.nodes[0].kind, NodeKind::Dir);
    assert!(!top.truncated);
    let names: Vec<(&str, NodeKind)> = docs
        .nodes
        .iter()
        .map(|node| (node.name.as_str(), node.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("sub", NodeKind::Dir),
            ("a.txt", NodeKind::File),
            ("My Notes [1].txt", NodeKind::File)
        ]
    );
    let a = docs.nodes.iter().find(|node| node.name == "a.txt").unwrap();
    assert_eq!(a.size, Some(5));
    assert!(a.mtime.unwrap() > Timestamp::UNIX_EPOCH);
}

async fn a_folder_that_is_not_in_the_snapshot_is_refused() {
    let Some(rig) = rig().await else { return };
    rig.make_source();
    rig.snapshot(&[&rig.root.join("src")]).await;
    let snapshot = rig.snapshots().await.remove(0);

    let relative = list_files(&rig.restic, &rig.repository(), &rig.key, &snapshot, "docs").await;

    assert!(matches!(relative, Err(ListError::Invalid(_))));
}
}
