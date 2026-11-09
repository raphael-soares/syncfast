//! What a backup shows as its status (spec phase 3, step 10), and the
//! `progress.json` a running one writes for it.

use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::state::{GateKind, LastOutcome, Tone};

use super::classify::BackupProgress;
use super::config::BackupConfig;
use super::state::{BackupState, is_running};

/// What a running backup is doing, as `progress.json` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunningStep {
    Backup,
    Check,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStatus {
    BackingUp,
    Checking,
    Error,
    SaveKey,
    Paused,
    Offline,
    Unavailable,
    Failing,
    Retrying,
    UpToDate,
}

impl BackupStatus {
    #[must_use]
    pub fn tone(self) -> Tone {
        match self {
            Self::BackingUp | Self::Checking | Self::SaveKey => Tone::Accent,
            Self::Error | Self::Failing => Tone::Danger,
            Self::Paused | Self::Offline | Self::Unavailable => Tone::Dim,
            Self::Retrying | Self::UpToDate => Tone::Secondary,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::BackingUp => "Backing up…",
            Self::Checking => "Checking…",
            Self::Error => "Needs attention",
            Self::SaveKey => "Save the key",
            Self::Paused => "Paused",
            Self::Offline => "Offline",
            Self::Unavailable => "Destination not available",
            Self::Failing => "Backup failing",
            Self::Retrying => "Will retry",
            Self::UpToDate => "Up to date",
        }
    }

    /// The name the CLI and agents read: stable, lowercase.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::BackingUp => "backing_up",
            Self::Checking => "checking",
            Self::Error => "error",
            Self::SaveKey => "save_key",
            Self::Paused => "paused",
            Self::Offline => "offline",
            Self::Unavailable => "unavailable",
            Self::Failing => "failing",
            Self::Retrying => "retrying",
            Self::UpToDate => "up_to_date",
        }
    }
}

/// The one precedence used by the sidebar, the tray and `syncfast backups`:
/// the first rule that matches wins. `running` is [`running_step`];
/// `failing_after` is the failures in a row from which a backup is "failing"
/// and not "retrying" (the notification setting).
#[must_use]
pub fn display_backup_status(
    backup: &BackupConfig,
    state: &BackupState,
    running: Option<RunningStep>,
    failing_after: u32,
) -> BackupStatus {
    let gate = state.gate.as_ref().map(|gate| gate.kind);
    let errored = state.last_outcome == Some(LastOutcome::Error);
    match running {
        Some(RunningStep::Backup) => BackupStatus::BackingUp,
        Some(RunningStep::Check) => BackupStatus::Checking,
        None if matches!(
            gate,
            Some(GateKind::ConfigError | GateKind::AuthError | GateKind::NeedsAttention)
        ) =>
        {
            BackupStatus::Error
        }
        None if state.key_unsaved => BackupStatus::SaveKey,
        None if !backup.enabled => BackupStatus::Paused,
        None if state.offline => BackupStatus::Offline,
        None if state.destination_unavailable => BackupStatus::Unavailable,
        None if errored && state.consecutive_failures >= failing_after => BackupStatus::Failing,
        None if errored => BackupStatus::Retrying,
        None => BackupStatus::UpToDate,
    }
}

/// `progress.json`: what a running backup writes at most once a second, and
/// what the GUI and `syncfast backups` read. `percent_done` is restic's
/// fraction, 0.0 to 1.0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupProgressFile {
    pub step: RunningStep,
    #[serde(default)]
    pub percent_done: Option<f64>,
    #[serde(default)]
    pub files_done: Option<u64>,
    #[serde(default)]
    pub total_files: Option<u64>,
    #[serde(default)]
    pub bytes_done: Option<u64>,
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub seconds_remaining: Option<u64>,
    /// One of the files being read right now.
    #[serde(default)]
    pub current: Option<String>,
}

impl BackupProgressFile {
    /// A step that reports no numbers, such as a check.
    #[must_use]
    pub fn of_step(step: RunningStep) -> Self {
        Self {
            step,
            percent_done: None,
            files_done: None,
            total_files: None,
            bytes_done: None,
            total_bytes: None,
            seconds_remaining: None,
            current: None,
        }
    }

    /// The `backup` step as one `status` line of restic describes it.
    #[must_use]
    pub fn from_status(status: &BackupProgress) -> Self {
        Self {
            step: RunningStep::Backup,
            percent_done: status.percent_done,
            files_done: status.files_done,
            total_files: status.total_files,
            bytes_done: status.bytes_done,
            total_bytes: status.total_bytes,
            seconds_remaining: status.seconds_remaining,
            current: status.current_files.first().cloned(),
        }
    }

