//! The history Activity shows: what each `runs/*.jsonl` log says about its
//! round. Read on demand, never on every poll.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use serde::Deserialize;
use syncfast_core::bisync::{Outcome, classify};
use syncfast_core::bisync_log::{LogLine, RunSummary, parse_log, summarize};
use syncfast_core::notify::last_error_message;
use syncfast_core::paths::Paths;
use syncfast_core::state::{DisplayStatus, LastRun, RUNS_KEPT, State};

/// How many messages of a round's log the expanded row shows.
pub const MESSAGES_KEPT: usize = 200;

/// rclone's exit code for a failure that is not critical. A round that did
/// not finish and whose exit code was not kept is classified as this one.
const EXIT_NOT_CRITICAL: i32 = 1;

/// One round, as a row of Activity.
#[derive(Debug, Clone, PartialEq)]
pub struct RunEntry {
    /// The log's file name, unique among the rounds of a sync.
    pub id: String,
    /// In the system's time zone.
    pub started: Option<Zoned>,
    /// `None` while the round runs.
    pub seconds: Option<u64>,
    /// [`DisplayStatus::Syncing`] for the round in progress.
    pub status: DisplayStatus,
    pub summary: RunSummary,
    /// The newest error line of a round that failed.
    pub detail: Option<String>,
    /// The last [`MESSAGES_KEPT`] messages of the log, oldest first.
    pub messages: Vec<String>,
}

impl RunEntry {
    #[must_use]
    pub fn running(&self) -> bool {
        self.status == DisplayStatus::Syncing
    }
}

/// What the reader knows about a round besides its log.
struct Known<'a> {
    in_progress: bool,
    /// The exit code and workdir, kept only for the sync's latest round.
    last: Option<(i32, &'a Path)>,
}

#[derive(Deserialize)]
struct Stamped {
    time: Timestamp,
}

fn time_of(line: Option<&str>) -> Option<Timestamp> {
    serde_json::from_str::<Stamped>(line?).ok().map(|s| s.time)
}

fn status_of(outcome: Outcome) -> DisplayStatus {
    match outcome {
        Outcome::Ok => DisplayStatus::UpToDate,
        Outcome::NeedsReview(_) | Outcome::TooManyDeletes | Outcome::AllFilesChanged => {
            DisplayStatus::NeedsReview
        }
        Outcome::AuthError | Outcome::ConfigError => DisplayStatus::Error,
        Outcome::Transient => DisplayStatus::Retrying,
    }
}

/// Only the latest round keeps its exit code (in `state.json`). An older log
/// is a success when it ends with rclone's success notice and a transient
/// failure otherwise, and it is judged with no workdir: the leftovers there
/// belong to the latest round.
fn status_and_detail(
    lines: &[LogLine],
    summary: &RunSummary,
    known: &Known<'_>,
) -> (DisplayStatus, Option<String>) {
    if known.in_progress {
        return (DisplayStatus::Syncing, None);
    }
    let outcome = match known.last {
        Some((code, workdir)) => classify(code, lines, workdir),
        None if summary.conflicts_new.is_some() => Outcome::Ok,
        None => classify(EXIT_NOT_CRITICAL, lines, Path::new("")),
    };
    let detail = (outcome != Outcome::Ok)
        .then(|| last_error_message(lines).map(str::to_owned))
        .flatten();
    (status_of(outcome), detail)
}

fn entry(file_name: &str, text: &str, known: &Known<'_>) -> RunEntry {
    let lines = parse_log(text);
    let summary = summarize(&lines);
    let mut raw = text.lines().filter(|line| !line.trim().is_empty());
    let first = time_of(raw.next());
    let last = time_of(text.lines().rev().find(|line| !line.trim().is_empty()));
    let seconds = match (known.in_progress, first, last) {
        (false, Some(first), Some(last)) => {
            let millis = last.as_millisecond() - first.as_millisecond();
            Some(u64::try_from((millis + 500) / 1000).unwrap_or(0))
        }
        _ => None,
    };
    let (status, detail) = status_and_detail(&lines, &summary, known);
    let skip = lines.len().saturating_sub(MESSAGES_KEPT);
    let messages = lines
        .iter()
        .skip(skip)
        .map(|line| line.msg.trim().to_owned())
        .filter(|msg| !msg.is_empty())
        .collect();
    RunEntry {
        id: file_name.to_owned(),
        started: first.map(|at| at.to_zoned(TimeZone::system())),
        seconds,
        status,
        summary,
        detail,
        messages,
    }
}

async fn log_names(runs: &Path) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(runs).await else {
        return Vec::new();
    };
    let mut names = Vec::new();
    while let Ok(Some(item)) = entries.next_entry().await {
        let name = item.file_name().to_string_lossy().into_owned();
        if Path::new(&name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        {
            names.push(name);
        }
    }
    names.sort_unstable_by(|a, b| b.cmp(a));
    names.truncate(RUNS_KEPT);
    names
}

fn is_log_of(last: &LastRun, runs: &Path, name: &str) -> bool {
    last.log_file == runs.join(name)
}

