//! Per-pair state under `$XDG_STATE_HOME/syncfast/pairs/<id>/`, the review
//! marker, the `runs/` history and the status shown to the user.
//!
//! "Running" and "paused" are never stored: a round is running while its
//! lock file is held, and a pair is paused when the config says
//! `enabled = false`.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::bisync::{Fault, ReviewKind};
use crate::bisync_log::RunSummary;
use crate::config::Pair;
use crate::paths::Paths;

/// How many rounds keep their log in `runs/`.
pub const RUNS_KEPT: usize = 50;
pub const PREVIEWS_KEPT: Duration = Duration::from_hours(24);
/// Key in [`State::notified`] for the "sync failing" notification.
pub const NOTIFIED_FAILURES: &str = "failures";
/// Failed rounds in a row before the status turns into [`DisplayStatus::Failing`].
const FAILING_AFTER: u32 = 3;
const LOCK_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("cannot read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is not valid: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    NeedsReviewFirstRun,
    NeedsReviewFiltersChanged,
    NeedsReviewCriticalError,
    TooManyDeletes,
    AllFilesChanged,
    ConfigError,
    AuthError,
    LocalMissing,
    /// A failure the user can act on (see [`crate::bisync::Fault`]). The
    /// only gate that does not stop rounds: the next one may succeed.
    NeedsAttention,
}

impl GateKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsReviewFirstRun => "needs_review_first_run",
            Self::NeedsReviewFiltersChanged => "needs_review_filters_changed",
            Self::NeedsReviewCriticalError => "needs_review_critical_error",
            Self::TooManyDeletes => "too_many_deletes",
            Self::AllFilesChanged => "all_files_changed",
            Self::ConfigError => "config_error",
            Self::AuthError => "auth_error",
            Self::LocalMissing => "local_missing",
            Self::NeedsAttention => "needs_attention",
        }
    }

    #[must_use]
    pub fn is_needs_review(self) -> bool {
        matches!(
            self,
            Self::NeedsReviewFirstRun
                | Self::NeedsReviewFiltersChanged
                | Self::NeedsReviewCriticalError
        )
    }

    /// Gates that only a confirmed `--force` round clears.
    #[must_use]
    pub fn is_force_review(self) -> bool {
        matches!(self, Self::TooManyDeletes | Self::AllFilesChanged)
    }

    /// Key in [`State::notified`] that records the gate was announced.
    #[must_use]
    pub fn notified_key(self) -> String {
        format!("gate:{}", self.as_str())
    }
}

/// Why a sync needs the user. Every gate but [`GateKind::NeedsAttention`]
/// blocks normal rounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub kind: GateKind,
    pub since: Timestamp,
    pub detail: String,
    /// The fault a `needs_attention` gate names; absent from older files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault: Option<Fault>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LastOutcome {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastRun {
    pub started: Timestamp,
    pub finished: Timestamp,
    pub exit_code: i32,
    pub summary: RunSummary,
    pub log_file: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub gate: Option<Gate>,
    pub last_outcome: Option<LastOutcome>,
    /// The last attempt was skipped for lack of network. Changes neither
    /// `gate` nor `last_outcome`.
    pub offline: bool,
    pub consecutive_failures: u32,
    /// When the current series of failed rounds began; cleared by a
    /// success.
    pub failing_since: Option<Timestamp>,
    pub last_success: Option<Timestamp>,
    pub last_attempt: Option<Timestamp>,
    pub applied_filters_hash: Option<String>,
    pub conflicts_pending: u32,
    pub last_run: Option<LastRun>,
    pub notified: Vec<String>,
}

impl State {
    /// A pair that has never synced: waiting for its first review.
    #[must_use]
    pub fn first_run(now: Timestamp) -> Self {
        Self {
            gate: Some(Gate {
                kind: GateKind::NeedsReviewFirstRun,
                since: now,
                detail: String::new(),
                fault: None,
            }),
            ..Self::default()
        }
    }

