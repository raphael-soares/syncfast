//! Rounds against the real rclone with two folders on this disk. Skipped,
//! with a warning, when rclone is not installed.

// Helpers here are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use jiff::Zoned;
use syncfast_core::bisync::{Mode, Outcome, ReviewKind};
use syncfast_core::bisync_log::ChangeKind;
use syncfast_core::network::{BoxFuture, Network};
use syncfast_core::notify::{Notice, Notifier};
use syncfast_core::paths::Paths;
use syncfast_core::preview::{PreviewError, preview};
use syncfast_core::rclone::Rclone;
use syncfast_core::run::{RunEnv, RunReport, STORED_CHANGES, run};
use syncfast_core::run_decision::SkipReason;
use syncfast_core::state::{ConfirmMarker, GateKind, LastOutcome, State, try_lock_pair};

const PAIR: &str = "docs";

struct FakeNetwork(AtomicBool);

impl Network for FakeNetwork {
    fn is_online(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { self.0.load(Ordering::SeqCst) })
    }
}

/// Keeps every notice with what `state.json` held when it was sent.
struct Recorder {
    notices: Mutex<Vec<Notice>>,
    state_file: PathBuf,
    on_disk: Mutex<Vec<Option<State>>>,
}

impl Notifier for Recorder {
    fn notify(&self, notice: &Notice) {
        self.notices.lock().unwrap().push(notice.clone());
        let state = fs::read_to_string(&self.state_file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        self.on_disk.lock().unwrap().push(state);
    }
}

impl Recorder {
    fn new(state_file: PathBuf) -> Self {
        Self {
            notices: Mutex::default(),
            state_file,
            on_disk: Mutex::default(),
        }
    }

    /// The state file as it was on disk at each notification.
    fn states_when_sent(&self) -> Vec<Option<State>> {
        self.on_disk.lock().unwrap().clone()
    }

    fn titles(&self) -> Vec<String> {
        self.notices
            .lock()
            .unwrap()
            .iter()
            .map(|notice| notice.title.clone())
            .collect()
    }

