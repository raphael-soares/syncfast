//! What the backend reads from disk on each poll: the config, the state of
//! every sync, whether a round holds its lock, and the live numbers of a
//! round that is running.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use syncfast_core::behavior::Behavior;
use syncfast_core::bisync_log::{Progress, last_stats, parse_log};
use syncfast_core::config::{Config, Pair};
use syncfast_core::paths::Paths;
use syncfast_core::settings::{Effective, Overrides};
use syncfast_core::state::{DisplayStatus, Gate, GateKind, State, display_status, is_running};
use syncfast_core::watch::limited_pairs;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

/// How much of the end of a running round's log is read for its stats.
const LOG_TAIL_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct PairSnapshot {
    pub pair: Pair,
    pub state: State,
    pub running: bool,
    pub status: DisplayStatus,
    pub progress: Option<Progress>,
    /// The watcher ran out of inotify watches for this sync.
    pub realtime_off: bool,
    /// What the sync runs with and where each value comes from.
    pub effective: Effective,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// The config has a global problem: no sync can be listed.
    pub config_error: Option<String>,
    pub pairs: Vec<PairSnapshot>,
    /// The built-in values with `[defaults]` on top.
    pub defaults: Behavior,
    /// The keys `[defaults]` sets.
    pub defaults_overrides: Overrides,
}

impl Snapshot {
    #[must_use]
    pub fn any_running(&self) -> bool {
        self.pairs.iter().any(|pair| pair.running)
    }
}

pub async fn load(paths: &Paths) -> Snapshot {
    let config = match Config::load(paths).await {
        Ok(config) => config,
        Err(err) => {
            return Snapshot {
                config_error: Some(err.to_string()),
                pairs: Vec::new(),
                ..Snapshot::default()
            };
        }
    };
    let limited = limited_pairs(paths);
    let mut pairs = Vec::with_capacity(config.pairs().len());
    for pair in config.pairs() {
        let realtime_off = limited.contains(&pair.id);
        pairs.push(load_pair(paths, &config, pair, realtime_off).await);
    }
    Snapshot {
        config_error: None,
        pairs,
        defaults: config.defaults(),
        defaults_overrides: config.defaults_overrides().clone(),
    }
}

async fn load_pair(
    paths: &Paths,
    config: &Config,
    pair: &Pair,
    realtime_off: bool,
) -> PairSnapshot {
    let state = match State::load(paths, &pair.id).await {
        Ok(state) => state,
        Err(err) => {
            log::warn!("{}: {err}", pair.id);
            unreadable_state(&err.to_string())
        }
    };
    let running = is_running(paths, &pair.id);
    let progress = if running {
        running_progress(&paths.runs_dir(&pair.id)).await
    } else {
        None
    };
    PairSnapshot {
        status: display_status(
            pair,
            &state,
            running,
            config.defaults().notifications.failing_after(),
        ),
        pair: pair.clone(),
        state,
        running,
        progress,
        realtime_off,
        effective: config.effective(pair),
    }
}

/// A state file that cannot be read shows as a configuration error rather
/// than as a healthy sync.
fn unreadable_state(detail: &str) -> State {
    State {
        gate: Some(Gate {
            kind: GateKind::ConfigError,
            // Fixed, so the same broken file gives the same snapshot on every
            // poll and the UI is not woken for nothing.
            since: Timestamp::UNIX_EPOCH,
            detail: detail.to_owned(),
            fault: None,
            auto_checked: None,
        }),
        ..State::default()
    }
}

/// The newest `--stats` line of the log of the round in progress: the
/// newest file of `runs/`, since names are timestamps.
async fn running_progress(runs: &Path) -> Option<Progress> {
    let log = newest_log(runs).await?;
    let text = tail(&log, LOG_TAIL_BYTES).await.ok()?;
    last_stats(&parse_log(&text))
}

async fn newest_log(runs: &Path) -> Option<PathBuf> {
    let mut entries = tokio::fs::read_dir(runs).await.ok()?;
    let mut newest: Option<PathBuf> = None;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if newest
            .as_ref()
            .is_none_or(|best| path.file_name() > best.file_name())
        {
            newest = Some(path);
        }
    }
    newest
}

/// The last `limit` bytes of a file as text, starting at a line boundary.
async fn tail(path: &Path, limit: u64) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let length = file.metadata().await?.len();
    let start = length.saturating_sub(limit);
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start == 0 {
        return Ok(text);
    }
    Ok(text
        .split_once('\n')
        .map_or_else(String::new, |(_, rest)| rest.to_owned()))
}

