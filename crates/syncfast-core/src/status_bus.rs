//! The status service `syncfast watch` publishes on the session bus, for a
//! file manager extension: which folders are sync roots and their state,
//! the state of each file and folder inside them, and which files are
//! conflicted. The extension lives outside the sandbox and never reads
//! syncfast's files.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::Metadata;
use std::io;
use std::ops::Bound;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;

use crate::conflicts::{self, Naming};
use crate::filters::DirMatcher;
use crate::paths::Paths;
use crate::state::{State, is_running};
use crate::watch::Trigger;

pub const BUS_NAME: &str = "io.github.raphael_soares.syncfast.Status";
pub const OBJECT_PATH: &str = "/io/github/raphael_soares/syncfast/Status";

/// How often the states are read again; `Changed` follows within this time
/// of a round starting or ending.
const POLL: Duration = Duration::from_secs(2);

/// Changed paths kept per sync before the record collapses into "the whole
/// sync changed".
const DIRTY_CAP: usize = 10_000;

/// Per sync id, each path the watcher saw change (relative to the root)
/// and when. The empty path stands for the whole sync.
type Dirty = HashMap<String, BTreeMap<PathBuf, Timestamp>>;

/// The bus the service connects to.
#[derive(Debug, Clone)]
pub enum Bus {
    Session,
    /// A bus at this address, for tests.
    Address(String),
}

/// What the service needs to know about an enabled sync.
#[derive(Debug, Clone)]
pub struct SyncInfo {
    id: String,
    root: PathBuf,
    naming: Naming,
    matcher: DirMatcher,
}

impl SyncInfo {
    #[must_use]
    pub fn new(id: String, root: PathBuf, naming: Naming, matcher: DirMatcher) -> Self {
        Self {
            id,
            root,
            naming,
            matcher,
        }
    }
}

/// The state shown on a sync's root, by precedence. `settling` is a trigger
/// waiting for the debounce, or for a running round and the settle time
/// after it.
#[must_use]
pub fn root_state(state: &State, running: bool, settling: bool) -> &'static str {
    if state.gate.is_some() {
        "attention"
    } else if state.conflicts_pending > 0 {
        "conflict"
    } else if running {
        "syncing"
    } else if settling {
        "pending"
    } else {
        "synced"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootEntry {
    id: String,
    path: String,
    state: &'static str,
}

/// What `Roots`, `Conflicts` and `Directory` answer.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Snapshot {
    roots: Vec<RootEntry>,
    conflicts: Vec<String>,
    syncs: Vec<SyncView>,
}

/// The round the files on disk match: the last one that succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Baseline {
    started: Timestamp,
    finished: Timestamp,
}

/// `None` when the sync never succeeded. An older success without its
/// round's start counts as a round that started when it finished.
fn baseline(state: &State) -> Option<Baseline> {
    let success = state.last_success?;
    Some(match &state.last_run {
        Some(run) if run.finished == success => Baseline {
            started: run.started,
            finished: run.finished,
        },
        _ => Baseline {
            started: success,
            finished: success,
        },
    })
}

/// What `Directory` needs to know about one sync.
#[derive(Debug, Clone)]
struct SyncView {
    id: String,
    root: PathBuf,
    matcher: DirMatcher,
    baseline: Option<Baseline>,
    running: bool,
    /// Relative to `root`, from the last round.
    errors: BTreeMap<PathBuf, String>,
    /// Relative to `root`.
    conflicts: HashSet<PathBuf>,
    /// Only so that a new change in the [`Dirty`] record counts as a
    /// change of the snapshot.
    dirty_len: usize,
    dirty_newest: Option<Timestamp>,
}

/// The matcher is left out: it follows the config, and a filter change
/// holds the sync for review, which changes the root state.
impl PartialEq for SyncView {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.root == other.root
            && self.baseline == other.baseline
            && self.running == other.running
            && self.errors == other.errors
            && self.conflicts == other.conflicts
            && self.dirty_len == other.dirty_len
            && self.dirty_newest == other.dirty_newest
    }
}

