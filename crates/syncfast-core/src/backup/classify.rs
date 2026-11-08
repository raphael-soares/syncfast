//! What a finished `restic backup` means (spec phase 3, step 7), and the
//! JSON lines it writes while it runs.

use serde::Deserialize;

use crate::bisync::{Fault, fault_of};

use super::state::BackupSummary;

const EXIT_OK: i32 = 0;
const EXIT_UNREADABLE: i32 = 3;
const EXIT_NO_REPOSITORY: i32 = 10;
const EXIT_LOCKED: i32 = 11;
const EXIT_WRONG_PASSWORD: i32 = 12;
const EXIT_INTERRUPTED: i32 = 130;
const SKIPPED_SUFFIX: &str = "does not exist, skipping";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupOutcome {
    Ok,
    /// A snapshot exists, but restic could not read some files.
    OkWithWarnings {
        warnings: u64,
    },
    Cancelled,
    /// The account refused the stored token.
    AuthError,
    NeedsAttention {
        fault: Fault,
        detail: String,
    },
    Locked {
        detail: String,
    },
    Transient {
        detail: String,
    },
}

/// The first row of the table that matches wins.
///
/// `summary` is the last `summary` line of stdout; `stderr_lines` are the
/// lines of stderr in order.
#[must_use]
pub fn classify_backup(
    exit_code: Option<i32>,
    summary: Option<&BackupSummary>,
    stderr_lines: &[String],
) -> BackupOutcome {
    let has_snapshot = summary.is_some_and(|summary| !summary.snapshot_id.is_empty());
    match exit_code {
        Some(EXIT_OK) if has_snapshot => return BackupOutcome::Ok,
        Some(EXIT_UNREADABLE) if has_snapshot => {
            return BackupOutcome::OkWithWarnings {
                warnings: count_warnings(stderr_lines),
            };
        }
        Some(EXIT_INTERRUPTED) => return BackupOutcome::Cancelled,
        Some(EXIT_WRONG_PASSWORD) => {
            return needs_attention(Fault::WrongKey, exit_code, stderr_lines);
        }
        Some(EXIT_NO_REPOSITORY) => {
            return needs_attention(Fault::RepositoryMissing, exit_code, stderr_lines);
        }
        _ => {}
    }
    match fault_of(&stderr_lines.join("\n")) {
        Some(Fault::Auth) => return BackupOutcome::AuthError,
        Some(
            fault @ (Fault::Account | Fault::Quota | Fault::Permission | Fault::RemoteMissing),
        ) => return needs_attention(fault, exit_code, stderr_lines),
        _ => {}
    }
    let detail = last_message(stderr_lines).unwrap_or_else(|| fallback_detail(exit_code));
    if exit_code == Some(EXIT_LOCKED) {
        return BackupOutcome::Locked { detail };
    }
    BackupOutcome::Transient { detail }
}

fn needs_attention(fault: Fault, exit_code: Option<i32>, stderr_lines: &[String]) -> BackupOutcome {
    let naming = stderr_lines
        .iter()
        .find(|line| fault_of(line) == Some(fault))
        .and_then(|line| message_of(line));
    let detail = naming
        .or_else(|| last_message(stderr_lines))
        .unwrap_or_else(|| fallback_detail(exit_code));
    BackupOutcome::NeedsAttention { fault, detail }
}

fn fallback_detail(exit_code: Option<i32>) -> String {
    match exit_code {
        Some(EXIT_OK) => "restic finished without a snapshot".to_owned(),
        Some(code) => format!("exit code {code}"),
        None => "killed by a signal".to_owned(),
    }
}

fn count_warnings(stderr_lines: &[String]) -> u64 {
    let count = stderr_lines
        .iter()
        .filter(|line| match parse_backup_line(line) {
            BackupLine::Error(_) => true,
            BackupLine::Other => is_plain(line) && line.trim_end().ends_with(SKIPPED_SUFFIX),
            _ => false,
        })
        .count();
    u64::try_from(count).unwrap_or(u64::MAX)
}

fn is_plain(line: &str) -> bool {
    !line.trim().is_empty() && !line.trim_start().starts_with('{')
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).trim().to_owned()
}

/// The message a stderr line carries: an `error` or `exit_error` line's, or
/// a plain line as it is.
fn message_of(line: &str) -> Option<String> {
    match parse_backup_line(line) {
        BackupLine::Error(message) | BackupLine::ExitError(message) => Some(first_line(&message)),
        BackupLine::Other if is_plain(line) => Some(line.trim().to_owned()),
        _ => None,
    }
}

