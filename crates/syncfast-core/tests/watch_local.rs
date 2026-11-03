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
use syncfast_core::watch::{Trigger, WatchEnv, Watcher};
use tokio::sync::oneshot;

const DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_WAIT: Duration = Duration::from_millis(1200);
const RECHECK: Duration = Duration::from_millis(100);

fn fast(pair: &Pair) -> Behavior {
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
    fn start<'a>(&'a self, pair_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap()
                .push((pair_id.to_owned(), Instant::now()));
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
    let bed = Bed::new();
    bed.write_config(pairs);
    prepare(&bed);
    let recorder = Arc::new(Recorder::default());
    let mut env = WatchEnv::new(bed.paths.clone(), bed.rclone.clone(), recorder.clone());
    env.behavior_for = fast;
    env.lock_recheck = RECHECK;
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
    run.bed.write_config_text("version = 2\n");
    sleep(Duration::from_millis(1200)).await;

    write(&run.local("docs"), "still.txt");
    let fired = wait_for(&run.recorder, 1, Duration::from_secs(3)).await;

    assert!(fired);
    run.stop().await;
}

#[tokio::test]
async fn watcher_stops_when_the_shutdown_future_completes() {
    let run = start(&[("docs", true)], |_| {}).await;

    run.stop().await;
}
