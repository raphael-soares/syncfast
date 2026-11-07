//! The watcher against real inotify on a tempdir, with short timings. The
//! trigger is a recorder, so no round and no systemctl ever runs.

#![allow(clippy::unwrap_used)]

mod common;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::Bed;
use syncfast_core::behavior::Behavior;
use syncfast_core::config::Pair;
use syncfast_core::network::BoxFuture;
use syncfast_core::state::try_lock_pair;
use syncfast_core::units::{Systemctl, SystemdMode, UserSystemctl};
use syncfast_core::watch::{Trigger, WatchEnv, Watcher, trigger_for};
use tokio::sync::oneshot;

const DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_WAIT: Duration = Duration::from_millis(1200);
const RECHECK: Duration = Duration::from_millis(100);

fn fast(_defaults: &Behavior, pair: &Pair) -> Behavior {
    Behavior {
        debounce: DEBOUNCE,
        max_wait: MAX_WAIT,
        watch: pair.id != "quiet",
        ..Behavior::default()
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Instant)>>);

impl Trigger for Recorder {
    fn start<'a>(&'a self, pair_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap()
                .push((pair_id.to_owned(), Instant::now()));
            Ok(())
        })
    }
}

impl Recorder {
    fn ids(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

struct Running {
    bed: Bed,
    recorder: Arc<Recorder>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    fn local(&self, id: &str) -> PathBuf {
        self.bed.local(id)
    }

    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.await.unwrap();
    }
}

/// Starts the watcher over `pairs`; `prepare` runs before it starts.
async fn start(pairs: &[(&str, bool)], prepare: impl FnOnce(&Bed)) -> Running {
    start_with(|bed| {
        bed.write_config(pairs);
        prepare(bed);
    })
    .await
}

/// Starts the watcher after `prepare` has written the config.
async fn start_with(prepare: impl FnOnce(&Bed)) -> Running {
    start_using(fast, prepare).await
}

/// Starts the watcher with `behavior_for` resolving each sync's behavior.
async fn start_using(
    behavior_for: fn(&Behavior, &Pair) -> Behavior,
    prepare: impl FnOnce(&Bed),
) -> Running {
    let bed = Bed::new();
    prepare(&bed);
    let recorder = Arc::new(Recorder::default());
    let mut env = WatchEnv::new(bed.paths.clone(), bed.rclone.clone(), recorder.clone());
    env.behavior_for = behavior_for;
    env.lock_recheck = RECHECK;
    env.root_recheck = RECHECK;
    let watcher = Watcher::start(env).await.unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        watcher
            .run(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    Running {
        bed,
        recorder,
        stop: Some(stop),
        task,
    }
}

fn write(dir: &Path, name: &str) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, name).unwrap();
}

async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Waits until `count` triggers happened, for at most `limit`.
async fn wait_for(recorder: &Recorder, count: usize, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if recorder.count() >= count {
            return true;
        }
        sleep(Duration::from_millis(20)).await;
    }
    recorder.count() >= count
}

#[tokio::test]
async fn burst_of_changes_fires_one_trigger_after_the_debounce() {
    let run = start(&[("docs", true)], |_| {}).await;
    let begun = Instant::now();

    for n in 0..5 {
        write(&run.local("docs"), &format!("f{n}.txt"));
        sleep(Duration::from_millis(40)).await;
    }
    sleep(DEBOUNCE / 2).await;
    let early = run.recorder.count();
    let fired = wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;

    assert_eq!(early, 0);
    assert!(fired);
    assert!(begun.elapsed() >= DEBOUNCE);
    assert_eq!(run.recorder.ids(), ["docs"]);
    run.stop().await;
}

#[tokio::test]
async fn continuous_edits_fire_at_max_wait_instead_of_waiting_for_quiet() {
    let run = start(&[("docs", true)], |_| {}).await;
    let begun = Instant::now();

    while begun.elapsed() < MAX_WAIT + DEBOUNCE {
        write(&run.local("docs"), "busy.txt");
        sleep(Duration::from_millis(60)).await;
    }

    let first = run.recorder.0.lock().unwrap().first().map(|(_, at)| *at);
    let first = first.expect("max_wait must have fired during the edits");
    assert!(first.duration_since(begun) + Duration::from_millis(100) >= MAX_WAIT);
    assert!(first.duration_since(begun) < MAX_WAIT + DEBOUNCE);
    run.stop().await;
}