    /// `None` when there is no file, or it is not progress (a half-written
    /// or foreign file must not make a backup look idle or broken).
    #[must_use]
    pub fn read(paths: &Paths, backup_id: &str) -> Option<Self> {
        let bytes = std::fs::read(paths.backup_progress_file(backup_id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// The step of `backup_id`, or `None` when no run holds its lock. A run
/// that has the lock and has written no progress yet is backing up.
#[must_use]
pub fn running_step(paths: &Paths, backup_id: &str) -> Option<RunningStep> {
    if !is_running(paths, backup_id) {
        return None;
    }
    Some(
        BackupProgressFile::read(paths, backup_id)
            .map_or(RunningStep::Backup, |progress| progress.step),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::bisync::Fault;
    use crate::state::Gate;

    fn backup(enabled: bool) -> BackupConfig {
        serde_json::from_value(serde_json::json!({
            "id": "docs",
            "name": "Documents",
            "paths": ["/home/ada/Documents"],
            "dest": "gdrive:Backups/laptop",
            "enabled": enabled,
        }))
        .unwrap()
    }

    fn gate(kind: GateKind, fault: Option<Fault>) -> Gate {
        Gate {
            kind,
            since: "2026-01-01T12:00:00Z".parse::<Timestamp>().unwrap(),
            detail: String::new(),
            fault,
            auto_checked: None,
        }
    }

    fn failed(times: u32) -> BackupState {
        BackupState {
            last_outcome: Some(LastOutcome::Error),
            consecutive_failures: times,
            ..BackupState::default()
        }
    }

    fn status(enabled: bool, state: &BackupState) -> BackupStatus {
        display_backup_status(&backup(enabled), state, None, 3)
    }

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
    fn row_1_a_running_backup_is_backing_up_before_anything_else() {
        let state = BackupState {
            gate: Some(gate(GateKind::AuthError, None)),
            key_unsaved: true,
            offline: true,
            ..failed(5)
        };

        assert_eq!(
            display_backup_status(&backup(false), &state, Some(RunningStep::Backup), 3),
            BackupStatus::BackingUp
        );
    }

    #[test]
    fn row_1b_a_running_check_is_checking() {
        let state = BackupState {
            gate: Some(gate(GateKind::NeedsAttention, Some(Fault::RepoDamaged))),
            ..BackupState::default()
        };

        assert_eq!(
            display_backup_status(&backup(true), &state, Some(RunningStep::Check), 3),
            BackupStatus::Checking
        );
    }

    #[test]
    fn row_2_a_gate_that_needs_the_person_is_an_error() {
        for kind in [
            GateKind::ConfigError,
            GateKind::AuthError,
            GateKind::NeedsAttention,
        ] {
            let state = BackupState {
                gate: Some(gate(kind, None)),
                offline: true,
                ..failed(1)
            };

            assert_eq!(status(false, &state), BackupStatus::Error, "{kind:?}");
        }
    }

    #[test]
    fn row_2b_an_unsaved_key_shows_save_the_key() {
        let state = BackupState {
            key_unsaved: true,
            ..BackupState::default()
        };

        assert_eq!(status(true, &state), BackupStatus::SaveKey);
    }

    #[test]
    fn row_2b_an_error_gate_wins_over_an_unsaved_key() {
        let state = BackupState {
            key_unsaved: true,
            gate: Some(gate(GateKind::NeedsAttention, Some(Fault::Quota))),
            ..BackupState::default()
        };

        assert_eq!(status(true, &state), BackupStatus::Error);
    }

    #[test]
    fn row_2b_an_unsaved_key_wins_over_paused_and_the_rest() {
        let state = BackupState {
            key_unsaved: true,
            offline: true,
            destination_unavailable: true,
            ..failed(5)
        };

        assert_eq!(status(false, &state), BackupStatus::SaveKey);
    }

    #[test]
    fn row_3_a_disabled_backup_is_paused() {
        let state = BackupState {
            offline: true,
            destination_unavailable: true,
            ..failed(5)
        };

        assert_eq!(status(false, &state), BackupStatus::Paused);
    }

    #[test]
    fn row_4_no_network_is_offline() {
        let state = BackupState {
            offline: true,
            destination_unavailable: true,
            ..failed(5)
        };

        assert_eq!(status(true, &state), BackupStatus::Offline);
    }

    #[test]
    fn row_5_a_destination_that_is_not_there_is_unavailable() {
        let state = BackupState {
            destination_unavailable: true,
            ..failed(5)
        };

        assert_eq!(status(true, &state), BackupStatus::Unavailable);
    }

    #[test]
    fn row_6_three_failures_in_a_row_is_failing() {
        assert_eq!(status(true, &failed(3)), BackupStatus::Failing);
        assert_eq!(status(true, &failed(10)), BackupStatus::Failing);
    }

    #[test]
    fn row_6_follows_the_notification_setting() {
        let show = |times, failing_after| {
            display_backup_status(&backup(true), &failed(times), None, failing_after)
        };

        assert_eq!(show(3, 5), BackupStatus::Retrying);
        assert_eq!(show(5, 5), BackupStatus::Failing);
        assert_eq!(show(1, 1), BackupStatus::Failing);
    }

    #[test]
    fn row_7_fewer_failures_is_retrying() {
        assert_eq!(status(true, &failed(1)), BackupStatus::Retrying);
        assert_eq!(status(true, &failed(2)), BackupStatus::Retrying);
    }

    #[test]
    fn row_8_otherwise_up_to_date() {
        assert_eq!(
            status(true, &BackupState::default()),
            BackupStatus::UpToDate
        );
        let ok = BackupState {
            last_outcome: Some(LastOutcome::Ok),
            consecutive_failures: 7,
            ..BackupState::default()
        };
        assert_eq!(status(true, &ok), BackupStatus::UpToDate);
    }

    #[test]
    fn the_table_gives_each_status_its_text_and_colour() {
        let rows = [
            (BackupStatus::BackingUp, "Backing up…", Tone::Accent),
            (BackupStatus::Checking, "Checking…", Tone::Accent),
            (BackupStatus::Error, "Needs attention", Tone::Danger),
            (BackupStatus::SaveKey, "Save the key", Tone::Accent),
            (BackupStatus::Paused, "Paused", Tone::Dim),
            (BackupStatus::Offline, "Offline", Tone::Dim),
            (
                BackupStatus::Unavailable,
                "Destination not available",
                Tone::Dim,
            ),
            (BackupStatus::Failing, "Backup failing", Tone::Danger),
            (BackupStatus::Retrying, "Will retry", Tone::Secondary),
            (BackupStatus::UpToDate, "Up to date", Tone::Secondary),
        ];

        for (status, label, tone) in rows {
            assert_eq!(status.label(), label, "{status:?}");
            assert_eq!(status.tone(), tone, "{status:?}");
        }
    }

    #[test]
    fn names_are_distinct_snake_case() {
        let all = [
            BackupStatus::BackingUp,
            BackupStatus::Checking,
            BackupStatus::Error,
            BackupStatus::SaveKey,
            BackupStatus::Paused,
            BackupStatus::Offline,
            BackupStatus::Unavailable,
            BackupStatus::Failing,
            BackupStatus::Retrying,
            BackupStatus::UpToDate,
        ];
        let names: std::collections::BTreeSet<_> = all.iter().map(|status| status.name()).collect();

        assert_eq!(names.len(), all.len());
        assert!(
            names
                .iter()
                .all(|name| { name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_') })
        );
        assert_eq!(BackupStatus::UpToDate.name(), "up_to_date");
    }

    #[test]
    fn the_progress_file_spells_the_step_in_snake_case() {
        let json = serde_json::to_string(&BackupProgressFile::of_step(RunningStep::Check)).unwrap();

        assert!(json.contains(r#""step":"check""#), "{json}");
        let back: BackupProgressFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, BackupProgressFile::of_step(RunningStep::Check));
    }

    #[test]
    fn a_status_line_becomes_progress_with_its_first_current_file() {
        let progress = BackupProgress {
            percent_done: Some(0.5),
            files_done: Some(1),
            total_files: Some(2),
            bytes_done: Some(10),
            total_bytes: Some(20),
            seconds_remaining: Some(7),
            current_files: vec!["/a".to_owned(), "/b".to_owned()],
        };

        let file = BackupProgressFile::from_status(&progress);

        assert_eq!(
            file,
            BackupProgressFile {
                step: RunningStep::Backup,
                percent_done: Some(0.5),
                files_done: Some(1),
                total_files: Some(2),
                bytes_done: Some(10),
                total_bytes: Some(20),
                seconds_remaining: Some(7),
                current: Some("/a".to_owned()),
            }
        );
    }

    #[test]
    fn a_progress_file_with_only_the_step_reads() {
        let file: BackupProgressFile = serde_json::from_str(r#"{"step":"backup"}"#).unwrap();

        assert_eq!(file, BackupProgressFile::of_step(RunningStep::Backup));
    }

    #[test]
    fn nothing_runs_without_a_lock_even_if_a_progress_file_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let file = paths.backup_progress_file("docs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"step":"check"}"#).unwrap();

        assert_eq!(running_step(&paths, "docs"), None);
    }

    #[test]
    fn a_locked_backup_reports_the_step_of_its_progress_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let _lock = crate::state::try_lock(&paths, paths.backup_lock_file("docs"))
            .unwrap()
            .unwrap();
        let file = paths.backup_progress_file("docs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"step":"check"}"#).unwrap();

        assert_eq!(running_step(&paths, "docs"), Some(RunningStep::Check));
        assert_eq!(running_step(&paths, "photos"), None);
    }

    #[test]
    fn a_locked_backup_with_no_progress_file_yet_is_backing_up() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let _lock = crate::state::try_lock(&paths, paths.backup_lock_file("docs"))
            .unwrap()
            .unwrap();

        assert_eq!(running_step(&paths, "docs"), Some(RunningStep::Backup));
    }

    #[test]
    fn a_progress_file_that_is_not_progress_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let file = paths.backup_progress_file("docs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{\"step\":").unwrap();

        assert_eq!(BackupProgressFile::read(&paths, "docs"), None);
    }
}
