//! The restic wrapper and the repository functions against a real restic
//! and rclone, on `type = local` remotes and a folder.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Zoned};
use syncfast_core::backup::classify::BackupOutcome;
use syncfast_core::backup::config::{BackupConfig, KeepSetting};
use syncfast_core::backup::destination::{
    Destination, RepositoryError, create_repository, open_repository,
};
use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::{KeyStore, MemoryKeyStore};
use syncfast_core::backup::run::{BackupRunEnv, BackupRunError, BackupRunReport, run_backup};
use syncfast_core::backup::run_decision::SkipReason;
use syncfast_core::backup::snapshots::SnapshotView;
use syncfast_core::backup::state::{BackupState, is_running};
use syncfast_core::backup::status::BackupProgressFile;
use syncfast_core::bisync::Fault;
use syncfast_core::config::Config;
use syncfast_core::network::{BoxFuture, Network};
use syncfast_core::notify::{Notice, Notifier};
use syncfast_core::paths::Paths;
use syncfast_core::rclone::{Rclone, RcloneError};
use syncfast_core::restic::{Repository, Restic, ResticError};
use syncfast_core::state::GateKind;

struct Bed {
    _dir: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
    restic: Restic,
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
    Some(Bed {
        _dir: dir,
        root,
        paths,
        restic,
    })
}

impl Bed {
    fn repository(&self, destination: Destination) -> Repository {
        Repository::new(destination, self.root.join("work"))
    }

    fn account(&self, name: &str) -> Repository {
        self.repository(Destination::Account {
            name: "testremote".to_owned(),
            path: self.root.join(name).to_string_lossy().into_owned(),
        })
    }

    fn folder(&self, name: &str) -> Repository {
        self.repository(Destination::Local(self.root.join(name)))
    }
}

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

deadline_tests! {
async fn a_repository_is_created_on_an_account_and_opens_with_its_key() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let repo = bed.account("repo");

    let made = create_repository(&bed.restic, &repo, &key).await.unwrap();

    assert_eq!(made.id.len(), 64);
    open_repository(&bed.restic, &repo, &key).await.unwrap();
    assert!(bed.root.join("repo/config").is_file());
    assert!(bed.restic.snapshots(&repo, &key).await.unwrap().is_empty());
}

async fn a_repository_is_created_in_a_local_folder() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let repo = bed.folder("disk");

    create_repository(&bed.restic, &repo, &key).await.unwrap();

    open_repository(&bed.restic, &repo, &key).await.unwrap();
    assert!(bed.root.join("disk/config").is_file());
}

async fn another_key_does_not_open_the_repository() {
    let Some(bed) = bed().await else { return };
    let repo = bed.account("repo");
    create_repository(&bed.restic, &repo, &Key::generate().unwrap())
        .await
        .unwrap();

    let err = open_repository(&bed.restic, &repo, &Key::generate().unwrap())
        .await
        .unwrap_err();

    assert!(matches!(err, ResticError::WrongKey), "{err}");
    assert_eq!(err.to_string(), "That key does not open this backup.");
}

async fn a_folder_without_a_repository_is_reported_as_such() {
    let Some(bed) = bed().await else { return };
    fs::create_dir_all(bed.root.join("empty")).unwrap();

    for repo in [
        bed.account("empty"),
        bed.account("absent"),
        bed.folder("empty"),
    ] {
        let err = open_repository(&bed.restic, &repo, &Key::generate().unwrap())
            .await
            .unwrap_err();

        assert!(matches!(err, ResticError::NoRepository), "{err}");
    }
}

async fn a_folder_with_files_never_becomes_a_repository() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    for name in ["account-folder", "local-folder"] {
        fs::create_dir_all(bed.root.join(name)).unwrap();
        fs::write(bed.root.join(name).join("photo.jpg"), b"x").unwrap();
    }

    for repo in [bed.account("account-folder"), bed.folder("local-folder")] {
        let err = create_repository(&bed.restic, &repo, &key)
            .await
            .unwrap_err();

        assert!(matches!(err, RepositoryError::NotEmpty), "{err}");
    }
    assert!(!bed.root.join("account-folder/config").exists());
    assert!(!bed.root.join("local-folder/config").exists());
}

