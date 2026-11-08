//! The history the Activity tab of a backup shows: what each
//! `backups/<id>/runs/*.jsonl` log says about its round. Read on demand,
//! never on every poll.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use jiff::Zoned;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::Value;
use syncfast_core::backup::state::CheckKind;
use syncfast_core::paths::Paths;
use syncfast_core::runs::MESSAGES_KEPT;
use syncfast_core::state::RUNS_KEPT;

const STAMP_FORMAT: &str = "%Y%m%d-%H%M%S";

/// What a round did: the backup itself or one of the checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Backup,
    /// `None` when the log does not say how much it read.
    Check(Option<CheckKind>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunResult {
    Running,
    Done,
    /// A snapshot exists, but this many files could not be read.
    DoneWithWarnings(u64),
    Failed,
}

/// One round, as a row of Activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRunEntry {
    /// The log's file name, unique among the rounds of a backup.
    pub id: String,
    /// In the system's time zone.
    pub started: Option<Zoned>,
    /// `None` while the round runs.
    pub seconds: Option<u64>,
    pub kind: RunKind,
    pub result: RunResult,
    pub files_new: Option<u64>,
    pub files_changed: Option<u64>,
    pub data_added: Option<u64>,
    /// Of a check: how many errors it found.
    pub check_errors: Option<u64>,
    /// The last [`MESSAGES_KEPT`] messages of the log, oldest first.
    pub messages: Vec<String>,
}

impl BackupRunEntry {
    #[must_use]
    pub fn running(&self) -> bool {
        self.result == RunResult::Running
    }

    /// New and changed files of a backup round.
    #[must_use]
    pub fn files_changed_total(&self) -> Option<u64> {
        Some(self.files_new? + self.files_changed.unwrap_or(0))
    }
}

#[derive(Deserialize)]
struct Line {
    #[serde(default)]
    syncfast_step: Option<String>,
    #[serde(default)]
    kind: Option<CheckKind>,
    #[serde(default)]
    message_type: Option<String>,
    #[serde(default)]
    snapshot_id: Option<String>,
    #[serde(default)]
    files_new: Option<u64>,
    #[serde(default)]
    files_changed: Option<u64>,
    #[serde(default)]
    data_added: Option<u64>,
    #[serde(default)]
    num_errors: Option<u64>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    item: Option<String>,
}

impl Line {
    fn error_text(&self) -> Option<String> {
        let message = match &self.error {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Object(body)) => body.get("message")?.as_str()?.to_owned(),
            _ => return None,
        };
        Some(match &self.item {
            Some(item) if !item.is_empty() => format!("{item}: {message}"),
            _ => message,
        })
    }
}

/// What the lines of a log add up to.
#[derive(Default)]
struct Tally {
    kind: Option<RunKind>,
    snapshot: bool,
    files_new: Option<u64>,
    files_changed: Option<u64>,
    data_added: Option<u64>,
    num_errors: Option<u64>,
    error_lines: u64,
    messages: Vec<String>,
}

impl Tally {
    fn add(&mut self, text: &str) {
        let Ok(line) = serde_json::from_str::<Line>(text) else {
            self.messages.push(text.to_owned());
            return;
        };
        if let Some(step) = &line.syncfast_step {
            self.kind = Some(if step == "check" {
                RunKind::Check(line.kind)
            } else {
                RunKind::Backup
            });
            return;
        }
        match line.message_type.as_deref() {
            Some("summary") => {
                self.snapshot |= line.snapshot_id.as_deref().is_some_and(|id| !id.is_empty());
                self.files_new = line.files_new.or(self.files_new);
                self.files_changed = line.files_changed.or(self.files_changed);
                self.data_added = line.data_added.or(self.data_added);
                self.num_errors = line.num_errors.or(self.num_errors);
            }
            Some("error") => {
                self.error_lines += 1;
                self.messages.extend(line.error_text());
            }
            Some("exit_error") => self.messages.extend(line.message),
            _ => {}
        }
    }
}

fn started_of(name: &str) -> Option<Zoned> {
    let stem = name.strip_suffix(".jsonl")?;
    DateTime::strptime(STAMP_FORMAT, stem)
        .ok()?
        .to_zoned(TimeZone::system())
        .ok()
}

