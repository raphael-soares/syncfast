//! Rounds against the real rclone with two folders on this disk. Skipped,
//! with a warning, when rclone is not installed.

// Helpers here are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use jiff::{SignedDuration, Timestamp, Zoned};
use syncfast_core::behavior::ReviewLevel;
use syncfast_core::bisync::{Fault, Mode, Outcome, ReviewKind};
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
            "version = 2\n\n[defaults]\nreview = \"manual\"\n\n[[pair]]\nid = \"{PAIR}\"\nname = \"Docs\"\nlocal = \"{}\"\nremote = \"testremote:{}\"\nenabled = {enabled}\nfilters = [{}]\n",
            self.local.display(),
            self.remote.display(),
            filters.join(", "),
        );
        let file = self.paths.config_file();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    /// The same enabled sync with `defaults` as the `[defaults]` table and
    /// `own` as keys of the sync itself.
    fn write_behavior(&self, defaults: &str, own: &str) {
        let text = format!(
            "version = 2\n\n[defaults]\n{defaults}\n[[pair]]\nid = \"{PAIR}\"\nname = \"Docs\"\nlocal = \"{}\"\nremote = \"testremote:{}\"\n{own}",
            self.local.display(),
            self.remote.display(),
        );
        fs::write(self.paths.config_file(), text).unwrap();
    }

    /// The same enabled sync with `keys` in the `[notifications]` table.
    fn write_notifications(&self, keys: &str) {
        self.write_behavior(&format!("\n[notifications]\n{keys}"), "");
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
async fn new_local_file_in_a_pair_that_started_empty_on_both_sides_reaches_the_remote() {
    let Some(bed) = bed() else { return };
    assert_eq!(
        bed.run().await,
        RunReport::Skipped(SkipReason::Gated),
        "the first round waits for review"
    );
    bed.first_sync().await;
    write(&bed.local, "new.txt", "hello");

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: Mode::Resync,
            outcome: Outcome::Ok
        }
    );
    assert_eq!(read(&bed.remote, "new.txt"), "hello");
    let state = bed.state().await;
    assert_eq!(state.last_outcome, Some(LastOutcome::Ok));
    assert!(state.gate.is_none());
    assert_eq!(state.consecutive_failures, 0);
    assert_eq!(
        names(&bed.paths.runs_dir(PAIR)).len(),
        2,
        "the refused attempt leaves no log of its own: first sync plus this round"
    );
    write(&bed.local, "second.txt", "again");
    assert_eq!(
        bed.run().await,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::Ok
        },
        "later rounds are normal again"
    );
    assert_eq!(read(&bed.remote, "second.txt"), "again");
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
async fn an_emptied_local_folder_needs_attention_at_once_and_the_remote_keeps_every_file() {
    let Some(bed) = bed() else { return };
    let files: Vec<String> = (0..60).map(|n| format!("file-{n:02}.txt")).collect();
    for name in &files {
        write(&bed.local, name, name);
    }
    bed.first_sync().await;
    for name in &files {
        fs::remove_file(bed.local.join(name)).unwrap();
    }

    let report = bed.run().await;

    let RunReport::Ran {
        mode: Mode::Normal,
        outcome: Outcome::NeedsAttention { fault, detail },
    } = report
    else {
        panic!("{report:?}");
    };
    assert_eq!(fault, Fault::LocalEmpty);
    assert!(
        detail.starts_with("Empty current Path1 listing"),
        "{detail}"
    );
    assert_eq!(names(&bed.remote), files);
    assert!(names(&bed.local).is_empty());
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::NeedsAttention)
    );
    assert_eq!(bed.notices.titles(), ["Docs: needs attention"]);
    assert_eq!(
        bed.notices.bodies(),
        ["The folder is empty. If it is on a drive, mount it."]
    );
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
async fn a_folder_with_a_git_repo_is_left_out_of_the_first_sync_and_listed_in_the_review() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "school/proj/main.c", "int main;");
    fs::create_dir_all(bed.local.join("school/proj/.git")).unwrap();
    write(&bed.local, "school/notes.txt", "notes");

    let reviewed = bed.preview(ReviewKind::Resync).await.unwrap();

    assert_eq!(reviewed.excluded_repos, ["school/proj"]);
    bed.first_sync().await;
    assert_eq!(read(&bed.remote, "school/notes.txt"), "notes");
    assert!(!bed.remote.join("school/proj").exists());
}