async fn an_empty_existing_folder_can_become_a_repository() {
    let Some(bed) = bed().await else { return };
    fs::create_dir_all(bed.root.join("ready")).unwrap();

    create_repository(
        &bed.restic,
        &bed.account("ready"),
        &Key::generate().unwrap(),
    )
    .await
    .unwrap();

    assert!(bed.root.join("ready/config").is_file());
}

async fn a_backup_shows_up_in_the_snapshots_with_this_machines_name() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let repo = bed.account("repo");
    create_repository(&bed.restic, &repo, &key).await.unwrap();
    let source = bed.root.join("docs");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("a.txt"), b"hello").unwrap();
    let source_arg = source.to_string_lossy().into_owned();

    bed.restic
        .run(&repo, &key, &["backup", "--json", &source_arg], false)
        .await
        .unwrap();
    let snapshots = bed.restic.snapshots(&repo, &key).await.unwrap();

    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].paths, [source_arg]);
    assert_eq!(
        snapshots[0].host,
        syncfast_core::restic::backup_host().unwrap()
    );
    assert_eq!(snapshots[0].files, Some(1));
}

async fn unlock_succeeds_on_a_repository_without_locks() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let repo = bed.account("repo");
    create_repository(&bed.restic, &repo, &key).await.unwrap();

    bed.restic.unlock(&repo, &key).await.unwrap();
}

async fn limits_do_not_stop_a_repository_from_opening() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let plain = bed.account("repo");
    create_repository(&bed.restic, &plain, &key).await.unwrap();
    let limited = plain.with_limits(
        syncfast_core::behavior::Bandwidth::KiBPerSec(1000),
        syncfast_core::behavior::Bandwidth::KiBPerSec(1000),
    );

    open_repository(&bed.restic, &limited, &key).await.unwrap();
}
}

// The backup run engine: `run_backup` against a real restic, real rclone
// (`type = local` remotes), a folder repository and a tempdir home.

/// A bed plus what a round reaches through traits: a key storage, a
/// network that can go offline, a clock that can run ahead and a notifier
/// that records.
struct Rig {
    bed: Bed,
    keys: MemoryKeyStore,
    online: AtomicBool,
    hours_ahead: AtomicI64,
    notices: Mutex<Vec<Notice>>,
}

impl Network for Rig {
    fn is_online(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { self.online.load(Ordering::SeqCst) })
    }
}

impl Notifier for Rig {
    fn notify(&self, notice: &Notice) {
        self.notices.lock().unwrap().push(notice.clone());
    }
}

fn backup(id: &str, sources: &[&Path], dest: String) -> BackupConfig {
    BackupConfig {
        id: id.to_owned(),
        name: "Documents".to_owned(),
        paths: sources.iter().map(|path| path.to_path_buf()).collect(),
        dest,
        every: "daily".to_owned(),
        keep: KeepSetting::default(),
        presets: vec!["system_junk".to_owned(), "build_artifacts".to_owned()],
        filters: Vec::new(),
        one_file_system: true,
        limit_upload: "off".to_owned(),
        limit_download: "off".to_owned(),
        enabled: true,
    }
}

/// Bytes restic cannot compress or deduplicate.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect()
}

