//! Integrity checks and stale locks: `run_backup` against a real restic on a
//! folder repository (spec phase 3, steps 8 and 16).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jiff::{SignedDuration, Zoned};
use syncfast_core::backup::check::CheckOutcome;
use syncfast_core::backup::classify::BackupOutcome;
use syncfast_core::backup::config::{BackupConfig, KeepSetting};
use syncfast_core::backup::destination::{Destination, create_repository};
use syncfast_core::backup::effects::{LOCKED_DETAIL, key_created_by_agent};
use syncfast_core::backup::key::{Key, KeyError};
use syncfast_core::backup::keystore::{KeyBackend, KeyStore, MemoryKeyStore};
use syncfast_core::backup::lifecycle::mark_key_saved;
use syncfast_core::backup::run::{BackupRunEnv, BackupRunError, BackupRunReport, run_backup};
use syncfast_core::backup::run_decision::{BackupRequest, SkipReason, save_request};
use syncfast_core::backup::snapshots::SnapshotView;
use syncfast_core::backup::state::{BackupState, CheckKind};
use syncfast_core::bisync::Fault;
use syncfast_core::config::Config;
use syncfast_core::network::{BoxFuture, Network};
use syncfast_core::notify::{Notice, Notifier};
use syncfast_core::paths::Paths;
use syncfast_core::rclone::{Rclone, RcloneError};
use syncfast_core::restic::{Repository, Restic, ResticError};
use syncfast_core::state::{Gate, GateKind};

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

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
    restic: Restic,
    keys: MemoryKeyStore,
    notices: Mutex<Vec<Notice>>,
}