fn seconds_between(started: &Zoned, finished: SystemTime) -> Option<u64> {
    let finished = jiff::Timestamp::try_from(finished).ok()?;
    let millis = finished.as_millisecond() - started.timestamp().as_millisecond();
    u64::try_from((millis + 500) / 1000).ok()
}

fn entry(
    name: &str,
    text: &str,
    finished: Option<SystemTime>,
    in_progress: bool,
) -> BackupRunEntry {
    let mut tally = Tally::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        tally.add(line);
    }
    let kind = tally.kind.unwrap_or(RunKind::Backup);
    let check_errors = match kind {
        RunKind::Check(_) => tally.num_errors.or(Some(tally.error_lines)),
        RunKind::Backup => None,
    };
    let result = if in_progress {
        RunResult::Running
    } else {
        match kind {
            RunKind::Backup if tally.snapshot && tally.error_lines > 0 => {
                RunResult::DoneWithWarnings(tally.error_lines)
            }
            RunKind::Backup if tally.snapshot => RunResult::Done,
            RunKind::Check(_) if tally.num_errors == Some(0) => RunResult::Done,
            _ => RunResult::Failed,
        }
    };
    let started = started_of(name);
    let seconds = match (in_progress, &started, finished) {
        (false, Some(started), Some(finished)) => seconds_between(started, finished),
        _ => None,
    };
    let skip = tally.messages.len().saturating_sub(MESSAGES_KEPT);
    BackupRunEntry {
        id: name.to_owned(),
        started,
        seconds,
        kind,
        result,
        files_new: tally.files_new,
        files_changed: tally.files_changed,
        data_added: tally.data_added,
        check_errors,
        messages: tally.messages.split_off(skip),
    }
}

/// The most recent rounds of a backup, newest first. `running` tells that a
/// round holds the lock: it is the newest log.
pub async fn load(paths: &Paths, backup_id: &str, running: bool) -> Arc<[BackupRunEntry]> {
    let runs = paths.backup_runs_dir(backup_id);
    let mut names = log_names(&runs).await;
    names.sort_unstable_by(|a, b| b.cmp(a));
    names.truncate(RUNS_KEPT);
    let mut entries = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let path = runs.join(name);
        let text = match tokio::fs::read(&path).await {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(err) => {
                log::warn!("cannot read {}: {err}", path.display());
                continue;
            }
        };
        let finished = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|meta| meta.modified().ok());
        entries.push(entry(name, &text, finished, running && index == 0));
    }
    entries.into()
}