impl Eq for SyncView {}

/// The entries of `map` that are `rel` or below it.
fn under<'a, V>(
    map: &'a BTreeMap<PathBuf, V>,
    rel: &'a Path,
) -> impl Iterator<Item = (&'a PathBuf, &'a V)> {
    map.range::<Path, _>((Bound::Included(rel), Bound::Unbounded))
        .take_while(move |(path, _)| path.starts_with(rel))
}

/// The entries of `dirty` that are `rel` or below it, and the whole-sync
/// entry.
fn dirty_under<'a>(
    dirty: &'a BTreeMap<PathBuf, Timestamp>,
    rel: &'a Path,
) -> impl Iterator<Item = (&'a PathBuf, &'a Timestamp)> {
    dirty
        .get_key_value(Path::new(""))
        .into_iter()
        .chain(under(dirty, rel))
}

/// The state of one file or folder of a sync, by precedence, and its
/// detail: the error message, or when it was last synced (RFC 3339).
/// `changed_at` is its ctime: a change while nothing watched still shows.
fn entry_state(
    view: &SyncView,
    dirty: &BTreeMap<PathBuf, Timestamp>,
    rel: &Path,
    is_dir: bool,
    changed_at: Option<Timestamp>,
) -> (&'static str, String) {
    if view.conflicts.contains(rel) {
        return ("conflict", String::new());
    }
    let error = if is_dir {
        under(&view.errors, rel).map(|(_, message)| message).next()
    } else {
        view.errors.get(rel)
    };
    if let Some(message) = error {
        return ("error", message.clone());
    }
    let Some(base) = view.baseline else {
        return (pending(view), String::new());
    };
    let touched = if is_dir {
        dirty_under(dirty, rel).next().is_some()
    } else {
        dirty.contains_key(Path::new("")) || dirty.contains_key(rel)
    };
    if touched || changed_at.is_some_and(|at| at > base.finished) {
        return (pending(view), String::new());
    }
    ("synced", base.finished.to_string())
}

fn pending(view: &SyncView) -> &'static str {
    if view.running { "syncing" } else { "pending" }
}

fn changed_at(meta: &Metadata) -> Option<Timestamp> {
    if meta.ctime() == 0 && meta.ctime_nsec() == 0 {
        return None;
    }
    Timestamp::new(meta.ctime(), i32::try_from(meta.ctime_nsec()).ok()?).ok()
}

/// Name, state and detail of each entry of `dir`, which is `rel` inside
/// the sync. Entries the filters leave out, names that are not UTF-8 and
/// entries that cannot be read are skipped.
fn list_directory(
    view: &SyncView,
    dirty: &BTreeMap<PathBuf, Timestamp>,
    dir: &Path,
    rel: &Path,
) -> Vec<(String, String, String)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            log::debug!("cannot list {}: {err}", dir.display());
            return Vec::new();
        }
    };
    let mut listed = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let child = rel.join(&name);
        let is_dir = meta.is_dir();
        let excluded = if is_dir {
            view.matcher.is_excluded(&child)
        } else {
            view.matcher.is_excluded_file(&child)
        };
        if excluded {
            continue;
        }
        let (state, detail) = entry_state(view, dirty, &child, is_dir, changed_at(&meta));
        listed.push((name, state.to_owned(), detail));
    }
    listed.sort_unstable();
    listed
}

/// One sync as read from disk, before it becomes part of a [`Snapshot`].
struct Sample {
    id: String,
    root: PathBuf,
    state: &'static str,
    /// Relative to `root`.
    conflicts: Vec<PathBuf>,
}