#[tokio::test]
async fn a_git_init_in_a_synced_folder_holds_the_sync_for_review_and_deletes_nothing_on_the_remote()
{
    let Some(bed) = bed() else { return };
    write(&bed.local, "work/a.txt", "a");
    bed.first_sync().await;
    fs::create_dir_all(bed.local.join("work/.git")).unwrap();

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::GateSet(GateKind::NeedsReviewFiltersChanged)
    );
    assert_eq!(read(&bed.remote, "work/a.txt"), "a");
    bed.confirm(ReviewKind::Resync).await;
    let confirmed = bed.run().await;
    assert_eq!(
        confirmed,
        RunReport::Ran {
            mode: Mode::Resync,
            outcome: Outcome::Ok
        }
    );
    assert_eq!(read(&bed.remote, "work/a.txt"), "a");
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

async fn failing_bodies(keys: &str, rounds: usize) -> Option<Vec<String>> {
    let bed = bed()?;
    synced_base(&bed).await;
    bed.write_notifications(keys);
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"drive quota exceeded"}"#, 1);
    for _ in 0..rounds {
        bed.run_with(&failing).await;
    }
    Some(bed.notices.bodies())
}

#[tokio::test]
async fn failing_uses_failures_after_and_zero_turns_it_off() {
    let Some(lowered) = failing_bodies("failures_after = 2\n", 3).await else {
        return;
    };
    let off = failing_bodies("failures_after = 0\n", 4).await.unwrap();

    assert_eq!(lowered, ["Failed 2 times in a row. drive quota exceeded"]);
    assert!(off.is_empty());
}

#[tokio::test]
async fn a_gate_notice_is_dropped_when_needs_action_is_off_but_still_recorded_as_notified() {
    let Some(bed) = bed() else { return };
    bed.write_notifications("needs_action = false\n");
    bed.break_config();

    bed.run().await;

    assert!(bed.notices.titles().is_empty());
    assert_eq!(bed.state().await.notified, ["gate:config_error"]);
}

#[tokio::test]
async fn new_conflicts_respect_conflicts() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "doc.txt", "base");
    write(&bed.local, "untouched.txt", "same");
    bed.first_sync().await;
    bed.write_notifications("conflicts = false\n");
    write(&bed.local, "doc.txt", "local edit");
    write(&bed.remote, "doc.txt", "remote edit, newer");
    set_age(&bed.local, "doc.txt", 3600);
    set_age(&bed.remote, "doc.txt", 60);

    bed.run().await;

    assert_eq!(bed.state().await.conflicts_pending, 1);
    assert!(bed.notices.titles().is_empty());
}

#[tokio::test]
async fn a_successful_round_notifies_only_when_it_changed_files() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    bed.write_notifications("success = true\n");
    write(&bed.local, "new.txt", "hello");

    bed.run().await;
    bed.run().await;

    assert_eq!(bed.notices.titles(), ["Docs: synced"]);
    assert_eq!(bed.notices.bodies(), ["1 uploaded"]);
}

#[tokio::test]
async fn a_successful_round_stays_quiet_by_default() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    write(&bed.local, "new.txt", "hello");

    bed.run().await;

    assert!(bed.notices.titles().is_empty());
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
async fn a_round_that_ends_any_way_but_failing_for_now_ends_the_failing_series() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"boom"}"#, 1);
    bed.run_with(&failing).await;
    assert!(bed.state().await.failing_since.is_some());
    let rejected = fake_rclone(
        &bed,
        r#"{"level":"error","msg":"couldn't fetch token: invalid_grant"}"#,
        1,
    );

    bed.run_with(&rejected).await;

    assert_eq!(bed.state().await.failing_since, None);
}

