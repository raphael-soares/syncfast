//! The one-way modes against the real rclone with folders on this disk.
//! Skipped, with a warning, when rclone is not installed.
//!
//! The remote is a `local` rclone remote, whose account root is the
//! directory rclone runs in. A wrapper script starts rclone in each bed's
//! root, so beds run in parallel and `mirror_up` keeps `.syncfast-versions`
//! there.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::{SignedDuration, Zoned};
use syncfast_core::behavior::{ReviewLevel, SyncMode};
use syncfast_core::bisync::{Fault, Outcome, ReviewKind, RoundMode};
use syncfast_core::network::{BoxFuture, Network};
use syncfast_core::notify::{Notice, Notifier};
use syncfast_core::paths::Paths;
use syncfast_core::preview::preview;
use syncfast_core::rclone::Rclone;
use syncfast_core::run::{RunEnv, RunReport, run};
use syncfast_core::run_decision::SkipReason;
use syncfast_core::runs;
use syncfast_core::state::{ConfirmMarker, GateKind, State};

const PAIR: &str = "docs";

struct Online(AtomicBool);

impl Network for Online {
    fn is_online(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { self.0.load(Ordering::SeqCst) })
    }
}

#[derive(Default)]
struct Notices(Mutex<Vec<String>>);

impl Notifier for Notices {
    fn notify(&self, notice: &Notice) {
        self.0.lock().unwrap().push(notice.title.clone());
    }
}