fn text_of(key: &Key) -> String {
    key.display_groups()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

impl Rig {
    async fn new() -> Option<Self> {
        Some(Self {
            bed: bed().await?,
            keys: MemoryKeyStore::default(),
            online: AtomicBool::new(true),
            hours_ahead: AtomicI64::new(0),
            notices: Mutex::new(Vec::new()),
        })
    }

    fn root(&self) -> &Path {
        &self.bed.root
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root().join(name)
    }

    fn dest(&self, name: &str) -> String {
        self.dir(name).to_string_lossy().into_owned()
    }

    /// A folder with these files, each `(path, content)`.
    fn folder(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let folder = self.dir(name);
        fs::create_dir_all(&folder).unwrap();
        for (path, content) in files {
            let file = folder.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, content).unwrap();
        }
        folder
    }

    fn repository(&self, backup: &BackupConfig) -> Repository {
        Repository::new(
            Destination::parse(&backup.dest).unwrap(),
            self.bed.paths.backup_state_dir(&backup.id),
        )
    }

    /// Creates the repository with a new key, saves the key and writes the
    /// backup to the config.
    async fn add(&self, backup: &BackupConfig) -> Key {
        let key = Key::generate().unwrap();
        create_repository(&self.bed.restic, &self.repository(backup), &key)
            .await
            .unwrap();
        self.keys.put(&backup.id, &backup.name, &key).await.unwrap();
        self.write(backup).await;
        key
    }

    async fn write(&self, backup: &BackupConfig) {
        Config::edit(&self.bed.paths, |config| config.add_backup(backup).unwrap())
            .await
            .unwrap();
    }

    async fn try_run(
        &self,
        id: &str,
        forward_signals: bool,
    ) -> Result<BackupRunReport, BackupRunError> {
        let clock = || {
            Zoned::now()
                .checked_add(SignedDuration::from_hours(
                    self.hours_ahead.load(Ordering::SeqCst),
                ))
                .unwrap()
        };
        let env = BackupRunEnv {
            paths: &self.bed.paths,
            restic: &self.bed.restic,
            keys: &self.keys,
            network: self,
            notifier: self,
            clock: &clock,
            forward_signals,
        };
        run_backup(&env, id).await
    }

    async fn run(&self, id: &str) -> BackupRunReport {
        self.try_run(id, false).await.unwrap()
    }

    async fn state(&self, id: &str) -> BackupState {
        BackupState::load(&self.bed.paths, id).await.unwrap()
    }

    async fn snapshots(&self, backup: &BackupConfig, key: &Key) -> Vec<SnapshotView> {
        self.bed
            .restic
            .snapshots(&self.repository(backup), key)
            .await
            .unwrap()
    }

    /// `restic ...` through the wrapper, which adds this machine's host to
    /// `backup`.
    async fn restic(&self, backup: &BackupConfig, key: &Key, args: &[&str]) -> String {
        let output = self
            .bed
            .restic
            .run(&self.repository(backup), key, args, false)
            .await
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    /// A snapshot of `folder` as it was on `time` (`2025-03-01 10:00:00`).
    async fn old_snapshot(&self, backup: &BackupConfig, key: &Key, time: &str, folder: &Path) {
        let folder = folder.to_string_lossy().into_owned();
        self.restic(backup, key, &["backup", "--time", time, &folder])
            .await;
    }

    fn notices(&self) -> Vec<Notice> {
        self.notices.lock().unwrap().clone()
    }
}

fn files_under(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(folder) else {
        return found;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

fn size_of(folder: &Path) -> u64 {
    files_under(folder)
        .iter()
        .map(|file| fs::metadata(file).unwrap().len())
        .sum()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// True if any process of this user has one of `needles` in its arguments
/// or its environment right now. Also counts the restic processes seen.
fn scan_processes(needles: &[Vec<u8>], restic_seen: &mut u32) -> bool {
    let mut leaked = false;
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let path = entry.path();
        if !path.join("cmdline").exists() {
            continue;
        }
        let cmdline = fs::read(path.join("cmdline")).unwrap_or_default();
        if contains(&cmdline, b"restic") && contains(&cmdline, b"\0backup\0") {
            *restic_seen += 1;
        }
        let environ = fs::read(path.join("environ")).unwrap_or_default();
        leaked |= needles
            .iter()
            .any(|needle| contains(&cmdline, needle) || contains(&environ, needle));
    }
    leaked
}

/// The pid of a restic that is running `backup` against `repo`.
fn backup_running_against(repo: &str) -> bool {
    fs::read_dir("/proc").unwrap().flatten().any(|entry| {
        let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
        contains(&cmdline, b"\0backup\0") && contains(&cmdline, repo.as_bytes())
    })
}

deadline_tests! {
async fn the_first_backup_stores_the_folder_without_what_the_presets_leave_out() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[
        ("a.txt", "hello"),
        ("sub/b.txt", "world"),
        ("debug.log", "noise"),
        ("~$report.docx", "lock"),
    ]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    let snapshots = rig.snapshots(&b, &key).await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].files, Some(2));
    let listing = rig.restic(&b, &key, &["ls", "latest"]).await;
    let listed: Vec<&str> = listing.lines().collect();
    assert!(listed.contains(&source.join("a.txt").to_str().unwrap()), "{listing}");
    assert!(listed.contains(&source.join("sub/b.txt").to_str().unwrap()), "{listing}");
    assert!(!listing.contains("debug.log"), "{listing}");
    assert!(!listing.contains("~$report"), "{listing}");
    let excludes = fs::read_to_string(rig.bed.paths.backup_excludes_file("docs")).unwrap();
    assert!(excludes.contains("*.log") && excludes.contains("~$$*"), "{excludes}");
}

async fn a_round_leaves_the_state_the_screens_read() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;

    rig.run("docs").await;

    let state = rig.state("docs").await;
    let snapshots = rig.snapshots(&b, &key).await;
    assert!(state.gate.is_none());
    assert_eq!(state.consecutive_failures, 0);
    assert!(state.last_success.is_some() && state.last_attempt.is_some());
    assert_eq!(state.last_snapshot.as_ref().unwrap().id, snapshots[0].id);
    let last = state.last_run.unwrap();
    assert_eq!(last.exit_code, 0);
    assert_eq!(last.summary.files_new, 1);
    let log = fs::read_to_string(&last.log_file).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines[0], r#"{"syncfast_step":"backup"}"#);
    assert!(lines.iter().any(|line| line.contains(r#""message_type":"summary""#)), "{log}");
    assert!(!lines.iter().any(|line| line.contains(r#""message_type":"status""#)), "{log}");
    assert!(last.log_file.starts_with(rig.bed.paths.backup_runs_dir("docs")));
    assert_eq!(BackupProgressFile::read(&rig.bed.paths, "docs"), None);
    assert!(!is_running(&rig.bed.paths, "docs"));
}

async fn a_changed_file_makes_a_second_snapshot_that_counts_it() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello"), ("b.txt", "same")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    rig.run("docs").await;
    fs::write(source.join("a.txt"), "hello again, longer").unwrap();

    rig.run("docs").await;

    assert_eq!(rig.snapshots(&b, &key).await.len(), 2);
    let summary = rig.state("docs").await.last_run.unwrap().summary;
    assert_eq!(summary.files_changed, 1);
    assert_eq!(summary.files_new, 0);
    assert_eq!(summary.files_unmodified, 1);
}

async fn the_daily_policy_keeps_the_newest_days_and_drops_the_rest() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.keep = KeepSetting::new(3, 0, 0, 0);
    let key = rig.add(&b).await;
    for day in 1..=6 {
        rig.old_snapshot(&b, &key, &format!("2025-03-0{day} 10:00:00"), &source).await;
    }

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    let times: Vec<String> = rig
        .snapshots(&b, &key)
        .await
        .iter()
        .map(|snapshot| snapshot.time.to_string())
        .collect();
    assert_eq!(times.len(), 3, "{times:?}");
    assert!(times[1].starts_with("2025-03-06"), "{times:?}");
    assert!(times[2].starts_with("2025-03-05"), "{times:?}");
}

async fn the_yearly_policy_keeps_the_latest_snapshot_of_the_two_newest_years_and_prunes_the_data() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.keep = KeepSetting::new(0, 0, 0, 2);
    let key = rig.add(&b).await;
    for (seed, time) in [
        (1, "2022-06-01 10:00:00"),
        (2, "2023-06-01 10:00:00"),
        (3, "2024-06-01 10:00:00"),
        (4, "2024-07-01 10:00:00"),
    ] {
        let old = rig.dir(&format!("old{seed}"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("data.bin"), noise(seed, 200_000)).unwrap();
        rig.old_snapshot(&b, &key, time, &old).await;
    }
    let before = size_of(&rig.dir("repo/data"));

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    let times: Vec<String> = rig
        .snapshots(&b, &key)
        .await
        .iter()
        .map(|snapshot| snapshot.time.to_string())
        .collect();
    assert_eq!(times.len(), 2, "{times:?}");
    assert!(times[1].starts_with("2024-07-01"), "{times:?}");
    assert!(size_of(&rig.dir("repo/data")) < before / 2, "packs of dropped snapshots remain");
}

async fn a_folder_that_is_gone_or_empty_holds_the_backup_until_it_is_back() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    rig.run("docs").await;
    fs::remove_dir_all(&source).unwrap();

    let missing = rig.run("docs").await;
    let again = rig.run("docs").await;

    assert_eq!(missing, BackupRunReport::GateSet(Fault::SourceMissing));
    assert_eq!(again, BackupRunReport::GateSet(Fault::SourceMissing));
    let gate = rig.state("docs").await.gate.unwrap();
    assert_eq!(gate.kind, GateKind::NeedsAttention);
    assert_eq!(gate.fault, Some(Fault::SourceMissing));
    assert!(gate.detail.contains(source.to_str().unwrap()), "{}", gate.detail);
    assert_eq!(rig.notices().len(), 1, "told once for the same trouble");
    assert_eq!(rig.notices()[0].pair_id, "backup:docs");
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);

    fs::create_dir_all(&source).unwrap();
    let empty = rig.run("docs").await;
    assert_eq!(empty, BackupRunReport::GateSet(Fault::SourceEmpty));
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);
    open_repository(&rig.bed.restic, &rig.repository(&b), &key).await.unwrap();

    fs::write(source.join("a.txt"), "hello").unwrap();
    let back = rig.run("docs").await;
    assert_eq!(back, BackupRunReport::Ran(BackupOutcome::Ok));
    assert!(rig.state("docs").await.gate.is_none());
}

async fn a_key_that_does_not_open_the_repository_gates_the_backup_and_leaves_it_intact() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    rig.run("docs").await;
    rig.keys.put("docs", "Documents", &Key::generate().unwrap()).await.unwrap();
    fs::write(source.join("a.txt"), "changed").unwrap();

    let report = rig.run("docs").await;

    assert!(
        matches!(&report, BackupRunReport::Ran(BackupOutcome::NeedsAttention { fault: Fault::WrongKey, .. })),
        "{report:?}"
    );
    assert_eq!(rig.state("docs").await.gate.unwrap().fault, Some(Fault::WrongKey));
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);

    rig.keys.put("docs", "Documents", &key).await.unwrap();
    let fixed = rig.run("docs").await;
    assert_eq!(fixed, BackupRunReport::Ran(BackupOutcome::Ok));
    assert!(rig.state("docs").await.gate.is_none());
    assert_eq!(rig.snapshots(&b, &key).await.len(), 2);
}