impl Network for Rig {
    fn is_online(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
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

/// The pid of the restic running `backup` against `repo`, if any.
fn backup_pid_against(repo: &str) -> Option<i32> {
    fs::read_dir("/proc").unwrap().flatten().find_map(|entry| {
        let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
        let cmdline = fs::read(entry.path().join("cmdline")).ok()?;
        (contains(&cmdline, b"\0backup\0") && contains(&cmdline, repo.as_bytes())).then_some(pid)
    })
}

async fn wait_for_backup(repo: &str) -> i32 {
    for _ in 0..1500 {
        if let Some(pid) = backup_pid_against(repo) {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no restic backup ever ran against {repo}");
}

async fn wait_for_a_lock(repo: &Path) {
    for _ in 0..1500 {
        if !files_under(&repo.join("locks")).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the backup never took a lock in {}", repo.display());
}

impl Rig {
    #[allow(clippy::print_stderr)]
    async fn new() -> Option<Self> {
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
        Some(Self {
            _dir: dir,
            root,
            paths,
            restic,
            keys: MemoryKeyStore::default(),
            notices: Mutex::new(Vec::new()),
        })
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn dest(&self, name: &str) -> String {
        self.dir(name).to_string_lossy().into_owned()
    }

    fn folder(&self, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
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
            self.paths.backup_state_dir(&backup.id),
        )
    }

    async fn add(&self, backup: &BackupConfig) -> Key {
        let key = Key::generate().unwrap();
        create_repository(&self.restic, &self.repository(backup), &key)
            .await
            .unwrap();
        self.keys.put(&backup.id, &backup.name, &key).await.unwrap();
        Config::edit(&self.paths, |config| config.add_backup(backup).unwrap())
            .await
            .unwrap();
        key
    }

    async fn try_run(&self, id: &str) -> Result<BackupRunReport, BackupRunError> {
        self.try_run_with(id, &self.keys).await
    }

    async fn try_run_with(
        &self,
        id: &str,
        keys: &dyn KeyStore,
    ) -> Result<BackupRunReport, BackupRunError> {
        let clock = || Zoned::now().checked_add(SignedDuration::ZERO).unwrap();
        let env = BackupRunEnv {
            paths: &self.paths,
            restic: &self.restic,
            keys,
            network: self,
            notifier: self,
            clock: &clock,
            forward_signals: false,
        };
        run_backup(&env, id).await
    }

    async fn run(&self, id: &str) -> BackupRunReport {
        self.try_run(id).await.unwrap()
    }

    async fn check(&self, id: &str, kind: CheckKind) -> BackupRunReport {
        save_request(&self.paths, id, BackupRequest::Check(kind))
            .await
            .unwrap();
        self.run(id).await
    }

    async fn state(&self, id: &str) -> BackupState {
        BackupState::load(&self.paths, id).await.unwrap()
    }

    async fn snapshots(&self, backup: &BackupConfig, key: &Key) -> Vec<SnapshotView> {
        self.restic
            .snapshots(&self.repository(backup), key)
            .await
            .unwrap()
    }

    async fn restic(&self, backup: &BackupConfig, key: &Key, args: &[&str]) -> String {
        let output = self
            .restic
            .run(&self.repository(backup), key, args, false)
            .await
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    async fn old_snapshot(&self, backup: &BackupConfig, key: &Key, time: &str, folder: &Path) {
        let folder = folder.to_string_lossy().into_owned();
        self.restic(backup, key, &["backup", "--time", time, &folder])
            .await;
    }

    fn notices(&self) -> Vec<Notice> {
        self.notices.lock().unwrap().clone()
    }

    /// Runs restic through a script that logs every call to `calls.log` and
    /// fails `backup` when `fail_backup` is set.
    fn use_recording_restic(&mut self, fail_backup: bool) -> PathBuf {
        let real = syncfast_core::restic::locate_binary().expect("restic is installed");
        let calls = self.dir("calls.log");
        let script = self.dir("restic-recording");
        let failing = if fail_backup {
            "for arg in \"$@\"; do\n  if [ \"$arg\" = backup ]; then\n    \
             echo 'Fatal: simulated backup failure' >&2\n    exit 1\n  fi\ndone\n"
        } else {
            ""
        };
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\n{failing}exec '{}' \"$@\"\n",
                calls.display(),
                real.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let rclone = self.restic.rclone().clone();
        self.restic = Restic::new(script, rclone, &self.paths);
        calls
    }

    /// Runs restic through a script that logs every call to `calls.log` and
    /// answers the listed subcommands with `rule` instead of running them.
    fn use_scripted_restic(&mut self, rules: &[(&str, Rule)]) -> PathBuf {
        let real = syncfast_core::restic::locate_binary().expect("restic is installed");
        let calls = self.dir("calls.log");
        let script = self.dir("restic-scripted");
        let locked = self.dir("locked.err");
        fs::write(&locked, include_str!("fixtures/restic-0.19.1/locked.err")).unwrap();
        let mut arms = String::new();
        for (subcommand, rule) in rules {
            let once = self.dir(&format!("answered-{subcommand}"));
            let body = match rule {
                Rule::LockedOnce => format!(
                    "if [ ! -e '{once}' ]; then touch '{once}'; cat '{locked}' >&2; exit 11; fi",
                    once = once.display(),
                    locked = locked.display()
                ),
                Rule::LockedAlways => {
                    format!("cat '{}' >&2; exit 11", locked.display())
                }
                Rule::FailAlways => "echo 'Fatal: simulated failure' >&2; exit 1".to_owned(),
                Rule::Interrupted => "exit 130".to_owned(),
                Rule::NotUtf8OnBothStreams => {
                    "printf 'bad \\377\\n'; printf 'bad \\377\\n' >&2".to_owned()
                }
            };
            writeln!(arms, "    {subcommand}) {body} ;;").unwrap();
        }
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nfor arg in \"$@\"; do\n  case \"$arg\" in\n{arms}  esac\ndone\nexec '{}' \"$@\"\n",
                calls.display(),
                real.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let rclone = self.restic.rclone().clone();
        self.restic = Restic::new(script, rclone, &self.paths);
        calls
    }

    /// A restic that is not there: every start fails.
    fn use_missing_restic(&mut self) {
        let rclone = self.restic.rclone().clone();
        self.restic = Restic::new(self.dir("no-such-restic"), rclone, &self.paths);
    }
}

async fn mark_damaged(rig: &Rig) {
    let mut state = rig.state("docs").await;
    state.gate = Some(Gate {
        kind: GateKind::NeedsAttention,
        since: Zoned::now().timestamp(),
        detail: "The check found 2 damaged files in the backup. Backups and cleanup are paused."
            .to_owned(),
        fault: Some(Fault::RepoDamaged),
        auto_checked: None,
    });
    state.save(&rig.paths, "docs").await.unwrap();
}

#[derive(Clone, Copy)]
enum Rule {
    LockedOnce,
    LockedAlways,
    FailAlways,
    Interrupted,
    NotUtf8OnBothStreams,
}

struct StopOnDrop(std::sync::Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn calls_with(log: &Path, word: &str) -> Vec<String> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.split_whitespace().any(|arg| arg == word))
        .map(str::to_owned)
        .collect()
}

/// The biggest pack of the repository: the one holding the file's data.
fn biggest_pack(repo: &Path) -> PathBuf {
    files_under(&repo.join("data"))
        .into_iter()
        .max_by_key(|file| fs::metadata(file).unwrap().len())
        .unwrap()
}

fn is_checked(report: &BackupRunReport) -> Option<&CheckOutcome> {
    match report {
        BackupRunReport::Checked(Some(outcome)) => Some(outcome),
        _ => None,
    }
}

/// A key store that answers like the person clicked `I Saved the Key` while
/// the round was reading the key.
struct SavesTheKeyWhenRead {
    inner: MemoryKeyStore,
    paths: Paths,
}

impl KeyStore for SavesTheKeyWhenRead {
    fn get<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<Option<Key>, KeyError>> {
        Box::pin(async move {
            mark_key_saved(&self.paths, backup_id).await.unwrap();
            self.inner.get(backup_id).await
        })
    }

    fn put<'a>(
        &'a self,
        backup_id: &'a str,
        backup_name: &'a str,
        key: &'a Key,
    ) -> BoxFuture<'a, Result<(), KeyError>> {
        self.inner.put(backup_id, backup_name, key)
    }

    fn delete<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), KeyError>> {
        self.inner.delete(backup_id)
    }

    fn backend(&self) -> Option<KeyBackend> {
        self.inner.backend()
    }

    fn probe(&self) -> BoxFuture<'_, Result<KeyBackend, KeyError>> {
        self.inner.probe()
    }
}

async fn fix_the_config(rig: &Rig, backup: &BackupConfig) {
    Config::edit(&rig.paths, |config| {
        config.remove_backup(&backup.id);
        config.add_backup(backup).unwrap();
    })
    .await
    .unwrap();
}

/// Both flags a skipped round leaves behind, long enough ago to be stale.
async fn leave_unreachable_flags(rig: &Rig) {
    let mut state = rig.state("docs").await;
    state.offline = true;
    state.destination_unavailable = true;
    state.unavailable_since = Some(
        Zoned::now()
            .checked_sub(SignedDuration::from_hours(1))
            .unwrap()
            .timestamp(),
    );
    state.save(&rig.paths, "docs").await.unwrap();
}

async fn assert_reachable(rig: &Rig) {
    let state = rig.state("docs").await;
    assert!(!state.offline, "{state:?}");
    assert!(!state.destination_unavailable, "{state:?}");
    assert_eq!(state.unavailable_since, None);
}

deadline_tests! {
async fn a_backup_killed_with_kill_9_does_not_stop_the_next_round_from_pruning() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("big.bin", &noise(1, 6 * 1024 * 1024))]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.keep = KeepSetting::new(1, 0, 0, 0);
    b.limit_upload = "512K".to_owned();
    let key = rig.add(&b).await;
    let small = rig.folder("old", &[("a.txt", b"hello")]);
    rig.old_snapshot(&b, &key, "2025-03-01 12:00:00", &small).await;
    rig.old_snapshot(&b, &key, "2025-03-02 12:00:00", &small).await;
    let repo = rig.dest("repo");