    /// Reads the pair's state. An absent file is [`State::first_run`].
    ///
    /// # Errors
    /// The file cannot be read or is not valid state JSON.
    pub async fn load(paths: &Paths, pair_id: &str) -> Result<Self, StateError> {
        let path = paths.state_file(pair_id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|source| StateError::Parse { path, source })
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::first_run(Timestamp::now()))
            }
            Err(source) => Err(StateError::Read { path, source }),
        }
    }

    /// As [`State::load`], but a file that cannot be read becomes a
    /// `config_error` gate carrying the error, so one broken file shows on
    /// its own sync instead of failing every listing.
    pub async fn load_or_config_error(paths: &Paths, pair_id: &str) -> Self {
        match Self::load(paths, pair_id).await {
            Ok(state) => state,
            Err(err) => Self {
                gate: Some(Gate {
                    kind: GateKind::ConfigError,
                    since: Timestamp::UNIX_EPOCH,
                    detail: err.to_string(),
                    fault: None,
                }),
                ..Self::default()
            },
        }
    }

    /// # Errors
    /// The file cannot be written.
    pub async fn save(&self, paths: &Paths, pair_id: &str) -> Result<(), StateError> {
        write_json(paths.state_file(pair_id), self).await
    }
}

/// What the user confirmed in the review. The GUI writes it; the next run
/// consumes it, but only while `filters_hash` still matches the filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmMarker {
    pub kind: ReviewKind,
    pub filters_hash: String,
}

impl ConfirmMarker {
    /// A marker that is not valid JSON counts as no marker: it is logged
    /// and deleted, so a bad file never blocks the pair.
    ///
    /// # Errors
    /// The file exists but cannot be read or deleted.
    pub async fn load(paths: &Paths, pair_id: &str) -> Result<Option<Self>, StateError> {
        let path = paths.confirm_file(pair_id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(marker) => Ok(Some(marker)),
                Err(err) => {
                    log::warn!("ignoring {}: {err}", path.display());
                    Self::remove(paths, pair_id).await?;
                    Ok(None)
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StateError::Read { path, source }),
        }
    }

    /// # Errors
    /// The file cannot be written.
    pub async fn save(&self, paths: &Paths, pair_id: &str) -> Result<(), StateError> {
        write_json(paths.confirm_file(pair_id), self).await
    }

    /// Deletes the marker; an absent one is fine.
    ///
    /// # Errors
    /// The file exists but cannot be deleted.
    pub async fn remove(paths: &Paths, pair_id: &str) -> Result<(), StateError> {
        let path = paths.confirm_file(pair_id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StateError::Write { path, source }),
        }
    }
}

pub(crate) async fn write_json(path: PathBuf, value: &impl Serialize) -> Result<(), StateError> {
    let mut json = serde_json::to_vec_pretty(value).map_err(|source| StateError::Parse {
        path: path.clone(),
        source,
    })?;
    json.push(b'\n');
    let target = path.clone();
    tokio::task::spawn_blocking(move || fastframe_file::write(&target, json))
        .await
        .unwrap_or_else(|join| Err(std::io::Error::other(join)))
        .map_err(|source| StateError::Write { path, source })
}

/// True while a round of `pair_id` holds its lock. Never blocks.
#[must_use]
pub fn is_running(paths: &Paths, pair_id: &str) -> bool {
    is_locked(&paths.lock_file(pair_id))
}

/// True while some process holds the lock file `path`. Never blocks.
#[must_use]
pub(crate) fn is_locked(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

/// Takes the pair's lock for a round or a preview, creating the runtime
/// folder and the lock file. `None` when another round holds it. The lock
/// is released when the returned file is dropped.
///
/// # Errors
/// The lock file cannot be created or locked.
pub fn try_lock_pair(paths: &Paths, pair_id: &str) -> Result<Option<File>, StateError> {
    try_lock(paths, paths.lock_file(pair_id))
}

/// [`try_lock_pair`], polled until the lock is free or `timeout` passes
/// (`None`).
///
/// # Errors
/// The lock file cannot be created or locked.
pub async fn lock_pair(
    paths: &Paths,
    pair_id: &str,
    timeout: Duration,
) -> Result<Option<File>, StateError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(lock) = try_lock_pair(paths, pair_id)? {
            return Ok(Some(lock));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(LOCK_POLL).await;
    }
}