impl Notices {
    fn titles(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct Bed {
    _dir: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
    rclone: Rclone,
    local: PathBuf,
    remote: PathBuf,
    online: Online,
    notices: Notices,
}

#[allow(clippy::print_stderr)]
fn bed(mode: &str) -> Option<Bed> {
    let Ok(found) = Rclone::from_env() else {
        eprintln!("skipping: rclone is not installed");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let rclone_conf = root.join("rclone.conf");
    fs::write(&rclone_conf, "[testremote]\ntype = local\n").unwrap();
    let wrapper = root.join("rclone-in-root");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncd '{}' || exit 1\nexec '{}' \"$@\"\n",
            root.display(),
            found.bin().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
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
        rclone: Rclone::new(wrapper).with_config(rclone_conf),
        root,
        local,
        remote,
        online: Online(AtomicBool::new(true)),
        notices: Notices::default(),
        paths,
        _dir: dir,
    };
    bed.set_mode(mode, "manual", "");
    Some(bed)
}

impl Bed {
    /// The sync in `mode`, with `review` for `[defaults]` and `own` as keys
    /// of the sync itself.
    fn set_mode(&self, mode: &str, review: &str, own: &str) {
        let text = format!(
            "version = 2\n\n[defaults]\nreview = \"{review}\"\n\n[[pair]]\nid = \"{PAIR}\"\nname = \"Docs\"\nlocal = \"{}\"\nremote = \"testremote:{}\"\nmode = \"{mode}\"\n{own}",
            self.local.display(),
            self.remote.display(),
        );
        let file = self.paths.config_file();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    fn env<'a>(&'a self, clock: &'a (dyn Fn() -> Zoned + Send + Sync)) -> RunEnv<'a> {
        RunEnv {
            paths: &self.paths,
            rclone: &self.rclone,
            network: &self.online,
            notifier: &self.notices,
            clock,
            forward_signals: true,
        }
    }

    async fn run(&self) -> RunReport {
        let clock = Zoned::now;
        run(&self.env(&clock), PAIR).await.unwrap()
    }

    async fn confirm(&self, kind: ReviewKind) {
        self.confirm_as(kind, None).await;
    }

    async fn confirm_in(&self, kind: ReviewKind, mode: SyncMode) {
        self.confirm_as(kind, Some(mode)).await;
    }

    /// Previews once and saves the marker for `mode`, or for the mode the
    /// preview ran in.
    async fn confirm_as(&self, kind: ReviewKind, mode: Option<SyncMode>) {
        let clock = Zoned::now;
        let reviewed = preview(&self.env(&clock), PAIR, kind).await.unwrap();
        ConfirmMarker {
            kind,
            mode: mode.unwrap_or(reviewed.mode),
            filters_hash: reviewed.filters_hash,
        }
        .save(&self.paths, PAIR)
        .await
        .unwrap();
    }

    /// Previews, confirms and runs the first round of the mode.
    async fn first_sync(&self) {
        self.confirm(ReviewKind::Resync).await;
        assert_eq!(self.run().await, ran_ok(RoundMode::Resync));
        assert!(self.reports_left().is_empty());
    }

    async fn state(&self) -> State {
        State::load(&self.paths, PAIR).await.unwrap()
    }

    /// Dry-run `--combined` reports still on disk: a preview must not leave
    /// any. A real round keeps its own, named after the mode, for Activity.
    fn reports_left(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for dir in [self.paths.runs_dir(PAIR), self.paths.pair_state_dir(PAIR)] {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if name.starts_with("preview-") && name.ends_with(".changes") {
                    found.push(path);
                }
            }
        }
        found
    }

    /// Where `mirror_up` keeps what it replaces: the account root of the
    /// `local` remote is the working directory, which is `root`.
    fn remote_versions(&self) -> PathBuf {
        self.root.join(".syncfast-versions").join(PAIR)
    }
}

fn versions_holding(dir: &Path, name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(stamps) = fs::read_dir(dir) else {
        return found;
    };
    for stamp in stamps {
        let file = stamp.unwrap().path().join(name);
        if let Ok(text) = fs::read_to_string(file) {
            found.push(text);
        }
    }
    found
}

fn ran_ok(mode: RoundMode) -> RunReport {
    RunReport::Ran {
        mode,
        outcome: Outcome::Ok,
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

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn numbered(count: usize) -> Vec<String> {
    (0..count).map(|n| format!("file-{n:02}.txt")).collect()
}

#[tokio::test]
async fn upload_never_deletes_and_replaces_a_changed_file() {
    let Some(bed) = bed("upload") else {
        return;
    };
    write(&bed.local, "changed.txt", "the new local text");
    write(&bed.local, "kept.txt", "kept");
    write(&bed.remote, "changed.txt", "old");
    write(&bed.remote, "only-remote.txt", "R");
    write(&bed.local, "only-local.txt", "L");
    bed.first_sync().await;

    assert_eq!(read(&bed.remote, "changed.txt"), "the new local text");
    assert_eq!(read(&bed.remote, "only-remote.txt"), "R");
    assert_eq!(
        names(&bed.remote),
        [
            "changed.txt",
            "kept.txt",
            "only-local.txt",
            "only-remote.txt"
        ]
    );
    fs::remove_file(bed.local.join("kept.txt")).unwrap();
    write(&bed.local, "changed.txt", "changed again, longer");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(RoundMode::Normal));
    assert_eq!(read(&bed.remote, "changed.txt"), "changed again, longer");
    assert_eq!(read(&bed.remote, "kept.txt"), "kept");
    assert!(!bed.paths.versions_dir(PAIR).exists());
    assert!(!bed.remote_versions().exists());
    assert!(!bed.paths.bisync_workdir(PAIR).exists());
}

#[tokio::test]
async fn download_keeps_the_replaced_local_file_in_versions() {
    let Some(bed) = bed("download") else {
        return;
    };
    write(&bed.local, "changed.txt", "old");
    write(&bed.local, "only-local.txt", "L");
    write(&bed.remote, "changed.txt", "the newer text from the cloud");
    write(&bed.remote, "only-remote.txt", "R");
    bed.first_sync().await;

    assert_eq!(
        read(&bed.local, "changed.txt"),
        "the newer text from the cloud"
    );
    assert_eq!(read(&bed.local, "only-remote.txt"), "R");
    assert_eq!(read(&bed.local, "only-local.txt"), "L");
    assert_eq!(
        versions_holding(&bed.paths.versions_dir(PAIR), "changed.txt"),
        ["old"]
    );
    assert_eq!(
        names(&bed.remote),
        ["changed.txt", "only-remote.txt"],
        "the cloud is never touched"
    );
}

#[tokio::test]
async fn mirror_up_deletes_extras_and_they_land_in_the_remote_versions() {
    let Some(bed) = bed("mirror_up") else {
        return;
    };
    write(&bed.local, "a.txt", "a");
    write(&bed.local, "b.txt", "b");
    write(&bed.remote, "a.txt", "different");
    write(&bed.remote, "extra.txt", "E");
    bed.first_sync().await;

    assert_eq!(names(&bed.remote), ["a.txt", "b.txt"]);
    assert_eq!(read(&bed.remote, "a.txt"), "a");
    let versions = bed.remote_versions();
    assert_eq!(versions_holding(&versions, "extra.txt"), ["E"]);
    assert_eq!(versions_holding(&versions, "a.txt"), ["different"]);
    assert!(!bed.paths.versions_dir(PAIR).exists());
    assert_eq!(bed.state().await.dest_files, Some(2));
}

#[tokio::test]
async fn mirror_down_deletes_extras_locally_and_they_land_in_versions() {
    let Some(bed) = bed("mirror_down") else {
        return;
    };
    write(&bed.remote, "a.txt", "a");
    write(&bed.remote, "b.txt", "b");
    write(&bed.local, "a.txt", "different");
    write(&bed.local, "extra.txt", "E");
    bed.first_sync().await;

    assert_eq!(names(&bed.local), ["a.txt", "b.txt"]);
    assert_eq!(read(&bed.local, "a.txt"), "a");
    let versions = bed.paths.versions_dir(PAIR);
    assert_eq!(versions_holding(&versions, "extra.txt"), ["E"]);
    assert_eq!(versions_holding(&versions, "a.txt"), ["different"]);
    assert!(!bed.remote_versions().exists());
    assert_eq!(bed.state().await.dest_files, Some(2));
}

#[tokio::test]
async fn an_empty_mirror_source_needs_attention_and_the_destination_keeps_every_file() {
    for (mode, fault) in [
        ("mirror_up", Fault::LocalEmpty),
        ("mirror_down", Fault::RemoteEmpty),
    ] {
        for level in ["manual", "deletions", "automatic"] {
            let Some(bed) = bed(mode) else { return };
            let (source, destination) = if mode == "mirror_up" {
                (&bed.local, &bed.remote)
            } else {
                (&bed.remote, &bed.local)
            };
            let files = numbered(5);
            for name in &files {
                write(source, name, name);
            }
            bed.first_sync().await;
            bed.set_mode(mode, level, "");
            for name in &files {
                fs::remove_file(source.join(name)).unwrap();
            }

            let report = bed.run().await;

            let RunReport::Ran {
                outcome: Outcome::NeedsAttention { fault: got, .. },
                ..
            } = report
            else {
                panic!("{mode} {level}: {report:?}");
            };
            assert_eq!(got, fault, "{mode} {level}");
            assert_eq!(names(destination), files, "{mode} {level}");
            let state = bed.state().await;
            assert_eq!(
                state.gate.map(|gate| gate.kind),
                Some(GateKind::NeedsAttention),
                "{mode} {level}"
            );
            assert_eq!(state.consecutive_failures, 1, "{mode} {level}");

            bed.confirm(ReviewKind::Resync).await;
            let again = bed.run().await;

            assert!(
                matches!(
                    again,
                    RunReport::Ran {
                        outcome: Outcome::NeedsAttention { .. },
                        ..
                    }
                ),
                "{mode} {level}: {again:?}"
            );
            assert_eq!(names(destination), files, "{mode} {level} confirmed");
        }
    }
}

#[tokio::test]
async fn more_deletions_than_max_delete_stops_at_too_many_deletes_and_the_confirmed_review_runs() {
    let Some(bed) = bed("mirror_up") else {
        return;
    };
    for name in numbered(10) {
        write(&bed.local, &name, &name);
    }
    bed.first_sync().await;
    for name in numbered(8) {
        fs::remove_file(bed.local.join(name)).unwrap();
    }

    let report = bed.run().await;

    assert_eq!(
        report,
        RunReport::Ran {
            mode: RoundMode::Normal,
            outcome: Outcome::TooManyDeletes
        }
    );
    let state = bed.state().await;
    assert_eq!(
        state.gate.as_ref().map(|gate| gate.kind),
        Some(GateKind::TooManyDeletes)
    );
    let gate = state.gate.unwrap();
    let deleted = 10 - names(&bed.remote).len();
    assert!(deleted >= 2, "{:?}", names(&bed.remote));
    assert_eq!(
        gate.detail,
        format!(
            "{deleted} files were already deleted before the stop. Deleted files are kept in .syncfast-versions on the account."
        )
    );
    assert_eq!(bed.run().await, RunReport::Skipped(SkipReason::Gated));
    assert!(
        names(&bed.remote).len() >= 5,
        "rclone stops after the cap: {:?}",
        names(&bed.remote)
    );

    bed.confirm(ReviewKind::Force).await;
    let confirmed = bed.run().await;

    assert_eq!(confirmed, ran_ok(RoundMode::Force));
    assert_eq!(names(&bed.remote), ["file-08.txt", "file-09.txt"]);
    assert!(bed.state().await.gate.is_none());
}

/// Ten files synced, eight deleted at the source: past the default cap.
async fn mirror_up_past_the_cap(level: &str) -> Option<Bed> {
    let bed = bed("mirror_up")?;
    for name in numbered(10) {
        write(&bed.local, &name, &name);
    }
    bed.first_sync().await;
    bed.set_mode("mirror_up", level, "");
    for name in numbered(8) {
        fs::remove_file(bed.local.join(name)).unwrap();
    }
    Some(bed)
}

#[tokio::test]
async fn a_mirror_past_max_delete_confirms_itself_in_the_same_run_at_automatic() {
    let Some(bed) = mirror_up_past_the_cap("automatic").await else {
        return;
    };

    let report = bed.run().await;

    assert_eq!(report, ran_ok(RoundMode::Force));
    assert_eq!(names(&bed.remote), ["file-08.txt", "file-09.txt"]);
    let state = bed.state().await;
    assert!(state.gate.is_none());
    let reviewed = state.last_run.and_then(|last| last.auto_reviewed).unwrap();
    assert_eq!(reviewed.gate, GateKind::TooManyDeletes);
    assert_eq!(reviewed.level, ReviewLevel::Automatic);
    assert!(
        bed.notices.titles().is_empty(),
        "{:?}",
        bed.notices.titles()
    );
}

#[tokio::test]
async fn a_mirror_past_max_delete_still_waits_at_deletions_and_manual() {
    for (level, checked) in [
        ("manual", None),
        ("deletions", Some(ReviewLevel::Deletions)),
    ] {
        let Some(bed) = mirror_up_past_the_cap(level).await else {
            return;
        };

        let report = bed.run().await;

        assert_eq!(
            report,
            RunReport::Ran {
                mode: RoundMode::Normal,
                outcome: Outcome::TooManyDeletes
            },
            "{level}"
        );
        assert!(names(&bed.remote).len() >= 5, "{level}");
        let gate = bed.state().await.gate.unwrap();
        assert_eq!(gate.kind, GateKind::TooManyDeletes, "{level}");
        assert_eq!(gate.auto_checked, checked, "{level}");
        assert_eq!(
            bed.notices.titles().len(),
            1,
            "{level}: {:?}",
            bed.notices.titles()
        );
    }
}

#[tokio::test]
async fn the_first_mirror_run_is_held_at_manual_and_deletions_and_confirms_itself_at_automatic() {
    for level in ["manual", "deletions", "automatic"] {
        let Some(bed) = bed("mirror_up") else {
            return;
        };
        bed.set_mode("mirror_up", level, "");
        write(&bed.local, "a.txt", "a");
        write(&bed.remote, "extra.txt", "E");

        let report = bed.run().await;

        if level == "automatic" {
            assert_eq!(report, ran_ok(RoundMode::Resync));
            assert_eq!(names(&bed.remote), ["a.txt"]);
            assert!(bed.state().await.gate.is_none());
            assert!(bed.reports_left().is_empty());
        } else {
            assert_eq!(report, RunReport::Skipped(SkipReason::Gated), "{level}");
            assert_eq!(
                bed.state().await.gate.map(|gate| gate.kind),
                Some(GateKind::NeedsReviewFirstRun),
                "{level}"
            );
            assert_eq!(names(&bed.remote), ["extra.txt"], "{level}");
            assert!(bed.reports_left().is_empty(), "{level}");
        }
    }
}

#[tokio::test]
async fn leaving_and_returning_to_bisync_rebuilds_the_listings() {
    let Some(bed) = bed("bisync") else {
        return;
    };
    write(&bed.local, "a.txt", "a");
    bed.first_sync().await;
    assert!(bed.paths.bisync_workdir(PAIR).exists());

    bed.set_mode("upload", "manual", "");
    assert_eq!(
        bed.run().await,
        RunReport::GateSet(GateKind::NeedsReviewModeChanged)
    );
    assert!(
        bed.paths.bisync_workdir(PAIR).exists(),
        "the listings stay until the new mode has worked once"
    );
    bed.confirm(ReviewKind::Resync).await;
    assert_eq!(bed.run().await, ran_ok(RoundMode::Resync));
    assert!(!bed.paths.bisync_workdir(PAIR).exists());
    write(&bed.local, "while-away.txt", "w");
    write(&bed.remote, "remote-only.txt", "R");
    assert_eq!(bed.run().await, ran_ok(RoundMode::Normal));

    bed.set_mode("bisync", "manual", "");
    assert_eq!(
        bed.run().await,
        RunReport::GateSet(GateKind::NeedsReviewModeChanged)
    );
    bed.confirm(ReviewKind::Resync).await;
    assert_eq!(bed.run().await, ran_ok(RoundMode::Resync));

    assert!(bed.paths.bisync_workdir(PAIR).exists());
    assert_eq!(read(&bed.local, "remote-only.txt"), "R");
    write(&bed.local, "after.txt", "x");
    assert_eq!(bed.run().await, ran_ok(RoundMode::Normal));
    assert_eq!(read(&bed.remote, "after.txt"), "x");
}

#[tokio::test]
async fn a_confirmation_given_in_one_mode_does_not_start_another_mode_uncapped() {
    let Some(bed) = bed("bisync") else {
        return;
    };
    write(&bed.local, "a.txt", "a");
    bed.first_sync().await;
    bed.set_mode("mirror_up", "manual", "");
    assert_eq!(
        bed.run().await,
        RunReport::GateSet(GateKind::NeedsReviewModeChanged)
    );

    bed.confirm_in(ReviewKind::Resync, SyncMode::Bisync).await;

    assert_eq!(bed.run().await, RunReport::Skipped(SkipReason::Gated));
    assert_eq!(ConfirmMarker::load(&bed.paths, PAIR).await.unwrap(), None);
    bed.confirm(ReviewKind::Resync).await;
    assert_eq!(bed.run().await, ran_ok(RoundMode::Resync));
}

#[tokio::test]
async fn remote_versions_are_purged_by_the_date_in_the_folder_name() {
    let Some(bed) = bed("mirror_up") else {
        return;
    };
    write(&bed.local, "a.txt", "a");
    bed.first_sync().await;
    let versions = bed.remote_versions();
    let old = Zoned::now()
        .checked_sub(SignedDuration::from_hours(24 * 40))
        .unwrap()
        .strftime("%Y%m%d-%H%M%S")
        .to_string();
    let recent = Zoned::now()
        .checked_sub(SignedDuration::from_hours(24))
        .unwrap()
        .strftime("%Y%m%d-%H%M%S")
        .to_string();
    write(&versions, &format!("{old}/old.txt"), "old");
    write(&versions, &format!("{recent}/recent.txt"), "recent");
    write(&versions, "not-a-stamp/other.txt", "other");
    write(&bed.local, "b.txt", "b");

    let report = bed.run().await;

    assert_eq!(report, ran_ok(RoundMode::Normal));
    assert!(!versions.join(&old).exists());
    assert!(versions.join(&recent).join("recent.txt").exists());
    assert!(versions.join("not-a-stamp").join("other.txt").exists());
}

#[tokio::test]
async fn an_idle_one_way_round_keeps_its_report_with_its_log_until_the_next_check() {
    let Some(bed) = bed("upload") else {
        return;
    };
    write(&bed.local, "a.txt", "a");
    bed.first_sync().await;

    assert_eq!(bed.run().await, ran_ok(RoundMode::Normal));
    let idle = |bed: &Bed| -> Vec<String> {
        names(&bed.paths.runs_dir(PAIR))
            .into_iter()
            .filter(|name| name.ends_with(".idle.jsonl"))
            .collect()
    };
    let first = idle(&bed);
    let stamp = |name: &String| name.split('.').next().unwrap().to_owned();
    let report = |bed: &Bed, stamp: &str| {
        bed.paths
            .runs_dir(PAIR)
            .join(format!("{stamp}.upload.changes"))
            .exists()
    };
    assert_eq!(first.len(), 1);
    assert!(report(&bed, &stamp(&first[0])), "the report stays linked");
    let rounds = runs::load(&bed.paths, PAIR, false).await;
    assert!(rounds.iter().any(|round| round.idle));

    assert_eq!(bed.run().await, ran_ok(RoundMode::Normal));

    let second = idle(&bed);
    assert_eq!(second.len(), 1);
    assert_ne!(first, second);
    assert!(!report(&bed, &stamp(&first[0])), "pruned with its log");
    assert!(report(&bed, &stamp(&second[0])));
}