    let (first, ()) = tokio::join!(rig.run("docs"), async {
        let pid = wait_for_backup(&repo).await;
        wait_for_a_lock(&rig.dir("repo")).await;
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).unwrap(),
            rustix::process::Signal::KILL,
        )
        .unwrap();
    });

    assert!(
        matches!(first, BackupRunReport::Ran(BackupOutcome::Transient { .. })),
        "{first:?}"
    );
    assert_eq!(rig.snapshots(&b, &key).await.len(), 2);

    fs::remove_file(source.join("big.bin")).unwrap();
    fs::write(source.join("a.txt"), "hello").unwrap();
    let second = rig.run("docs").await;

    assert_eq!(second, BackupRunReport::Ran(BackupOutcome::Ok));
    let state = rig.state("docs").await;
    assert_eq!(state.prune_failures, 0);
    assert_eq!(state.prune_error, None);
    assert!(state.gate.is_none());
    assert_eq!(rig.restic(&b, &key, &["list", "locks"]).await.trim(), "");
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1, "forget must have run");
}

async fn a_corrupted_pack_gates_the_backup_until_a_full_check_finds_it_whole_again() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("big.bin", &noise(2, 512 * 1024)), ("a.txt", b"hello")]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.keep = KeepSetting::new(1, 0, 0, 0);
    let key = rig.add(&b).await;
    assert_eq!(rig.run("docs").await, BackupRunReport::Ran(BackupOutcome::Ok));
    let pack = biggest_pack(&rig.dir("repo"));
    let whole = fs::read(&pack).unwrap();
    let mut broken = whole.clone();
    let middle = broken.len() / 2;
    for byte in &mut broken[middle..middle + 16] {
        *byte ^= 0xFF;
    }
    fs::set_permissions(&pack, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&pack, &broken).unwrap();

    let full = rig.check("docs", CheckKind::Full).await;

    assert!(matches!(is_checked(&full), Some(CheckOutcome::Damaged { .. })), "{full:?}");
    let state = rig.state("docs").await;
    let gate = state.gate.as_ref().unwrap();
    assert_eq!(gate.kind, GateKind::NeedsAttention);
    assert_eq!(gate.fault, Some(Fault::RepoDamaged));
    assert!(gate.detail.starts_with("The check found "), "{}", gate.detail);
    assert!(!state.last_check.as_ref().unwrap().ok);
    assert_eq!(state.consecutive_failures, 0);
    assert_eq!(rig.notices().len(), 1);

    rig.old_snapshot(&b, &key, "2025-03-01 12:00:00", &source).await;
    rig.old_snapshot(&b, &key, "2025-03-02 12:00:00", &source).await;
    let before = rig.snapshots(&b, &key).await.len();
    let held = rig.run("docs").await;

    assert_eq!(held, BackupRunReport::Skipped(SkipReason::Gated));
    assert_eq!(rig.snapshots(&b, &key).await.len(), before, "a gated round must not back up or forget");

    let quick = rig.check("docs", CheckKind::Quick).await;
    assert_eq!(is_checked(&quick), Some(&CheckOutcome::Ok), "a quick check reads no pack: {quick:?}");
    assert!(rig.state("docs").await.gate.is_some(), "a quick check proves nothing about packs");

    fs::write(&pack, &whole).unwrap();
    let again = rig.check("docs", CheckKind::Full).await;

    assert_eq!(is_checked(&again), Some(&CheckOutcome::Ok), "{again:?}");
    let state = rig.state("docs").await;
    assert!(state.gate.is_none());
    assert!(state.last_check.as_ref().unwrap().ok);
    assert_eq!(rig.run("docs").await, BackupRunReport::Ran(BackupOutcome::Ok));
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1, "forget runs again once the gate is gone");
    assert_eq!(rig.notices().len(), 1);
}