async fn a_key_missing_from_the_key_storage_gates_the_backup_without_touching_restic() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    rig.keys.delete("docs").await.unwrap();

    let report = rig.run("docs").await;
    let again = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::KeyUnavailable);
    assert_eq!(again, BackupRunReport::KeyUnavailable);
    let state = rig.state("docs").await;
    assert_eq!(state.gate.unwrap().fault, Some(Fault::KeyMissing));
    assert_eq!(state.consecutive_failures, 2);
    assert_eq!(rig.notices().len(), 1);
    assert!(rig.snapshots(&b, &key).await.is_empty());
}

async fn a_destination_that_is_not_there_waits_and_later_asks_for_attention() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    fs::rename(rig.dir("repo"), rig.dir("repo-away")).unwrap();

    let first = rig.run("docs").await;

    assert_eq!(first, BackupRunReport::Skipped(SkipReason::DestinationUnavailable));
    assert_eq!(first.exit_code(), 75);
    let state = rig.state("docs").await;
    assert!(state.destination_unavailable);
    assert!(state.gate.is_none());
    assert!(!rig.dir("repo").exists(), "nothing may recreate the missing repository");

    rig.hours_ahead.store(72, Ordering::SeqCst);
    let later = rig.run("docs").await;
    assert_eq!(later.exit_code(), 75);
    let gate = rig.state("docs").await.gate.unwrap();
    assert_eq!(gate.kind, GateKind::NeedsAttention);
    assert_eq!(gate.fault, Some(Fault::DestinationMissing));

    fs::rename(rig.dir("repo-away"), rig.dir("repo")).unwrap();
    let back = rig.run("docs").await;
    assert_eq!(back, BackupRunReport::Ran(BackupOutcome::Ok));
    let state = rig.state("docs").await;
    assert!(!state.destination_unavailable && state.gate.is_none());
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);
}