    fn bodies(&self) -> Vec<String> {
        self.notices
            .lock()
            .unwrap()
            .iter()
            .map(|notice| notice.body.clone())
            .collect()
    }
}

struct Bed {
    _dir: tempfile::TempDir,
    paths: Paths,
    rclone: Rclone,
    local: PathBuf,
    remote: PathBuf,
    online: FakeNetwork,
    notices: Recorder,
}

#[allow(clippy::print_stderr)]
fn bed() -> Option<Bed> {
    let Ok(found) = Rclone::from_env() else {
        eprintln!("skipping: rclone is not installed");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let rclone_conf = root.join("rclone.conf");
    fs::write(&rclone_conf, "[testremote]\ntype = local\n").unwrap();
    let paths = Paths::new(
        root.join("home"),
        root.join("config"),
        root.join("state"),
        root.join("data"),
        root.join("run"),
    );
    let (local, remote) = (root.join("local"), root.join("remote"));
    fs::create_dir_all(&local).unwrap();
    fs::create_dir_all(&remote).unwrap();
    let bed = Bed {
        rclone: Rclone::new(found.bin().to_owned()).with_config(rclone_conf),
        local,
        remote,
        online: FakeNetwork(AtomicBool::new(true)),
        notices: Recorder::new(paths.state_file(PAIR)),
        paths,
        _dir: dir,
    };
    bed.write_config(true, &[]);
    Some(bed)
}

impl Bed {
    fn write_config(&self, enabled: bool, filters: &[&str]) {
        let filters: Vec<String> = filters.iter().map(|f| format!("{f:?}")).collect();
        let text = format!(
            "version = 1\n\n[[pair]]\nid = \"{PAIR}\"\nname = \"Docs\"\nlocal = \"{}\"\nremote = \"testremote:{}\"\nenabled = {enabled}\nfilters = [{}]\n",
            self.local.display(),
            self.remote.display(),
            filters.join(", "),
        );
        let file = self.paths.config_file();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    /// Makes the pair invalid: its local folder becomes a relative path.
    fn break_config(&self) {
        let text = fs::read_to_string(self.paths.config_file())
            .unwrap()
            .replace(
                &format!("local = \"{}\"", self.local.display()),
                "local = \"relative/dir\"",
            );
        fs::write(self.paths.config_file(), text).unwrap();
    }

    async fn confirm(&self, kind: ReviewKind) {
        let reviewed = self.preview(kind).await.unwrap();
        ConfirmMarker {
            kind,
            filters_hash: reviewed.filters_hash,
        }
        .save(&self.paths, PAIR)
        .await
        .unwrap();
    }

    async fn run(&self) -> RunReport {
        self.run_with(&self.rclone).await
    }

    async fn run_with(&self, rclone: &Rclone) -> RunReport {
        let clock = Zoned::now;
        let env = RunEnv {
            paths: &self.paths,
            rclone,
            network: &self.online,
            notifier: &self.notices,
            clock: &clock,
            forward_signals: true,
        };
        run(&env, PAIR).await.unwrap()
    }

    async fn preview(
        &self,
        kind: ReviewKind,
    ) -> Result<syncfast_core::preview::Preview, PreviewError> {
        self.preview_with(&self.rclone, kind).await
    }

    async fn preview_with(
        &self,
        rclone: &Rclone,
        kind: ReviewKind,
    ) -> Result<syncfast_core::preview::Preview, PreviewError> {
        let clock = Zoned::now;
        let env = RunEnv {
            paths: &self.paths,
            rclone,
            network: &self.online,
            notifier: &self.notices,
            clock: &clock,
            forward_signals: true,
        };
        preview(&env, PAIR, kind).await
    }

    async fn state(&self) -> State {
        State::load(&self.paths, PAIR).await.unwrap()
    }

    /// Previews, confirms and runs the first sync.
    async fn first_sync(&self) {
        let reviewed = self.preview(ReviewKind::Resync).await.unwrap();
        ConfirmMarker {
            kind: ReviewKind::Resync,
            filters_hash: reviewed.filters_hash,
        }
        .save(&self.paths, PAIR)
        .await
        .unwrap();
        let report = self.run().await;
        assert_eq!(
            report,
            RunReport::Ran {
                mode: Mode::Resync,
                outcome: Outcome::Ok
            }
        );
    }

    fn versions_holding(&self, name: &str) -> Vec<(PathBuf, String)> {
        let mut found = Vec::new();
        let mut pending = vec![self.paths.versions_dir(PAIR)];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.file_name().is_some_and(|n| n == name) {
                    let text = fs::read_to_string(&path).unwrap();
                    found.push((path, text));
                }
            }
        }
        found
    }
}

fn write(dir: &Path, name: &str, text: &str) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

fn read(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).unwrap()
}

fn set_age(dir: &Path, name: &str, seconds_ago: u64) {
    let file = fs::File::options()
        .write(true)
        .open(dir.join(name))
        .unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(seconds_ago))
        .unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn first_round_waits_for_review_then_a_confirmed_resync_equalizes_both_sides() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "only-local.txt", "L");
    write(&bed.remote, "only-remote.txt", "R");
    write(&bed.local, "both.txt", "old local");
    write(&bed.remote, "both.txt", "newer remote");
    set_age(&bed.local, "both.txt", 3600);
    set_age(&bed.remote, "both.txt", 60);

    let first = bed.run().await;

    assert_eq!(first, RunReport::Skipped(SkipReason::Gated));
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::NeedsReviewFirstRun)
    );
    assert_eq!(names(&bed.local), ["both.txt", "only-local.txt"]);
    assert_eq!(names(&bed.remote), ["both.txt", "only-remote.txt"]);
    assert_eq!(bed.notices.titles(), ["Docs: review needed"]);
    assert_eq!(
        bed.notices.bodies(),
        ["Review the first sync before it starts."]
    );

    let reviewed = bed.preview(ReviewKind::Resync).await.unwrap();

    let kinds: Vec<(ChangeKind, &str)> = reviewed
        .summary
        .changes
        .iter()
        .map(|change| (change.kind, change.path.as_str()))
        .collect();
    assert!(
        kinds.contains(&(ChangeKind::Upload, "only-local.txt")),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&(ChangeKind::Download, "only-remote.txt")),
        "{kinds:?}"
    );
    assert_eq!(
        names(&bed.local),
        ["both.txt", "only-local.txt"],
        "preview changes nothing"
    );
    assert!(
        names(&bed.paths.pair_state_dir(PAIR))
            .iter()
            .all(|name| !name.starts_with("preview-")),
        "the preview log is deleted"
    );

    ConfirmMarker {
        kind: ReviewKind::Resync,
        filters_hash: reviewed.filters_hash,
    }
    .save(&bed.paths, PAIR)
    .await
    .unwrap();
    let confirmed = bed.run().await;

    assert_eq!(
        confirmed,
        RunReport::Ran {
            mode: Mode::Resync,
            outcome: Outcome::Ok
        }
    );
    let state = bed.state().await;
    assert!(state.gate.is_none());
    assert_eq!(state.last_outcome, Some(LastOutcome::Ok));
    assert!(state.last_success.is_some());
    for dir in [&bed.local, &bed.remote] {
        assert_eq!(
            names(dir),
            ["both.txt", "only-local.txt", "only-remote.txt"]
        );
        assert_eq!(read(dir, "both.txt"), "newer remote");
    }
    assert!(
        ConfirmMarker::load(&bed.paths, PAIR)
            .await
            .unwrap()
            .is_none(),
        "the marker is consumed"
    );
    assert_eq!(bed.notices.titles().len(), 1, "success never notifies");
}