#[tokio::test]
async fn changes_in_excluded_directories_do_not_fire() {
    let run = start(&[("docs", true)], |bed| {
        write(&bed.local("docs"), "node_modules/dep/index.js");
        write(&bed.local("docs"), "src/.keep");
    })
    .await;

    fs::create_dir_all(run.local("docs").join("target")).unwrap();
    write(&run.local("docs"), "node_modules/dep/more.js");
    write(&run.local("docs"), "target/debug/out.o");
    sleep(DEBOUNCE * 3).await;
    let from_excluded = run.recorder.count();
    write(&run.local("docs"), "src/main.rs");
    let fired = wait_for(&run.recorder, 1, Duration::from_secs(2)).await;

    assert_eq!(from_excluded, 0);
    assert!(fired);
    run.stop().await;
}

#[tokio::test]
async fn new_directory_is_watched_including_what_was_created_inside_it_at_once() {
    let run = start(&[("docs", true)], |_| {}).await;

    fs::create_dir_all(run.local("docs").join("a/b/c")).unwrap();
    let first = wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(Duration::from_millis(100)).await;
    write(&run.local("docs"), "a/b/c/deep.txt");
    let second = wait_for(&run.recorder, 2, Duration::from_secs(2)).await;

    assert!(first);
    assert!(second);
    run.stop().await;
}

#[tokio::test]
async fn directory_moved_into_the_tree_is_watched() {
    let run = start(&[("docs", true)], |_| {}).await;
    let outside = run.bed.root.join("elsewhere");
    write(&outside, "moved/inside.txt");

    fs::rename(outside.join("moved"), run.local("docs").join("moved")).unwrap();
    let first = wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(Duration::from_millis(100)).await;
    write(&run.local("docs"), "moved/after.txt");
    let second = wait_for(&run.recorder, 2, Duration::from_secs(2)).await;

    assert!(first);
    assert!(second);
    run.stop().await;
}

#[tokio::test]
async fn directory_moved_out_of_the_tree_stops_reporting() {
    let run = start(&[("docs", true)], |bed| {
        write(&bed.local("docs"), "leaving/x.txt");
    })
    .await;
    let outside = run.bed.root.join("elsewhere");
    fs::create_dir_all(&outside).unwrap();

    fs::rename(run.local("docs").join("leaving"), outside.join("leaving")).unwrap();
    wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;
    let before = run.recorder.count();
    write(&outside, "leaving/y.txt");
    sleep(DEBOUNCE * 3).await;

    assert_eq!(before, 1);
    assert_eq!(run.recorder.count(), before);
    run.stop().await;
}

#[tokio::test]
async fn events_during_a_round_only_mark_dirty_and_fire_once_after_it_ends() {
    let run = start(&[("docs", true)], |_| {}).await;
    let round = try_lock_pair(&run.bed.paths, "docs").unwrap().unwrap();

    write(&run.local("docs"), "written-by-the-round.txt");
    sleep(DEBOUNCE * 3).await;
    let during = run.recorder.count();
    write(&run.local("docs"), "another.txt");
    sleep(DEBOUNCE * 2).await;
    let still_during = run.recorder.count();
    let released = Instant::now();
    drop(round);
    let fired = wait_for(&run.recorder, 1, Duration::from_secs(3)).await;
    let fired_after = released.elapsed();
    sleep(DEBOUNCE * 3).await;

    assert_eq!((during, still_during), (0, 0));
    assert!(fired);
    assert!(fired_after >= DEBOUNCE, "{fired_after:?}");
    assert_eq!(run.recorder.count(), 1);
    run.stop().await;
}

#[tokio::test]
async fn due_trigger_waits_while_a_round_runs() {
    let run = start(&[("docs", true)], |_| {}).await;
    write(&run.local("docs"), "first.txt");
    sleep(Duration::from_millis(60)).await;
    let round = try_lock_pair(&run.bed.paths, "docs").unwrap().unwrap();

    sleep(DEBOUNCE * 3).await;
    let during = run.recorder.count();
    drop(round);
    let fired = wait_for(&run.recorder, 1, Duration::from_secs(3)).await;
    sleep(DEBOUNCE * 3).await;

    assert_eq!(during, 0);
    assert!(fired);
    assert_eq!(run.recorder.count(), 1);
    run.stop().await;
}