#[tokio::test]
async fn a_fault_sets_the_attention_gate_once_and_the_next_good_round_clears_it() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    let full = fake_rclone(
        &bed,
        r#"{"level":"error","msg":"googleapi: Error 403: storageQuotaExceeded"}"#,
        1,
    );

    let first = bed.run_with(&full).await;
    let second = bed.run_with(&full).await;

    for report in [first, second] {
        assert_eq!(
            report,
            RunReport::Ran {
                mode: Mode::Normal,
                outcome: Outcome::NeedsAttention {
                    fault: Fault::Quota,
                    detail: "googleapi: Error 403: storageQuotaExceeded".to_owned()
                }
            }
        );
    }
    let state = bed.state().await;
    let gate = state.gate.unwrap();
    assert_eq!(gate.kind, GateKind::NeedsAttention);
    assert_eq!(gate.detail, "googleapi: Error 403: storageQuotaExceeded");
    assert_eq!(gate.fault, Some(Fault::Quota));
    assert_eq!(state.consecutive_failures, 2);
    assert_eq!(state.last_outcome, Some(LastOutcome::Error));
    assert!(state.failing_since.is_some());
    assert_eq!(bed.notices.titles(), ["Docs: needs attention"]);
    assert_eq!(bed.notices.bodies(), ["Google Drive is full."]);

    write(&bed.local, "after.txt", "after");
    let recovered = bed.run().await;

    assert_eq!(
        recovered,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::Ok
        }
    );
    let state = bed.state().await;
    assert!(state.gate.is_none());
    assert!(state.failing_since.is_none());
    assert!(state.notified.is_empty(), "{:?}", state.notified);
    assert_eq!(read(&bed.remote, "after.txt"), "after");
}

#[tokio::test]
async fn a_confirmed_first_sync_that_needs_attention_keeps_the_failure_and_resumes_once_fixed() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.confirm(ReviewKind::Resync).await;
    let full = fake_rclone(
        &bed,
        r#"{"level":"error","msg":"googleapi: Error 403: storageQuotaExceeded"}"#,
        1,
    );

    let first = bed.run_with(&full).await;
    let second = bed.run_with(&full).await;

    for report in [first, second] {
        assert!(
            matches!(
                report,
                RunReport::Ran {
                    mode: Mode::Resync,
                    outcome: Outcome::NeedsAttention {
                        fault: Fault::Quota,
                        ..
                    }
                }
            ),
            "{report:?}"
        );
    }
    let state = bed.state().await;
    assert_eq!(
        state.gate.map(|gate| gate.kind),
        Some(GateKind::NeedsAttention)
    );
    assert_eq!(state.applied_filters_hash, None);
    assert!(
        ConfirmMarker::load(&bed.paths, PAIR)
            .await
            .unwrap()
            .is_some()
    );

    let recovered = bed.run().await;

    assert_eq!(
        recovered,
        RunReport::Ran {
            mode: Mode::Resync,
            outcome: Outcome::Ok
        }
    );
    assert!(bed.state().await.gate.is_none());
    assert!(
        ConfirmMarker::load(&bed.paths, PAIR)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(read(&bed.remote, "base.txt"), "base");
}

#[tokio::test]
async fn transient_failures_for_over_six_hours_escalate_to_attention_once() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"boom"}"#, 1);
    bed.run_with(&failing).await;
    let mut state = bed.state().await;
    assert!(state.gate.is_none());
    state.failing_since = Some(Timestamp::now() - SignedDuration::from_hours(7));
    state.save(&bed.paths, PAIR).await.unwrap();

    let report = bed.run_with(&failing).await;
    bed.run_with(&failing).await;

    assert!(matches!(
        report,
        RunReport::Ran {
            outcome: Outcome::Transient,
            ..
        }
    ));
    let gate = bed.state().await.gate.unwrap();
    assert_eq!(gate.kind, GateKind::NeedsAttention);
    assert_eq!(gate.detail, "Syncing has failed for over 6 hours: boom");
    assert_eq!(
        bed.notices.titles(),
        ["Docs: needs attention", "Docs: sync failing"]
    );
}

#[tokio::test]
async fn failures_of_less_than_six_hours_do_not_escalate() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"boom"}"#, 1);
    bed.run_with(&failing).await;
    let mut state = bed.state().await;
    state.failing_since = Some(Timestamp::now() - SignedDuration::from_hours(5));
    state.save(&bed.paths, PAIR).await.unwrap();

    bed.run_with(&failing).await;

    assert!(bed.state().await.gate.is_none());
    assert!(bed.notices.titles().is_empty());
}