/// Roots in the order given; the conflicted files absolute, sorted and
/// without repeats. A path that is not UTF-8 cannot cross the bus and is
/// left out.
fn snapshot(samples: Vec<Sample>) -> Snapshot {
    let mut result = Snapshot::default();
    for sample in samples {
        for file in &sample.conflicts {
            if let Some(path) = sample.root.join(file).to_str() {
                result.conflicts.push(path.to_owned());
            }
        }
        if let Some(path) = sample.root.to_str() {
            result.roots.push(RootEntry {
                id: sample.id,
                path: path.to_owned(),
                state: sample.state,
            });
        }
    }
    result.conflicts.sort();
    result.conflicts.dedup();
    result
}

/// The root that holds `path`, the innermost when roots nest. Compares
/// whole path components: `/a/Drive2` is not inside `/a/Drive`.
fn root_containing<'a>(roots: &'a [RootEntry], path: &Path) -> Option<&'a RootEntry> {
    roots
        .iter()
        .filter(|root| path.starts_with(&root.path))
        .max_by_key(|root| Path::new(&root.path).components().count())
}

/// Stores `next`; true when it differs from what was there.
fn replace(current: &mut Snapshot, next: Snapshot) -> bool {
    if *current == next {
        return false;
    }
    *current = next;
    true
}

/// The conflict files under `info.root`, and each one's original when it
/// still exists, relative to the root.
fn scan_conflicts(info: &SyncInfo) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for conflict in conflicts::find_conflicts(&info.root, &info.matcher, info.naming)? {
        if let Some(original) = conflicts::original_path(&conflict, info.naming)
            && info.root.join(&original).exists()
        {
            files.push(original);
        }
        files.push(conflict);
    }
    Ok(files)
}

/// The scan of one sync and what it was read at. The tree is walked again
/// only when a round has ended since or the count changed, not on every
/// poll.
struct Scanned {
    pending: u32,
    finished: Option<Timestamp>,
    files: Vec<PathBuf>,
}

async fn conflicts_of(
    cache: &mut HashMap<String, Scanned>,
    info: &SyncInfo,
    state: &State,
) -> Vec<PathBuf> {
    if state.conflicts_pending == 0 {
        cache.remove(&info.id);
        return Vec::new();
    }
    let finished = state.last_run.as_ref().map(|run| run.finished);
    if let Some(scanned) = cache.get(&info.id)
        && scanned.pending == state.conflicts_pending
        && scanned.finished == finished
    {
        return scanned.files.clone();
    }
    let owned = info.clone();
    let files = match tokio::task::spawn_blocking(move || scan_conflicts(&owned)).await {
        Ok(Ok(files)) => files,
        Ok(Err(err)) => {
            log::warn!(
                "cannot look for conflicts in {}: {err}",
                info.root.display()
            );
            Vec::new()
        }
        Err(err) => {
            log::warn!("conflict scan failed: {err}");
            Vec::new()
        }
    };
    cache.insert(
        info.id.clone(),
        Scanned {
            pending: state.conflicts_pending,
            finished,
            files: files.clone(),
        },
    );
    files
}

struct Service {
    shared: Arc<Mutex<Snapshot>>,
    dirty: Arc<Mutex<Dirty>>,
    trigger: Arc<dyn Trigger>,
}

impl Service {
    fn read<T>(&self, pick: impl FnOnce(&Snapshot) -> T) -> T {
        pick(&self.shared.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The sync that holds `path`, `path` relative to its root, and the part
    /// of the change record that matters for that folder.
    fn view_of(&self, path: &Path) -> Option<(SyncView, PathBuf, BTreeMap<PathBuf, Timestamp>)> {
        let (view, rel) = self.read(|snapshot| {
            let root = root_containing(&snapshot.roots, path)?;
            let view = snapshot.syncs.iter().find(|view| view.id == root.id)?;
            let rel = path.strip_prefix(&view.root).ok()?.to_owned();
            Some((view.clone(), rel))
        })?;
        let dirty = self
            .dirty
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&view.id)
            .map(|record| {
                dirty_under(record, &rel)
                    .map(|(path, at)| (path.clone(), *at))
                    .collect()
            })
            .unwrap_or_default();
        Some((view, rel, dirty))
    }
}

#[zbus::interface(name = "io.github.raphael_soares.syncfast.Status")]
impl Service {
    /// The local folder and the state of each enabled sync.
    fn roots(&self) -> Vec<(String, String)> {
        self.read(|snapshot| {
            snapshot
                .roots
                .iter()
                .map(|root| (root.path.clone(), root.state.to_owned()))
                .collect()
        })
    }