#[tokio::test]
async fn each_pair_fires_by_itself() {
    let run = start(&[("docs", true), ("notes", true)], |_| {}).await;

    write(&run.local("notes"), "n.txt");
    wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;

    assert_eq!(run.recorder.ids(), ["notes"]);
    run.stop().await;
}

#[tokio::test]
async fn paused_pairs_and_pairs_with_watch_off_are_not_watched() {
    let run = start(&[("docs", false), ("quiet", true), ("live", true)], |_| {}).await;

    write(&run.local("docs"), "a.txt");
    write(&run.local("quiet"), "b.txt");
    write(&run.local("live"), "c.txt");
    wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;

    assert_eq!(run.recorder.ids(), ["live"]);
    run.stop().await;
}

#[tokio::test]
async fn config_change_makes_the_watcher_pick_up_a_new_pair() {
    let run = start(&[("docs", true)], |_| {}).await;
    run.bed.write_config(&[("docs", true), ("notes", true)]);
    sleep(Duration::from_millis(1200)).await;

    write(&run.local("notes"), "n.txt");
    let fired = wait_for(&run.recorder, 1, Duration::from_secs(3)).await;

    assert!(fired);
    assert_eq!(run.recorder.ids(), ["notes"]);
    run.stop().await;
}

#[tokio::test]
async fn config_change_through_a_symlink_is_noticed() {
    let run = start(&[("docs", true)], |bed| {
        let real = bed.root.join("dotfiles/config.toml");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::rename(bed.paths.config_file(), &real).unwrap();
        std::os::unix::fs::symlink(&real, bed.paths.config_file()).unwrap();
    })
    .await;
    let real = run.bed.root.join("dotfiles/config.toml");
    let text = fs::read_to_string(&real).unwrap();
    let mut with_notes = text.clone();
    let local = run.bed.local("notes");
    fs::create_dir_all(&local).unwrap();
    write!(
        with_notes,
        "\n[[pair]]\nid = \"notes\"\nname = \"notes\"\nlocal = \"{}\"\nremote = \"testremote:{}\"\n",
        local.display(),
        run.bed.remote("notes").display(),
    )
    .unwrap();
    fs::write(&real, with_notes).unwrap();
    sleep(Duration::from_millis(1200)).await;

    write(&local, "n.txt");
    let fired = wait_for(&run.recorder, 1, Duration::from_secs(3)).await;

    assert!(fired);
    assert_eq!(run.recorder.ids(), ["notes"]);
    run.stop().await;
}

#[tokio::test]
async fn broken_config_keeps_the_previous_watches() {
    let run = start(&[("docs", true)], |_| {}).await;
    run.bed.write_config_text("version = 3\n");
    sleep(Duration::from_millis(1200)).await;

    write(&run.local("docs"), "still.txt");
    let fired = wait_for(&run.recorder, 1, Duration::from_secs(3)).await;

    assert!(fired);
    run.stop().await;
}

fn pair_block(id: &str, local: &Path, remote: &Path) -> String {
    format!(
        "\n[[pair]]\nid = \"{id}\"\nname = \"{id}\"\nlocal = \"{}\"\nremote = \"testremote:{}\"\n",
        local.display(),
        remote.display()
    )
}

fn triggers_of(recorder: &Recorder, id: &str) -> usize {
    recorder.ids().iter().filter(|got| *got == id).count()
}

#[tokio::test]
async fn config_inside_a_synced_folder_serves_the_tree_and_the_reload() {
    let run = start_with(|bed| {
        let text = format!(
            "version = 1\n{}",
            pair_block("docs", &bed.paths.config_home, &bed.remote("docs"))
        );
        bed.write_config_text(&text);
    })
    .await;
    let config_dir = run.bed.paths.config_dir();

    write(&config_dir, "note.txt");
    let from_tree = wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;
    let notes_local = run.bed.local("notes");
    fs::create_dir_all(&notes_local).unwrap();
    let text = format!(
        "version = 1\n{}{}",
        pair_block("docs", &run.bed.paths.config_home, &run.bed.remote("docs")),
        pair_block("notes", &notes_local, &run.bed.remote("notes"))
    );
    run.bed.write_config_text(&text);
    sleep(Duration::from_millis(1200)).await;
    write(&notes_local, "n.txt");
    let reloaded = wait_for_id(&run.recorder, "notes", Duration::from_secs(3)).await;
    sleep(DEBOUNCE * 2).await;
    let docs_before = triggers_of(&run.recorder, "docs");
    write(&config_dir, "after.txt");
    sleep(DEBOUNCE * 3).await;

    assert!(from_tree);
    assert!(reloaded);
    assert_eq!(triggers_of(&run.recorder, "docs"), docs_before + 1);
    run.stop().await;
}