async fn an_account_destination_waits_offline_and_backs_up_once_online() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let b = backup("docs", &[&source], format!("testremote:{}", rig.dest("repo")));
    let key = rig.add(&b).await;
    rig.online.store(false, Ordering::SeqCst);

    let offline = rig.run("docs").await;

    assert_eq!(offline, BackupRunReport::Skipped(SkipReason::Offline));
    assert_eq!(offline.exit_code(), 75);
    assert!(rig.state("docs").await.offline);
    assert!(rig.snapshots(&b, &key).await.is_empty());

    rig.online.store(true, Ordering::SeqCst);
    let online = rig.run("docs").await;
    assert_eq!(online, BackupRunReport::Ran(BackupOutcome::Ok));
    assert!(!rig.state("docs").await.offline);
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);
}

async fn a_file_restic_cannot_read_still_gives_a_snapshot_and_a_warning() {
    if rustix::process::getuid().is_root() {
        return;
    }
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello"), ("secret.txt", "closed")]);
    fs::set_permissions(source.join("secret.txt"), fs::Permissions::from_mode(0o000)).unwrap();
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;

    let report = rig.run("docs").await;

    assert!(
        matches!(report, BackupRunReport::Ran(BackupOutcome::OkWithWarnings { warnings }) if warnings >= 1),
        "{report:?}"
    );
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);
    let state = rig.state("docs").await;
    assert_eq!(state.last_run.unwrap().exit_code, 3);
    assert_eq!(state.consecutive_failures, 0);
    assert!(state.last_success.is_some());
}