#[tokio::test]
async fn a_corrupt_state_file_is_set_aside_and_the_pair_goes_back_to_its_first_review() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    write(&bed.local, "new.txt", "new");
    let state_file = bed.paths.state_file(PAIR);
    fs::write(&state_file, "{ this is not json").unwrap();

    let report = bed.run().await;

    assert_eq!(report, RunReport::Skipped(SkipReason::Gated));
    assert_eq!(
        bed.state().await.gate.map(|gate| gate.kind),
        Some(GateKind::NeedsReviewFirstRun)
    );
    let aside: Vec<String> = names(state_file.parent().unwrap())
        .into_iter()
        .filter(|name| name.starts_with("state.json.corrupt-"))
        .collect();
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert_eq!(
        read(state_file.parent().unwrap(), &aside[0]),
        "{ this is not json"
    );
    assert!(!bed.remote.join("new.txt").exists());
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
async fn a_failed_round_still_prunes_expired_versions_and_old_run_logs() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    let versions = bed.paths.versions_dir(PAIR);
    write(&versions, "20200101-000000/old.txt", "expired");
    for n in 0..55 {
        write(
            &bed.paths.runs_dir(PAIR),
            &format!("20200101-0000{n:02}.jsonl"),
            "",
        );
    }
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"boom"}"#, 1);

    bed.run_with(&failing).await;

    assert!(!versions.join("20200101-000000").exists());
    assert_eq!(names(&bed.paths.runs_dir(PAIR)).len(), 50);
}

#[tokio::test]
async fn a_versions_folder_that_cannot_be_pruned_does_not_fail_the_round() {
    let Some(bed) = bed() else { return };
    synced_base(&bed).await;
    let versions = bed.paths.versions_dir(PAIR);
    fs::remove_dir_all(&versions).ok();
    write(versions.parent().unwrap(), PAIR, "not a folder");
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"boom"}"#, 1);

    let report = bed.run_with(&failing).await;

    assert!(matches!(report, RunReport::Ran { .. }), "{report:?}");
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

fn ran_ok(mode: Mode) -> RunReport {
    RunReport::Ran {
        mode,
        outcome: Outcome::Ok,
    }
}

#[tokio::test]
async fn a_syncs_own_max_delete_lets_a_mass_deletion_through() {
    let Some(bed) = bed() else { return };
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        write(&bed.local, name, name);
    }
    bed.first_sync().await;
    bed.write_behavior("", "max_delete = 80\n");
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::remove_file(bed.local.join(name)).unwrap();
    }

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    assert_eq!(names(&bed.remote), ["d.txt"]);
}

#[tokio::test]
async fn a_defaults_max_delete_applies_to_a_sync_that_sets_none() {
    let Some(bed) = bed() else { return };
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        write(&bed.local, name, name);
    }
    bed.first_sync().await;
    bed.write_behavior("max_delete = 10\n", "");
    fs::remove_file(bed.local.join("a.txt")).unwrap();

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::TooManyDeletes
        }
    );
    assert_eq!(names(&bed.remote), ["a.txt", "b.txt", "c.txt", "d.txt"]);
}

#[tokio::test]
async fn a_sync_with_versions_off_keeps_no_old_version() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "a.txt", "version one");
    write(&bed.local, "untouched.txt", "same");
    bed.first_sync().await;
    bed.write_behavior("", "versions = false\n");
    write(&bed.remote, "a.txt", "version two, longer");
    set_age(&bed.remote, "a.txt", 0);

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    assert_eq!(read(&bed.local, "a.txt"), "version two, longer");
    assert!(bed.versions_holding("a.txt").is_empty());
}

#[tokio::test]
async fn a_defaults_conflict_rule_applies_unless_the_sync_sets_its_own() {
    for (own, local_wins) in [("", true), ("conflict_resolve = \"newer\"\n", false)] {
        let Some(bed) = bed() else { return };
        write(&bed.local, "doc.txt", "base");
        write(&bed.local, "untouched.txt", "same");
        bed.first_sync().await;
        bed.write_behavior("conflict_resolve = \"local\"\n", own);
        write(&bed.local, "doc.txt", "local edit");
        write(&bed.remote, "doc.txt", "remote edit, newer");
        set_age(&bed.local, "doc.txt", 3600);
        set_age(&bed.remote, "doc.txt", 60);

        let report = bed.run().await;

        assert_eq!(report, ran_ok(Mode::Normal));
        let winner = if local_wins {
            "local edit"
        } else {
            "remote edit, newer"
        };
        assert_eq!(read(&bed.local, "doc.txt"), winner);
        assert_eq!(read(&bed.remote, "doc.txt"), winner);
    }
}