async fn wait_for_id(recorder: &Recorder, id: &str, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if triggers_of(recorder, id) > 0 {
            return true;
        }
        sleep(Duration::from_millis(20)).await;
    }
    triggers_of(recorder, id) > 0
}

#[tokio::test]
async fn recreated_directory_with_the_same_name_is_watched_again() {
    let run = start(&[("docs", true)], |bed| {
        write(&bed.local("docs"), "sub/x.txt");
    })
    .await;

    fs::remove_dir_all(run.local("docs").join("sub")).unwrap();
    let removed = wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;
    write(&run.local("docs"), "sub/y.txt");
    let recreated = wait_for(&run.recorder, 2, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;
    write(&run.local("docs"), "sub/z.txt");
    let inside = wait_for(&run.recorder, 3, Duration::from_secs(2)).await;

    assert!(removed);
    assert!(recreated);
    assert!(inside);
    run.stop().await;
}

#[tokio::test]
async fn folder_renamed_away_and_back_is_noticed_and_watched_again() {
    let run = start(&[("docs", true)], |_| {}).await;
    let local = run.local("docs");
    let away = run.bed.root.join("away");

    fs::rename(&local, &away).unwrap();
    let left = wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;
    fs::rename(&away, &local).unwrap();
    let back = wait_for(&run.recorder, 2, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;
    write(&local, "after.txt");
    let after = wait_for(&run.recorder, 3, Duration::from_secs(2)).await;

    assert!(left);
    assert!(back);
    assert!(after);
    run.stop().await;
}

#[tokio::test]
async fn folder_deleted_and_created_again_is_watched_again() {
    let run = start(&[("docs", true)], |_| {}).await;
    let local = run.local("docs");

    fs::remove_dir_all(&local).unwrap();
    let gone = wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;
    fs::create_dir(&local).unwrap();
    let back = wait_for(&run.recorder, 2, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;
    write(&local, "after.txt");
    let after = wait_for(&run.recorder, 3, Duration::from_secs(2)).await;

    assert!(gone);
    assert!(back);
    assert!(after);
    run.stop().await;
}

#[tokio::test]
async fn folder_missing_at_start_is_watched_once_it_appears() {
    let run = start(&[("docs", true)], |bed| {
        fs::remove_dir_all(bed.local("docs")).unwrap();
    })
    .await;

    fs::create_dir(run.local("docs")).unwrap();
    let appeared = wait_for(&run.recorder, 1, Duration::from_secs(2)).await;
    sleep(DEBOUNCE * 2).await;
    write(&run.local("docs"), "first.txt");
    let watched = wait_for(&run.recorder, 2, Duration::from_secs(2)).await;

    assert!(appeared);
    assert!(watched);
    run.stop().await;
}

#[tokio::test]
async fn child_trigger_runs_the_cli_binary_with_run_and_the_pair_id() {
    let bed = Bed::new();
    let out = bed.root.join("args.txt");
    let bin = common::script(
        &bed.root,
        "syncfast",
        &format!("echo \"$@\" > {}", out.display()),
    );
    let trigger = trigger_for(SystemdMode::Off, bin, Arc::new(UserSystemctl::default()));

    trigger.start("docs").await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut args = String::new();
    while args.is_empty() && Instant::now() < deadline {
        sleep(Duration::from_millis(20)).await;
        args = fs::read_to_string(&out).unwrap_or_default();
    }

    assert_eq!(args, "run docs\n");
}

#[tokio::test]
async fn child_trigger_reports_a_binary_that_cannot_be_started() {
    let bed = Bed::new();
    let missing = bed.root.join("no-such-syncfast");
    let trigger = trigger_for(
        SystemdMode::Off,
        missing.clone(),
        Arc::new(UserSystemctl::default()),
    );

    let err = trigger.start("docs").await.unwrap_err();

    assert!(
        err.starts_with(&format!("cannot start {} run docs", missing.display())),
        "{err}"
    );
}

struct RefusingSystemctl;

impl Systemctl for RefusingSystemctl {
    fn run<'a>(&'a self, _args: &'a [&'a str]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("Unit syncfast-docs.service not found.".to_owned()) })
    }
}