async fn log_names(runs: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(mut dir) = tokio::fs::read_dir(runs).await else {
        return names;
    };
    while let Ok(Some(item)) = dir.next_entry().await {
        let name = item.file_name().to_string_lossy().into_owned();
        if Path::new(&name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    const OK: &str = concat!(
        r#"{"syncfast_step":"backup"}"#,
        "\n",
        r#"{"message_type":"summary","files_new":3,"files_changed":2,"files_unmodified":10,"data_added":2048,"snapshot_id":"abc123"}"#,
        "\n",
    );

    fn at(seconds: i64) -> SystemTime {
        let start = started_of("20260102-150405.jsonl").unwrap();
        SystemTime::from(
            jiff::Timestamp::from_second(start.timestamp().as_second() + seconds).unwrap(),
        )
    }

    #[test]
    fn a_log_with_a_snapshot_is_done_with_its_numbers_and_duration() {
        let run = entry("20260102-150405.jsonl", OK, Some(at(7)), false);

        assert_eq!(run.result, RunResult::Done);
        assert_eq!(run.kind, RunKind::Backup);
        assert_eq!(run.files_changed_total(), Some(5));
        assert_eq!(run.data_added, Some(2048));
        assert_eq!(run.seconds, Some(7));
    }

    #[test]
    fn unreadable_files_make_a_snapshot_a_done_with_warnings() {
        let text = format!(
            "{OK}{}\n",
            r#"{"message_type":"error","error":{"message":"permission denied"},"item":"/home/a/x"}"#
        );

        let run = entry("20260102-150405.jsonl", &text, Some(at(1)), false);

        assert_eq!(run.result, RunResult::DoneWithWarnings(1));
        assert_eq!(run.messages, ["/home/a/x: permission denied"]);
    }

    #[test]
    fn a_log_without_a_snapshot_failed_and_keeps_the_exit_message() {
        let text = concat!(
            r#"{"syncfast_step":"backup"}"#,
            "\n",
            r#"{"message_type":"exit_error","code":1,"message":"repository not found"}"#,
            "\n",
            "plain stderr line\n",
        );

        let run = entry("20260102-150405.jsonl", text, Some(at(2)), false);

        assert_eq!(run.result, RunResult::Failed);
        assert_eq!(run.messages, ["repository not found", "plain stderr line"]);
    }

    #[test]
    fn the_round_in_progress_has_no_duration() {
        let run = entry("20260102-150405.jsonl", OK, Some(at(9)), true);

        assert_eq!(run.result, RunResult::Running);
        assert_eq!(run.seconds, None);
    }

    #[test]
    fn a_check_log_reports_its_kind_and_its_errors() {
        let text = concat!(
            r#"{"syncfast_step":"check","kind":"sample"}"#,
            "\n",
            r#"{"message_type":"summary","num_errors":2}"#,
            "\n",
        );

        let run = entry("20260102-150405.jsonl", text, Some(at(3)), false);

        assert_eq!(run.kind, RunKind::Check(Some(CheckKind::Sample)));
        assert_eq!(run.check_errors, Some(2));
        assert_eq!(run.result, RunResult::Failed);
    }

    #[test]
    fn only_the_last_messages_of_a_long_log_are_kept() {
        let mut text = String::from("{\"syncfast_step\":\"backup\"}\n");
        for index in 0..MESSAGES_KEPT + 20 {
            writeln!(text, "line {index}").unwrap();
        }

        let run = entry("20260102-150405.jsonl", &text, None, false);

        assert_eq!(run.messages.len(), MESSAGES_KEPT);
        assert_eq!(run.messages.last().map(String::as_str), Some("line 219"));
    }

    #[test]
    fn the_name_of_the_log_is_the_start_in_local_time() {
        let started = started_of("20260102-150405.jsonl").unwrap();

        assert_eq!(
            started.strftime("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-01-02 15:04:05"
        );
        assert!(started_of("notes.txt").is_none());
    }

    fn paths(root: &Path) -> Paths {
        Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        )
    }

    fn write_log(paths: &Paths, name: &str, bytes: &[u8]) {
        let runs = paths.backup_runs_dir("docs");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(runs.join(name), bytes).unwrap();
    }

    #[tokio::test]
    async fn a_backup_that_never_ran_has_no_activity() {
        let dir = tempfile::tempdir().unwrap();

        assert!(load(&paths(dir.path()), "docs", false).await.is_empty());
    }

    #[tokio::test]
    async fn rounds_come_newest_first_and_other_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_log(&paths, "20260101-100000.jsonl", OK.as_bytes());
        write_log(&paths, "20260102-100000.jsonl", OK.as_bytes());
        write_log(&paths, "notes.txt", b"not a log");

        let runs = load(&paths, "docs", false).await;

        let ids: Vec<_> = runs.iter().map(|run| run.id.as_str()).collect();
        assert_eq!(ids, ["20260102-100000.jsonl", "20260101-100000.jsonl"]);
    }

    #[tokio::test]
    async fn only_the_newest_round_is_running_while_the_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_log(&paths, "20260101-100000.jsonl", OK.as_bytes());
        write_log(&paths, "20260102-100000.jsonl", OK.as_bytes());

        let runs = load(&paths, "docs", true).await;

        assert_eq!(runs[0].result, RunResult::Running);
        assert_eq!(runs[1].result, RunResult::Done);
    }

    #[tokio::test]
    async fn a_corrupt_or_empty_log_is_a_failed_round_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_log(&paths, "20260101-100000.jsonl", &[0xff, 0xfe, b'\n', b'{']);
        write_log(&paths, "20260102-100000.jsonl", b"");

        let runs = load(&paths, "docs", false).await;

        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| run.result == RunResult::Failed));
    }

    #[tokio::test]
    async fn the_log_view_keeps_the_last_two_hundred_lines_of_a_long_log() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let mut text = String::from("{\"syncfast_step\":\"backup\"}\n");
        for index in 0..500 {
            writeln!(text, "line {index}").unwrap();
        }
        write_log(&paths, "20260101-100000.jsonl", text.as_bytes());

        let runs = load(&paths, "docs", false).await;

        assert_eq!(runs[0].messages.len(), 200);
        assert_eq!(runs[0].messages[0], "line 300");
    }
}