    fn conflicts(&self) -> Vec<String> {
        self.read(|snapshot| snapshot.conflicts.clone())
    }

    /// Name, state and detail of each entry of the folder `path`, inside a
    /// sync. The state is `synced` (the detail says when, RFC 3339),
    /// `pending`, `syncing`, `error` (the detail is the message) or
    /// `conflict`. Entries the filters leave out are not listed; empty when
    /// `path` is not inside a sync or is left out itself.
    async fn directory(&self, path: String) -> Vec<(String, String, String)> {
        let dir = PathBuf::from(path);
        let Some((view, rel, dirty)) = self.view_of(&dir) else {
            return Vec::new();
        };
        if !rel.as_os_str().is_empty() && view.matcher.is_excluded(&rel) {
            return Vec::new();
        }
        tokio::task::spawn_blocking(move || list_directory(&view, &dirty, &dir, &rel))
            .await
            .unwrap_or_else(|err| {
                log::warn!("listing a folder failed: {err}");
                Vec::new()
            })
    }

    /// Starts a round of the sync whose folder holds `path`. False when no
    /// folder does, or the round could not be started.
    async fn sync_now(&self, path: String) -> bool {
        let Some(id) = self.read(|snapshot| {
            root_containing(&snapshot.roots, Path::new(&path)).map(|root| root.id.clone())
        }) else {
            return false;
        };
        match self.trigger.start(&id).await {
            Ok(()) => true,
            Err(err) => {
                log::warn!("{err}");
                false
            }
        }
    }

    #[zbus(signal)]
    async fn changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The running service. Dropping it takes the name off the bus.
#[derive(Debug)]
pub struct StatusHandle {
    syncs: watch::Sender<Arc<[SyncInfo]>>,
    waiting: watch::Sender<HashSet<String>>,
    dirty: Arc<Mutex<Dirty>>,
    task: JoinHandle<()>,
}

impl StatusHandle {
    /// The enabled syncs, after the config changed.
    pub fn set_syncs(&self, syncs: Vec<SyncInfo>) {
        self.syncs.send_replace(syncs.into());
    }

    /// `rel` changed in sync `id` now; empty when anything may have.
    pub fn note_change(&self, id: &str, rel: PathBuf) {
        let now = Timestamp::now();
        let mut dirty = self.dirty.lock().unwrap_or_else(PoisonError::into_inner);
        let record = dirty.entry(id.to_owned()).or_default();
        record.insert(rel, now);
        if record.len() > DIRTY_CAP {
            *record = BTreeMap::from([(PathBuf::new(), now)]);
        }
    }