#[tokio::test]
async fn systemd_trigger_reports_the_command_and_the_reason_when_systemctl_refuses() {
    let trigger = trigger_for(
        SystemdMode::User,
        PathBuf::new(),
        Arc::new(RefusingSystemctl),
    );

    let err = trigger.start("docs").await.unwrap_err();

    assert_eq!(
        err,
        "systemctl --user start --no-block syncfast-docs.service: Unit syncfast-docs.service not found."
    );
}

#[tokio::test]
async fn the_config_decides_which_syncs_are_watched_and_how_long_they_wait() {
    let run = start_using(Behavior::for_pair, |bed| {
        let mut text = String::from(
            "version = 2\n\n[defaults]\nwatch = false\ndebounce = \"5s\"\nmax_wait = \"5s\"\n",
        );
        text.push_str(&pair_block("off", &bed.local("off"), &bed.remote("off")));
        text.push_str(&pair_block("on", &bed.local("on"), &bed.remote("on")));
        text.push_str("watch = true\n");
        fs::create_dir_all(bed.local("off")).unwrap();
        fs::create_dir_all(bed.local("on")).unwrap();
        bed.write_config_text(&text);
    })
    .await;
    let started = Instant::now();

    write(&run.local("off"), "a.txt");
    write(&run.local("on"), "a.txt");
    let fired = wait_for_id(&run.recorder, "on", Duration::from_secs(9)).await;
    let waited = started.elapsed();

    assert!(fired);
    assert!(waited >= Duration::from_secs(4), "{waited:?}");
    assert_eq!(triggers_of(&run.recorder, "off"), 0);
    run.stop().await;
}

fn two_syncs(bed: &Bed, docs: &str) -> String {
    fs::create_dir_all(bed.local("docs")).unwrap();
    fs::create_dir_all(bed.local("notes")).unwrap();
    let mut text =
        String::from("version = 2\n\n[defaults]\ndebounce = \"5s\"\nmax_wait = \"5s\"\n");
    text.push_str(&pair_block("docs", &bed.local("docs"), &bed.remote("docs")));
    text.push_str(docs);
    text.push_str(&pair_block(
        "notes",
        &bed.local("notes"),
        &bed.remote("notes"),
    ));
    text
}

#[tokio::test]
async fn a_config_change_updates_the_timing_of_one_sync_and_keeps_the_others_watched() {
    let run = start_using(Behavior::for_pair, |bed| {
        bed.write_config_text(&two_syncs(bed, "debounce = \"2m\"\nmax_wait = \"2m\"\n"));
    })
    .await;
    run.bed.write_config_text(&two_syncs(&run.bed, ""));
    sleep(Duration::from_millis(1200)).await;
    let started = Instant::now();

    write(&run.local("docs"), "a.txt");
    write(&run.local("notes"), "a.txt");
    let docs = wait_for_id(&run.recorder, "docs", Duration::from_secs(9)).await;
    let notes = wait_for_id(&run.recorder, "notes", Duration::from_secs(9)).await;
    let waited = started.elapsed();

    assert!(docs && notes);
    assert!(waited >= Duration::from_secs(4), "{waited:?}");
    run.stop().await;
}

#[tokio::test]
async fn turning_watch_off_for_one_sync_stops_only_that_sync() {
    let run = start_using(Behavior::for_pair, |bed| {
        bed.write_config_text(&two_syncs(bed, ""));
    })
    .await;
    run.bed
        .write_config_text(&two_syncs(&run.bed, "watch = false\n"));
    sleep(Duration::from_millis(1200)).await;

    write(&run.local("docs"), "a.txt");
    write(&run.local("notes"), "a.txt");
    let notes = wait_for_id(&run.recorder, "notes", Duration::from_secs(9)).await;
    sleep(Duration::from_secs(1)).await;

    assert!(notes);
    assert_eq!(triggers_of(&run.recorder, "docs"), 0);
    run.stop().await;
}