#[tokio::test]
async fn preview_of_a_sync_that_is_running_is_busy() {
    let Some(bed) = bed() else { return };
    let _round = try_lock_pair(&bed.paths, PAIR).unwrap().unwrap();

    let result = bed.preview(ReviewKind::Resync).await;

    assert!(matches!(result, Err(PreviewError::Busy)), "{result:?}");
}

#[tokio::test]
async fn new_local_file_is_uploaded_and_counted() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    write(&bed.local, "new.txt", "hello");

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::Ok
        }
    );
    assert_eq!(read(&bed.remote, "new.txt"), "hello");
    let summary = bed.state().await.last_run.unwrap().summary;
    assert_eq!(summary.uploaded, Some(1));
    assert_eq!(summary.downloaded, Some(0));
}

#[tokio::test]
async fn same_file_changed_on_both_sides_keeps_the_newer_and_leaves_a_conflict_copy() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "doc.txt", "base");
    write(&bed.local, "untouched.txt", "same");
    bed.first_sync().await;
    write(&bed.local, "doc.txt", "local edit");
    write(&bed.remote, "doc.txt", "remote edit, newer");
    set_age(&bed.local, "doc.txt", 3600);
    set_age(&bed.remote, "doc.txt", 60);

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::Ok
        }
    );
    assert_eq!(read(&bed.local, "doc.txt"), "remote edit, newer");
    assert_eq!(read(&bed.local, "doc.txt.conflict1"), "local edit");
    let state = bed.state().await;
    assert_eq!(state.last_run.unwrap().summary.conflicts_new, Some(1));
    assert_eq!(state.conflicts_pending, 1);
    assert!(
        bed.notices
            .titles()
            .contains(&"Docs: 1 new conflicts".to_owned()),
        "{:?}",
        bed.notices.titles()
    );
}

#[tokio::test]
async fn file_replaced_on_the_remote_keeps_the_old_local_version_in_versions() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "a.txt", "version one");
    write(&bed.local, "untouched.txt", "same");
    bed.first_sync().await;
    write(&bed.remote, "a.txt", "version two, longer");
    set_age(&bed.remote, "a.txt", 0);

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::Ok
        }
    );
    assert_eq!(read(&bed.local, "a.txt"), "version two, longer");
    let kept = bed.versions_holding("a.txt");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].1, "version one");
    let stamp = kept[0]
        .0
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    assert_eq!(stamp.len(), 15, "dated folder: {stamp}");
}