/// [`try_lock_pair`] for any lock file in the runtime folder.
pub(crate) fn try_lock(paths: &Paths, path: PathBuf) -> Result<Option<File>, StateError> {
    let open = || -> std::io::Result<Option<File>> {
        std::fs::create_dir_all(paths.runtime_subdir())?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(err)) => Err(err),
        }
    };
    open().map_err(|source| StateError::Write { path, source })
}

/// Keeps the [`RUNS_KEPT`] newest `.jsonl` logs in the pair's `runs/`
/// folder (names are timestamps, so the highest names are the newest) and
/// deletes the rest, then the preview logs older than [`PREVIEWS_KEPT`]. A
/// log that cannot be deleted is a warning, not a stop. A missing folder is
/// fine.
///
/// # Errors
/// The runs folder cannot be listed.
pub async fn prune_runs(paths: &Paths, pair_id: &str) -> Result<(), StateError> {
    let dir = paths.runs_dir(pair_id);
    let mut names: Vec<OsString> = file_names(&dir)
        .await?
        .into_iter()
        .filter(|name| is_jsonl(name))
        .collect();
    names.sort();
    let excess = names.len().saturating_sub(RUNS_KEPT);
    for name in names.into_iter().take(excess) {
        remove_or_warn(&dir.join(name)).await;
    }
    prune_previews(&paths.pair_state_dir(pair_id)).await;
    Ok(())
}

fn is_jsonl(name: &OsStr) -> bool {
    Path::new(name).extension() == Some(OsStr::new("jsonl"))
}

async fn file_names(dir: &Path) -> Result<Vec<OsString>, StateError> {
    let read = |source| StateError::Read {
        path: dir.to_path_buf(),
        source,
    };
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(read(source)),
    };
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(read)? {
        names.push(entry.file_name());
    }
    Ok(names)
}

async fn prune_previews(dir: &Path) {
    let names = match file_names(dir).await {
        Ok(names) => names,
        Err(err) => {
            log::warn!("{err}");
            return;
        }
    };
    for name in names {
        let text = name.to_string_lossy();
        if !text.starts_with("preview-") || !is_jsonl(&name) {
            continue;
        }
        let path = dir.join(&name);
        let age = tokio::fs::metadata(&path)
            .await
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok());
        if age.is_some_and(|age| age > PREVIEWS_KEPT) {
            remove_or_warn(&path).await;
        }
    }
}

async fn remove_or_warn(path: &Path) {
    if let Err(err) = tokio::fs::remove_file(path).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("cannot delete {}: {err}", path.display());
    }
}

/// Name of the theme colour token that paints a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Danger,
    Warning,
    Dim,
    Secondary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStatus {
    Syncing,
    Error,
    NeedsReview,
    /// The number of unresolved conflicts.
    Conflicts(u32),
    Paused,
    Offline,
    Failing,
    Retrying,
    UpToDate,
}

impl DisplayStatus {
    #[must_use]
    pub fn tone(self) -> Tone {
        match self {
            Self::Syncing => Tone::Accent,
            Self::Error | Self::Failing => Tone::Danger,
            Self::NeedsReview | Self::Conflicts(_) => Tone::Warning,
            Self::Paused | Self::Offline => Tone::Dim,
            Self::Retrying | Self::UpToDate => Tone::Secondary,
        }
    }

    /// Lucide icon name. Colour is never the only signal.
    #[must_use]
    pub fn icon(self) -> &'static str {
        match self {
            Self::Syncing => "refresh-cw",
            Self::Error | Self::Failing => "circle-alert",
            Self::NeedsReview => "eye",
            Self::Conflicts(_) => "copy",
            Self::Paused => "pause",
            Self::Offline => "cloud-off",
            Self::Retrying => "clock",
            Self::UpToDate => "check",
        }
    }

    #[must_use]
    pub fn text(self) -> String {
        match self {
            Self::Syncing => "Syncing…".to_owned(),
            Self::Error => "Needs attention".to_owned(),
            Self::NeedsReview => "Review needed".to_owned(),
            Self::Conflicts(count) => format!("{count} conflicts"),
            Self::Paused => "Paused".to_owned(),
            Self::Offline => "Offline".to_owned(),
            Self::Failing => "Sync failing".to_owned(),
            Self::Retrying => "Will retry".to_owned(),
            Self::UpToDate => "Up to date".to_owned(),
        }
    }

    /// The name `syncfast status` prints.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Syncing => "Syncing",
            Self::Error => "Error",
            Self::NeedsReview => "NeedsReview",
            Self::Conflicts(_) => "Conflicts",
            Self::Paused => "Paused",
            Self::Offline => "Offline",
            Self::Failing => "Failing",
            Self::Retrying => "Retrying",
            Self::UpToDate => "UpToDate",
        }
    }
}