#[cfg(test)]
mod tests {
    use syncfast_core::state::LastOutcome;

    use super::*;

    fn paths(root: &Path) -> Paths {
        Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        )
    }

    fn write_config(paths: &Paths, text: &str) {
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    const NOTES: &str = "version = 1\n\n[[pair]]\nid = \"notes\"\nname = \"Notes\"\nlocal = \"/data/notes\"\nremote = \"r:notes\"\n";

    #[tokio::test]
    async fn without_a_config_file_there_are_no_syncs_and_no_error() {
        let dir = tempfile::tempdir().unwrap();

        let snapshot = load(&paths(dir.path())).await;

        assert_eq!(snapshot, Snapshot::default());
    }

    #[tokio::test]
    async fn config_with_a_global_error_reports_it_and_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, "version = 4\n");

        let snapshot = load(&paths).await;

        assert_eq!(
            snapshot.config_error.as_deref(),
            Some("unsupported config version 4")
        );
        assert!(snapshot.pairs.is_empty());
    }

    #[tokio::test]
    async fn a_sync_named_in_the_watch_limit_file_has_real_time_sync_off() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, NOTES);
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        std::fs::write(paths.watch_limit_file(), r#"{"pairs":["notes"]}"#).unwrap();

        let limited = load(&paths).await;
        std::fs::remove_file(paths.watch_limit_file()).unwrap();
        let watched = load(&paths).await;

        assert!(limited.pairs[0].realtime_off);
        assert!(!watched.pairs[0].realtime_off);
    }

    #[tokio::test]
    async fn sync_with_a_clean_state_shows_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, NOTES);
        State {
            last_outcome: Some(LastOutcome::Ok),
            ..State::default()
        }
        .save(&paths, "notes")
        .await
        .unwrap();

        let snapshot = load(&paths).await;

        assert_eq!(snapshot.pairs[0].status, DisplayStatus::UpToDate);
        assert!(!snapshot.any_running());
    }

    #[tokio::test]
    async fn unreadable_state_file_shows_the_sync_as_needing_attention() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, NOTES);
        let state_file = paths.state_file("notes");
        std::fs::create_dir_all(state_file.parent().unwrap()).unwrap();
        std::fs::write(state_file, "{ broken").unwrap();

        let snapshot = load(&paths).await;

        assert_eq!(snapshot.pairs[0].status, DisplayStatus::Error);
    }

    #[tokio::test]
    async fn the_same_unreadable_state_file_gives_the_same_snapshot_on_every_poll() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, NOTES);
        let state_file = paths.state_file("notes");
        std::fs::create_dir_all(state_file.parent().unwrap()).unwrap();
        std::fs::write(state_file, "{ broken").unwrap();

        let first = load(&paths).await;
        let second = load(&paths).await;

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn running_sync_carries_the_newest_stats_line_of_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, NOTES);
        let runs = paths.runs_dir("notes");
        std::fs::create_dir_all(&runs).unwrap();
        let stats = |transfers: u64| {
            format!(
                "{{\"level\":\"notice\",\"msg\":\"\",\"stats\":{{\"bytes\":10,\"totalBytes\":100,\"transfers\":{transfers},\"totalTransfers\":4,\"speed\":2.5,\"transferring\":[{{\"name\":\"a.txt\"}}]}}}}\n"
            )
        };
        std::fs::write(runs.join("20260101-100000.jsonl"), stats(1)).unwrap();
        std::fs::write(
            runs.join("20260101-110000.jsonl"),
            format!("{}{}", stats(2), stats(3)),
        )
        .unwrap();
        let _round = syncfast_core::state::try_lock_pair(&paths, "notes")
            .unwrap()
            .unwrap();

        let snapshot = load(&paths).await;

        let pair = &snapshot.pairs[0];
        assert_eq!(pair.status, DisplayStatus::Syncing);
        let progress = pair.progress.as_ref().unwrap();
        assert_eq!(progress.transfers, 3);
        assert_eq!(progress.total_transfers, 4);
    }

    #[tokio::test]
    async fn tail_starts_at_a_line_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("log");
        std::fs::write(&file, "first line\nsecond\nthird\n").unwrap();

        let text = tail(&file, 8).await.unwrap();

        assert_eq!(text, "third\n");
    }
}