#[tokio::test]
async fn a_sync_that_keeps_versions_longer_does_not_lose_old_ones() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    bed.write_behavior("", "versions_keep_days = 3650\n");
    let versions = bed.paths.versions_dir(PAIR);
    write(&versions, "20200101-000000/old.txt", "kept");
    write(&bed.local, "new.txt", "new");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    assert!(versions.join("20200101-000000/old.txt").exists());
}

async fn both_sides_edit_doc_with_the_remote_newer(bed: &Bed) {
    write(&bed.local, "doc.txt", "base");
    write(&bed.local, "untouched.txt", "same");
    bed.first_sync().await;
    write(&bed.local, "doc.txt", "local edit");
    write(&bed.remote, "doc.txt", "remote edit, newer");
    set_age(&bed.local, "doc.txt", 3600);
    set_age(&bed.remote, "doc.txt", 60);
}

#[tokio::test]
async fn a_numbered_loser_is_kept_beside_the_winner_on_both_sides() {
    let Some(bed) = bed() else { return };
    both_sides_edit_doc_with_the_remote_newer(&bed).await;
    bed.write_behavior("", "conflict_loser = \"num\"\n");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    for side in [&bed.local, &bed.remote] {
        assert_eq!(read(side, "doc.txt"), "remote edit, newer");
        assert_eq!(read(side, "doc.txt.conflict1"), "local edit");
    }
    assert_eq!(bed.state().await.conflicts_pending, 1);
}

#[tokio::test]
async fn a_loser_named_by_path_is_kept_as_path1_and_counted_as_pending() {
    let Some(bed) = bed() else { return };
    both_sides_edit_doc_with_the_remote_newer(&bed).await;
    bed.write_behavior("", "conflict_loser = \"pathname\"\n");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    for side in [&bed.local, &bed.remote] {
        assert_eq!(names(side), ["doc.txt", "doc.txt.path1", "untouched.txt"]);
        assert_eq!(read(side, "doc.txt"), "remote edit, newer");
        assert_eq!(read(side, "doc.txt.path1"), "local edit");
    }
    assert_eq!(bed.state().await.conflicts_pending, 1);
}

#[tokio::test]
async fn without_a_winner_both_copies_are_renamed_by_path_and_the_original_is_gone() {
    let Some(bed) = bed() else { return };
    both_sides_edit_doc_with_the_remote_newer(&bed).await;
    bed.write_behavior(
        "",
        "conflict_resolve = \"none\"\nconflict_loser = \"pathname\"\n",
    );

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    for side in [&bed.local, &bed.remote] {
        assert_eq!(
            names(side),
            ["doc.txt.path1", "doc.txt.path2", "untouched.txt"]
        );
        assert_eq!(read(side, "doc.txt.path1"), "local edit");
        assert_eq!(read(side, "doc.txt.path2"), "remote edit, newer");
    }
    assert_eq!(bed.state().await.conflicts_pending, 2);
}

#[tokio::test]
async fn a_deleted_loser_leaves_one_file_and_the_local_copy_goes_to_versions() {
    let Some(bed) = bed() else { return };
    both_sides_edit_doc_with_the_remote_newer(&bed).await;
    bed.write_behavior("", "conflict_loser = \"delete\"\n");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    for side in [&bed.local, &bed.remote] {
        assert_eq!(names(side), ["doc.txt", "untouched.txt"]);
        assert_eq!(read(side, "doc.txt"), "remote edit, newer");
    }
    let kept = bed.versions_holding("doc.txt");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].1, "local edit");
    let state = bed.state().await;
    assert_eq!(state.conflicts_pending, 0);
    assert_eq!(state.last_run.unwrap().summary.conflicts_new, Some(1));
    assert!(
        bed.notices
            .titles()
            .contains(&"Docs: 1 conflicts resolved".to_owned()),
        "{:?}",
        bed.notices.titles()
    );
}

