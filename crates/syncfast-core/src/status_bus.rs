//! The status service `syncfast watch` publishes on the session bus, for a
//! file manager extension: which folders are sync roots, their state, and
//! which files are conflicted. The extension lives outside the sandbox and
//! never reads syncfast's files.

use std::collections::{HashMap, HashSet};
use std::io;
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

/// What `Roots` and `Conflicts` answer.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Snapshot {
    roots: Vec<RootEntry>,
    conflicts: Vec<String>,
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
    trigger: Arc<dyn Trigger>,
}

impl Service {
    fn read<T>(&self, pick: impl FnOnce(&Snapshot) -> T) -> T {
        pick(&self.shared.lock().unwrap_or_else(PoisonError::into_inner))
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
    task: JoinHandle<()>,
}

impl StatusHandle {
    /// The enabled syncs, after the config changed.
    pub fn set_syncs(&self, syncs: Vec<SyncInfo>) {
        self.syncs.send_replace(syncs.into());
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
    let task = tokio::spawn(async move {
        if let Err(err) = serve(bus, paths, trigger, syncs_rx, waiting_rx).await {
            log::warn!("not publishing the sync status on the session bus: {err}");
        }
    });
    StatusHandle {
        syncs,
        waiting,
        task,
    }
}

async fn serve(
    bus: Bus,
    paths: Paths,
    trigger: Arc<dyn Trigger>,
    mut syncs: watch::Receiver<Arc<[SyncInfo]>>,
    mut waiting: watch::Receiver<HashSet<String>>,
) -> zbus::Result<()> {
    let shared = Arc::new(Mutex::new(Snapshot::default()));
    let builder = match bus {
        Bus::Session => Builder::session()?,
        Bus::Address(address) => Builder::address(address.as_str())?,
    };
    let connection = builder
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            Service {
                shared: shared.clone(),
                trigger,
            },
        )?
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
        for info in infos.iter() {
            let state = State::load_or_config_error(&paths, &info.id).await;
            let running = is_running(&paths, &info.id);
            samples.push(Sample {
                id: info.id.clone(),
                root: info.root.clone(),
                state: root_state(&state, running, settling.contains(&info.id)),
                conflicts: conflicts_of(&mut cache, info, &state).await,
            });
        }
        cache.retain(|id, _| infos.iter().any(|info| info.id == *id));
        let changed = replace(
            &mut shared.lock().unwrap_or_else(PoisonError::into_inner),
            snapshot(samples),
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
}