#[tokio::test]
async fn deleting_most_files_locally_stops_at_the_gate_until_a_forced_round_is_confirmed() {
    let Some(bed) = bed() else { return };
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        write(&bed.local, name, name);
    }
    bed.first_sync().await;
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::remove_file(bed.local.join(name)).unwrap();
    }

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::TooManyDeletes
        }
    );
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::TooManyDeletes)
    );
    assert_eq!(names(&bed.remote), ["a.txt", "b.txt", "c.txt", "d.txt"]);
    assert!(
        bed.notices
            .titles()
            .contains(&"Docs: many deletions".to_owned()),
        "{:?}",
        bed.notices.titles()
    );

    let reviewed = bed.preview(ReviewKind::Force).await.unwrap();
    let mut deleted: Vec<&str> = reviewed
        .summary
        .changes
        .iter()
        .filter(|change| change.kind == ChangeKind::DeleteRemote)
        .map(|change| change.path.as_str())
        .collect();
    deleted.sort_unstable();
    assert_eq!(deleted, ["a.txt", "b.txt", "c.txt"]);
    ConfirmMarker {
        kind: ReviewKind::Force,
        filters_hash: reviewed.filters_hash,
    }
    .save(&bed.paths, PAIR)
    .await
    .unwrap();
    let confirmed = bed.run().await;

    assert_eq!(
        confirmed,
        RunReport::Ran {
            mode: Mode::Force,
            outcome: Outcome::Ok
        }
    );
    assert_eq!(names(&bed.remote), ["d.txt"]);
    assert!(bed.state().await.gate.is_none());
}

#[tokio::test]
async fn editing_the_only_file_stops_at_the_gate_until_a_forced_round_is_confirmed() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "only.txt", "base");
    bed.first_sync().await;
    write(&bed.local, "only.txt", "edited locally, longer");

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::AllFilesChanged
        }
    );
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::AllFilesChanged)
    );
    assert_eq!(read(&bed.remote, "only.txt"), "base");
    assert_eq!(
        bed.notices.bodies(),
        ["Every file changed at once. Review before continuing."]
    );

    let reviewed = bed.preview(ReviewKind::Force).await.unwrap();
    ConfirmMarker {
        kind: ReviewKind::Force,
        filters_hash: reviewed.filters_hash,
    }
    .save(&bed.paths, PAIR)
    .await
    .unwrap();
    let confirmed = bed.run().await;

    assert_eq!(
        confirmed,
        RunReport::Ran {
            mode: Mode::Force,
            outcome: Outcome::Ok
        }
    );
    assert_eq!(read(&bed.remote, "only.txt"), "edited locally, longer");
    assert!(bed.state().await.gate.is_none());
}

#[tokio::test]
async fn changing_the_filters_sets_the_gate_and_syncs_nothing() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    bed.write_config(true, &["- *.skip"]);
    write(&bed.local, "new.txt", "new");

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::GateSet(GateKind::NeedsReviewFiltersChanged)
    );
    assert_eq!(names(&bed.remote), ["base.txt"]);
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::NeedsReviewFiltersChanged)
    );
    assert!(
        bed.notices
            .bodies()
            .contains(&"Filters changed. Review what will sync.".to_owned())
    );
    let again = bed.run().await;
    assert_eq!(again, RunReport::Skipped(SkipReason::Gated));
    assert_eq!(
        bed.notices.titles().len(),
        1,
        "the filters notice is not repeated"
    );
}

#[tokio::test]
async fn missing_local_folder_sets_the_gate_and_the_next_round_resumes_when_it_is_back() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    let moved = bed.local.with_file_name("local-away");
    fs::rename(&bed.local, &moved).unwrap();

    let missing = bed.run().await;

    assert_eq!(missing, RunReport::GateSet(GateKind::LocalMissing));
    assert_eq!(
        names(&bed.remote),
        ["base.txt"],
        "nothing is deleted remotely"
    );
    assert!(
        bed.notices
            .titles()
            .contains(&"Docs: folder not found".to_owned())
    );

    fs::rename(&moved, &bed.local).unwrap();
    let back = bed.run().await;

    assert_eq!(
        back,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::Ok
        }
    );
    assert!(bed.state().await.gate.is_none());

    fs::rename(&bed.local, &moved).unwrap();
    let again = bed.run().await;

    assert_eq!(again, RunReport::GateSet(GateKind::LocalMissing));
    let folder_notices = bed
        .notices
        .titles()
        .iter()
        .filter(|title| *title == "Docs: folder not found")
        .count();
    assert_eq!(folder_notices, 2, "a second disappearance notifies again");
}

