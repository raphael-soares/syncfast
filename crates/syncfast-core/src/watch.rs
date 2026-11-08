//! `syncfast watch` (spec step 9): inotify on every enabled sync's local
//! tree, and a debounced round trigger per sync.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::future::{Future, pending, poll_fn};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures_core::Stream as _;
use inotify::{EventMask, EventStream, Inotify, WatchDescriptor, WatchMask, Watches};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::{Instant, sleep_until};

use crate::behavior::Behavior;
use crate::config::{Config, GlobalConfigError, Pair};
use crate::filters::{self, DirMatcher};
use crate::network::BoxFuture;
use crate::paths::Paths;
use crate::rclone::{Rclone, RcloneError};
use crate::state::is_running;
use crate::units::{Systemctl, SystemdMode, backup_service_name, command_text, service_name};

/// How often a trigger blocked by a running round looks at the lock again.
pub const LOCK_RECHECK: Duration = Duration::from_secs(5);
/// How often a sync whose folder has no watch (unmounted, renamed away)
/// looks for the folder again.
pub const ROOT_RECHECK: Duration = Duration::from_secs(10);
/// Quiet time after `config.toml` changes before the watches are rebuilt.
const CONFIG_RELOAD_DELAY: Duration = Duration::from_millis(500);
const EVENT_BUFFER_BYTES: usize = 4096;

/// The masks carry `MASK_ADD` because the folder that holds `config.toml`
/// can also be a directory of a synced tree; both uses share one watch.
const TREE_MASK: WatchMask = WatchMask::CREATE
    .union(WatchMask::DELETE)
    .union(WatchMask::MODIFY)
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::CLOSE_WRITE)
    .union(WatchMask::DELETE_SELF)
    .union(WatchMask::MOVE_SELF)
    .union(WatchMask::MASK_ADD);
const CONFIG_MASK: WatchMask = WatchMask::CLOSE_WRITE
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::CREATE)
    .union(WatchMask::DELETE)
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::MASK_ADD);

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("{0}")]
    Config(#[from] GlobalConfigError),
    #[error("{0}")]
    Rclone(#[from] RcloneError),
    #[error("inotify: {0}")]
    Inotify(#[source] io::Error),
}

impl WatchError {
    /// 2 for a config the user must fix, 1 for anything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config(_) => 2,
            _ => 1,
        }
    }
}

/// Starts a round of a sync or a run of a backup. Never waits for it to end.
pub trait Trigger: Send + Sync {
    /// `Err` says why the round could not be started.
    fn start<'a>(&'a self, pair_id: &'a str) -> BoxFuture<'a, Result<(), String>>;

    /// `Err` says why the backup could not be started.
    fn start_backup<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
}

/// `systemctl --user start --no-block syncfast-<id>.service`.
pub struct SystemdTrigger {
    systemctl: Arc<dyn Systemctl>,
}

impl SystemdTrigger {
    #[must_use]
    pub fn new(systemctl: Arc<dyn Systemctl>) -> Self {
        Self { systemctl }
    }
}

impl std::fmt::Debug for SystemdTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemdTrigger").finish_non_exhaustive()
    }
}

impl SystemdTrigger {
    async fn start_unit(&self, service: &str) -> Result<(), String> {
        let args = ["start", "--no-block", service];
        self.systemctl
            .run(&args)
            .await
            .map_err(|stderr| format!("{}: {stderr}", command_text(&args)))
    }
}

impl Trigger for SystemdTrigger {
    fn start<'a>(&'a self, pair_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.start_unit(&service_name(pair_id)).await })
    }

    fn start_backup<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.start_unit(&backup_service_name(backup_id)).await })
    }
}

/// `syncfast run <id>` as a child process, for [`SystemdMode::Off`].
#[derive(Debug, Clone)]
pub struct ChildTrigger {
    bin: PathBuf,
}

impl ChildTrigger {
    #[must_use]
    pub fn new(bin: PathBuf) -> Self {
        Self { bin }
    }
}

