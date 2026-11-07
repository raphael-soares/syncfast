//! The history Activity shows: what each `runs/*.jsonl` log says about its
//! round. Read on demand, never on every poll.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::behavior::SyncMode;
use crate::bisync::{Outcome, classify};
use crate::bisync_log::{LogLine, RunSummary, parse_log, summarize};
use crate::notify::last_error_message;
use crate::oneway::{classify_oneway, exit_code_from_log, mode_of_changes_file, summarize_oneway};
use crate::paths::Paths;
use crate::run::read_changes;
use crate::state::{AutoReviewed, DisplayStatus, LastRun, RUNS_KEPT, State};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use serde::Deserialize;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

/// How many messages of a round's log the expanded row shows.
pub const MESSAGES_KEPT: usize = 200;

/// How much of the end of a log is read; a round that logs more than this
/// shows its last lines only.
const LOG_BYTES_KEPT: u64 = 4 * 1024 * 1024;
/// How much of the start of a log is searched for its first line, which
/// carries the time the round started.
const LOG_HEAD_BYTES: u64 = 4096;

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
    /// Set on the latest round when syncfast confirmed the review before it.
    pub auto_reviewed: Option<AutoReviewed>,
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
    /// The report of a one-way round, which is read with `classify_oneway`
    /// and `summarize_oneway` instead of the bisync readers.
    one_way: Option<OneWay>,
}

struct OneWay {
    mode: SyncMode,
    changes: String,
}

impl Known<'_> {
    fn one_way_exit_code(&self, lines: &[LogLine]) -> i32 {
        self.last
            .map_or_else(|| exit_code_from_log(lines), |(code, _)| code)
    }
}

#[derive(Deserialize)]
struct Stamped {
    time: Timestamp,
}

fn time_of(line: Option<&str>) -> Option<Timestamp> {
    serde_json::from_str::<Stamped>(line?).ok().map(|s| s.time)
}

fn status_of(outcome: &Outcome) -> DisplayStatus {
    match outcome {
        Outcome::Ok => DisplayStatus::UpToDate,
        Outcome::NeedsReview(_) | Outcome::TooManyDeletes | Outcome::AllFilesChanged => {
            DisplayStatus::NeedsReview
        }
        Outcome::AuthError | Outcome::ConfigError | Outcome::NeedsAttention { .. } => {
            DisplayStatus::Error
        }
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
    let outcome = match (&known.one_way, known.last) {
        (Some(_), _) => classify_oneway(known.one_way_exit_code(lines), lines),
        (None, Some((code, workdir))) => classify(code, lines, workdir),
        (None, None) if summary.conflicts_new.is_some() => Outcome::Ok,
        (None, None) => classify(EXIT_NOT_CRITICAL, lines, Path::new("")),
    };
    let detail = (outcome != Outcome::Ok)
        .then(|| last_error_message(lines).map(str::to_owned))
        .flatten();
    (status_of(&outcome), detail)
}

fn entry(file_name: &str, text: &str, known: &Known<'_>) -> RunEntry {
    let lines = parse_log(text);
    let summary = match &known.one_way {
        Some(one_way) if !known.in_progress => summarize_oneway(
            &one_way.changes,
            one_way.mode,
            known.one_way_exit_code(&lines),
        ),
        _ => summarize(&lines),
    };
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
        auto_reviewed: None,
    }
}

async fn dir_names(runs: &Path) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(runs).await else {
        return Vec::new();
    };
    let mut names = Vec::new();
    while let Ok(Some(item)) = entries.next_entry().await {
        names.push(item.file_name().to_string_lossy().into_owned());
    }
    names
}

fn log_names(names: &[String]) -> Vec<&str> {
    let mut logs: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        })
        .collect();
    logs.sort_unstable_by(|a, b| b.cmp(a));
    logs.truncate(RUNS_KEPT);
    logs
}

/// The report a one-way round kept next to `log`, `<stamp>.<mode>.changes`.
async fn one_way_of(runs: &Path, names: &[String], log: &str) -> Option<OneWay> {
    let stem = log.strip_suffix(".jsonl")?;
    let name = names
        .iter()
        .find(|name| name.starts_with(stem) && mode_of_changes_file(name).is_some())?;
    Some(OneWay {
        mode: mode_of_changes_file(name)?,
        changes: read_changes(&runs.join(name)).await,
    })
}