async fn a_cleanup_that_meets_a_live_lock_waits_for_the_other_process_and_says_so() {
    let Some(rig) = Rig::new().await else { return };
    let big = rig.folder("big", &[("big.bin", &noise(3, 4 * 1024 * 1024))]);
    let small = rig.folder("small", &[("a.txt", b"hello")]);
    let mut b = backup("docs", &[&small], rig.dest("repo"));
    b.keep = KeepSetting::new(1, 0, 0, 0);
    let key = rig.add(&b).await;
    let old = rig.folder("old", &[("o.txt", b"old")]);
    rig.old_snapshot(&b, &key, "2025-03-01 12:00:00", &old).await;
    rig.old_snapshot(&b, &key, "2025-03-02 12:00:00", &old).await;
    let big_path = big.to_string_lossy().into_owned();
    let mut other = rig
        .restic
        .spawn(
            &rig.repository(&b),
            &key,
            &["backup", "--limit-upload", "512", "--quiet", &big_path],
        )
        .await
        .unwrap();
    wait_for_a_lock(&rig.dir("repo")).await;

    let report = rig.run("docs").await;
    other.kill().await.unwrap();

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    let state = rig.state("docs").await;
    assert_eq!(state.prune_failures, 1);
    assert_eq!(state.prune_error.as_deref(), Some(LOCKED_DETAIL));
    assert!(state.gate.is_none());
}