impl ChildTrigger {
    /// `syncfast <subcommand> <id>` as a detached child.
    fn spawn(&self, subcommand: &str, id: &str) -> Result<(), String> {
        let mut child = Command::new(&self.bin)
            .arg(subcommand)
            .arg(id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|err| {
                format!(
                    "cannot start {} {subcommand} {id}: {err}",
                    self.bin.display()
                )
            })?;
        // Reaps the child; its outcome is in the sync's or backup's state.
        tokio::spawn(async move {
            if let Err(err) = child.wait().await {
                log::warn!("cannot wait for a child: {err}");
            }
        });
        Ok(())
    }
}

impl Trigger for ChildTrigger {
    fn start<'a>(&'a self, pair_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.spawn("run", pair_id) })
    }

    fn start_backup<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.spawn("backup-run", backup_id) })
    }
}

/// The trigger for `mode`: systemd units, or the CLI binary at `bin`.
#[must_use]
pub fn trigger_for(
    mode: SystemdMode,
    bin: PathBuf,
    systemctl: Arc<dyn Systemctl>,
) -> Arc<dyn Trigger> {
    match mode {
        SystemdMode::User => Arc::new(SystemdTrigger::new(systemctl)),
        SystemdMode::Off => Arc::new(ChildTrigger::new(bin)),
    }
}

/// What the watcher needs from the outside world.
pub struct WatchEnv {
    pub paths: Paths,
    pub rclone: Rclone,
    pub trigger: Arc<dyn Trigger>,
    /// The timings (and `watch` switch) of a sync.
    pub behavior_for: fn(&Behavior, &Pair) -> Behavior,
    pub lock_recheck: Duration,
    pub root_recheck: Duration,
}

impl WatchEnv {
    #[must_use]
    pub fn new(paths: Paths, rclone: Rclone, trigger: Arc<dyn Trigger>) -> Self {
        Self {
            paths,
            rclone,
            trigger,
            behavior_for: Behavior::for_pair,
            lock_recheck: LOCK_RECHECK,
            root_recheck: ROOT_RECHECK,
        }
    }
}

impl std::fmt::Debug for WatchEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchEnv")
            .field("paths", &self.paths)
            .field("rclone", &self.rclone)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// Changes seen. Fires `debounce` after the last one, or `max_wait`
    /// after the first, whichever comes first.
    Pending {
        first: Instant,
        last: Instant,
    },
    /// A round was running when the sync was due. Events only mark it
    /// dirty (they are mostly the round's own writes); the lock is polled.
    Blocked {
        check_at: Instant,
    },
    /// The round ended: one more `debounce`, then a single trigger.
    Settling {
        fire_at: Instant,
    },
}

#[derive(Debug, Clone, Copy)]
struct Timing {
    debounce: Duration,
    max_wait: Duration,
    recheck: Duration,
}

/// When a sync's trigger fires. Pure: time and the lock state come in as
/// arguments.
#[derive(Debug, Clone, Copy)]
struct Schedule {
    timing: Timing,
    phase: Phase,
}

impl Schedule {
    fn new(timing: Timing) -> Self {
        Self {
            timing,
            phase: Phase::Idle,
        }
    }

    /// A change under the sync's local folder.
    fn event(&mut self, now: Instant, running: impl FnOnce() -> bool) {
        self.phase = match self.phase {
            Phase::Idle if running() => Phase::Blocked {
                check_at: now + self.timing.recheck,
            },
            Phase::Idle => Phase::Pending {
                first: now,
                last: now,
            },
            Phase::Pending { first, .. } => Phase::Pending { first, last: now },
            blocked_or_settling => blocked_or_settling,
        };
    }

    fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Idle => None,
            Phase::Pending { first, last } => Some(self.pending_fire_at(first, last)),
            Phase::Blocked { check_at } => Some(check_at),
            Phase::Settling { fire_at } => Some(fire_at),
        }
    }

    fn pending_fire_at(&self, first: Instant, last: Instant) -> Instant {
        (last + self.timing.debounce).min(first + self.timing.max_wait)
    }

    /// Advances to `now`; true when the trigger must fire.
    fn due(&mut self, now: Instant, running: impl FnOnce() -> bool) -> bool {
        if self.deadline().is_none_or(|deadline| now < deadline) {
            return false;
        }
        match self.phase {
            Phase::Idle => false,
            Phase::Pending { .. } => {
                if running() {
                    self.phase = Phase::Blocked {
                        check_at: now + self.timing.recheck,
                    };
                    false
                } else {
                    self.phase = Phase::Idle;
                    true
                }
            }
            Phase::Blocked { .. } => {
                self.phase = if running() {
                    Phase::Blocked {
                        check_at: now + self.timing.recheck,
                    }
                } else {
                    Phase::Settling {
                        fire_at: now + self.timing.debounce,
                    }
                };
                false
            }
            Phase::Settling { .. } => {
                self.phase = Phase::Idle;
                true
            }
        }
    }
}