/// The log's text. Past [`LOG_BYTES_KEPT`] it is the first line (for the
/// start time) and the last complete lines.
async fn read_log(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let length = file.metadata().await?.len();
    let mut bytes = Vec::new();
    if length <= LOG_BYTES_KEPT {
        file.read_to_end(&mut bytes).await?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    (&mut file)
        .take(LOG_HEAD_BYTES)
        .read_to_end(&mut bytes)
        .await?;
    bytes.truncate(
        bytes
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(0, |end| end + 1),
    );
    // One byte before the window says whether its first line is whole.
    let start = length - LOG_BYTES_KEPT - 1;
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).await?;
    let whole = tail.first() == Some(&b'\n');
    let skip = if whole {
        1
    } else {
        tail.iter()
            .position(|&byte| byte == b'\n')
            .map_or(tail.len(), |end| end + 1)
    };
    bytes.extend_from_slice(&tail[skip..]);
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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
    let names = dir_names(&runs).await;
    for (index, name) in log_names(&names).into_iter().enumerate() {
        let text = match read_log(&runs.join(name)).await {
            Ok(text) => text,
            Err(err) => {
                log::warn!("cannot read {}: {err}", runs.join(name).display());
                continue;
            }
        };
        let latest = state
            .as_ref()
            .and_then(|state| state.last_run.as_ref())
            .filter(|last| is_log_of(last, &runs, name));
        let last = latest.map(|last| (last.exit_code, workdir.as_path()));
        let known = Known {
            in_progress: running && index == 0 && last.is_none(),
            last,
            one_way: one_way_of(&runs, &names, name).await,
        };
        let mut run = entry(name, &text, &known);
        run.auto_reviewed = latest.and_then(|last| last.auto_reviewed);
        entries.push(run);
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
            one_way: None,
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
            one_way: None,
        };

        let run = entry("a.jsonl", log, &known);

        assert_eq!(run.status, DisplayStatus::Error);
    }

    #[test]
    fn the_round_in_progress_is_syncing_and_has_no_duration() {
        let known = Known {
            in_progress: true,
            last: None,
            one_way: None,
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
                auto_reviewed: None,
            }),
            ..State::default()
        };
        state.save(&paths, "p").await.unwrap();

        let runs = load(&paths, "p", true).await;

        assert_eq!(runs[0].status, DisplayStatus::UpToDate);
    }

    #[tokio::test]
    async fn a_log_over_the_cap_still_parses_its_last_lines_and_keeps_its_start_time() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let filler = format!(
            "{{\"time\":\"2026-11-03T13:46:00.000-03:00\",\"level\":\"info\",\"msg\":\"{}\"}}\n",
            "x".repeat(1000)
        );
        let mut text = String::from(OK.lines().next().unwrap());
        text.push('\n');
        while u64::try_from(text.len()).unwrap() < LOG_BYTES_KEPT + 5000 {
            text.push_str(&filler);
        }
        text.push_str(OK.lines().nth(1).unwrap());
        text.push('\n');
        write_log(&paths, "20261103-120000.jsonl", &text);

        let runs = load(&paths, "p", false).await;

        assert_eq!(runs[0].status, DisplayStatus::UpToDate);
        assert_eq!(runs[0].messages.last().unwrap(), "Bisync successful");
        assert_eq!(runs[0].messages.len(), MESSAGES_KEPT);
        assert_eq!(runs[0].seconds, Some(3));
    }

    #[tokio::test]
    async fn reading_a_log_over_the_cap_drops_the_line_the_window_cuts() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("log");
        let line = "y".repeat(99) + "\n";
        let count = usize::try_from(LOG_BYTES_KEPT).unwrap() / line.len() + 3;
        std::fs::write(&file, line.repeat(count)).unwrap();

        let text = read_log(&file).await.unwrap();

        assert!(
            text.lines().all(|row| row.len() == 99),
            "a cut line was kept"
        );
        let cap = usize::try_from(LOG_BYTES_KEPT).unwrap();
        assert!(text.len() <= cap + line.len());
    }

    fn keep_round(paths: &Paths, stamp: &str, mode: &str, fixture: &str) {
        use crate::bisync_log::fixtures::fixture_path;
        let dir = paths.runs_dir("p");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(
            fixture_path(&format!("{fixture}.jsonl")),
            dir.join(format!("{stamp}.jsonl")),
        )
        .unwrap();
        std::fs::copy(
            fixture_path(&format!("{fixture}.changes")),
            dir.join(format!("{stamp}.{mode}.changes")),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn a_good_one_way_round_is_up_to_date_with_its_counters() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        keep_round(&paths, "20261107-163613", "mirror_up", "oneway_sync_delete");

        let runs = load(&paths, "p", false).await;

        assert_eq!(runs[0].status, DisplayStatus::UpToDate);
        assert_eq!(runs[0].detail, None);
        assert_eq!(
            (
                runs[0].summary.uploaded,
                runs[0].summary.deleted_remote,
                runs[0].summary.deleted_local
            ),
            (Some(3), Some(1), Some(0))
        );
    }

    #[tokio::test]
    async fn a_one_way_round_stopped_by_the_delete_cap_needs_review_without_counters() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        keep_round(
            &paths,
            "20261107-163700",
            "mirror_down",
            "oneway_sync_max_delete",
        );

        let runs = load(&paths, "p", false).await;

        assert_eq!(runs[0].status, DisplayStatus::NeedsReview);
        assert_eq!(runs[0].summary.deleted_local, None);
        assert_eq!(runs[0].summary.changes.len(), 2);
    }

    #[tokio::test]
    async fn a_one_way_round_that_failed_is_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        keep_round(&paths, "20261107-163800", "upload", "oneway_sync_errors");

        let runs = load(&paths, "p", false).await;

        assert_eq!(runs[0].status, DisplayStatus::Retrying);
    }

    #[tokio::test]
    async fn load_of_a_sync_with_no_rounds_is_empty() {
        let dir = tempfile::tempdir().unwrap();

        let runs = load(&paths(dir.path()), "p", false).await;

        assert!(runs.is_empty());
    }
}