async fn a_requested_check_runs_only_the_check_and_leaves_its_own_log() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    assert_eq!(rig.run("docs").await, BackupRunReport::Ran(BackupOutcome::Ok));
    fs::write(source.join("a.txt"), "changed").unwrap();
    let before = rig.snapshots(&b, &key).await.len();

    let report = rig.check("docs", CheckKind::Sample).await;

    assert_eq!(is_checked(&report), Some(&CheckOutcome::Ok), "{report:?}");
    assert_eq!(rig.snapshots(&b, &key).await.len(), before, "a check must not back up");
    assert!(!rig.paths.backup_request_file("docs").exists());
    let state = rig.state("docs").await;
    let last = state.last_check.unwrap();
    assert_eq!((last.kind, last.ok), (CheckKind::Sample, true));
    let logs: Vec<PathBuf> = files_under(&rig.paths.backup_runs_dir("docs"));
    let headers: Vec<String> = logs
        .iter()
        .map(|log| fs::read_to_string(log).unwrap().lines().next().unwrap().to_owned())
        .collect();
    assert!(headers.contains(&r#"{"syncfast_step":"check","kind":"sample"}"#.to_owned()), "{headers:?}");
    assert!(headers.contains(&r#"{"syncfast_step":"backup"}"#.to_owned()), "{headers:?}");

    assert_eq!(rig.run("docs").await, BackupRunReport::Ran(BackupOutcome::Ok));
    assert_eq!(rig.snapshots(&b, &key).await.len(), before + 1);
}

async fn a_monthly_sample_check_follows_the_backup_that_finds_it_due_and_moves_the_date() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    let calls = rig.use_recording_restic(false);
    assert_eq!(rig.run("docs").await, BackupRunReport::Ran(BackupOutcome::Ok));
    assert!(calls_with(&calls, "check").is_empty(), "the first backup is not due a check");
    let mut state = rig.state("docs").await;
    let past = Zoned::now()
        .checked_sub(SignedDuration::from_hours(1))
        .unwrap()
        .timestamp();
    state.next_check = Some(past);
    state.save(&rig.paths, "docs").await.unwrap();
    fs::write(source.join("a.txt"), "changed").unwrap();

    assert_eq!(rig.run("docs").await, BackupRunReport::Ran(BackupOutcome::Ok));

    let checks = calls_with(&calls, "check");
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert!(checks[0].contains("--read-data-subset=5%"), "{}", checks[0]);
    let state = rig.state("docs").await;
    let last = state.last_check.unwrap();
    assert_eq!((last.kind, last.ok), (CheckKind::Sample, true));
    assert!(state.next_check.unwrap() > past + SignedDuration::from_hours(24 * 29));
    fs::write(source.join("a.txt"), "changed again").unwrap();
    assert_eq!(rig.run("docs").await, BackupRunReport::Ran(BackupOutcome::Ok));
    assert_eq!(calls_with(&calls, "check").len(), 1, "the check is not due again");
}

async fn a_series_of_failed_backups_runs_one_quick_check_that_reads_no_data() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    let calls = rig.use_recording_restic(true);

    for _ in 0..3 {
        let report = rig.run("docs").await;
        assert!(
            matches!(report, BackupRunReport::Ran(BackupOutcome::Transient { .. })),
            "{report:?}"
        );
    }

    let checks = calls_with(&calls, "check");
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert!(!checks[0].contains("--read-data"), "{}", checks[0]);
    let state = rig.state("docs").await;
    assert_eq!(state.consecutive_failures, 3);
    let last = state.last_check.unwrap();
    assert_eq!((last.kind, last.ok), (CheckKind::Quick, true));
    assert!(state.gate.is_none());
}

async fn the_key_is_in_no_file_after_checks_and_requests() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello"), ("big.bin", &noise(4, 256 * 1024))]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    let needles: Vec<Vec<u8>> = [
        key.display_groups().chars().filter(char::is_ascii_alphanumeric).collect::<String>(),
        key.display_groups(),
    ]
    .into_iter()
    .map(String::into_bytes)
    .collect();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let _stops_the_watcher = StopOnDrop(stop.clone());
    let watcher = {
        let (stop, needles) = (stop.clone(), needles.clone());
        tokio::task::spawn_blocking(move || {
            let mut leaked = false;
            while !stop.load(Ordering::SeqCst) {
                for entry in fs::read_dir("/proc").unwrap().flatten() {
                    let path = entry.path();
                    let cmdline = fs::read(path.join("cmdline")).unwrap_or_default();
                    let environ = fs::read(path.join("environ")).unwrap_or_default();
                    leaked |= needles
                        .iter()
                        .any(|needle| contains(&cmdline, needle) || contains(&environ, needle));
                }
                std::thread::sleep(Duration::from_millis(4));
            }
            leaked
        })
    };

    save_request(&rig.paths, "docs", BackupRequest::Check(CheckKind::Full))
        .await
        .unwrap();
    let request = fs::read(rig.paths.backup_request_file("docs")).unwrap();
    let full = rig.run("docs").await;
    let backup_round = rig.run("docs").await;
    let sample = rig.check("docs", CheckKind::Sample).await;
    stop.store(true, Ordering::SeqCst);
    let leaked = watcher.await.unwrap();

    assert!(needles.iter().all(|needle| !contains(&request, needle)));
    assert_eq!(is_checked(&full), Some(&CheckOutcome::Ok));
    assert_eq!(backup_round, BackupRunReport::Ran(BackupOutcome::Ok));
    assert_eq!(is_checked(&sample), Some(&CheckOutcome::Ok));
    assert!(!leaked, "a key was in a command line or an environment");
    for file in files_under(&rig.root) {
        let bytes = fs::read(&file).unwrap();
        for needle in &needles {
            assert!(!contains(&bytes, needle), "a key is in {}", file.display());
        }
    }
    let said = format!("{full:?} {backup_round:?} {sample:?} {:?}", rig.notices());
    assert!(needles.iter().all(|needle| !said.contains(&String::from_utf8_lossy(needle).into_owned())));
}

