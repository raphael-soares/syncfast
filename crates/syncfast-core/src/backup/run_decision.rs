//! What a backup round does before it touches restic: the pure decision
//! table of the spec (step 6), and the two things its input is read from
//! disk with (the source folders and `request.json`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bisync::Fault;
use crate::paths::Paths;
use crate::state::{Gate, GateKind, StateError, write_json};

use super::state::CheckKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Paused,
    Gated,
    Offline,
    DestinationUnavailable,
}

/// What the folders to back up look like right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceCheck {
    Ok,
    /// Absent, or not a folder.
    Missing(PathBuf),
    /// A folder with no entry in it.
    Empty(PathBuf),
}

/// A person asked for something other than the scheduled backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupRequest {
    Check(CheckKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Backup,
    Check(CheckKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupDecision {
    Skip(SkipReason),
    Run(RunKind),
    /// `detail` is the folder that is missing or empty.
    SetGate {
        kind: GateKind,
        fault: Fault,
        detail: String,
    },
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the inputs of the decision table"
)]
#[derive(Debug, Clone)]
pub struct DecisionInput<'a> {
    pub enabled: bool,
    pub online: bool,
    /// The destination is an rclone account folder, so it needs the network.
    pub dest_is_remote: bool,
    /// For a local destination: `<dest>/config` exists. A disk that is not
    /// mounted leaves its mount point empty, which is not "no repository".
    pub dest_available: bool,
    pub gate: Option<&'a Gate>,
    pub source_check: SourceCheck,
    pub request: Option<BackupRequest>,
}

/// The first row of the table that matches wins. A `config_error` gate that
/// the config no longer earns matches no row, so it is as if there were none.
#[must_use]
pub fn decide_backup(input: &DecisionInput<'_>) -> BackupDecision {
    let gate = input.gate;
    let asked_for_check = matches!(input.request, Some(BackupRequest::Check(_)));
    if !input.enabled {
        return BackupDecision::Skip(SkipReason::Paused);
    }
    if gate.is_some_and(|gate| gate.kind == GateKind::AuthError) {
        return BackupDecision::Skip(SkipReason::Gated);
    }
    if gate.is_some_and(|gate| {
        gate.kind == GateKind::NeedsAttention && gate.fault == Some(Fault::RepoDamaged)
    }) && !asked_for_check
    {
        return BackupDecision::Skip(SkipReason::Gated);
    }
    if input.dest_is_remote && !input.online {
        return BackupDecision::Skip(SkipReason::Offline);
    }
    if !input.dest_is_remote && !input.dest_available {
        return BackupDecision::Skip(SkipReason::DestinationUnavailable);
    }
    if let Some(BackupRequest::Check(kind)) = input.request {
        return BackupDecision::Run(RunKind::Check(kind));
    }
    match &input.source_check {
        SourceCheck::Missing(path) => BackupDecision::SetGate {
            kind: GateKind::NeedsAttention,
            fault: Fault::SourceMissing,
            detail: path.display().to_string(),
        },
        SourceCheck::Empty(path) => BackupDecision::SetGate {
            kind: GateKind::NeedsAttention,
            fault: Fault::SourceEmpty,
            detail: path.display().to_string(),
        },
        SourceCheck::Ok => BackupDecision::Run(RunKind::Backup),
    }
}

/// The first folder of `paths` that is missing, is not a folder or holds
/// nothing. Looks only at the top level of each.
#[must_use]
pub fn source_check(paths: &[PathBuf]) -> SourceCheck {
    for path in paths {
        match std::fs::read_dir(path) {
            Err(_) => return SourceCheck::Missing(path.clone()),
            Ok(mut entries) => {
                if entries.next().is_none() {
                    return SourceCheck::Empty(path.clone());
                }
            }
        }
    }
    SourceCheck::Ok
}

/// `request.json`: what a person asked the next round to do instead of
/// backing up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupRequestFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckKind>,
}

impl BackupRequestFile {
    #[must_use]
    pub fn request(self) -> Option<BackupRequest> {
        self.check.map(BackupRequest::Check)
    }
}

impl From<BackupRequest> for BackupRequestFile {
    fn from(request: BackupRequest) -> Self {
        match request {
            BackupRequest::Check(kind) => Self { check: Some(kind) },
        }
    }
}

/// Writes the request the next round of `backup_id` reads.
///
/// # Errors
/// The file cannot be written.
pub async fn save_request(
    paths: &Paths,
    backup_id: &str,
    request: BackupRequest,
) -> Result<(), StateError> {
    write_json(
        paths.backup_request_file(backup_id),
        &BackupRequestFile::from(request),
    )
    .await
}