/// The one precedence used by the sidebar, the tray and `syncfast status`:
/// the first rule that matches wins.
#[must_use]
pub fn display_status(pair: &Pair, state: &State, running: bool) -> DisplayStatus {
    let gate = state.gate.as_ref().map(|gate| gate.kind);
    if running {
        DisplayStatus::Syncing
    } else if matches!(
        gate,
        Some(
            GateKind::ConfigError
                | GateKind::AuthError
                | GateKind::LocalMissing
                | GateKind::NeedsAttention
        )
    ) {
        DisplayStatus::Error
    } else if gate.is_some() {
        DisplayStatus::NeedsReview
    } else if state.conflicts_pending > 0 {
        DisplayStatus::Conflicts(state.conflicts_pending)
    } else if !pair.enabled {
        DisplayStatus::Paused
    } else if state.offline {
        DisplayStatus::Offline
    } else if state.last_outcome == Some(LastOutcome::Error) {
        if state.consecutive_failures >= FAILING_AFTER {
            DisplayStatus::Failing
        } else {
            DisplayStatus::Retrying
        }
    } else {
        DisplayStatus::UpToDate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(enabled: bool) -> Pair {
        Pair {
            id: "drive".into(),
            name: "Drive".into(),
            local: "/home/ada/Drive".into(),
            remote: "gdrive:".into(),
            enabled,
            presets: vec![],
            filters: vec![],
            overrides: crate::settings::Overrides::default(),
        }
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn gate(kind: GateKind) -> Gate {
        Gate {
            kind,
            since: at("2026-01-01T12:00:00Z"),
            detail: String::new(),
            fault: None,
        }
    }

    fn temp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        );
        (dir, paths)
    }

    fn full_state() -> State {
        State {
            gate: Some(Gate {
                kind: GateKind::TooManyDeletes,
                since: at("2026-01-01T12:00:00Z"),
                detail: "3 of 4".into(),
                fault: Some(Fault::Quota),
            }),
            last_outcome: Some(LastOutcome::Error),
            offline: true,
            consecutive_failures: 2,
            failing_since: Some(at("2026-01-01T09:00:00Z")),
            last_success: Some(at("2026-01-01T10:00:00Z")),
            last_attempt: Some(at("2026-01-01T11:00:00Z")),
            applied_filters_hash: Some("abc".into()),
            conflicts_pending: 4,
            last_run: Some(LastRun {
                started: at("2026-01-01T11:00:00Z"),
                finished: at("2026-01-01T11:00:09Z"),
                exit_code: 1,
                summary: RunSummary {
                    uploaded: Some(1),
                    changes: vec![crate::bisync_log::Change {
                        kind: crate::bisync_log::ChangeKind::Upload,
                        path: "a.txt".into(),
                    }],
                    ..RunSummary::default()
                },
                log_file: "/s/runs/x.jsonl".into(),
            }),
            notified: vec!["gate:too_many_deletes".into(), "failures".into()],
        }
    }

    #[tokio::test]
    async fn a_saved_state_loads_back_equal() {
        let (_dir, paths) = temp_paths();
        let state = full_state();

        state.save(&paths, "drive").await.unwrap();
        let loaded = State::load(&paths, "drive").await.unwrap();

        assert_eq!(loaded, state);
    }

    #[tokio::test]
    async fn an_absent_state_file_is_waiting_for_the_first_review() {
        let (_dir, paths) = temp_paths();

        let state = State::load(&paths, "drive").await.unwrap();

        assert_eq!(
            state.gate.map(|gate| gate.kind),
            Some(GateKind::NeedsReviewFirstRun)
        );
        assert_eq!(state.last_outcome, None);
        assert_eq!(state.last_success, None);
    }

    #[tokio::test]
    async fn a_corrupt_state_file_is_an_error_not_a_reset() {
        let (_dir, paths) = temp_paths();
        let file = paths.state_file("drive");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{ not json").unwrap();

        let result = State::load(&paths, "drive").await;

        assert!(matches!(result, Err(StateError::Parse { .. })));
    }

    #[tokio::test]
    async fn the_state_file_has_the_documented_json_shape() {
        let (_dir, paths) = temp_paths();
        let state = State {
            gate: Some(gate(GateKind::NeedsReviewFiltersChanged)),
            last_outcome: Some(LastOutcome::Ok),
            ..State::default()
        };

        state.save(&paths, "drive").await.unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(paths.state_file("drive")).unwrap()).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "gate": {
                    "kind": "needs_review_filters_changed",
                    "since": "2026-01-01T12:00:00Z",
                    "detail": ""
                },
                "last_outcome": "ok",
                "offline": false,
                "consecutive_failures": 0,
                "failing_since": null,
                "last_success": null,
                "last_attempt": null,
                "applied_filters_hash": null,
                "conflicts_pending": 0,
                "last_run": null,
                "notified": []
            })
        );
    }

    #[tokio::test]
    async fn a_gate_saved_without_a_fault_loads_with_none_and_a_named_fault_round_trips() {
        let (_dir, paths) = temp_paths();
        let file = paths.state_file("drive");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            r#"{"gate":{"kind":"needs_attention","since":"2026-01-01T12:00:00Z","detail":"x"}}"#,
        )
        .unwrap();

        let old = State::load(&paths, "drive").await.unwrap();
        let mut named = old.clone();
        named.gate.as_mut().unwrap().fault = Some(Fault::Permission);
        named.save(&paths, "drive").await.unwrap();
        let reloaded = State::load(&paths, "drive").await.unwrap();

        assert_eq!(old.gate.unwrap().fault, None);
        assert_eq!(reloaded.gate.unwrap().fault, Some(Fault::Permission));
    }

    #[tokio::test]
    async fn the_marker_round_trips_and_is_removed() {
        let (_dir, paths) = temp_paths();
        let marker = ConfirmMarker {
            kind: ReviewKind::Force,
            filters_hash: "f00".into(),
        };

        assert_eq!(ConfirmMarker::load(&paths, "drive").await.unwrap(), None);
        marker.save(&paths, "drive").await.unwrap();
        let text = std::fs::read_to_string(paths.confirm_file("drive")).unwrap();
        let loaded = ConfirmMarker::load(&paths, "drive").await.unwrap();
        ConfirmMarker::remove(&paths, "drive").await.unwrap();
        let after = ConfirmMarker::load(&paths, "drive").await.unwrap();

        assert_eq!(loaded, Some(marker));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            serde_json::json!({ "kind": "force", "filters_hash": "f00" })
        );
        assert_eq!(after, None);
    }

    #[tokio::test]
    async fn a_marker_that_is_not_valid_json_counts_as_none_and_is_deleted() {
        let (_dir, paths) = temp_paths();
        let file = paths.confirm_file("drive");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        for garbage in [
            "{\"kind\": \"res",
            "",
            "{\"kind\": \"sideways\", \"filters_hash\": \"x\"}",
        ] {
            std::fs::write(&file, garbage).unwrap();

            let loaded = ConfirmMarker::load(&paths, "drive").await.unwrap();

            assert_eq!(loaded, None, "{garbage:?}");
            assert!(!file.exists(), "{garbage:?}");
        }
    }

    #[tokio::test]
    async fn removing_an_absent_marker_is_fine() {
        let (_dir, paths) = temp_paths();

        ConfirmMarker::remove(&paths, "drive").await.unwrap();
    }

    #[tokio::test]
    async fn a_state_file_that_cannot_be_read_is_a_read_error_not_a_first_run() {
        let (_dir, paths) = temp_paths();
        std::fs::create_dir_all(paths.state_file("drive")).unwrap();

        let result = State::load(&paths, "drive").await;

        assert!(matches!(result, Err(StateError::Read { .. })), "{result:?}");
    }

    #[tokio::test]
    async fn a_marker_that_cannot_be_read_is_a_read_error_not_no_marker() {
        let (_dir, paths) = temp_paths();
        std::fs::create_dir_all(paths.confirm_file("drive")).unwrap();

        let result = ConfirmMarker::load(&paths, "drive").await;

        assert!(matches!(result, Err(StateError::Read { .. })), "{result:?}");
    }

    #[tokio::test]
    async fn a_marker_that_cannot_be_deleted_is_a_write_error() {
        let (_dir, paths) = temp_paths();
        std::fs::create_dir_all(paths.confirm_file("drive")).unwrap();

        let result = ConfirmMarker::remove(&paths, "drive").await;

        assert!(
            matches!(result, Err(StateError::Write { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_pair_whose_lock_file_is_absent_is_not_running() {
        let (_dir, paths) = temp_paths();

        assert!(!is_running(&paths, "drive"));
    }

    #[test]
    fn a_pair_is_running_while_its_lock_is_held_and_idle_after() {
        let (_dir, paths) = temp_paths();
        let lock = paths.lock_file("drive");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let holder = File::create(&lock).unwrap();
        assert!(!is_running(&paths, "drive"));

        holder.try_lock().unwrap();
        let while_held = is_running(&paths, "drive");
        holder.unlock().unwrap();
        let after = is_running(&paths, "drive");

        assert!(while_held);
        assert!(!after);
    }

    #[test]
    fn only_one_holder_gets_the_pair_lock_and_it_shows_as_running() {
        let (_dir, paths) = temp_paths();

        let first = try_lock_pair(&paths, "drive").unwrap();
        let second = try_lock_pair(&paths, "drive").unwrap();
        let running = is_running(&paths, "drive");
        drop(first);
        let after = try_lock_pair(&paths, "drive").unwrap();

        assert!(second.is_none());
        assert!(running);
        assert!(after.is_some());
    }

    #[tokio::test]
    async fn waiting_for_the_pair_lock_gives_up_after_the_timeout_and_gets_it_once_free() {
        let (_dir, paths) = temp_paths();
        let held = try_lock_pair(&paths, "drive").unwrap().unwrap();

        let timed_out = lock_pair(&paths, "drive", Duration::from_millis(150))
            .await
            .unwrap();
        drop(held);
        let free = lock_pair(&paths, "drive", Duration::from_millis(150))
            .await
            .unwrap();

        assert!(timed_out.is_none());
        assert!(free.is_some());
    }

    #[test]
    fn checking_does_not_keep_the_lock() {
        let (_dir, paths) = temp_paths();
        let lock = paths.lock_file("drive");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let file = File::create(&lock).unwrap();

        let _ = is_running(&paths, "drive");

        assert!(file.try_lock().is_ok());
    }

    fn make_runs(paths: &Paths, count: usize) {
        let dir = paths.runs_dir("drive");
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..count {
            std::fs::write(dir.join(format!("20260101-{n:06}.jsonl")), "").unwrap();
        }
    }

    fn run_names(paths: &Paths) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(paths.runs_dir("drive"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn pruning_keeps_the_fifty_newest_runs_by_name() {
        let (_dir, paths) = temp_paths();
        make_runs(&paths, 53);

        prune_runs(&paths, "drive").await.unwrap();

        let names = run_names(&paths);
        assert_eq!(names.len(), 50);
        assert_eq!(
            names.first().map(String::as_str),
            Some("20260101-000003.jsonl")
        );
        assert_eq!(
            names.last().map(String::as_str),
            Some("20260101-000052.jsonl")
        );
    }

    #[tokio::test]
    async fn pruning_fifty_or_fewer_runs_deletes_nothing() {
        let (_dir, paths) = temp_paths();
        make_runs(&paths, 50);

        prune_runs(&paths, "drive").await.unwrap();

        assert_eq!(run_names(&paths).len(), 50);
    }

    #[tokio::test]
    async fn pruning_a_pair_without_runs_is_fine() {
        let (_dir, paths) = temp_paths();

        prune_runs(&paths, "drive").await.unwrap();
    }

    #[tokio::test]
    async fn pruning_a_runs_folder_that_cannot_be_listed_is_a_read_error() {
        let (_dir, paths) = temp_paths();
        let runs = paths.runs_dir("drive");
        std::fs::create_dir_all(runs.parent().unwrap()).unwrap();
        std::fs::write(&runs, "").unwrap();

        let result = prune_runs(&paths, "drive").await;

        assert!(matches!(result, Err(StateError::Read { .. })), "{result:?}");
    }

    fn age(path: &Path, by: Duration) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - by)
            .unwrap();
    }

    #[tokio::test]
    async fn pruning_leaves_fresh_previews_and_state_alone() {
        let (_dir, paths) = temp_paths();
        make_runs(&paths, 51);
        let pair_dir = paths.pair_state_dir("drive");
        std::fs::write(pair_dir.join("preview-20260101-000000.jsonl"), "").unwrap();
        std::fs::write(pair_dir.join("state.json"), "{}").unwrap();

        prune_runs(&paths, "drive").await.unwrap();

        assert!(pair_dir.join("preview-20260101-000000.jsonl").exists());
        assert!(pair_dir.join("state.json").exists());
    }

    #[tokio::test]
    async fn pruning_deletes_previews_older_than_a_day_and_nothing_else_there() {
        let (_dir, paths) = temp_paths();
        make_runs(&paths, 1);
        let pair_dir = paths.pair_state_dir("drive");
        for name in ["preview-old.jsonl", "preview-new.jsonl", "state.json"] {
            std::fs::write(pair_dir.join(name), "").unwrap();
        }
        age(&pair_dir.join("preview-old.jsonl"), PREVIEWS_KEPT * 2);
        age(&pair_dir.join("state.json"), PREVIEWS_KEPT * 2);

        prune_runs(&paths, "drive").await.unwrap();

        assert!(!pair_dir.join("preview-old.jsonl").exists());
        assert!(pair_dir.join("preview-new.jsonl").exists());
        assert!(pair_dir.join("state.json").exists());
    }

    #[tokio::test]
    async fn only_jsonl_logs_count_toward_the_fifty_kept() {
        let (_dir, paths) = temp_paths();
        make_runs(&paths, 50);
        let runs = paths.runs_dir("drive");
        for name in ["20250101-000000.tmp", "notes.txt"] {
            std::fs::write(runs.join(name), "").unwrap();
        }

        prune_runs(&paths, "drive").await.unwrap();

        assert_eq!(run_names(&paths).len(), 52);
    }

    #[tokio::test]
    async fn a_log_that_cannot_be_deleted_does_not_stop_the_pruning() {
        let (_dir, paths) = temp_paths();
        make_runs(&paths, 52);
        let runs = paths.runs_dir("drive");
        let stuck = runs.join("20260101-000000.jsonl");
        std::fs::remove_file(&stuck).unwrap();
        std::fs::create_dir(&stuck).unwrap();

        prune_runs(&paths, "drive").await.unwrap();

        assert!(stuck.exists());
        assert!(!runs.join("20260101-000001.jsonl").exists());
        assert_eq!(run_names(&paths).len(), 51);
    }

    fn status(enabled: bool, state: &State, running: bool) -> DisplayStatus {
        display_status(&pair(enabled), state, running)
    }

    #[test]
    fn running_beats_every_other_condition() {
        let state = full_state();

        assert_eq!(status(false, &state, true), DisplayStatus::Syncing);
    }

    #[test]
    fn a_config_auth_missing_folder_or_attention_gate_is_an_error() {
        for kind in [
            GateKind::ConfigError,
            GateKind::AuthError,
            GateKind::LocalMissing,
            GateKind::NeedsAttention,
        ] {
            let state = State {
                gate: Some(gate(kind)),
                conflicts_pending: 2,
                ..State::default()
            };

            assert_eq!(
                status(true, &state, false),
                DisplayStatus::Error,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_review_or_deletion_gate_needs_review_even_when_paused() {
        for kind in [
            GateKind::NeedsReviewFirstRun,
            GateKind::NeedsReviewFiltersChanged,
            GateKind::NeedsReviewCriticalError,
            GateKind::TooManyDeletes,
            GateKind::AllFilesChanged,
        ] {
            let state = State {
                gate: Some(gate(kind)),
                conflicts_pending: 2,
                ..State::default()
            };

            assert_eq!(
                status(false, &state, false),
                DisplayStatus::NeedsReview,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn pending_conflicts_beat_paused_offline_and_failing() {
        let state = State {
            conflicts_pending: 3,
            offline: true,
            last_outcome: Some(LastOutcome::Error),
            consecutive_failures: 9,
            ..State::default()
        };

        assert_eq!(status(false, &state, false), DisplayStatus::Conflicts(3));
    }

    #[test]
    fn a_disabled_pair_is_paused_even_when_offline_or_failing() {
        let state = State {
            offline: true,
            last_outcome: Some(LastOutcome::Error),
            consecutive_failures: 9,
            ..State::default()
        };

        assert_eq!(status(false, &state, false), DisplayStatus::Paused);
    }

    #[test]
    fn offline_beats_failing() {
        let state = State {
            offline: true,
            last_outcome: Some(LastOutcome::Error),
            consecutive_failures: 9,
            ..State::default()
        };

        assert_eq!(status(true, &state, false), DisplayStatus::Offline);
    }

    #[test]
    fn three_failures_in_a_row_is_failing() {
        let state = State {
            last_outcome: Some(LastOutcome::Error),
            consecutive_failures: 3,
            ..State::default()
        };

        assert_eq!(status(true, &state, false), DisplayStatus::Failing);
    }

    #[test]
    fn fewer_than_three_failures_will_retry() {
        let state = State {
            last_outcome: Some(LastOutcome::Error),
            consecutive_failures: 2,
            ..State::default()
        };

        assert_eq!(status(true, &state, false), DisplayStatus::Retrying);
    }

    #[test]
    fn a_pair_with_nothing_wrong_is_up_to_date() {
        let state = State {
            last_outcome: Some(LastOutcome::Ok),
            ..State::default()
        };

        assert_eq!(status(true, &state, false), DisplayStatus::UpToDate);
    }

    #[test]
    fn every_status_has_the_documented_icon_text_and_tone() {
        let table = [
            (
                DisplayStatus::Syncing,
                "refresh-cw",
                "Syncing…",
                Tone::Accent,
            ),
            (
                DisplayStatus::Error,
                "circle-alert",
                "Needs attention",
                Tone::Danger,
            ),
            (
                DisplayStatus::NeedsReview,
                "eye",
                "Review needed",
                Tone::Warning,
            ),
            (
                DisplayStatus::Conflicts(2),
                "copy",
                "2 conflicts",
                Tone::Warning,
            ),
            (DisplayStatus::Paused, "pause", "Paused", Tone::Dim),
            (DisplayStatus::Offline, "cloud-off", "Offline", Tone::Dim),
            (
                DisplayStatus::Failing,
                "circle-alert",
                "Sync failing",
                Tone::Danger,
            ),
            (
                DisplayStatus::Retrying,
                "clock",
                "Will retry",
                Tone::Secondary,
            ),
            (
                DisplayStatus::UpToDate,
                "check",
                "Up to date",
                Tone::Secondary,
            ),
        ];

        for (status, icon, text, tone) in table {
            assert_eq!(status.icon(), icon);
            assert_eq!(status.text(), text);
            assert_eq!(status.tone(), tone, "{status:?}");
        }
    }

    #[test]
    fn status_names_are_the_variant_names_without_the_conflict_count() {
        assert_eq!(DisplayStatus::UpToDate.name(), "UpToDate");
        assert_eq!(DisplayStatus::NeedsReview.name(), "NeedsReview");
        assert_eq!(DisplayStatus::Conflicts(7).name(), "Conflicts");
    }

    #[test]
    fn gate_kinds_serialize_to_the_documented_names_and_notified_keys() {
        let kinds = [
            (GateKind::NeedsReviewFirstRun, "needs_review_first_run"),
            (
                GateKind::NeedsReviewFiltersChanged,
                "needs_review_filters_changed",
            ),
            (
                GateKind::NeedsReviewCriticalError,
                "needs_review_critical_error",
            ),
            (GateKind::TooManyDeletes, "too_many_deletes"),
            (GateKind::AllFilesChanged, "all_files_changed"),
            (GateKind::ConfigError, "config_error"),
            (GateKind::AuthError, "auth_error"),
            (GateKind::LocalMissing, "local_missing"),
            (GateKind::NeedsAttention, "needs_attention"),
        ];

        for (kind, name) in kinds {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{name}\""));
            assert_eq!(kind.as_str(), name);
            assert_eq!(kind.notified_key(), format!("gate:{name}"));
        }
    }
}