async fn a_check_that_meets_a_lock_unlocks_without_remove_all_and_runs_again() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    let calls = rig.use_scripted_restic(&[("check", Rule::LockedOnce)]);

    let report = rig.check("docs", CheckKind::Quick).await;

    assert_eq!(is_checked(&report), Some(&CheckOutcome::Ok), "{report:?}");
    let unlocks = calls_with(&calls, "unlock");
    assert_eq!(unlocks.len(), 1, "{unlocks:?}");
    assert!(!unlocks[0].contains("--remove-all"), "{}", unlocks[0]);
    assert_eq!(calls_with(&calls, "check").len(), 2);
}

async fn a_check_that_stays_locked_is_locked_and_asks_nothing_more() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    let calls = rig.use_scripted_restic(&[("check", Rule::LockedAlways)]);

    let report = rig.check("docs", CheckKind::Quick).await;

    assert_eq!(is_checked(&report), Some(&CheckOutcome::Locked), "{report:?}");
    assert_eq!(calls_with(&calls, "unlock").len(), 1);
    assert_eq!(calls_with(&calls, "check").len(), 2);
    assert!(rig.state("docs").await.last_check.is_none());
}

async fn output_that_is_not_utf8_does_not_end_the_backup_or_hide_its_summary() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    rig.use_scripted_restic(&[("backup", Rule::NotUtf8OnBothStreams)]);

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);
    assert!(rig.state("docs").await.last_success.is_some());
}

async fn a_backup_that_meets_a_lock_unlocks_without_remove_all_and_runs_again() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    let calls = rig.use_scripted_restic(&[("backup", Rule::LockedOnce)]);

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    let unlocks = calls_with(&calls, "unlock");
    assert_eq!(unlocks.len(), 1, "{unlocks:?}");
    assert!(!unlocks[0].contains("--remove-all"), "{}", unlocks[0]);
    assert_eq!(calls_with(&calls, "backup").len(), 2);
    assert_eq!(rig.snapshots(&b, &key).await.len(), 1);
}

async fn a_backup_that_stays_locked_is_locked_and_runs_no_check() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    let calls = rig.use_scripted_restic(&[("backup", Rule::LockedAlways)]);

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Locked));
    assert_eq!(calls_with(&calls, "unlock").len(), 1);
    assert_eq!(calls_with(&calls, "backup").len(), 2);
    assert!(calls_with(&calls, "check").is_empty());
}

async fn a_monthly_check_that_stays_locked_leaves_the_backup_ok_and_the_check_due() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    assert_eq!(rig.run("docs").await, BackupRunReport::Ran(BackupOutcome::Ok));
    let mut state = rig.state("docs").await;
    let past = Zoned::now()
        .checked_sub(SignedDuration::from_hours(1))
        .unwrap()
        .timestamp();
    state.next_check = Some(past);
    state.save(&rig.paths, "docs").await.unwrap();
    fs::write(source.join("a.txt"), "changed").unwrap();
    let calls = rig.use_scripted_restic(&[("check", Rule::LockedAlways)]);

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    assert_eq!(calls_with(&calls, "check").len(), 2);
    assert_eq!(calls_with(&calls, "unlock").len(), 1);
    let state = rig.state("docs").await;
    assert_eq!(state.next_check, Some(past));
    assert_eq!(state.consecutive_failures, 0);
    assert!(state.gate.is_none());
}

