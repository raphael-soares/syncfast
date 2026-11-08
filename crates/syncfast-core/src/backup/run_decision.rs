//! What a backup round does before it touches restic: the pure decision
//! table of the spec (step 6) and the read of the source folders it takes as
//! input.

use std::path::PathBuf;

use crate::bisync::Fault;
use crate::state::{Gate, GateKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Paused,
    Gated,
    Offline,
    DestinationUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceCheck {
    Ok,
    /// Absent, or not a folder.
    Missing(PathBuf),
    /// A folder with no entry in it.
    Empty(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupDecision {
    Skip(SkipReason),
    Run,
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
}

/// The first row of the table that matches wins. A `config_error` gate that
/// the config no longer earns matches no row, so it is as if there were none.
#[must_use]
pub fn decide_backup(input: &DecisionInput<'_>) -> BackupDecision {
    if !input.enabled {
        return BackupDecision::Skip(SkipReason::Paused);
    }
    if input
        .gate
        .is_some_and(|gate| gate.kind == GateKind::AuthError)
    {
        return BackupDecision::Skip(SkipReason::Gated);
    }
    if input.dest_is_remote && !input.online {
        return BackupDecision::Skip(SkipReason::Offline);
    }
    if !input.dest_is_remote && !input.dest_available {
        return BackupDecision::Skip(SkipReason::DestinationUnavailable);
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
        SourceCheck::Ok => BackupDecision::Run,
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

    #[test]
    fn row_1_a_paused_backup_skips_whatever_else_is_true() {
        let auth = gate(GateKind::AuthError, None);

        let decision = decide_with(
            Some(&auth),
            DecisionInput {
                enabled: false,
                online: false,
                source_check: SourceCheck::Missing("/x".into()),
                ..input()
            },
        );

        assert_eq!(decision, BackupDecision::Skip(SkipReason::Paused));
    }

    #[test]
    fn row_2_an_auth_error_gate_skips() {
        let auth = gate(GateKind::AuthError, None);

        let decision = decide_with(
            Some(&auth),
            DecisionInput {
                online: false,
                ..input()
            },
        );

        assert_eq!(decision, BackupDecision::Skip(SkipReason::Gated));
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

        assert_eq!(decision, BackupDecision::Run);
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

        assert_eq!(decision, BackupDecision::Run);
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
        assert_eq!(decide_backup(&input()), BackupDecision::Run);
    }

    #[test]
    fn a_config_error_gate_is_as_if_there_were_none() {
        let stale = gate(GateKind::ConfigError, None);

        assert_eq!(decide_with(Some(&stale), input()), BackupDecision::Run);
    }

    #[test]
    fn an_attention_gate_does_not_stop_the_backup() {
        for fault in [None, Some(Fault::Quota), Some(Fault::KeyMissing)] {
            let attention = gate(GateKind::NeedsAttention, fault);

            assert_eq!(
                decide_with(Some(&attention), input()),
                BackupDecision::Run,
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
}