async fn the_key_is_never_written_or_shown_anywhere_a_round_can_reach() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    let wrong = Key::generate().unwrap();
    let needles: Vec<Vec<u8>> = [&key, &wrong]
        .iter()
        .flat_map(|key| [text_of(key), key.display_groups()])
        .map(String::into_bytes)
        .collect();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let watcher = {
        let (stop, needles) = (stop.clone(), needles.clone());
        tokio::task::spawn_blocking(move || {
            let (mut leaked, mut seen) = (false, 0);
            while !stop.load(Ordering::SeqCst) {
                leaked |= scan_processes(&needles, &mut seen);
                std::thread::sleep(Duration::from_millis(4));
            }
            (leaked, seen)
        })
    };

    let first = rig.run("docs").await;
    rig.keys.put("docs", "Documents", &wrong).await.unwrap();
    fs::write(source.join("a.txt"), "changed").unwrap();
    let second = rig.run("docs").await;
    stop.store(true, Ordering::SeqCst);
    let (leaked, seen) = watcher.await.unwrap();

    assert_eq!(first, BackupRunReport::Ran(BackupOutcome::Ok));
    assert!(matches!(second, BackupRunReport::Ran(BackupOutcome::NeedsAttention { .. })));
    assert!(seen >= 1, "the scan never saw a restic backup, so it proves nothing");
    assert!(!leaked, "a key was in a command line or an environment");
    for file in files_under(rig.root()) {
        let bytes = fs::read(&file).unwrap();
        for needle in &needles {
            assert!(!contains(&bytes, needle), "a key is in {}", file.display());
        }
    }
    let said = format!("{first:?} {second:?} {:?}", rig.notices());
    assert!(needles.iter().all(|needle| !said.contains(&String::from_utf8_lossy(needle).into_owned())));
}

async fn only_this_machines_snapshots_are_pruned_and_another_machine_opens_the_same_repository() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.keep = KeepSetting::new(1, 0, 0, 0);
    let key = rig.add(&b).await;
    rig.old_snapshot(&b, &key, "2025-01-01 10:00:00", &source).await;
    for time in ["2025-01-02 10:00:00", "2025-01-03 10:00:00"] {
        let cache = rig.dir("other-cache");
        let status = tokio::process::Command::new("restic")
            .args(["-r", &rig.dest("repo"), "backup", "--host", "outro", "--time", time])
            .arg(&source)
            .env("RESTIC_PASSWORD", text_of(&key))
            .env("RESTIC_CACHE_DIR", cache)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await
            .unwrap();
        assert!(status.success());
    }

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    let other_paths = Paths::new(
        rig.dir("home2"),
        rig.dir("config2"),
        rig.dir("state2"),
        rig.dir("data2"),
        rig.dir("run2"),
    );
    let second = Restic::from_env(rig.bed.restic.rclone().clone(), &other_paths)
        .await
        .unwrap();
    let repository = Repository::new(
        Destination::parse(&b.dest).unwrap(),
        other_paths.backup_state_dir("docs"),
    );
    let same_key = Key::parse(&key.display_groups()).unwrap();
    open_repository(&second, &repository, &same_key).await.unwrap();
    let snapshots = second.snapshots(&repository, &same_key).await.unwrap();
    let own = syncfast_core::restic::backup_host().unwrap();
    let hosts: Vec<&str> = snapshots.iter().map(|snapshot| snapshot.host.as_str()).collect();
    assert_eq!(hosts.iter().filter(|host| **host == "outro").count(), 2, "{hosts:?}");
    assert_eq!(hosts.iter().filter(|host| **host == own).count(), 1, "{hosts:?}");
}