#[tokio::test]
async fn pending_first_review_survives_the_local_folder_going_away_and_coming_back() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "a.txt", "a");
    assert_eq!(bed.run().await, RunReport::Skipped(SkipReason::Gated));
    let moved = bed.local.with_file_name("local-away");
    fs::rename(&bed.local, &moved).unwrap();

    let away = bed.run().await;

    assert_eq!(away, RunReport::Skipped(SkipReason::Gated));
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::NeedsReviewFirstRun)
    );
    fs::rename(&moved, &bed.local).unwrap();
    let back = bed.run().await;

    assert_eq!(back, RunReport::Skipped(SkipReason::Gated));
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::NeedsReviewFirstRun)
    );
    assert!(
        bed.notices
            .titles()
            .iter()
            .all(|title| title != "Docs: folder not found")
    );
    bed.confirm(ReviewKind::Resync).await;
    assert_eq!(
        bed.run().await,
        RunReport::Ran {
            mode: Mode::Resync,
            outcome: Outcome::Ok
        }
    );
    assert_eq!(read(&bed.remote, "a.txt"), "a");
}

#[tokio::test]
async fn review_gate_survives_pausing_and_resuming() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "a.txt", "a");
    assert_eq!(bed.run().await, RunReport::Skipped(SkipReason::Gated));

    bed.write_config(false, &[]);
    let paused = bed.run().await;
    bed.write_config(true, &[]);
    let resumed = bed.run().await;

    assert_eq!(paused, RunReport::Skipped(SkipReason::Paused));
    assert_eq!(resumed, RunReport::Skipped(SkipReason::Gated));
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::NeedsReviewFirstRun)
    );
    assert!(bed.remote.read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn offline_round_is_skipped_quietly_and_recorded() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    bed.online.0.store(false, Ordering::SeqCst);
    write(&bed.local, "new.txt", "new");

    let report = bed.run().await;

    assert_eq!(report, RunReport::Skipped(SkipReason::Offline));
    assert!(bed.state().await.offline);
    assert_eq!(names(&bed.remote), ["base.txt"]);
    assert!(
        bed.notices.titles().is_empty(),
        "being offline never notifies"
    );

    bed.online.0.store(true, Ordering::SeqCst);
    bed.run().await;

    assert!(!bed.state().await.offline);
    assert_eq!(names(&bed.remote), ["base.txt", "new.txt"]);
}

#[tokio::test]
async fn invalid_pair_gets_the_config_error_gate_and_one_notification() {
    let Some(bed) = bed() else { return };
    bed.break_config();

    let first = bed.run().await;
    let second = bed.run().await;

    assert_eq!(first, RunReport::ConfigInvalid);
    assert_eq!(second, RunReport::ConfigInvalid);
    let gate = bed.state().await.gate.unwrap();
    assert_eq!(gate.kind, GateKind::ConfigError);
    assert_eq!(gate.detail, "local folder must be an absolute path");
    assert_eq!(bed.notices.titles(), ["Docs: configuration error"]);
    assert_eq!(
        bed.notices.bodies(),
        ["local folder must be an absolute path"]
    );
}

#[tokio::test]
async fn a_gate_is_on_disk_by_the_time_its_notification_is_sent() {
    let Some(bed) = bed() else { return };
    bed.break_config();

    bed.run().await;

    let sent = bed.notices.states_when_sent();
    assert_eq!(sent.len(), 1);
    let on_disk = sent[0].as_ref().expect("state.json was readable");
    assert_eq!(on_disk.gate.as_ref().unwrap().kind, GateKind::ConfigError);
    assert_eq!(on_disk.notified, ["gate:config_error"]);
}

#[tokio::test]
async fn new_conflicts_are_on_disk_by_the_time_their_notification_is_sent() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "doc.txt", "base");
    write(&bed.local, "untouched.txt", "same");
    bed.first_sync().await;
    write(&bed.local, "doc.txt", "local edit");
    write(&bed.remote, "doc.txt", "remote edit, newer");
    set_age(&bed.local, "doc.txt", 3600);
    set_age(&bed.remote, "doc.txt", 60);

    bed.run().await;

    let sent = bed.notices.states_when_sent();
    assert_eq!(sent.len(), 1);
    let on_disk = sent[0].as_ref().expect("state.json was readable");
    assert_eq!(on_disk.conflicts_pending, 1);
    assert_eq!(
        on_disk.last_run.as_ref().unwrap().summary.conflicts_new,
        Some(1)
    );
}