/// Reads and deletes `request.json`. A file that cannot be read or is not a
/// request is deleted and logged: it must not ask again on every round.
#[must_use]
pub fn take_request(paths: &Paths, backup_id: &str) -> Option<BackupRequest> {
    let path = paths.backup_request_file(backup_id);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!("cannot read {}: {err}", path.display());
            remove_or_warn(&path);
            return None;
        }
    };
    remove_or_warn(&path);
    match serde_json::from_slice::<BackupRequestFile>(&bytes) {
        Ok(file) => file.request(),
        Err(err) => {
            log::warn!("{} is not a request: {err}", path.display());
            None
        }
    }
}

fn remove_or_warn(path: &Path) {
    if let Err(err) = std::fs::remove_file(path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("cannot delete {}: {err}", path.display());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn input() -> DecisionInput<'static> {
        DecisionInput {
            enabled: true,
            online: true,
            dest_is_remote: true,
            dest_available: true,
            gate: None,
            source_check: SourceCheck::Ok,
            request: None,
        }
    }

    fn gate(kind: GateKind, fault: Option<Fault>) -> Gate {
        Gate {
            kind,
            since: "2026-01-01T12:00:00Z".parse().unwrap(),
            detail: String::new(),
            fault,
            auto_checked: None,
        }
    }

    fn decide_with(gate: Option<&Gate>, input: DecisionInput<'_>) -> BackupDecision {
        decide_backup(&DecisionInput { gate, ..input })
    }

    fn paths_in(root: &Path) -> Paths {
        Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        )
    }

    #[test]
    fn row_1_a_paused_backup_skips_whatever_else_is_true() {
        let auth = gate(GateKind::AuthError, None);

        let decision = decide_with(
            Some(&auth),
            DecisionInput {
                enabled: false,
                online: false,
                request: Some(BackupRequest::Check(CheckKind::Full)),
                source_check: SourceCheck::Missing("/x".into()),
                ..input()
            },
        );

        assert_eq!(decision, BackupDecision::Skip(SkipReason::Paused));
    }

    #[test]
    fn row_2_an_auth_error_gate_skips_even_when_a_check_was_asked_for() {
        let auth = gate(GateKind::AuthError, None);

        let decision = decide_with(
            Some(&auth),
            DecisionInput {
                online: false,
                request: Some(BackupRequest::Check(CheckKind::Quick)),
                ..input()
            },
        );

        assert_eq!(decision, BackupDecision::Skip(SkipReason::Gated));
    }

    #[test]
    fn row_3_a_damaged_repository_holds_the_backup() {
        let damaged = gate(GateKind::NeedsAttention, Some(Fault::RepoDamaged));

        let decision = decide_with(
            Some(&damaged),
            DecisionInput {
                online: false,
                ..input()
            },
        );

        assert_eq!(decision, BackupDecision::Skip(SkipReason::Gated));
    }

    #[test]
    fn row_3_a_damaged_repository_still_allows_a_check() {
        let damaged = gate(GateKind::NeedsAttention, Some(Fault::RepoDamaged));

        let decision = decide_with(
            Some(&damaged),
            DecisionInput {
                request: Some(BackupRequest::Check(CheckKind::Full)),
                ..input()
            },
        );

        assert_eq!(
            decision,
            BackupDecision::Run(RunKind::Check(CheckKind::Full))
        );
    }

    #[test]
    fn row_4_an_account_destination_without_network_is_offline() {
        let decision = decide_backup(&DecisionInput {
            online: false,
            ..input()
        });

        assert_eq!(decision, BackupDecision::Skip(SkipReason::Offline));
    }

    #[test]
    fn row_4_a_local_destination_does_not_need_the_network() {
        let decision = decide_backup(&DecisionInput {
            online: false,
            dest_is_remote: false,
            ..input()
        });

        assert_eq!(decision, BackupDecision::Run(RunKind::Backup));
    }

    #[test]
    fn row_5_a_local_destination_that_is_not_there_is_unavailable() {
        let decision = decide_backup(&DecisionInput {
            dest_is_remote: false,
            dest_available: false,
            ..input()
        });

        assert_eq!(
            decision,
            BackupDecision::Skip(SkipReason::DestinationUnavailable)
        );
    }

    #[test]
    fn row_5_an_account_destination_ignores_the_local_availability() {
        let decision = decide_backup(&DecisionInput {
            dest_available: false,
            ..input()
        });

        assert_eq!(decision, BackupDecision::Run(RunKind::Backup));
    }

    #[test]
    fn row_6_a_requested_check_runs_without_looking_at_the_sources() {
        let decision = decide_backup(&DecisionInput {
            request: Some(BackupRequest::Check(CheckKind::Sample)),
            source_check: SourceCheck::Empty("/x".into()),
            ..input()
        });

        assert_eq!(
            decision,
            BackupDecision::Run(RunKind::Check(CheckKind::Sample))
        );
    }

    #[test]
    fn row_6_a_requested_check_still_waits_for_the_destination() {
        let decision = decide_backup(&DecisionInput {
            online: false,
            request: Some(BackupRequest::Check(CheckKind::Quick)),
            ..input()
        });

        assert_eq!(decision, BackupDecision::Skip(SkipReason::Offline));
    }

    #[test]
    fn row_7_a_missing_folder_sets_the_source_missing_gate() {
        let decision = decide_backup(&DecisionInput {
            source_check: SourceCheck::Missing("/media/disk/docs".into()),
            ..input()
        });

        assert_eq!(
            decision,
            BackupDecision::SetGate {
                kind: GateKind::NeedsAttention,
                fault: Fault::SourceMissing,
                detail: "/media/disk/docs".to_owned(),
            }
        );
    }

    #[test]
    fn row_8_an_empty_folder_sets_the_source_empty_gate() {
        let decision = decide_backup(&DecisionInput {
            source_check: SourceCheck::Empty("/media/disk/docs".into()),
            ..input()
        });

        assert_eq!(
            decision,
            BackupDecision::SetGate {
                kind: GateKind::NeedsAttention,
                fault: Fault::SourceEmpty,
                detail: "/media/disk/docs".to_owned(),
            }
        );
    }

    #[test]
    fn row_9_otherwise_the_backup_runs() {
        assert_eq!(
            decide_backup(&input()),
            BackupDecision::Run(RunKind::Backup)
        );
    }

    #[test]
    fn a_config_error_gate_is_as_if_there_were_none() {
        let stale = gate(GateKind::ConfigError, None);

        assert_eq!(
            decide_with(Some(&stale), input()),
            BackupDecision::Run(RunKind::Backup)
        );
    }

    #[test]
    fn an_attention_gate_with_another_fault_does_not_stop_the_backup() {
        for fault in [None, Some(Fault::Quota), Some(Fault::KeyMissing)] {
            let attention = gate(GateKind::NeedsAttention, fault);

            assert_eq!(
                decide_with(Some(&attention), input()),
                BackupDecision::Run(RunKind::Backup),
                "{fault:?}"
            );
        }
    }

    #[test]
    fn source_check_of_nothing_is_ok() {
        assert_eq!(source_check(&[]), SourceCheck::Ok);
    }

    #[test]
    fn source_check_reports_a_missing_folder() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");

        assert_eq!(
            source_check(std::slice::from_ref(&gone)),
            SourceCheck::Missing(gone)
        );
    }

    #[test]
    fn source_check_reports_a_file_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();

        assert_eq!(
            source_check(std::slice::from_ref(&file)),
            SourceCheck::Missing(file)
        );
    }

    #[test]
    fn source_check_reports_an_empty_folder() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            source_check(&[dir.path().to_path_buf()]),
            SourceCheck::Empty(dir.path().to_path_buf())
        );
    }

    #[test]
    fn source_check_accepts_a_folder_with_an_entry_even_a_hidden_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".hidden"), "x").unwrap();

        assert_eq!(source_check(&[dir.path().to_path_buf()]), SourceCheck::Ok);
    }

    #[test]
    fn source_check_names_the_first_folder_with_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("full");
        let empty = dir.path().join("empty");
        let gone = dir.path().join("gone");
        std::fs::create_dir(&full).unwrap();
        std::fs::write(full.join("a"), "x").unwrap();
        std::fs::create_dir(&empty).unwrap();

        assert_eq!(
            source_check(&[full.clone(), empty.clone(), gone.clone()]),
            SourceCheck::Empty(empty.clone())
        );
        assert_eq!(
            source_check(&[full, gone.clone(), empty]),
            SourceCheck::Missing(gone)
        );
    }

    #[test]
    fn the_request_file_spells_the_check_kind() {
        for (kind, text) in [
            (CheckKind::Quick, "quick"),
            (CheckKind::Sample, "sample"),
            (CheckKind::Full, "full"),
        ] {
            let json = format!(r#"{{"check":"{text}"}}"#);
            let file: BackupRequestFile = serde_json::from_str(&json).unwrap();

            assert_eq!(file.request(), Some(BackupRequest::Check(kind)));
            assert_eq!(serde_json::to_string(&file).unwrap(), json);
        }
    }

    #[test]
    fn an_empty_request_file_asks_for_nothing() {
        let file: BackupRequestFile = serde_json::from_str("{}").unwrap();

        assert_eq!(file.request(), None);
    }

    #[test]
    fn take_request_without_a_file_is_none() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(take_request(&paths_in(dir.path()), "docs"), None);
    }

    #[tokio::test]
    async fn take_request_returns_what_was_saved_and_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        save_request(&paths, "docs", BackupRequest::Check(CheckKind::Full))
            .await
            .unwrap();

        assert_eq!(
            take_request(&paths, "docs"),
            Some(BackupRequest::Check(CheckKind::Full))
        );
        assert!(!paths.backup_request_file("docs").exists());
        assert_eq!(take_request(&paths, "docs"), None);
    }

    #[test]
    fn take_request_deletes_a_file_it_cannot_understand() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let file = paths.backup_request_file("docs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"check":"everything"}"#).unwrap();

        assert_eq!(take_request(&paths, "docs"), None);
        assert!(!file.exists());
    }
}