/// The last `error` or `exit_error` message; failing that, the last plain
/// line.
fn last_message(stderr_lines: &[String]) -> Option<String> {
    let structured = stderr_lines
        .iter()
        .rev()
        .find_map(|line| match parse_backup_line(line) {
            BackupLine::Error(message) | BackupLine::ExitError(message) => {
                Some(first_line(&message))
            }
            _ => None,
        });
    structured.or_else(|| {
        stderr_lines
            .iter()
            .rev()
            .find(|line| is_plain(line))
            .map(|line| line.trim().to_owned())
    })
}

/// A `status` line: how far the backup has got.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BackupProgress {
    pub percent_done: Option<f64>,
    pub files_done: Option<u64>,
    pub total_files: Option<u64>,
    pub bytes_done: Option<u64>,
    pub total_bytes: Option<u64>,
    pub seconds_remaining: Option<u64>,
    pub current_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackupLine {
    Status(BackupProgress),
    Summary(BackupSummary),
    /// An `error` line of stderr: the message of one file that failed.
    Error(String),
    /// The `exit_error` line that ends a failed run.
    ExitError(String),
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "message_type", rename_all = "snake_case")]
enum RawLine {
    Status(BackupProgress),
    Summary(BackupSummary),
    Error {
        #[serde(default)]
        error: RawMessage,
    },
    ExitError {
        #[serde(default)]
        message: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Default)]
struct RawMessage {
    #[serde(default)]
    message: String,
}