async fn a_post_failure_check_that_stays_locked_does_not_count_a_second_failure() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    let calls = rig.use_scripted_restic(&[
        ("backup", Rule::FailAlways),
        ("check", Rule::LockedAlways),
    ]);

    let report = rig.run("docs").await;

    assert!(
        matches!(report, BackupRunReport::Ran(BackupOutcome::Transient { .. })),
        "{report:?}"
    );
    assert_eq!(calls_with(&calls, "check").len(), 2);
    assert_eq!(calls_with(&calls, "unlock").len(), 1);
    assert_eq!(rig.state("docs").await.consecutive_failures, 1);
}

async fn a_check_again_without_the_key_keeps_the_damaged_gate_and_the_backup_held() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    mark_damaged(&rig).await;
    rig.keys.delete("docs").await.unwrap();

    let again = rig.check("docs", CheckKind::Full).await;

    assert_eq!(again, BackupRunReport::KeyUnavailable);
    let gate = rig.state("docs").await.gate.unwrap();
    assert_eq!(gate.fault, Some(Fault::RepoDamaged));
    assert_eq!(rig.run("docs").await, BackupRunReport::Skipped(SkipReason::Gated));
    assert_eq!(rig.state("docs").await.gate.unwrap().fault, Some(Fault::RepoDamaged));
}

async fn a_config_error_keeps_the_damaged_gate_after_the_config_is_fixed() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.every = "sometimes".to_owned();
    rig.add(&b).await;
    mark_damaged(&rig).await;

    assert_eq!(rig.run("docs").await, BackupRunReport::ConfigInvalid);
    assert_eq!(rig.state("docs").await.gate.unwrap().fault, Some(Fault::RepoDamaged));
    b.every = "daily".to_owned();
    Config::edit(&rig.paths, |config| {
        config.remove_backup("docs");
        config.add_backup(&b).unwrap();
    })
    .await
    .unwrap();

    assert_eq!(rig.run("docs").await, BackupRunReport::Skipped(SkipReason::Gated));
    assert_eq!(rig.state("docs").await.gate.unwrap().fault, Some(Fault::RepoDamaged));
}

async fn a_request_survives_a_round_that_could_not_read_the_config() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.every = "sometimes".to_owned();
    rig.add(&b).await;
    save_request(&rig.paths, "docs", BackupRequest::Check(CheckKind::Full))
        .await
        .unwrap();

    assert_eq!(rig.run("docs").await, BackupRunReport::ConfigInvalid);
    b.every = "daily".to_owned();
    fix_the_config(&rig, &b).await;
    let report = rig.run("docs").await;

    assert!(is_checked(&report).is_some(), "{report:?}");
}

async fn a_request_is_dropped_by_a_round_that_skips_a_paused_backup() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let mut b = backup("docs", &[&source], rig.dest("repo"));
    b.enabled = false;
    rig.add(&b).await;
    save_request(&rig.paths, "docs", BackupRequest::Check(CheckKind::Full))
        .await
        .unwrap();

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Skipped(SkipReason::Paused));
    assert!(!rig.paths.backup_request_file("docs").exists());
}

async fn a_request_is_dropped_by_a_round_that_skips_a_gated_backup() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    let state = BackupState {
        gate: Some(Gate {
            kind: GateKind::AuthError,
            since: Zoned::now().timestamp(),
            detail: "Sign in again.".to_owned(),
            fault: None,
            auto_checked: None,
        }),
        ..BackupState::default()
    };
    state.save(&rig.paths, "docs").await.unwrap();
    save_request(&rig.paths, "docs", BackupRequest::Check(CheckKind::Full))
        .await
        .unwrap();

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Skipped(SkipReason::Gated));
    assert!(!rig.paths.backup_request_file("docs").exists());
}

async fn a_request_waits_for_a_destination_that_is_not_there_yet() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    let away = rig.dir("repo-away");
    fs::rename(rig.dir("repo"), &away).unwrap();
    save_request(&rig.paths, "docs", BackupRequest::Check(CheckKind::Full))
        .await
        .unwrap();

    let first = rig.run("docs").await;
    fs::rename(&away, rig.dir("repo")).unwrap();
    let second = rig.run("docs").await;

    assert_eq!(first, BackupRunReport::Skipped(SkipReason::DestinationUnavailable));
    assert!(is_checked(&second).is_some(), "{second:?}");
}