#[tokio::test]
async fn a_deleted_loser_with_versions_off_is_not_kept_anywhere() {
    let Some(bed) = bed() else { return };
    both_sides_edit_doc_with_the_remote_newer(&bed).await;
    bed.write_behavior("", "conflict_loser = \"delete\"\nversions = false\n");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    assert_eq!(names(&bed.local), ["doc.txt", "untouched.txt"]);
    assert_eq!(read(&bed.local, "doc.txt"), "remote edit, newer");
    assert!(bed.versions_holding("doc.txt").is_empty());
}

#[tokio::test]
async fn a_pathname_sync_does_not_count_a_leftover_numbered_copy_as_missing() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    bed.write_behavior("", "conflict_loser = \"pathname\"\n");
    write(
        &bed.local,
        "old.txt.conflict3",
        "left by an earlier setting",
    );
    write(&bed.local, "new.txt", "new");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    assert_eq!(bed.state().await.conflicts_pending, 1);
}

#[tokio::test]
async fn a_num_sync_does_not_count_path_files_as_conflicts() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    write(&bed.local, "report.txt.path1", "a name the user chose");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    assert_eq!(bed.state().await.conflicts_pending, 0);
}

#[tokio::test]
async fn prune_runs_with_versions_off() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    bed.write_behavior("", "versions = false\nversions_keep_days = 7\n");
    let versions = bed.paths.versions_dir(PAIR);
    write(&versions, "20200101-000000/old.txt", "expired");
    write(&bed.local, "new.txt", "new");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    assert!(!versions.join("20200101-000000").exists());
}

#[tokio::test]
async fn the_sync_retention_decides_which_version_folders_go() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    bed.write_behavior("", "versions_keep_days = 1\n");
    let versions = bed.paths.versions_dir(PAIR);
    let two_days_ago = Zoned::now()
        .checked_sub(SignedDuration::from_hours(48))
        .unwrap();
    let folder = two_days_ago.strftime("%Y%m%d-%H%M%S").to_string();
    write(&versions, &format!("{folder}/old.txt"), "two days old");
    write(&bed.local, "new.txt", "new");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Normal));
    assert!(!versions.join(folder).exists());
}

fn review_at(bed: &Bed, level: &str) {
    bed.write_behavior(&format!("review = \"{level}\"\n"), "");
}

async fn four_files_synced(bed: &Bed) {
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        write(&bed.local, name, name);
    }
    bed.first_sync().await;
}

fn delete_three_locally(bed: &Bed) {
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::remove_file(bed.local.join(name)).unwrap();
    }
}

fn restore_three_locally(bed: &Bed) {
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(&bed.local, name, name);
    }
}

#[tokio::test]
async fn a_first_run_waits_for_review_at_deletions_even_when_nothing_is_deleted() {
    for level in ["manual", "deletions"] {
        let Some(bed) = bed() else { return };
        review_at(&bed, level);
        write(&bed.local, "a.txt", "a");

        let report = bed.run().await;

        assert_eq!(report, RunReport::Skipped(SkipReason::Gated), "{level}");
        let state = bed.state().await;
        assert_eq!(
            state.gate.map(|gate| gate.kind),
            Some(GateKind::NeedsReviewFirstRun),
            "{level}"
        );
        assert!(names(&bed.remote).is_empty(), "{level}");
    }
}

#[tokio::test]
async fn a_first_run_confirms_itself_at_automatic_and_says_so_in_the_last_round() {
    let Some(bed) = bed() else { return };
    review_at(&bed, "automatic");
    write(&bed.local, "a.txt", "a");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Resync));
    assert_eq!(read(&bed.remote, "a.txt"), "a");
    let state = bed.state().await;
    assert!(state.gate.is_none());
    let reviewed = state.last_run.and_then(|last| last.auto_reviewed).unwrap();
    assert_eq!(reviewed.gate, GateKind::NeedsReviewFirstRun);
    assert_eq!(reviewed.level, ReviewLevel::Automatic);
    assert_eq!(reviewed.counts.upload, 1);
    assert!(
        bed.notices.titles().is_empty(),
        "{:?}",
        bed.notices.titles()
    );
}

