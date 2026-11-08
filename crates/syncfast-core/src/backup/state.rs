//! Per-backup state under `$XDG_STATE_HOME/syncfast/backups/<id>/`.
//!
//! As for syncs, "running" is never stored: it is the lock file.

use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::state::{Gate, LastOutcome, StateError, is_locked, write_json};

/// How much of the repository a `restic check` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Quick,
    Sample,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSnapshot {
    pub id: String,
    pub time: Timestamp,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupSummary {
    pub snapshot_id: String,
    pub files_new: u64,
    pub files_changed: u64,
    pub files_unmodified: u64,
    pub data_added: u64,
    pub data_added_packed: u64,
    pub total_files_processed: u64,
    pub total_bytes_processed: u64,
    /// Files restic could not read (its exit code 3).
    pub warnings: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRun {
    pub started: Timestamp,
    pub finished: Timestamp,
    pub exit_code: i32,
    pub summary: BackupSummary,
    pub log_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastCheck {
    pub at: Timestamp,
    pub kind: CheckKind,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools, reason = "the shape of state.json")]
pub struct BackupState {
    pub gate: Option<Gate>,
    pub last_outcome: Option<LastOutcome>,
    /// The last attempt was skipped for lack of network.
    pub offline: bool,
    /// The local destination was not there at the last attempt.
    pub destination_unavailable: bool,
    pub unavailable_since: Option<Timestamp>,
    pub consecutive_failures: u32,
    pub failing_since: Option<Timestamp>,
    pub last_success: Option<Timestamp>,
    pub last_attempt: Option<Timestamp>,
    pub last_snapshot: Option<LastSnapshot>,
    pub last_run: Option<LastRun>,
    pub last_check: Option<LastCheck>,
    /// When the next sample check is due.
    pub next_check: Option<Timestamp>,
    pub check_after_failure_done: bool,
    pub prune_failures: u32,
    /// The message of the last failed `forget`; gone when one succeeds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prune_error: Option<String>,
    /// An agent created the backup and the person has not yet confirmed
    /// saving the key. Holds no round.
    pub key_unsaved: bool,
    pub notified: Vec<String>,
}

impl BackupState {
    /// Reads the backup's state; an absent file is the default state.
    ///
    /// # Errors
    /// The file cannot be read or is not valid state JSON.
    pub async fn load(paths: &Paths, backup_id: &str) -> Result<Self, StateError> {
        let path = paths.backup_state_file(backup_id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|source| StateError::Parse { path, source })
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(StateError::Read { path, source }),
        }
    }

    /// # Errors
    /// The file cannot be written.
    pub async fn save(&self, paths: &Paths, backup_id: &str) -> Result<(), StateError> {
        write_json(paths.backup_state_file(backup_id), self).await
    }
}

/// True while a run of `backup_id` holds its lock. Never blocks.
#[must_use]
pub fn is_running(paths: &Paths, backup_id: &str) -> bool {
    is_locked(&paths.backup_lock_file(backup_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bisync::Fault;
    use crate::state::GateKind;

    fn paths_in(root: &std::path::Path) -> Paths {
        Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        )
    }

    #[test]
    fn a_file_without_the_newer_fields_loads_with_their_defaults() {
        let state: BackupState = serde_json::from_str(r#"{"consecutive_failures": 2}"#).unwrap();

        assert_eq!(state.consecutive_failures, 2);
        assert!(!state.key_unsaved);
        assert!(!state.destination_unavailable);
        assert!(!state.check_after_failure_done);
        assert_eq!(state.prune_failures, 0);
        assert_eq!(state.prune_error, None);
        assert!(state.notified.is_empty());
        assert_eq!(state.last_snapshot, None);
    }

    #[test]
    fn an_empty_object_is_the_default_state() {
        let state: BackupState = serde_json::from_str("{}").unwrap();

        assert_eq!(state, BackupState::default());
    }

    #[test]
    fn the_spec_json_loads_every_field() {
        let json = r#"{
            "gate": {"kind": "needs_attention", "since": "2026-05-01T10:00:00Z",
                     "detail": "no key", "fault": "key_missing"},
            "last_outcome": "error",
            "offline": true,
            "destination_unavailable": true,
            "unavailable_since": "2026-05-01T09:00:00Z",
            "consecutive_failures": 3,
            "failing_since": "2026-05-01T08:00:00Z",
            "last_success": "2026-04-30T08:00:00Z",
            "last_attempt": "2026-05-01T10:00:00Z",
            "last_snapshot": {"id": "abc123", "time": "2026-04-30T08:00:00Z"},
            "last_run": {"started": "2026-04-30T08:00:00Z", "finished": "2026-04-30T08:05:00Z",
                         "exit_code": 3,
                         "summary": {"snapshot_id": "abc123", "warnings": 2},
                         "log_file": "/state/runs/1.log"},
            "last_check": {"at": "2026-04-01T00:00:00Z", "kind": "sample", "ok": false,
                           "detail": "1 error"},
            "next_check": "2026-05-01T00:00:00Z",
            "check_after_failure_done": true,
            "prune_failures": 1,
            "prune_error": "repository is locked",
            "key_unsaved": true,
            "notified": ["failures"]
        }"#;

        let state: BackupState = serde_json::from_str(json).unwrap();

        let gate = state.gate.as_ref().unwrap();
        assert_eq!(gate.kind, GateKind::NeedsAttention);
        assert_eq!(gate.fault, Some(Fault::KeyMissing));
        assert_eq!(state.last_outcome, Some(LastOutcome::Error));
        assert!(state.offline && state.destination_unavailable && state.key_unsaved);
        assert_eq!(state.last_snapshot.as_ref().unwrap().id, "abc123");
        let run = state.last_run.as_ref().unwrap();
        assert_eq!(run.exit_code, 3);
        assert_eq!(run.summary.warnings, 2);
        assert_eq!(run.summary.files_new, 0);
        let check = state.last_check.as_ref().unwrap();
        assert_eq!(check.kind, CheckKind::Sample);
        assert!(!check.ok);
        assert!(state.check_after_failure_done);
        assert_eq!(state.prune_failures, 1);
        assert_eq!(state.prune_error.as_deref(), Some("repository is locked"));
        assert_eq!(state.notified, ["failures"]);
    }

    #[tokio::test]
    async fn an_absent_file_loads_as_the_default_state_and_a_saved_one_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        assert_eq!(
            BackupState::load(&paths, "docs").await.unwrap(),
            BackupState::default()
        );
        let state = BackupState {
            consecutive_failures: 4,
            key_unsaved: true,
            notified: vec!["failures".into()],
            ..BackupState::default()
        };

        state.save(&paths, "docs").await.unwrap();

        assert_eq!(BackupState::load(&paths, "docs").await.unwrap(), state);
    }

    #[tokio::test]
    async fn a_file_that_is_not_json_is_a_parse_error_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let file = paths.backup_state_file("docs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "nope").unwrap();

        let err = BackupState::load(&paths, "docs").await.unwrap_err();

        assert!(matches!(err, StateError::Parse { .. }));
        assert!(err.to_string().contains("state.json"));
    }

    #[test]
    fn a_backup_without_a_lock_holder_is_not_running() {
        let dir = tempfile::tempdir().unwrap();

        assert!(!is_running(&paths_in(dir.path()), "docs"));
    }

    #[test]
    fn a_backup_is_running_while_its_lock_is_held_and_only_that_backup() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());

        let lock = crate::state::try_lock(&paths, paths.backup_lock_file("docs"))
            .unwrap()
            .unwrap();

        assert!(is_running(&paths, "docs"));
        assert!(!is_running(&paths, "photos"));
        drop(lock);
        assert!(!is_running(&paths, "docs"));
    }
}