/// An rclone stand-in: lists `testremote`, and every bisync appends `log`
/// to the `--log-file` and exits with `code`.
fn fake_rclone(bed: &Bed, log: &str, code: i32) -> Rclone {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let script = bed.paths.state_home.join(format!(
        "fake-rclone-{}.sh",
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = listremotes ]; then echo 'testremote:'; exit 0; fi\n\
         while [ $# -gt 0 ]; do if [ \"$1\" = --log-file ]; then file=\"$2\"; fi; shift; done\n\
         cat \"$0.log\" >> \"$file\"\nexit {code}\n"
    );
    fs::create_dir_all(&bed.paths.state_home).unwrap();
    fs::write(&script, body).unwrap();
    fs::write(script.with_extension("sh.log"), format!("{log}\n")).unwrap();
    let mut mode = fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    fs::set_permissions(&script, mode).unwrap();
    Rclone::new(script)
}

async fn synced_base(bed: &Bed) {
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
}

#[tokio::test]
async fn third_failure_in_a_row_notifies_once_with_the_last_error_line() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"drive quota exceeded"}"#, 1);

    let mut reports = Vec::new();
    for _ in 0..4 {
        reports.push(bed.run_with(&failing).await);
    }

    assert!(reports.iter().all(|report| matches!(
        report,
        RunReport::Ran {
            outcome: Outcome::Transient,
            ..
        }
    )));
    let state = bed.state().await;
    assert_eq!(state.consecutive_failures, 4);
    assert_eq!(state.last_outcome, Some(LastOutcome::Error));
    assert!(state.gate.is_none());
    assert_eq!(
        *bed.notices.notices.lock().unwrap(),
        [Notice {
            pair_id: PAIR.to_owned(),
            title: "Docs: sync failing".to_owned(),
            body: "Failed 3 times in a row. drive quota exceeded".to_owned(),
        }]
    );
}

#[tokio::test]
async fn auth_failure_sets_the_account_gate_and_names_the_account() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    let rejected = fake_rclone(
        &bed,
        r#"{"level":"error","msg":"couldn't fetch token: invalid_grant"}"#,
        1,
    );

    let report = bed.run_with(&rejected).await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::AuthError
        }
    );
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::AuthError)
    );
    assert_eq!(
        bed.notices.bodies().last().map(String::as_str),
        Some("Reconnect \"testremote\" in Settings > Accounts.")
    );
}

#[tokio::test]
async fn success_after_failures_clears_the_failure_notification_key() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"boom"}"#, 1);
    for _ in 0..3 {
        bed.run_with(&failing).await;
    }
    assert!(bed.state().await.notified.contains(&"failures".to_owned()));

    bed.run().await;

    let state = bed.state().await;
    assert_eq!(state.consecutive_failures, 0);
    assert_eq!(state.last_outcome, Some(LastOutcome::Ok));
    assert!(state.notified.is_empty(), "{:?}", state.notified);
}

#[tokio::test]
async fn config_error_of_a_pair_that_never_synced_leaves_the_first_review_pending() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "a.txt", "a");
    bed.break_config();
    assert_eq!(bed.run().await, RunReport::ConfigInvalid);

    bed.write_config(true, &[]);
    let fixed = bed.run().await;

    assert_eq!(fixed, RunReport::GateSet(GateKind::NeedsReviewFirstRun));
    assert!(bed.remote.read_dir().unwrap().next().is_none());
    assert_eq!(
        bed.notices.titles(),
        ["Docs: configuration error", "Docs: review needed"]
    );
    bed.confirm(ReviewKind::Resync).await;
    assert_eq!(
        bed.run().await,
        RunReport::Ran {
            mode: Mode::Resync,
            outcome: Outcome::Ok
        }
    );
}

#[tokio::test]
async fn config_error_of_a_synced_pair_clears_itself_once_the_config_is_valid() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    bed.break_config();
    assert_eq!(bed.run().await, RunReport::ConfigInvalid);
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::ConfigError)
    );
    write(&bed.local, "new.txt", "new");

    bed.write_config(true, &[]);
    let fixed = bed.run().await;

    assert_eq!(
        fixed,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::Ok
        }
    );
    assert!(bed.state().await.gate.is_none());
    assert_eq!(read(&bed.remote, "new.txt"), "new");
}