async fn the_upload_limit_slows_the_backup_down() {
    let Some(rig) = Rig::new().await else { return };
    let slow_source = rig.dir("slow-docs");
    fs::create_dir_all(&slow_source).unwrap();
    fs::write(slow_source.join("big.bin"), noise(7, 2 * 1024 * 1024)).unwrap();
    let fast_source = rig.dir("fast-docs");
    fs::create_dir_all(&fast_source).unwrap();
    fs::write(fast_source.join("big.bin"), noise(8, 2 * 1024 * 1024)).unwrap();
    let mut slow = backup("slow", &[&slow_source], rig.dest("slow-repo"));
    slow.limit_upload = "512K".to_owned();
    let fast = backup("fast", &[&fast_source], rig.dest("fast-repo"));
    rig.add(&slow).await;
    rig.add(&fast).await;

    let started = Instant::now();
    rig.run("fast").await;
    let fast_took = started.elapsed();
    let started = Instant::now();
    let report = rig.run("slow").await;
    let slow_took = started.elapsed();

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    assert!(slow_took >= Duration::from_secs(2), "{slow_took:?}");
    assert!(fast_took < slow_took, "{fast_took:?} vs {slow_took:?}");
}

async fn a_second_round_of_the_same_backup_does_not_wait_for_the_first_but_reports_busy() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.dir("docs");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("big.bin"), noise(9, 2 * 1024 * 1024)).unwrap();
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.limit_upload = "512K".to_owned();
    let key = rig.add(&b).await;
    let repo = rig.dest("repo");

    let (first, second) = tokio::join!(rig.run("docs"), async {
        while !backup_running_against(&repo) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        rig.run("docs").await
    });

    assert_eq!(first, BackupRunReport::Ran(BackupOutcome::Ok));
    assert_eq!(second, BackupRunReport::Busy);
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);
}

async fn an_interrupt_stops_restic_cleanly_and_leaves_no_lock_and_a_consistent_state() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.dir("docs");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("big.bin"), noise(10, 6 * 1024 * 1024)).unwrap();
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.limit_upload = "512K".to_owned();
    let key = rig.add(&b).await;
    let repo = rig.dest("repo");

    let (report, ()) = tokio::join!(rig.try_run("docs", true), async {
        while !backup_running_against(&repo) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::INT)
            .unwrap();
    });

    assert_eq!(report.unwrap(), BackupRunReport::Ran(BackupOutcome::Cancelled));
    let state = rig.state("docs").await;
    assert!(state.gate.is_none());
    assert_eq!(state.consecutive_failures, 0);
    assert!(state.last_attempt.is_some());
    assert!(!is_running(&rig.bed.paths, "docs"));
    assert_eq!(BackupProgressFile::read(&rig.bed.paths, "docs"), None);
    assert_eq!(rig.restic(&b, &key, &["list", "locks"]).await.trim(), "");
    assert!(rig.snapshots(&b, &key).await.is_empty());
}

async fn a_paused_backup_does_nothing_and_an_unknown_id_is_an_error_for_the_user_to_fix() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.enabled = false;
    let key = rig.add(&b).await;

    let paused = rig.run("docs").await;
    let unknown = rig.try_run("nope", false).await.unwrap_err();

    assert_eq!(paused, BackupRunReport::Skipped(SkipReason::Paused));
    assert_eq!(paused.exit_code(), 0);
    assert!(rig.snapshots(&b, &key).await.is_empty());
    assert!(matches!(unknown, BackupRunError::UnknownBackup(_)));
    assert_eq!(unknown.exit_code(), 2);
}

async fn a_backup_the_config_got_wrong_is_gated_with_the_reason_until_the_config_is_fixed() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", "hello")]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.every = "sometimes".to_owned();
    let key = rig.add(&b).await;

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::ConfigInvalid);
    let gate = rig.state("docs").await.gate.unwrap();
    assert_eq!(gate.kind, GateKind::ConfigError);
    assert!(!gate.detail.is_empty());
    assert_eq!(rig.notices().len(), 1);
    assert!(rig.snapshots(&b, &key).await.is_empty());

    b.every = "daily".to_owned();
    Config::edit(&rig.bed.paths, |config| config.replace_backup(&b)).await.unwrap();
    let fixed = rig.run("docs").await;
    assert_eq!(fixed, BackupRunReport::Ran(BackupOutcome::Ok));
    assert!(rig.state("docs").await.gate.is_none());
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);
}
}