#[tokio::test]
async fn a_filters_change_confirms_itself_at_deletions_when_the_preview_deletes_nothing() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    bed.write_behavior("review = \"deletions\"\n", "filters = [\"- *.skip\"]\n");
    write(&bed.local, "new.txt", "new");
    write(&bed.local, "x.skip", "skipped");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Resync));
    assert_eq!(names(&bed.remote), ["base.txt", "new.txt"]);
    let state = bed.state().await;
    assert!(state.gate.is_none());
    let reviewed = state.last_run.and_then(|last| last.auto_reviewed).unwrap();
    assert_eq!(reviewed.gate, GateKind::NeedsReviewFiltersChanged);
    assert!(!reviewed.counts.deletes());
    assert!(
        bed.notices.titles().is_empty(),
        "{:?}",
        bed.notices.titles()
    );
}

#[tokio::test]
async fn a_filters_change_waits_for_review_at_manual() {
    let Some(bed) = bed() else { return };
    write(&bed.local, "base.txt", "base");
    bed.first_sync().await;
    bed.write_config(true, &["- *.skip"]);

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::GateSet(GateKind::NeedsReviewFiltersChanged)
    );
    assert_eq!(bed.run().await, RunReport::Skipped(SkipReason::Gated));
    assert!(bed.state().await.gate.unwrap().auto_checked.is_none());
}

#[tokio::test]
async fn mass_deletions_are_held_at_deletions_checked_once_and_checked_again_at_a_new_level() {
    let Some(bed) = bed() else { return };
    four_files_synced(&bed).await;
    review_at(&bed, "deletions");
    delete_three_locally(&bed);
    assert_eq!(
        bed.run().await,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::TooManyDeletes
        }
    );

    let held = bed.run().await;

    assert_eq!(held, RunReport::Skipped(SkipReason::Gated));
    assert_eq!(names(&bed.remote), ["a.txt", "b.txt", "c.txt", "d.txt"]);
    let gate = bed.state().await.gate.unwrap();
    assert_eq!(gate.kind, GateKind::TooManyDeletes);
    assert_eq!(gate.auto_checked, Some(ReviewLevel::Deletions));
    restore_three_locally(&bed);
    assert_eq!(
        bed.run().await,
        RunReport::Skipped(SkipReason::Gated),
        "the same level does not dry run the gate again"
    );

    review_at(&bed, "automatic");
    assert_eq!(bed.run().await, ran_ok(Mode::Force));
    assert!(bed.state().await.gate.is_none());
}

#[tokio::test]
async fn a_mass_deletion_gate_confirms_itself_at_deletions_once_the_preview_deletes_nothing() {
    let Some(bed) = bed() else { return };
    four_files_synced(&bed).await;
    delete_three_locally(&bed);
    assert_eq!(
        bed.run().await,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::TooManyDeletes
        }
    );
    assert_eq!(bed.run().await, RunReport::Skipped(SkipReason::Gated));
    restore_three_locally(&bed);
    review_at(&bed, "deletions");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Force));
    assert_eq!(names(&bed.remote), ["a.txt", "b.txt", "c.txt", "d.txt"]);
    let reviewed = bed
        .state()
        .await
        .last_run
        .and_then(|last| last.auto_reviewed)
        .unwrap();
    assert_eq!(reviewed.gate, GateKind::TooManyDeletes);
    assert_eq!(reviewed.level, ReviewLevel::Deletions);
    assert!(!reviewed.counts.deletes());
}

#[tokio::test]
async fn automatic_confirms_a_mass_deletion_and_the_remote_loses_the_files() {
    let Some(bed) = bed() else { return };
    four_files_synced(&bed).await;
    review_at(&bed, "automatic");
    delete_three_locally(&bed);
    assert_eq!(
        bed.run().await,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::TooManyDeletes
        }
    );

    let report = bed.run().await;

    assert_eq!(report, ran_ok(Mode::Force));
    assert_eq!(names(&bed.remote), ["d.txt"]);
    let state = bed.state().await;
    assert!(state.gate.is_none());
    let reviewed = state.last_run.and_then(|last| last.auto_reviewed).unwrap();
    assert_eq!(reviewed.gate, GateKind::TooManyDeletes);
    assert_eq!(reviewed.counts.delete_remote, 3);
}

async fn automatic_review_of_a_mass_deletion(bed: &Bed, notifications: &str) {
    four_files_synced(bed).await;
    bed.write_behavior(
        &format!("review = \"automatic\"\n\n[notifications]\n{notifications}"),
        "",
    );
    delete_three_locally(bed);
    assert_eq!(
        bed.run().await,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::TooManyDeletes
        }
    );
    assert_eq!(bed.run().await, ran_ok(Mode::Force));
}