/// A sync whose tree is watched.
#[derive(Debug)]
struct Watched {
    id: String,
    root: PathBuf,
    matcher: DirMatcher,
    timing: Timing,
}

/// The directories with a watch, by descriptor.
#[derive(Debug)]
struct Dir {
    pair: usize,
    /// Relative to the sync's local folder; empty for the folder itself.
    rel: PathBuf,
}

fn is_watch_limit(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::StorageFull
}

/// What the user reads next to a sync that [`limited_pairs`] names.
pub const WATCH_LIMIT_NOTICE: &str = "Real-time sync is off: raise fs.inotify.max_user_watches.";

#[derive(Debug, Default, Serialize, Deserialize)]
struct WatchLimit {
    pairs: Vec<String>,
}

/// The ids of the syncs the running watcher could not watch fully. Empty
/// when the file is absent or unreadable.
#[must_use]
pub fn limited_pairs(paths: &Paths) -> Vec<String> {
    std::fs::read(paths.watch_limit_file())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WatchLimit>(&bytes).ok())
        .map(|limit| limit.pairs)
        .unwrap_or_default()
}

/// Records `ids` in `watch-limit.json`, or removes the file when there are
/// none.
fn record_limited_pairs(paths: &Paths, ids: Vec<String>) {
    let file = paths.watch_limit_file();
    if ids.is_empty() {
        if let Err(err) = std::fs::remove_file(&file)
            && err.kind() != io::ErrorKind::NotFound
        {
            log::warn!("cannot remove {}: {err}", file.display());
        }
        return;
    }
    let outcome = serde_json::to_vec(&WatchLimit { pairs: ids })
        .map_err(io::Error::other)
        .and_then(|json| fastframe_file::write(&file, json));
    if let Err(err) = outcome {
        log::warn!("cannot write {}: {err}", file.display());
    }
}

/// The inotify watcher. [`Watcher::start`] puts the watches in place;
/// [`Watcher::run`] then serves events until told to stop.
pub struct Watcher {
    env: WatchEnv,
    stream: EventStream<Vec<u8>>,
    watches: Watches,
    pairs: Vec<Watched>,
    dirs: HashMap<WatchDescriptor, Dir>,
    /// Syncs that ran into the inotify limit and are left to the timer.
    limit_hit: HashSet<usize>,
    schedules: HashMap<String, Schedule>,
    config_watch: Option<(WatchDescriptor, OsString)>,
    reload_at: Option<Instant>,
    /// Syncs whose folder has no watch because it is gone; see
    /// [`ROOT_RECHECK`].
    missing_roots: HashSet<usize>,
    roots_check_at: Option<Instant>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("pairs", &self.pairs)
            .field("directories", &self.dirs.len())
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Loads the config and watches every enabled sync with `watch` on.
    ///
    /// # Errors
    /// The config has a global problem, rclone cannot list its accounts, or
    /// inotify cannot start.
    pub async fn start(env: WatchEnv) -> Result<Self, WatchError> {
        let inotify = Inotify::init().map_err(WatchError::Inotify)?;
        let registry = inotify.watches();
        let stream = inotify
            .into_event_stream(vec![0; EVENT_BUFFER_BYTES])
            .map_err(WatchError::Inotify)?;
        let pairs = load_pairs(&env).await?;
        let mut watcher = Self {
            env,
            stream,
            watches: registry,
            pairs,
            dirs: HashMap::new(),
            limit_hit: HashSet::new(),
            schedules: HashMap::new(),
            config_watch: None,
            reload_at: None,
            missing_roots: HashSet::new(),
            roots_check_at: None,
        };
        watcher.watch_config().await;
        watcher.watch_trees();
        Ok(watcher)
    }