async fn a_missing_destination_leaves_the_damaged_gate_alone_however_long_it_lasts() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    mark_damaged(&rig).await;
    let mut state = rig.state("docs").await;
    state.unavailable_since = Some(
        Zoned::now()
            .checked_sub(SignedDuration::from_hours(100))
            .unwrap()
            .timestamp(),
    );
    state.save(&rig.paths, "docs").await.unwrap();
    fs::rename(rig.dir("repo"), rig.dir("repo-away")).unwrap();

    let report = rig.check("docs", CheckKind::Full).await;

    assert_eq!(report, BackupRunReport::Skipped(SkipReason::DestinationUnavailable));
    let state = rig.state("docs").await;
    assert_eq!(state.gate.unwrap().fault, Some(Fault::RepoDamaged));
    assert!(state.destination_unavailable);
}

async fn a_check_that_runs_clears_offline_and_the_missing_destination() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    leave_unreachable_flags(&rig).await;

    let report = rig.check("docs", CheckKind::Full).await;

    assert!(is_checked(&report).is_some(), "{report:?}");
    assert_reachable(&rig).await;
}

async fn a_source_that_is_gone_clears_offline_and_the_missing_destination() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    leave_unreachable_flags(&rig).await;
    fs::remove_dir_all(&source).unwrap();

    let report = rig.run("docs").await;

    assert!(matches!(report, BackupRunReport::GateSet(_)), "{report:?}");
    assert_reachable(&rig).await;
}

async fn a_missing_key_clears_offline_and_the_missing_destination() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    leave_unreachable_flags(&rig).await;
    rig.keys.delete("docs").await.unwrap();

    assert_eq!(rig.run("docs").await, BackupRunReport::KeyUnavailable);
    assert_reachable(&rig).await;
}

async fn a_backup_that_cannot_start_restic_is_a_failed_round_not_an_error() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    rig.use_missing_restic();

    let report = rig.run("docs").await;

    assert!(
        matches!(report, BackupRunReport::Ran(BackupOutcome::Transient { .. })),
        "{report:?}"
    );
    let state = rig.state("docs").await;
    assert_eq!(state.consecutive_failures, 1);
    assert!(state.last_attempt.is_some());
    assert!(!rig.paths.backup_progress_file("docs").exists());
}

async fn a_cleanup_that_is_interrupted_is_neither_a_failure_nor_followed_by_a_check() {
    let Some(mut rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    rig.add(&b).await;
    assert_eq!(rig.run("docs").await, BackupRunReport::Ran(BackupOutcome::Ok));
    let mut state = rig.state("docs").await;
    let past = Zoned::now()
        .checked_sub(SignedDuration::from_hours(1))
        .unwrap()
        .timestamp();
    state.next_check = Some(past);
    state.save(&rig.paths, "docs").await.unwrap();
    fs::write(source.join("a.txt"), "changed").unwrap();
    let calls = rig.use_scripted_restic(&[("forget", Rule::Interrupted)]);

    let report = rig.run("docs").await;

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    assert_eq!(calls_with(&calls, "forget").len(), 1);
    assert_eq!(calls_with(&calls, "check"), Vec::<String>::new());
    let state = rig.state("docs").await;
    assert_eq!(state.prune_failures, 0);
    assert_eq!(state.prune_error, None);
    assert_eq!(state.next_check, Some(past));
    assert!(state.gate.is_none());
}

async fn the_key_saved_while_a_round_runs_stays_saved() {
    let Some(rig) = Rig::new().await else { return };
    let source = rig.folder("docs", &[("a.txt", b"hello")]);
    let b = backup("docs", &[&source], rig.dest("repo"));
    let key = rig.add(&b).await;
    let mut state = rig.state("docs").await;
    key_created_by_agent(&mut state);
    state.save(&rig.paths, "docs").await.unwrap();
    let keys = SavesTheKeyWhenRead {
        inner: MemoryKeyStore::default(),
        paths: rig.paths.clone(),
    };
    keys.inner.put("docs", &b.name, &key).await.unwrap();

    let report = rig.try_run_with("docs", &keys).await.unwrap();

    assert_eq!(report, BackupRunReport::Ran(BackupOutcome::Ok));
    let state = rig.state("docs").await;
    assert!(!state.key_unsaved);
    assert!(!state.notified.iter().any(|key| key == "key_unsaved"));
}
}