#[tokio::test]
async fn an_automatic_review_that_deleted_files_notifies_when_success_is_on() {
    let Some(bed) = bed() else { return };

    automatic_review_of_a_mass_deletion(&bed, "success = true\n").await;

    assert!(
        bed.notices
            .titles()
            .contains(&"Docs: reviewed automatically".to_owned()),
        "{:?}",
        bed.notices.titles()
    );
    assert!(
        bed.notices
            .bodies()
            .contains(&"Deleted 3 files after an automatic review.".to_owned()),
        "{:?}",
        bed.notices.bodies()
    );
}

#[tokio::test]
async fn an_automatic_review_that_deleted_files_is_silent_when_success_is_off() {
    let Some(bed) = bed() else { return };

    automatic_review_of_a_mass_deletion(&bed, "success = false\n").await;

    assert!(
        !bed.notices
            .titles()
            .contains(&"Docs: reviewed automatically".to_owned()),
        "{:?}",
        bed.notices.titles()
    );
}

#[tokio::test]
async fn an_automatic_review_that_deleted_nothing_is_silent_even_with_success_on() {
    let Some(bed) = bed() else { return };
    bed.write_behavior(
        "review = \"automatic\"\n\n[notifications]\nsuccess = true\n",
        "",
    );
    write(&bed.local, "a.txt", "a");

    assert_eq!(bed.run().await, ran_ok(Mode::Resync));

    assert!(
        !bed.notices
            .titles()
            .contains(&"Docs: reviewed automatically".to_owned()),
        "{:?}",
        bed.notices.titles()
    );
}

#[tokio::test]
async fn manual_never_confirms_a_gate_by_itself() {
    let Some(bed) = bed() else { return };
    four_files_synced(&bed).await;
    delete_three_locally(&bed);
    assert_eq!(
        bed.run().await,
        RunReport::Ran {
            mode: Mode::Normal,
            outcome: Outcome::TooManyDeletes
        }
    );
    restore_three_locally(&bed);

    let report = bed.run().await;

    assert_eq!(report, RunReport::Skipped(SkipReason::Gated));
    assert!(bed.state().await.gate.unwrap().auto_checked.is_none());
}

#[tokio::test]
async fn an_emptied_local_folder_needs_attention_at_every_review_level() {
    for level in ["manual", "deletions", "automatic"] {
        let Some(bed) = bed() else { return };
        let files: Vec<String> = (0..60).map(|n| format!("file-{n:02}.txt")).collect();
        for name in &files {
            write(&bed.local, name, name);
        }
        bed.first_sync().await;
        review_at(&bed, level);
        for name in &files {
            fs::remove_file(bed.local.join(name)).unwrap();
        }

        let report = bed.run().await;

        assert!(
            matches!(
                report,
                RunReport::Ran {
                    mode: Mode::Normal,
                    outcome: Outcome::NeedsAttention {
                        fault: Fault::LocalEmpty,
                        ..
                    }
                }
            ),
            "{level}: {report:?}"
        );
        assert_eq!(names(&bed.remote), files, "{level}");
        assert_eq!(
            bed.state().await.gate.map(|gate| gate.kind),
            Some(GateKind::NeedsAttention),
            "{level}"
        );
    }
}

#[tokio::test]
async fn a_failed_dry_run_leaves_the_gate_unchecked_and_counts_as_a_failure() {
    let Some(bed) = bed() else { return };
    review_at(&bed, "automatic");
    write(&bed.local, "a.txt", "a");
    let failing = fake_rclone(&bed, r#"{"level":"error","msg":"drive quota exceeded"}"#, 1);

    let report = bed.run_with(&failing).await;

    assert_eq!(report, RunReport::Skipped(SkipReason::Gated));
    let state = bed.state().await;
    assert_eq!(state.consecutive_failures, 1);
    let gate = state.gate.unwrap();
    assert_eq!(gate.kind, GateKind::NeedsReviewFirstRun);
    assert!(gate.auto_checked.is_none());
    assert!(names(&bed.remote).is_empty());

    assert_eq!(bed.run().await, ran_ok(Mode::Resync));
}