    /// Serves events and fires triggers until `shutdown` completes.
    ///
    /// # Errors
    /// The inotify stream failed.
    pub async fn run(mut self, shutdown: impl Future<Output = ()>) -> Result<(), WatchError> {
        let mut shutdown = pin!(shutdown);
        loop {
            let deadline = self.next_deadline();
            tokio::select! {
                () = &mut shutdown => {
                    record_limited_pairs(&self.env.paths, Vec::new());
                    return Ok(());
                }
                event = poll_fn(|cx| std::pin::Pin::new(&mut self.stream).poll_next(cx)) => {
                    match event {
                        Some(Ok(event)) => self.handle(&event.wd, event.mask, event.name.as_deref()),
                        Some(Err(err)) => return Err(WatchError::Inotify(err)),
                        None => return Err(WatchError::Inotify(io::Error::other("event stream ended"))),
                    }
                }
                () = wait_until(deadline) => self.tick().await,
            }
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.schedules
            .values()
            .filter_map(Schedule::deadline)
            .chain(self.reload_at)
            .chain(self.roots_check_at)
            .min()
    }

    fn mark(&mut self, pair: usize, now: Instant) {
        let watched = &self.pairs[pair];
        let paths = &self.env.paths;
        if let Some(schedule) = self.schedules.get_mut(&watched.id) {
            schedule.event(now, || is_running(paths, &watched.id));
        }
    }

    fn note_missing_root(&mut self, pair: usize) {
        self.missing_roots.insert(pair);
        self.roots_check_at
            .get_or_insert(Instant::now() + self.env.root_recheck);
    }

    /// Watches again the syncs whose folder is back, and marks them: what
    /// changed while the folder was away has not been synced.
    fn rewatch_missing_roots(&mut self, now: Instant) {
        let back: Vec<usize> = self
            .missing_roots
            .iter()
            .copied()
            .filter(|&pair| self.pairs[pair].root.is_dir())
            .collect();
        for pair in back {
            self.missing_roots.remove(&pair);
            self.watch_subtree(pair, PathBuf::new());
            self.mark(pair, now);
        }
        if !self.missing_roots.is_empty() {
            self.roots_check_at
                .get_or_insert(now + self.env.root_recheck);
        }
    }

    async fn tick(&mut self) {
        let now = Instant::now();
        if self.reload_at.is_some_and(|at| now >= at) {
            self.reload_at = None;
            self.reload().await;
        }
        if self.roots_check_at.is_some_and(|at| now >= at) {
            self.roots_check_at = None;
            self.rewatch_missing_roots(now);
        }
        let mut fire = Vec::new();
        for index in 0..self.pairs.len() {
            let id = &self.pairs[index].id;
            let running = || is_running(&self.env.paths, id);
            if self
                .schedules
                .get_mut(id)
                .is_some_and(|schedule| schedule.due(now, running))
            {
                fire.push(id.clone());
            }
        }
        for id in fire {
            if let Err(err) = self.env.trigger.start(&id).await {
                log::warn!("{err}");
            }
        }
    }

    fn handle(&mut self, wd: &WatchDescriptor, mask: EventMask, name: Option<&std::ffi::OsStr>) {
        let now = Instant::now();
        if mask.contains(EventMask::Q_OVERFLOW) {
            log::warn!("inotify queue overflowed; checking every sync again");
            for pair in 0..self.pairs.len() {
                self.mark(pair, now);
            }
            self.watch_trees();
            return;
        }
        let is_config = self
            .config_watch
            .as_ref()
            .is_some_and(|(config, _)| config == wd);
        if is_config
            && let Some((_, file)) = &self.config_watch
            && name == Some(file.as_os_str())
        {
            self.reload_at = Some(now + CONFIG_RELOAD_DELAY);
        }
        if mask.contains(EventMask::IGNORED) {
            if is_config {
                self.config_watch = None;
            }
            if let Some(dir) = self.dirs.remove(wd)
                && dir.rel.is_empty()
            {
                self.note_missing_root(dir.pair);
            }
            return;
        }
        let Some(dir) = self.dirs.get(wd) else {
            return;
        };
        let pair = dir.pair;
        if mask.contains(EventMask::MOVE_SELF) {
            if dir.rel.is_empty() {
                self.forget_subtree(pair, Path::new(""));
                self.note_missing_root(pair);
                self.mark(pair, now);
            }
            return;
        }
        let rel = name.map_or_else(|| dir.rel.clone(), |name| dir.rel.join(name));
        if name.is_some() && self.pairs[pair].matcher.is_excluded(&rel) {
            return;
        }
        if mask.contains(EventMask::ISDIR) {
            if mask.intersects(EventMask::CREATE | EventMask::MOVED_TO) {
                self.watch_subtree(pair, rel);
            } else if mask.contains(EventMask::MOVED_FROM) {
                self.forget_subtree(pair, &rel);
            }
        }
        self.mark(pair, now);
    }

    /// Puts a watch on every directory of every sync again.
    fn watch_trees(&mut self) {
        let stale: Vec<WatchDescriptor> = self.dirs.drain().map(|(wd, _)| wd).collect();
        for wd in stale {
            self.drop_watch(wd);
        }
        self.limit_hit.clear();
        self.missing_roots.clear();
        self.roots_check_at = None;
        let timings: Vec<(String, Timing)> = self
            .pairs
            .iter()
            .map(|pair| (pair.id.clone(), pair.timing))
            .collect();
        let mut kept = HashMap::new();
        for (id, timing) in timings {
            let mut schedule = self
                .schedules
                .remove(&id)
                .unwrap_or_else(|| Schedule::new(timing));
            schedule.timing = timing;
            kept.insert(id, schedule);
        }
        self.schedules = kept;
        for pair in 0..self.pairs.len() {
            self.watch_subtree(pair, PathBuf::new());
        }
        self.publish_limit();
    }

    fn publish_limit(&self) {
        let mut ids: Vec<String> = self
            .limit_hit
            .iter()
            .map(|&pair| self.pairs[pair].id.clone())
            .collect();
        ids.sort();
        record_limited_pairs(&self.env.paths, ids);
    }

    /// Watches `start` (relative to the sync's folder) and every directory
    /// below it that the filters do not exclude.
    fn watch_subtree(&mut self, pair: usize, start: PathBuf) {
        let mut pending = vec![start];
        while let Some(rel) = pending.pop() {
            if self.limit_hit.contains(&pair) {
                return;
            }
            let watched = &self.pairs[pair];
            if !rel.as_os_str().is_empty() && watched.matcher.is_excluded(&rel) {
                continue;
            }
            let dir = watched.root.join(&rel);
            match self.watches.add(&dir, TREE_MASK) {
                Ok(wd) => {
                    self.dirs.insert(
                        wd,
                        Dir {
                            pair,
                            rel: rel.clone(),
                        },
                    );
                }
                Err(err) if is_watch_limit(&err) => {
                    log::warn!(
                        "inotify watch limit reached for \"{}\"; relying on the timer",
                        watched.id
                    );
                    self.limit_hit.insert(pair);
                    self.publish_limit();
                    return;
                }
                Err(err) => {
                    log::warn!("cannot watch {}: {err}", dir.display());
                    if rel.as_os_str().is_empty() {
                        self.note_missing_root(pair);
                    }
                    continue;
                }
            }
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    pending.extend(
                        entries
                            .filter_map(Result::ok)
                            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                            .map(|entry| rel.join(entry.file_name())),
                    );
                }
                Err(err) => log::warn!("cannot list {}: {err}", dir.display()),
            }
        }
    }

    /// A directory moved out of the tree keeps its inode, and with it the
    /// watches below it; they would report changes outside the sync.
    fn forget_subtree(&mut self, pair: usize, rel: &Path) {
        let gone: Vec<WatchDescriptor> = self
            .dirs
            .iter()
            .filter(|(_, dir)| dir.pair == pair && dir.rel.starts_with(rel))
            .map(|(wd, _)| wd.clone())
            .collect();
        for wd in gone {
            self.dirs.remove(&wd);
            self.drop_watch(wd);
        }
    }

    /// Removes a watch unless `config.toml`'s folder shares it.
    fn drop_watch(&mut self, wd: WatchDescriptor) {
        let shared = self
            .config_watch
            .as_ref()
            .is_some_and(|(config, _)| *config == wd);
        if !shared {
            let _ = self.watches.remove(wd);
        }
    }

    /// Watches the folder that holds the real `config.toml` (the target of
    /// the stow symlink), because that is where saves land.
    async fn watch_config(&mut self) {
        if let Some((wd, _)) = self.config_watch.take() {
            let _ = self.watches.remove(wd);
        }
        let file = self.env.paths.config_file();
        let target = tokio::fs::canonicalize(&file).await.unwrap_or(file);
        let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
            return;
        };
        match self.watches.add(dir, CONFIG_MASK) {
            Ok(wd) => self.config_watch = Some((wd, name.to_owned())),
            Err(err) => log::warn!("cannot watch {} for changes: {err}", dir.display()),
        }
    }

    async fn reload(&mut self) {
        match load_pairs(&self.env).await {
            Ok(pairs) => self.pairs = pairs,
            Err(err) => {
                log::warn!("keeping the previous configuration: {err}");
                return;
            }
        }
        self.watch_config().await;
        self.watch_trees();
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => sleep_until(at).await,
        None => pending().await,
    }
}