/// A line that is not JSON, or is a kind of message nobody reads, is
/// [`BackupLine::Other`].
#[must_use]
pub fn parse_backup_line(line: &str) -> BackupLine {
    match serde_json::from_str::<RawLine>(line) {
        Ok(RawLine::Status(progress)) => BackupLine::Status(progress),
        Ok(RawLine::Summary(summary)) => BackupLine::Summary(summary),
        Ok(RawLine::Error { error }) => BackupLine::Error(error.message),
        Ok(RawLine::ExitError { message }) => BackupLine::ExitError(message),
        Ok(RawLine::Other) | Err(_) => BackupLine::Other,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::Path;

    use super::*;

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/restic-0.19.1")
            .join(name);
        String::from_utf8(std::fs::read(path).unwrap()).unwrap()
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    fn summary_of(stdout: &str) -> Option<BackupSummary> {
        stdout
            .lines()
            .rev()
            .find_map(|line| match parse_backup_line(line) {
                BackupLine::Summary(summary) => Some(summary),
                _ => None,
            })
    }

    fn exit_of(scenario: &str) -> Option<i32> {
        fixture(&format!("{scenario}.exit")).trim().parse().ok()
    }

    fn classify_fixture(scenario: &str) -> BackupOutcome {
        let summary = summary_of(&fixture(&format!("{scenario}.out")));
        let stderr = lines(&fixture(&format!("{scenario}.err")));
        classify_backup(exit_of(scenario), summary.as_ref(), &stderr)
    }

    fn snapshot() -> BackupSummary {
        BackupSummary {
            snapshot_id: "d8e1bca8".to_owned(),
            ..BackupSummary::default()
        }
    }

    #[test]
    fn row_1_exit_0_with_a_snapshot_is_ok() {
        assert_eq!(classify_fixture("backup_ok"), BackupOutcome::Ok);
    }

    #[test]
    fn row_2_exit_3_with_a_snapshot_counts_the_files_restic_could_not_read() {
        assert_eq!(
            classify_fixture("backup_unreadable"),
            BackupOutcome::OkWithWarnings { warnings: 1 }
        );
    }

    #[test]
    fn row_2_a_source_that_vanished_counts_as_a_warning() {
        assert_eq!(
            classify_fixture("backup_missing_path"),
            BackupOutcome::OkWithWarnings { warnings: 1 }
        );
    }

    #[test]
    fn row_2_counts_every_error_line() {
        let stderr = lines(concat!(
            "{\"message_type\":\"error\",\"error\":{\"message\":\"a: permission denied\"}}\n",
            "{\"message_type\":\"error\",\"error\":{\"message\":\"b: permission denied\"}}\n",
            "{\"message_type\":\"error\",\"error\":{\"message\":\"c: permission denied\"}}\n",
            "{\"message_type\":\"exit_error\",\"code\":3,\"message\":\"Warning: at least one source file could not be read\"}\n",
        ));

        assert_eq!(
            classify_backup(Some(3), Some(&snapshot()), &stderr),
            BackupOutcome::OkWithWarnings { warnings: 3 }
        );
    }

    #[test]
    fn rows_1_and_2_need_a_snapshot() {
        let stderr = lines(
            "{\"message_type\":\"exit_error\",\"code\":3,\"message\":\"Warning: at least one source file could not be read\"}",
        );
        let without_id = BackupSummary::default();

        for (code, summary) in [
            (0, None),
            (0, Some(&without_id)),
            (3, None),
            (3, Some(&without_id)),
        ] {
            assert!(
                matches!(
                    classify_backup(Some(code), summary, &stderr),
                    BackupOutcome::Transient { .. }
                ),
                "exit {code}"
            );
        }
    }

    #[test]
    fn row_3_exit_130_is_cancelled() {
        assert_eq!(
            classify_backup(Some(130), None, &[]),
            BackupOutcome::Cancelled
        );
    }

    #[test]
    fn row_4_exit_12_is_a_wrong_key() {
        assert_eq!(
            classify_fixture("wrong_password"),
            BackupOutcome::NeedsAttention {
                fault: Fault::WrongKey,
                detail: "Fatal: wrong password or no key found".to_owned(),
            }
        );
    }

    #[test]
    fn row_5_exit_10_is_a_missing_repository_and_keeps_the_first_line() {
        assert_eq!(
            classify_fixture("no_repo"),
            BackupOutcome::NeedsAttention {
                fault: Fault::RepositoryMissing,
                detail: "Fatal: repository does not exist: unable to open config file: <config/> does not exist"
                    .to_owned(),
            }
        );
    }

    #[test]
    fn row_6_rclone_authentication_text_is_an_auth_error() {
        for text in [
            "Failed to create file system: couldn't fetch token: invalid_grant",
            "oauth2: cannot fetch token: 400 Bad Request",
        ] {
            let stderr = vec![
                text.to_owned(),
                "{\"message_type\":\"exit_error\",\"code\":1,\"message\":\"Fatal: unable to open repository\"}"
                    .to_owned(),
            ];

            assert_eq!(
                classify_backup(Some(1), None, &stderr),
                BackupOutcome::AuthError,
                "{text}"
            );
        }
    }

    #[test]
    fn row_7_account_quota_permission_and_missing_folder_need_attention() {
        let cases = [
            (
                "Error 403: Google Drive API has not been used in project 1 before or it is disabled",
                Fault::Account,
            ),
            ("googleapi: Error 403: storageQuotaExceeded", Fault::Quota),
            ("googleapi: insufficientFilePermissions", Fault::Permission),
            ("directory not found", Fault::RemoteMissing),
        ];

        for (text, fault) in cases {
            let stderr = vec![
                "ignored first line".to_owned(),
                text.to_owned(),
                "{\"message_type\":\"exit_error\",\"code\":1,\"message\":\"Fatal: unable to save snapshot\"}"
                    .to_owned(),
            ];

            assert_eq!(
                classify_backup(Some(1), None, &stderr),
                BackupOutcome::NeedsAttention {
                    fault,
                    detail: text.to_owned(),
                },
                "{text}"
            );
        }
    }

    #[test]
    fn row_7_names_the_fault_with_the_message_of_a_json_error_line() {
        let stderr = vec![
            r#"{"message_type":"error","error":{"message":"directory not found"},"during":"x","item":"y"}"#
                .to_owned(),
            r#"{"message_type":"exit_error","code":1,"message":"Fatal: unable to save snapshot"}"#.to_owned(),
        ];

        assert_eq!(
            classify_backup(Some(1), None, &stderr),
            BackupOutcome::NeedsAttention {
                fault: Fault::RemoteMissing,
                detail: "directory not found".to_owned(),
            }
        );
    }

    #[test]
    fn row_7_ignores_a_broken_json_line_when_it_picks_the_detail() {
        let stderr = vec![
            r"{broken: directory not found".to_owned(),
            "the plain line".to_owned(),
        ];

        assert_eq!(
            classify_backup(Some(1), None, &stderr),
            BackupOutcome::NeedsAttention {
                fault: Fault::RemoteMissing,
                detail: "the plain line".to_owned(),
            }
        );
    }

    #[test]
    fn row_2_does_not_count_a_broken_json_line_that_ends_like_a_skipped_source() {
        let stderr = vec![r"{broken /gone does not exist, skipping".to_owned()];

        assert_eq!(
            classify_backup(Some(3), Some(&snapshot()), &stderr),
            BackupOutcome::OkWithWarnings { warnings: 0 }
        );
    }

    #[test]
    fn row_7_wins_over_exit_3_without_a_snapshot() {
        let stderr = vec!["googleapi: Error 403: storageQuotaExceeded".to_owned()];

        assert!(matches!(
            classify_backup(Some(3), None, &stderr),
            BackupOutcome::NeedsAttention {
                fault: Fault::Quota,
                ..
            }
        ));
    }

    #[test]
    fn row_8_exit_11_is_locked() {
        assert_eq!(
            classify_fixture("locked"),
            BackupOutcome::Locked {
                detail: "unable to create lock in backend: repository is already locked by PID 1943426 on fixture-host by user (UID 1000, GID 1000)".to_owned(),
            }
        );
    }

    #[test]
    fn row_9_anything_else_is_transient_with_the_last_message() {
        let stderr = lines(concat!(
            "{\"message_type\":\"error\",\"error\":{\"message\":\"connection reset\"}}\n",
            "{\"message_type\":\"exit_error\",\"code\":1,\"message\":\"Fatal: unable to save snapshot: connection reset\"}\n",
        ));

        assert_eq!(
            classify_backup(Some(1), None, &stderr),
            BackupOutcome::Transient {
                detail: "Fatal: unable to save snapshot: connection reset".to_owned(),
            }
        );
    }

    #[test]
    fn row_9_an_unknown_exit_code_is_transient() {
        for code in [1, 2, 42] {
            assert_eq!(
                classify_backup(Some(code), None, &[]),
                BackupOutcome::Transient {
                    detail: format!("exit code {code}"),
                }
            );
        }
    }

    #[test]
    fn row_9_a_signal_has_no_exit_code() {
        assert_eq!(
            classify_backup(None, None, &[]),
            BackupOutcome::Transient {
                detail: "killed by a signal".to_owned(),
            }
        );
    }

    #[test]
    fn row_9_without_json_the_last_plain_line_is_the_detail() {
        let stderr = lines("first\nsecond\n\n");

        assert_eq!(
            classify_backup(Some(1), None, &stderr),
            BackupOutcome::Transient {
                detail: "second".to_owned(),
            }
        );
    }

    #[test]
    fn a_status_fixture_ends_with_the_summary_of_a_snapshot() {
        let stdout = fixture("backup_status.out");
        let parsed: Vec<BackupLine> = stdout.lines().map(parse_backup_line).collect();

        let statuses = parsed
            .iter()
            .filter(|line| matches!(line, BackupLine::Status(_)))
            .count();
        let summary = summary_of(&stdout).unwrap();
        assert!(matches!(parsed.last(), Some(BackupLine::Summary(_))));

        assert_eq!(parsed.len(), 17);
        assert_eq!(statuses, 16);
        assert!(!summary.snapshot_id.is_empty());
        assert_eq!(
            classify_backup(Some(0), Some(&summary), &[]),
            BackupOutcome::Ok
        );
    }

    #[test]
    fn a_status_line_reads_its_numbers() {
        let line = r#"{"message_type":"status","seconds_elapsed":1,"seconds_remaining":15,"percent_done":0.0619,"total_files":2,"files_done":1,"total_bytes":1610612742,"bytes_done":99851592,"current_files":["/tmp/fixture/src/big.bin"]}"#;

        assert_eq!(
            parse_backup_line(line),
            BackupLine::Status(BackupProgress {
                percent_done: Some(0.0619),
                files_done: Some(1),
                total_files: Some(2),
                bytes_done: Some(99_851_592),
                total_bytes: Some(1_610_612_742),
                seconds_remaining: Some(15),
                current_files: vec!["/tmp/fixture/src/big.bin".to_owned()],
            })
        );
    }

    #[test]
    fn a_status_line_may_lack_the_estimates() {
        let line = r#"{"message_type":"status","total_files":3}"#;

        assert_eq!(
            parse_backup_line(line),
            BackupLine::Status(BackupProgress {
                total_files: Some(3),
                ..BackupProgress::default()
            })
        );
    }

    #[test]
    fn the_summary_line_of_a_backup_reads_what_it_added() {
        let summary = summary_of(&fixture("backup_ok.out")).unwrap();

        assert_eq!(summary.files_new, 5);
        assert_eq!(summary.files_changed, 0);
        assert_eq!(summary.data_added, 4464);
        assert_eq!(summary.data_added_packed, 3101);
        assert_eq!(summary.total_files_processed, 5);
        assert_eq!(summary.total_bytes_processed, 41);
        assert!(!summary.snapshot_id.is_empty());
    }

    #[test]
    fn error_and_exit_error_lines_carry_their_messages() {
        let err = fixture("backup_unreadable.err");
        let parsed: Vec<BackupLine> = err.lines().map(parse_backup_line).collect();

        assert!(
            matches!(&parsed[0], BackupLine::Error(message) if message.ends_with("permission denied"))
        );
        assert_eq!(
            parsed[1],
            BackupLine::ExitError("Warning: at least one source file could not be read".to_owned())
        );
    }

    #[test]
    fn text_that_is_not_a_known_message_is_other() {
        for line in [
            "",
            "plain text",
            "{not json",
            r#"{"message_type":"verbose_status","action":"new"}"#,
            r#"{"no_type":1}"#,
        ] {
            assert_eq!(parse_backup_line(line), BackupLine::Other, "{line}");
        }
    }
}