    /// The syncs with a trigger on its way.
    pub fn set_waiting(&self, ids: HashSet<String>) {
        self.waiting.send_if_modified(|current| {
            let changed = *current != ids;
            if changed {
                *current = ids;
            }
            changed
        });
    }
}

impl Drop for StatusHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Publishes the status on `bus` and keeps it current. Without a bus it
/// logs a warning and publishes nothing.
#[must_use]
pub fn spawn(bus: Bus, paths: Paths, trigger: Arc<dyn Trigger>) -> StatusHandle {
    let (syncs, syncs_rx) = watch::channel(Arc::<[SyncInfo]>::from(Vec::new()));
    let (waiting, waiting_rx) = watch::channel(HashSet::new());
    let dirty = Arc::new(Mutex::new(Dirty::new()));
    let service = Service {
        shared: Arc::new(Mutex::new(Snapshot::default())),
        dirty: dirty.clone(),
        trigger,
    };
    let task = tokio::spawn(async move {
        if let Err(err) = serve(bus, paths, service, syncs_rx, waiting_rx).await {
            log::warn!("not publishing the sync status on the session bus: {err}");
        }
    });
    StatusHandle {
        syncs,
        waiting,
        dirty,
        task,
    }
}

/// Forgets the changes the last good round started after, so they were
/// synced; then what the snapshot keeps of the record.
fn prune_dirty(dirty: &mut Dirty, views: &mut [SyncView]) {
    dirty.retain(|id, _| views.iter().any(|view| view.id == *id));
    for view in views {
        let Some(record) = dirty.get_mut(&view.id) else {
            continue;
        };
        if let Some(base) = view.baseline {
            record.retain(|_, at| *at >= base.started);
        }
        view.dirty_len = record.len();
        view.dirty_newest = record.values().max().copied();
    }
}

async fn serve(
    bus: Bus,
    paths: Paths,
    service: Service,
    mut syncs: watch::Receiver<Arc<[SyncInfo]>>,
    mut waiting: watch::Receiver<HashSet<String>>,
) -> zbus::Result<()> {
    let shared = service.shared.clone();
    let dirty = service.dirty.clone();
    let builder = match bus {
        Bus::Session => Builder::session()?,
        Bus::Address(address) => Builder::address(address.as_str())?,
    };
    let connection = builder
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, service)?
        .build()
        .await?;
    let interface = connection
        .object_server()
        .interface::<_, Service>(OBJECT_PATH)
        .await?;
    let mut ticker = interval(POLL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut cache = HashMap::new();
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            changed = syncs.changed() => if changed.is_err() { return Ok(()); },
            changed = waiting.changed() => if changed.is_err() { return Ok(()); },
        }
        let infos = syncs.borrow_and_update().clone();
        let settling = waiting.borrow_and_update().clone();
        let mut samples = Vec::with_capacity(infos.len());
        let mut views = Vec::with_capacity(infos.len());
        for info in infos.iter() {
            let state = State::load_or_config_error(&paths, &info.id).await;
            let running = is_running(&paths, &info.id);
            let conflicts = conflicts_of(&mut cache, info, &state).await;
            views.push(SyncView {
                id: info.id.clone(),
                root: info.root.clone(),
                matcher: info.matcher.clone(),
                baseline: baseline(&state),
                running,
                errors: state
                    .last_run
                    .iter()
                    .flat_map(|run| &run.summary.errors)
                    .map(|error| (PathBuf::from(&error.path), error.message.clone()))
                    .collect(),
                conflicts: conflicts.iter().cloned().collect(),
                dirty_len: 0,
                dirty_newest: None,
            });
            samples.push(Sample {
                id: info.id.clone(),
                root: info.root.clone(),
                state: root_state(&state, running, settling.contains(&info.id)),
                conflicts,
            });
        }
        cache.retain(|id, _| infos.iter().any(|info| info.id == *id));
        prune_dirty(
            &mut dirty.lock().unwrap_or_else(PoisonError::into_inner),
            &mut views,
        );
        let mut next = snapshot(samples);
        next.syncs = views;
        let changed = replace(
            &mut shared.lock().unwrap_or_else(PoisonError::into_inner),
            next,
        );
        if changed && let Err(err) = Service::changed(interface.signal_emitter()).await {
            log::warn!("cannot signal a status change: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use jiff::Timestamp;

    use super::*;
    use crate::state::{Gate, GateKind};

    fn gate() -> Gate {
        Gate {
            kind: GateKind::ConfigError,
            since: Timestamp::UNIX_EPOCH,
            detail: String::new(),
            fault: None,
            auto_checked: None,
        }
    }

    fn state(gate_set: bool, conflicts: u32) -> State {
        State {
            gate: gate_set.then(gate),
            conflicts_pending: conflicts,
            ..State::default()
        }
    }

    #[test]
    fn root_state_follows_the_precedence_gate_conflict_running_settling_synced() {
        let everything = state(true, 2);
        let conflicted = state(false, 2);
        let clean = state(false, 0);

        assert_eq!(root_state(&everything, true, true), "attention");
        assert_eq!(root_state(&conflicted, true, true), "conflict");
        assert_eq!(root_state(&clean, true, true), "syncing");
        assert_eq!(root_state(&clean, false, true), "pending");
        assert_eq!(root_state(&clean, false, false), "synced");
    }

    fn entry(id: &str, path: &str) -> RootEntry {
        RootEntry {
            id: id.to_owned(),
            path: path.to_owned(),
            state: "synced",
        }
    }

    fn found<'a>(roots: &'a [RootEntry], path: &str) -> Option<&'a str> {
        root_containing(roots, Path::new(path)).map(|root| root.id.as_str())
    }