#[tokio::test]
async fn a_sync_that_is_running_makes_the_round_busy_and_changes_nothing() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    write(&bed.local, "new.txt", "new");
    let held = try_lock_pair(&bed.paths, PAIR).unwrap().unwrap();

    let report = bed.run().await;

    assert_eq!(report, RunReport::Busy);
    assert_eq!(names(&bed.remote), ["base.txt"]);
    drop(held);
    assert!(matches!(bed.run().await, RunReport::Ran { .. }));
}

#[tokio::test]
async fn marker_with_an_old_filters_hash_is_discarded_and_the_gate_holds() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "a.txt", "a");
    ConfirmMarker {
        kind: ReviewKind::Resync,
        filters_hash: "hash-of-other-filters".to_owned(),
    }
    .save(&bed.paths, PAIR)
    .await
    .unwrap();

    let report = bed.run().await;

    assert_eq!(report, RunReport::Skipped(SkipReason::Gated));
    assert!(!bed.paths.confirm_file(PAIR).exists());
    assert!(bed.remote.read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn marker_survives_a_transient_failure_and_is_deleted_once_a_gate_is_set() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "a.txt", "a");
    bed.confirm(ReviewKind::Resync).await;
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"boom"}"#, 1);

    let transient = bed.run_with(&failing).await;

    assert_eq!(
        transient,
        RunReport::Ran {
            mode: Mode::Resync,
            outcome: Outcome::Transient
        }
    );
    assert!(bed.paths.confirm_file(PAIR).exists());
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::NeedsReviewFirstRun)
    );
    let deleting = fake_rclone(
        &bed,
        r#"{"level":"error","msg":"too many deletes (>50%, 3 of 4)"}"#,
        1,
    );

    let gated = bed.run_with(&deleting).await;

    assert_eq!(
        gated,
        RunReport::Ran {
            mode: Mode::Resync,
            outcome: Outcome::TooManyDeletes
        }
    );
    assert!(!bed.paths.confirm_file(PAIR).exists());
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::TooManyDeletes)
    );
}

#[tokio::test]
async fn state_keeps_only_the_first_changes_of_a_big_round_but_all_the_counts() {
    let Some(bed) = bed() else { return };
    for n in 0..STORED_CHANGES + 20 {
        write(&bed.local, &format!("f{n:03}.txt"), "x");
    }

    bed.first_sync().await;

    let summary = bed.state().await.last_run.unwrap().summary;
    assert_eq!(summary.changes.len(), STORED_CHANGES);
    assert_eq!(
        summary.uploaded,
        Some(u32::try_from(STORED_CHANGES + 20).unwrap())
    );
}

#[tokio::test]
async fn a_successful_round_prunes_expired_versions_and_old_run_logs() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    let versions = bed.paths.versions_dir(PAIR);
    write(&versions, "20200101-000000/old.txt", "expired");
    write(&versions, "kept-by-hand.txt", "mine");
    for n in 0..55 {
        write(
            &bed.paths.runs_dir(PAIR),
            &format!("20200101-0000{n:02}.jsonl"),
            "",
        );
    }
    write(&bed.local, "new.txt", "new");

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::Ok
        }
    );
    assert!(!versions.join("20200101-000000").exists());
    assert!(versions.join("kept-by-hand.txt").exists());
    assert_eq!(names(&bed.paths.runs_dir(PAIR)).len(), 50);
    let newest = bed.state().await.last_run.unwrap().log_file;
    assert!(newest.exists(), "the newest log is kept");
}

#[tokio::test]
async fn failed_preview_reports_the_last_error_and_deletes_its_log() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "a.txt", "a");
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"drive is unreachable"}"#, 1);

    let result = bed.preview_with(&failing, ReviewKind::Resync).await;

    match result {
        Err(PreviewError::Failed { exit_code, message }) => {
            assert_eq!(exit_code, 1);
            assert_eq!(message, "drive is unreachable");
        }
        other => panic!("unexpected {other:?}"),
    }
    let leftovers: Vec<String> = names(&bed.paths.pair_state_dir(PAIR))
        .into_iter()
        .filter(|name| name.starts_with("preview-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