/// The valid, enabled syncs that want their folder watched.
async fn load_pairs(env: &WatchEnv) -> Result<Vec<Watched>, WatchError> {
    let config = Config::load(&env.paths).await?;
    let accounts = env.rclone.listremotes().await?;
    let validation = config.validate(&env.paths, &accounts);
    let defaults = config.defaults();
    let mut watched = Vec::new();
    for pair in validation.pairs.iter().filter(|pair| pair.enabled) {
        let behavior = (env.behavior_for)(&defaults, pair);
        if !behavior.watch || !behavior.mode.local_is_source() {
            continue;
        }
        watched.push(Watched {
            id: pair.id.clone(),
            root: pair.local.clone(),
            matcher: DirMatcher::new(&filters::render_live(pair).await.text),
            timing: Timing {
                debounce: behavior.debounce,
                max_wait: behavior.max_wait,
                recheck: env.lock_recheck,
            },
        });
    }
    Ok(watched)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBOUNCE: Duration = Duration::from_secs(30);
    const MAX_WAIT: Duration = Duration::from_secs(300);
    const RECHECK: Duration = Duration::from_secs(5);

    fn schedule() -> (Schedule, Instant) {
        let timing = Timing {
            debounce: DEBOUNCE,
            max_wait: MAX_WAIT,
            recheck: RECHECK,
        };
        (Schedule::new(timing), Instant::now())
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn burst_of_events_fires_once_a_debounce_after_the_last_one() {
        let (mut schedule, t0) = schedule();

        for at in [0, 10, 25] {
            schedule.event(t0 + secs(at), || false);
        }

        assert_eq!(schedule.deadline(), Some(t0 + secs(25) + DEBOUNCE));
        assert!(!schedule.due(t0 + secs(54), || false));
        assert!(schedule.due(t0 + secs(55), || false));
        assert_eq!(schedule.deadline(), None);
    }

    #[test]
    fn continuous_events_fire_at_max_wait_after_the_first() {
        let (mut schedule, t0) = schedule();

        for at in (0..=290).step_by(10) {
            schedule.event(t0 + secs(at), || false);
        }

        assert_eq!(schedule.deadline(), Some(t0 + MAX_WAIT));
        assert!(!schedule.due(t0 + secs(299), || false));
        assert!(schedule.due(t0 + MAX_WAIT, || false));
    }

    #[test]
    fn event_while_a_round_runs_does_not_start_the_debounce() {
        let (mut schedule, t0) = schedule();

        schedule.event(t0, || true);

        assert_eq!(schedule.deadline(), Some(t0 + RECHECK));
        assert!(!schedule.due(t0 + secs(30), || true));
    }

    #[test]
    fn due_trigger_waits_for_the_round_then_settles_one_debounce_and_fires_once() {
        let (mut schedule, t0) = schedule();
        schedule.event(t0, || false);

        let at_due = schedule.due(t0 + DEBOUNCE, || true);
        let still_running = schedule.due(t0 + DEBOUNCE + RECHECK, || true);
        let freed = schedule.due(t0 + DEBOUNCE + 2 * RECHECK, || false);
        let freed_at = t0 + DEBOUNCE + 2 * RECHECK;
        let too_early = schedule.due(freed_at + DEBOUNCE - secs(1), || false);
        let settled = schedule.due(freed_at + DEBOUNCE, || false);
        let after = schedule.due(freed_at + 2 * DEBOUNCE, || false);

        assert_eq!(
            [at_due, still_running, freed, too_early, settled, after],
            [false, false, false, false, true, false]
        );
    }

    #[test]
    fn events_while_blocked_or_settling_change_nothing() {
        let (mut schedule, t0) = schedule();
        schedule.event(t0, || true);
        let blocked = schedule.deadline();

        schedule.event(t0 + secs(1), || false);
        schedule.event(t0 + secs(2), || true);

        assert_eq!(schedule.deadline(), blocked);
        assert!(!schedule.due(t0 + RECHECK, || false));
        let settling = schedule.deadline();
        schedule.event(t0 + RECHECK + secs(1), || false);
        assert_eq!(schedule.deadline(), settling);
        assert!(schedule.due(t0 + RECHECK + DEBOUNCE, || false));
    }

    #[test]
    fn nothing_is_due_without_events() {
        let (mut schedule, t0) = schedule();

        assert_eq!(schedule.deadline(), None);
        assert!(!schedule.due(t0 + secs(3600), || false));
    }

    fn temp_paths() -> (tempfile::TempDir, Paths) {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            root.path().join("home"),
            root.path().join("config"),
            root.path().join("state"),
            root.path().join("data"),
            root.path().join("run"),
        );
        (root, paths)
    }

    #[test]
    fn limited_pairs_are_written_for_status_and_read_back() {
        let (_root, paths) = temp_paths();

        record_limited_pairs(&paths, vec!["docs".to_owned(), "notes".to_owned()]);

        assert_eq!(limited_pairs(&paths), ["docs", "notes"]);
        let json = std::fs::read_to_string(paths.watch_limit_file()).unwrap();
        assert_eq!(json, r#"{"pairs":["docs","notes"]}"#);
    }

    #[test]
    fn watching_everything_again_removes_the_limit_file() {
        let (_root, paths) = temp_paths();
        record_limited_pairs(&paths, vec!["docs".to_owned()]);

        record_limited_pairs(&paths, Vec::new());

        assert!(!paths.watch_limit_file().exists());
        assert!(limited_pairs(&paths).is_empty());
    }

    #[test]
    fn clearing_the_limit_when_there_is_none_is_fine() {
        let (_root, paths) = temp_paths();

        record_limited_pairs(&paths, Vec::new());

        assert!(limited_pairs(&paths).is_empty());
    }

    #[test]
    fn an_unreadable_limit_file_names_no_pair() {
        let (_root, paths) = temp_paths();
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        std::fs::write(paths.watch_limit_file(), "not json").unwrap();

        assert!(limited_pairs(&paths).is_empty());
    }

    #[test]
    fn running_out_of_inotify_watches_is_recognized_as_enospc() {
        assert!(is_watch_limit(&io::Error::from_raw_os_error(28)));
        assert!(!is_watch_limit(&io::Error::from_raw_os_error(2)));
    }

    #[derive(Default)]
    struct RecordingSystemctl(std::sync::Mutex<Vec<Vec<String>>>);

    impl Systemctl for RecordingSystemctl {
        fn run<'a>(&'a self, args: &'a [&'a str]) -> BoxFuture<'a, Result<(), String>> {
            let call = args.iter().map(|arg| (*arg).to_owned()).collect();
            self.0.lock().unwrap().push(call);
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn the_systemd_trigger_starts_the_backup_service_without_waiting() {
        let systemctl = Arc::new(RecordingSystemctl::default());
        let trigger = SystemdTrigger::new(Arc::clone(&systemctl) as Arc<dyn Systemctl>);

        trigger.start_backup("docs").await.unwrap();

        assert_eq!(
            *systemctl.0.lock().unwrap(),
            [["start", "--no-block", "syncfast-backup_docs.service"]]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_trigger_runs_backup_run_with_the_id() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("syncfast");
        let argv = dir.path().join("argv");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s ' \"$@\" > '{}'\n", argv.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        ChildTrigger::new(script)
            .start_backup("docs")
            .await
            .unwrap();

        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&argv)
                && !text.is_empty()
            {
                assert_eq!(text.trim(), "backup-run docs");
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the child never wrote its arguments");
    }
}