/// The most recent rounds of a sync, newest first. `running` tells that a
/// round holds the lock: its log is the newest one, unless that log is the
/// latest finished round's and the new round has not created its own yet.
pub async fn load(paths: &Paths, pair_id: &str, running: bool) -> Arc<[RunEntry]> {
    let runs = paths.runs_dir(pair_id);
    let state = State::load(paths, pair_id).await.ok();
    let workdir: PathBuf = paths.bisync_workdir(pair_id);
    let mut entries = Vec::new();
    for (index, name) in log_names(&runs).await.iter().enumerate() {
        let text = match tokio::fs::read(runs.join(name)).await {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(err) => {
                log::warn!("cannot read {}: {err}", runs.join(name).display());
                continue;
            }
        };
        let last = state
            .as_ref()
            .and_then(|state| state.last_run.as_ref())
            .filter(|last| is_log_of(last, &runs, name))
            .map(|last| (last.exit_code, workdir.as_path()));
        let finished_last = state
            .as_ref()
            .and_then(|state| state.last_run.as_ref())
            .is_some_and(|last| is_log_of(last, &runs, name));
        let known = Known {
            in_progress: running && index == 0 && !finished_last,
            last,
        };
        entries.push(entry(name, &text, &known));
    }
    entries.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = concat!(
        r#"{"time":"2026-11-03T13:45:45.855-03:00","level":"info","msg":"Synching Path1"}"#,
        "\n",
        r#"{"time":"2026-11-03T13:45:49.100-03:00","level":"info","msg":"Bisync successful"}"#,
        "\n",
    );
    const FAILED: &str = concat!(
        r#"{"time":"2026-11-03T13:50:00.000-03:00","level":"info","msg":"Synching Path1"}"#,
        "\n",
        r#"{"time":"2026-11-03T13:50:06.000-03:00","level":"error","msg":"network error"}"#,
        "\n",
        r#"{"time":"2026-11-03T13:50:06.500-03:00","level":"notice","msg":"Failed to bisync: network error"}"#,
        "\n",
    );

    fn finished() -> Known<'static> {
        Known {
            in_progress: false,
            last: None,
        }
    }

    #[test]
    fn a_log_ending_in_the_success_notice_is_up_to_date_with_its_duration() {
        let run = entry("20261103-134545.jsonl", OK, &finished());

        assert_eq!(run.status, DisplayStatus::UpToDate);
        assert_eq!(run.seconds, Some(3));
        assert_eq!(run.detail, None);
    }

    #[test]
    fn an_older_log_that_did_not_finish_will_retry_and_keeps_the_error_line() {
        let run = entry("20261103-135000.jsonl", FAILED, &finished());

        assert_eq!(run.status, DisplayStatus::Retrying);
        assert_eq!(run.detail.as_deref(), Some("network error"));
    }

    #[test]
    fn the_latest_round_is_classified_with_its_kept_exit_code() {
        let log = r#"{"level":"notice","msg":"Failed to bisync: invalid_grant"}"#;
        let known = Known {
            in_progress: false,
            last: Some((EXIT_NOT_CRITICAL, Path::new(""))),
        };

        let run = entry("a.jsonl", log, &known);

        assert_eq!(run.status, DisplayStatus::Error);
    }

    #[test]
    fn the_round_in_progress_is_syncing_and_has_no_duration() {
        let known = Known {
            in_progress: true,
            last: None,
        };

        let run = entry("a.jsonl", FAILED, &known);

        assert_eq!(run.status, DisplayStatus::Syncing);
        assert_eq!(run.seconds, None);
        assert!(run.running());
    }

    #[test]
    fn only_the_last_200_messages_are_kept_oldest_first() {
        let lines: Vec<String> = (0..250)
            .map(|n| format!("{{\"level\":\"info\",\"msg\":\"m{n}\"}}"))
            .collect();
        let text = lines.join("\n");

        let run = entry("a.jsonl", &text, &finished());

        assert_eq!(run.messages.len(), MESSAGES_KEPT);
        assert_eq!(run.messages.first().map(String::as_str), Some("m50"));
        assert_eq!(run.messages.last().map(String::as_str), Some("m249"));
    }

    fn write_log(paths: &Paths, name: &str, text: &str) {
        let dir = paths.runs_dir("p");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), text).unwrap();
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

    #[tokio::test]
    async fn load_lists_the_newest_fifty_logs_first_and_ignores_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        for n in 0..55 {
            write_log(&paths, &format!("20261103-1200{n:02}.jsonl"), OK);
        }
        write_log(&paths, "notes.txt", "x");

        let runs = load(&paths, "p", false).await;

        assert_eq!(runs.len(), RUNS_KEPT);
        assert_eq!(runs[0].id, "20261103-120054.jsonl");
        assert_eq!(runs[49].id, "20261103-120005.jsonl");
    }

    #[tokio::test]
    async fn load_marks_only_the_newest_log_as_running_while_a_round_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_log(&paths, "20261103-120000.jsonl", OK);
        write_log(&paths, "20261103-130000.jsonl", FAILED);

        let runs = load(&paths, "p", true).await;

        assert!(runs[0].running());
        assert!(!runs[1].running());
    }

    #[tokio::test]
    async fn load_does_not_mark_a_finished_round_as_running_before_the_new_one_has_a_log() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_log(&paths, "20261103-120000.jsonl", OK);
        let state = State {
            last_run: Some(LastRun {
                started: Timestamp::UNIX_EPOCH,
                finished: Timestamp::UNIX_EPOCH,
                exit_code: 0,
                summary: RunSummary::default(),
                log_file: paths.runs_dir("p").join("20261103-120000.jsonl"),
            }),
            ..State::default()
        };
        state.save(&paths, "p").await.unwrap();

        let runs = load(&paths, "p", true).await;

        assert_eq!(runs[0].status, DisplayStatus::UpToDate);
    }

    #[tokio::test]
    async fn load_of_a_sync_with_no_rounds_is_empty() {
        let dir = tempfile::tempdir().unwrap();

        let runs = load(&paths(dir.path()), "p", false).await;

        assert!(runs.is_empty());
    }
}