    #[test]
    fn a_path_belongs_to_the_root_that_holds_it_by_whole_components() {
        let roots = [
            entry("drive", "/home/u/Drive"),
            entry("udesc", "/home/u/Udesc/"),
        ];

        assert_eq!(found(&roots, "/home/u/Drive"), Some("drive"));
        assert_eq!(found(&roots, "/home/u/Drive/notes/a.txt"), Some("drive"));
        assert_eq!(found(&roots, "/home/u/Udesc/x"), Some("udesc"));
        assert_eq!(found(&roots, "/home/u/Drive2"), None);
        assert_eq!(found(&roots, "/home/u/Drive2/a.txt"), None);
        assert_eq!(found(&roots, "/home/u"), None);
        assert_eq!(found(&roots, "/elsewhere/Drive/a"), None);
        assert_eq!(found(&roots, "Drive/a"), None);
    }

    #[test]
    fn nested_roots_answer_with_the_innermost() {
        let roots = [entry("outer", "/s"), entry("inner", "/s/work")];

        assert_eq!(found(&roots, "/s/work/a"), Some("inner"));
        assert_eq!(found(&roots, "/s/other/a"), Some("outer"));
    }

    fn sample(id: &str, root: &str, state: &'static str, conflicts: &[&str]) -> Sample {
        Sample {
            id: id.to_owned(),
            root: PathBuf::from(root),
            state,
            conflicts: conflicts.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn snapshot_lists_roots_in_order_and_conflicts_absolute_sorted_and_unique() {
        let built = snapshot(vec![
            sample("b", "/b", "pending", &["z.txt.conflict1"]),
            sample(
                "a",
                "/a",
                "conflict",
                &["x/y.txt", "x/y.txt.conflict1", "x/y.txt"],
            ),
        ]);

        let roots: Vec<_> = built.roots.iter().map(|r| (&*r.path, r.state)).collect();
        assert_eq!(roots, [("/b", "pending"), ("/a", "conflict")]);
        assert_eq!(
            built.conflicts,
            ["/a/x/y.txt", "/a/x/y.txt.conflict1", "/b/z.txt.conflict1"]
        );
    }

    #[test]
    fn a_change_is_reported_once_and_an_identical_snapshot_is_not_one() {
        let mut current = Snapshot::default();
        let first = || snapshot(vec![sample("a", "/a", "synced", &[])]);

        let from_empty = replace(&mut current, first());
        let same = replace(&mut current, first());
        let new_state = replace(
            &mut current,
            snapshot(vec![sample("a", "/a", "syncing", &[])]),
        );
        let new_conflict = replace(
            &mut current,
            snapshot(vec![sample("a", "/a", "syncing", &["f.conflict1"])]),
        );

        assert_eq!(
            [from_empty, same, new_state, new_conflict],
            [true, false, true, true]
        );
    }

    fn info(root: &Path) -> SyncInfo {
        SyncInfo::new(
            "docs".to_owned(),
            root.to_owned(),
            Naming::Num,
            DirMatcher::new("- skipped/\n"),
        )
    }

    #[test]
    fn scan_returns_the_conflict_files_and_the_originals_that_exist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(root.join("skipped")).unwrap();
        fs::write(root.join("a.txt"), "x").unwrap();
        fs::write(root.join("a.txt.conflict1"), "x").unwrap();
        fs::write(root.join("sub/gone.txt.conflict1"), "x").unwrap();
        fs::write(root.join("skipped/b.txt.conflict1"), "x").unwrap();
        fs::write(root.join("plain.txt"), "x").unwrap();

        let mut files = scan_conflicts(&info(root)).unwrap();
        files.sort();

        assert_eq!(
            files,
            [
                PathBuf::from("a.txt"),
                PathBuf::from("a.txt.conflict1"),
                PathBuf::from("sub/gone.txt.conflict1"),
            ]
        );
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn run(started: &str, finished: &str) -> crate::state::LastRun {
        crate::state::LastRun {
            started: at(started),
            finished: at(finished),
            exit_code: 0,
            summary: crate::bisync_log::RunSummary::default(),
            log_file: PathBuf::new(),
            auto_reviewed: None,
        }
    }

    #[test]
    fn the_baseline_is_the_last_good_round_or_nothing() {
        let good = State {
            last_success: Some(at("2026-01-01T10:05:00Z")),
            last_run: Some(run("2026-01-01T10:00:00Z", "2026-01-01T10:05:00Z")),
            ..State::default()
        };
        let failed_after = State {
            last_success: Some(at("2026-01-01T09:00:00Z")),
            last_run: Some(run("2026-01-01T10:00:00Z", "2026-01-01T10:05:00Z")),
            ..State::default()
        };

        assert_eq!(
            baseline(&good),
            Some(Baseline {
                started: at("2026-01-01T10:00:00Z"),
                finished: at("2026-01-01T10:05:00Z"),
            })
        );
        assert_eq!(
            baseline(&failed_after),
            Some(Baseline {
                started: at("2026-01-01T09:00:00Z"),
                finished: at("2026-01-01T09:00:00Z"),
            })
        );
        assert_eq!(baseline(&State::default()), None);
    }

    const STARTED: &str = "2026-01-01T10:00:00Z";
    const DURING: &str = "2026-01-01T10:02:00Z";
    const FINISHED: &str = "2026-01-01T10:05:00Z";
    const AFTER: &str = "2026-01-01T10:09:00Z";
    const BEFORE: &str = "2026-01-01T09:00:00Z";

    fn view() -> SyncView {
        SyncView {
            id: "docs".to_owned(),
            root: PathBuf::from("/r"),
            matcher: DirMatcher::new(""),
            baseline: Some(Baseline {
                started: at(STARTED),
                finished: at(FINISHED),
            }),
            running: false,
            errors: BTreeMap::new(),
            conflicts: HashSet::new(),
            dirty_len: 0,
            dirty_newest: None,
        }
    }

    fn record(paths: &[&str]) -> BTreeMap<PathBuf, Timestamp> {
        paths
            .iter()
            .map(|path| (PathBuf::from(path), at(DURING)))
            .collect()
    }

    fn file(view: &SyncView, dirty: &[&str], rel: &str, ctime: &str) -> (&'static str, String) {
        entry_state(view, &record(dirty), Path::new(rel), false, Some(at(ctime)))
    }

    fn folder(view: &SyncView, dirty: &[&str], rel: &str) -> &'static str {
        entry_state(view, &record(dirty), Path::new(rel), true, Some(at(BEFORE))).0
    }

    #[test]
    fn an_untouched_file_is_synced_as_of_the_last_good_round() {
        assert_eq!(
            file(&view(), &[], "a.txt", BEFORE),
            ("synced", "2026-01-01T10:05:00Z".to_owned())
        );
    }

    #[test]
    fn a_file_the_round_downloaded_is_synced() {
        assert_eq!(file(&view(), &[], "a.txt", DURING).0, "synced");
    }

    #[test]
    fn a_file_changed_after_the_round_is_pending_even_unseen_by_the_watcher() {
        assert_eq!(
            file(&view(), &[], "a.txt", AFTER),
            ("pending", String::new())
        );
    }

    #[test]
    fn a_file_changed_during_the_round_is_pending_by_the_record() {
        assert_eq!(file(&view(), &["a.txt"], "a.txt", DURING).0, "pending");
        assert_eq!(file(&view(), &["b.txt"], "a.txt", DURING).0, "synced");
    }

    #[test]
    fn a_folder_is_pending_when_anything_below_it_changed() {
        let dirty = ["x/y/z.txt"];

        assert_eq!(folder(&view(), &dirty, "x"), "pending");
        assert_eq!(folder(&view(), &dirty, "x/y"), "pending");
        assert_eq!(folder(&view(), &dirty, "x/w"), "synced");
        assert_eq!(folder(&view(), &dirty, "x.d"), "synced");
        assert_eq!(file(&view(), &dirty, "x", BEFORE).0, "synced");
    }

    #[test]
    fn a_whole_sync_change_leaves_every_entry_pending() {
        assert_eq!(file(&view(), &[""], "a.txt", BEFORE).0, "pending");
        assert_eq!(folder(&view(), &[""], "x"), "pending");
    }

    #[test]
    fn a_sync_that_never_succeeded_is_all_pending() {
        let never = SyncView {
            baseline: None,
            ..view()
        };

        assert_eq!(file(&never, &[], "a.txt", BEFORE).0, "pending");
    }

    #[test]
    fn a_running_round_turns_pending_into_syncing() {
        let running = SyncView {
            running: true,
            ..view()
        };

        assert_eq!(file(&running, &[], "a.txt", AFTER).0, "syncing");
        assert_eq!(file(&running, &[], "b.txt", BEFORE).0, "synced");
    }

    #[test]
    fn an_error_beats_pending_and_a_folder_shows_the_first_error_below_it() {
        let failing = SyncView {
            errors: BTreeMap::from([
                (PathBuf::from("d/b.txt"), "second".to_owned()),
                (PathBuf::from("d/a.txt"), "first".to_owned()),
            ]),
            ..view()
        };

        assert_eq!(
            file(&failing, &["d/a.txt"], "d/a.txt", AFTER),
            ("error", "first".to_owned())
        );
        assert_eq!(
            entry_state(&failing, &record(&[]), Path::new("d"), true, None),
            ("error", "first".to_owned())
        );
        assert_eq!(folder(&failing, &[], "e"), "synced");
    }

    #[test]
    fn a_conflict_beats_an_error() {
        let both = SyncView {
            errors: BTreeMap::from([(PathBuf::from("a.txt"), "denied".to_owned())]),
            conflicts: HashSet::from([PathBuf::from("a.txt")]),
            ..view()
        };

        assert_eq!(
            file(&both, &[], "a.txt", BEFORE),
            ("conflict", String::new())
        );
    }

    #[test]
    fn the_record_forgets_what_the_last_good_round_started_after() {
        let mut dirty = Dirty::from([
            (
                "docs".to_owned(),
                BTreeMap::from([
                    (PathBuf::from("old.txt"), at(BEFORE)),
                    (PathBuf::from("new.txt"), at(DURING)),
                ]),
            ),
            ("gone".to_owned(), record(&["a"])),
        ]);
        let mut views = [view()];

        prune_dirty(&mut dirty, &mut views);

        assert_eq!(dirty.keys().collect::<Vec<_>>(), ["docs"]);
        assert_eq!(
            dirty["docs"].keys().collect::<Vec<_>>(),
            [Path::new("new.txt")]
        );
        assert_eq!(
            (views[0].dirty_len, views[0].dirty_newest),
            (1, Some(at(DURING)))
        );
    }
}
