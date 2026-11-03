//! One round of one pair: `syncfast run <id>` (spec step 7). The round
//! decides whether it may run, runs `rclone bisync`, and turns how it ended
//! into the pair's state and, when the user must act, a notification.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;

use jiff::{SignedDuration, Timestamp, Zoned};
use rustix::process::{Pid, Signal};
use tokio::process::Child;
use tokio::signal::unix::{SignalKind, signal};

use crate::behavior::Behavior;
use crate::bisync::{Mode, Outcome, bisync_args, classify, log_file};
use crate::bisync_log::{LogLine, read_log, summarize};
use crate::config::{Config, GlobalConfigError, Pair, PairError};
use crate::conflicts::find_conflicts;
use crate::filters::{self, DirMatcher, FiltersError};
use crate::network::Network;
use crate::notify::{Alert, Notifier, last_error_message, notice};
use crate::paths::Paths;
use crate::rclone::{Rclone, RcloneError};
use crate::run_decision::{DecideInput, Decision, SkipReason, decide};
use crate::state::{
    ConfirmMarker, Gate, GateKind, LastOutcome, LastRun, NOTIFIED_FAILURES, State, StateError,
    prune_runs, try_lock_pair,
};
use crate::versions;

/// Changes kept in `state.json`: the GUI rereads it every second or two, and
/// the full list stays in the run's log.
pub const STORED_CHANGES: usize = 100;
/// Failed rounds in a row that raise the "sync failing" notification.
const NOTIFY_FAILURES_AFTER: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    Config(#[from] GlobalConfigError),
    #[error("no sync has the id \"{0}\"")]
    UnknownPair(String),
    #[error("{0}")]
    Rclone(#[from] RcloneError),
    #[error("{0}")]
    State(#[from] StateError),
    #[error("{0}")]
    Filters(#[from] FiltersError),
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl RunError {
    fn io(context: &'static str) -> impl FnOnce(std::io::Error) -> Self {
        move |source| Self::Io { context, source }
    }

    /// 2 for a config the user must fix, 1 for anything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config(_) | Self::UnknownPair(_) => 2,
            _ => 1,
        }
    }
}

/// What a round needs from the outside world.
pub struct RunEnv<'a> {
    pub paths: &'a Paths,
    pub rclone: &'a Rclone,
    pub network: &'a dyn Network,
    pub notifier: &'a dyn Notifier,
    pub clock: &'a (dyn Fn() -> Zoned + Send + Sync),
}

impl std::fmt::Debug for RunEnv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunEnv")
            .field("paths", &self.paths)
            .field("rclone", &self.rclone)
            .finish_non_exhaustive()
    }
}

impl RunEnv<'_> {
    pub(crate) fn now(&self) -> Zoned {
        (self.clock)()
    }
}

/// How a round ended. Every variant is a normal end (exit code 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunReport {
    /// Another round of the pair holds the lock.
    Busy,
    /// The pair is invalid; its gate is now `config_error`.
    ConfigInvalid,
    Skipped(SkipReason),
    GateSet(GateKind),
    Ran {
        mode: Mode,
        outcome: Outcome,
    },
}

pub(crate) enum PairLookup {
    Valid(Pair),
    /// The pair as written in the config, and why it cannot run.
    Invalid(Pair, PairError),
}

pub(crate) async fn lookup_pair(env: &RunEnv<'_>, pair_id: &str) -> Result<PairLookup, RunError> {
    let config = Config::load(env.paths).await?;
    if !config.pairs().iter().any(|pair| pair.id == pair_id) {
        return Err(RunError::UnknownPair(pair_id.to_owned()));
    }
    let accounts = env.rclone.listremotes().await?;
    let validation = config.validate(env.paths, &accounts);
    if let Some(pair) = validation.pairs.into_iter().find(|pair| pair.id == pair_id) {
        return Ok(PairLookup::Valid(pair));
    }
    let written = config.pairs().iter().find(|pair| pair.id == pair_id);
    let error = validation
        .errors
        .into_iter()
        .find(|error| error.pair_id == pair_id);
    match (written, error) {
        (Some(pair), Some(error)) => Ok(PairLookup::Invalid(pair.clone(), error)),
        _ => Err(RunError::UnknownPair(pair_id.to_owned())),
    }
}

/// Records `kind` as the gate, keeping `since` when the pair was already
/// gated for the same reason.
fn set_gate(state: &mut State, kind: GateKind, detail: String, now: Timestamp) {
    let since = state
        .gate
        .as_ref()
        .filter(|gate| gate.kind == kind)
        .map_or(now, |gate| gate.since);
    state.gate = Some(Gate {
        kind,
        since,
        detail,
    });
}

/// Notifies once per transition: the key stays in `state.notified` until a
/// round succeeds (or the gate is cleared).
fn announce(env: &RunEnv<'_>, pair: &Pair, state: &mut State, key: String, alert: &Alert) {
    if state.notified.contains(&key) {
        return;
    }
    env.notifier.notify(&notice(pair, alert));
    state.notified.push(key);
}

fn announce_gate(env: &RunEnv<'_>, pair: &Pair, state: &mut State, kind: GateKind) {
    let detail = state
        .gate
        .as_ref()
        .map(|gate| gate.detail.clone())
        .unwrap_or_default();
    announce(
        env,
        pair,
        state,
        kind.notified_key(),
        &Alert::Gate { kind, detail },
    );
}

fn forward_interrupt(child: &Child) {
    let Some(pid) = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
    else {
        return;
    };
    if let Err(err) = rustix::process::kill_process(pid, Signal::INT) {
        log::warn!("cannot interrupt rclone: {err}");
    }
}

/// Runs rclone with `args` and waits for it. rclone's own output is
/// dropped (its log goes to `--log-file`). SIGINT and SIGTERM do not stop
/// the round: they are passed to rclone as SIGINT, which makes bisync shut
/// down cleanly, and the round waits for it.
///
/// # Errors
/// rclone cannot be started or waited for.
pub(crate) async fn execute(rclone: &Rclone, args: Vec<OsString>) -> Result<i32, RunError> {
    let mut interrupt = signal(SignalKind::interrupt()).map_err(RunError::io("signal handler"))?;
    let mut terminate = signal(SignalKind::terminate()).map_err(RunError::io("signal handler"))?;
    let mut child = rclone
        .command(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(RcloneError::Spawn)?;
    loop {
        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(RunError::io("waiting for rclone"))?;
                return Ok(status.code().unwrap_or(-1));
            }
            _ = interrupt.recv() => forward_interrupt(&child),
            _ = terminate.recv() => forward_interrupt(&child),
        }
    }
}

pub(crate) async fn read_log_or_empty(path: &Path) -> Vec<LogLine> {
    match read_log(path).await {
        Ok(lines) => lines,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                log::warn!("cannot read {}: {err}", path.display());
            }
            Vec::new()
        }
    }
}

/// A free log name: two rounds that start in the same second must not
/// share a log file, because rclone appends to an existing one.
pub(crate) fn fresh_start(paths: &Paths, pair_id: &str, mode: Mode, mut now: Zoned) -> Zoned {
    while log_file(paths, pair_id, mode, &now).exists() {
        match now.checked_add(SignedDuration::from_secs(1)) {
            Ok(next) => now = next,
            Err(_) => break,
        }
    }
    now
}

/// Runs one round of `pair_id`.
///
/// # Errors
/// The config has a global error or no sync has this id (exit code 2), or
/// syncfast itself failed on rclone, state or a file (exit code 1).
pub async fn run(env: &RunEnv<'_>, pair_id: &str) -> Result<RunReport, RunError> {
    let pair = match lookup_pair(env, pair_id).await? {
        PairLookup::Valid(pair) => pair,
        PairLookup::Invalid(pair, error) => {
            let mut state = State::load(env.paths, pair_id).await?;
            set_gate(
                &mut state,
                GateKind::ConfigError,
                error.message,
                env.now().timestamp(),
            );
            announce_gate(env, &pair, &mut state, GateKind::ConfigError);
            state.save(env.paths, pair_id).await?;
            return Ok(RunReport::ConfigInvalid);
        }
    };
    let Some(_lock) = try_lock_pair(env.paths, pair_id)? else {
        return Ok(RunReport::Busy);
    };
    let behavior = Behavior::for_pair(&pair);
    let online = env.network.is_online().await;
    let filters_file = filters::write_if_changed(env.paths, &pair).await?;
    let mut state = State::load(env.paths, pair_id).await?;
    let marker = ConfirmMarker::load(env.paths, pair_id).await?;
    let local_exists = tokio::fs::metadata(&pair.local)
        .await
        .is_ok_and(|meta| meta.is_dir());
    let gate_kind = state.gate.as_ref().map(|gate| gate.kind);
    let decision = decide(&DecideInput {
        pair_enabled: pair.enabled,
        local_exists,
        online,
        gate: gate_kind,
        applied_filters_hash: state.applied_filters_hash.as_deref(),
        current_filters_hash: &filters_file.hash,
        marker: marker.as_ref(),
    });
    if marker.is_some() && decision.discards_marker() {
        ConfirmMarker::remove(env.paths, pair_id).await?;
    }
    let mut dirty = false;
    // These two gates end by themselves once the cause is gone; the next
    // announcement of the same gate must notify again.
    if let Some(kind @ (GateKind::LocalMissing | GateKind::ConfigError)) = gate_kind
        && local_exists
        && decision != Decision::Skip(SkipReason::Paused)
    {
        state.gate = None;
        state.notified.retain(|key| *key != kind.notified_key());
        dirty = true;
    }
    match decision {
        Decision::Skip(SkipReason::Paused) => Ok(RunReport::Skipped(SkipReason::Paused)),
        Decision::Skip(SkipReason::Offline) => {
            state.offline = true;
            state.save(env.paths, pair_id).await?;
            Ok(RunReport::Skipped(SkipReason::Offline))
        }
        Decision::Skip(SkipReason::Gated) => {
            // A gate nobody wrote through this code path (first run, or one
            // set by the GUI) is announced the first time a round sees it.
            if let Some(kind) = state.gate.as_ref().map(|gate| gate.kind) {
                let before = state.notified.len();
                announce_gate(env, &pair, &mut state, kind);
                dirty |= state.notified.len() != before;
            }
            if dirty {
                state.save(env.paths, pair_id).await?;
            }
            Ok(RunReport::Skipped(SkipReason::Gated))
        }
        Decision::SetGate(kind) => {
            let unchanged = state.gate.as_ref().is_some_and(|gate| gate.kind == kind);
            if !unchanged {
                set_gate(&mut state, kind, String::new(), env.now().timestamp());
            }
            announce_gate(env, &pair, &mut state, kind);
            state.save(env.paths, pair_id).await?;
            Ok(RunReport::GateSet(kind))
        }
        Decision::Run(mode) => {
            let round = Round {
                env,
                pair: &pair,
                behavior: &behavior,
                filters_hash: &filters_file.hash,
            };
            let outcome = round.execute(&mut state, mode).await?;
            Ok(RunReport::Ran { mode, outcome })
        }
    }
}

struct Round<'a> {
    env: &'a RunEnv<'a>,
    pair: &'a Pair,
    behavior: &'a Behavior,
    filters_hash: &'a str,
}

impl Round<'_> {
    async fn execute(&self, state: &mut State, mode: Mode) -> Result<Outcome, RunError> {
        let (env, pair) = (self.env, self.pair);
        let id = pair.id.as_str();
        let started = env.now();
        state.last_attempt = Some(started.timestamp());
        state.offline = false;
        state.save(env.paths, id).await?;

        tokio::fs::create_dir_all(env.paths.runs_dir(id))
            .await
            .map_err(RunError::io("creating the runs folder"))?;
        let stamp = fresh_start(env.paths, id, mode, started.clone());
        let log_path = log_file(env.paths, id, mode, &stamp);
        let args = bisync_args(pair, self.behavior, env.paths, mode, &stamp);
        let exit_code = execute(env.rclone, args).await?;
        let finished = env.now().timestamp();

        let lines = read_log_or_empty(&log_path).await;
        let outcome = classify(exit_code, &lines, &env.paths.bisync_workdir(id));
        let mut summary = summarize(&lines);
        let conflicts_new = summary.conflicts_new;
        summary.changes.truncate(STORED_CHANGES);
        state.last_run = Some(LastRun {
            started: started.timestamp(),
            finished,
            exit_code,
            summary,
            log_file: log_path,
        });
        let detail = last_error_message(&lines).map(str::to_owned);
        self.apply_outcome(state, outcome, exit_code, conflicts_new, detail, finished)
            .await?;
        state.save(env.paths, id).await?;

        if outcome != Outcome::Transient {
            ConfirmMarker::remove(env.paths, id).await?;
        }
        if outcome == Outcome::Ok {
            versions::prune(env.paths, id, self.behavior.versions_keep_days, &env.now())
                .await
                .map_err(RunError::io("pruning old versions"))?;
        }
        prune_runs(env.paths, id).await?;
        Ok(outcome)
    }

    async fn apply_outcome(
        &self,
        state: &mut State,
        outcome: Outcome,
        exit_code: i32,
        conflicts_new: Option<u32>,
        detail: Option<String>,
        finished: Timestamp,
    ) -> Result<(), RunError> {
        let (env, pair) = (self.env, self.pair);
        let gate = match outcome {
            Outcome::Ok => {
                state.gate = None;
                state.last_outcome = Some(LastOutcome::Ok);
                state.consecutive_failures = 0;
                state.applied_filters_hash = Some(self.filters_hash.to_owned());
                state.last_success = Some(finished);
                state
                    .notified
                    .retain(|key| key != NOTIFIED_FAILURES && !key.starts_with("gate:"));
                state.conflicts_pending = self.count_conflicts(state.conflicts_pending).await;
                if let Some(count) = conflicts_new.filter(|count| *count > 0) {
                    env.notifier
                        .notify(&notice(pair, &Alert::NewConflicts(count)));
                }
                return Ok(());
            }
            Outcome::TooManyDeletes => (GateKind::TooManyDeletes, String::new()),
            Outcome::AllFilesChanged => (GateKind::AllFilesChanged, String::new()),
            Outcome::NeedsReview(_) => (
                GateKind::NeedsReviewCriticalError,
                detail.unwrap_or_default(),
            ),
            Outcome::AuthError => (GateKind::AuthError, detail.unwrap_or_default()),
            Outcome::ConfigError => (
                GateKind::ConfigError,
                format!(
                    "rclone rejected its arguments: {}",
                    detail.unwrap_or_else(|| format!("exit code {exit_code}"))
                ),
            ),
            Outcome::Transient => {
                state.last_outcome = Some(LastOutcome::Error);
                state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                if state.consecutive_failures >= NOTIFY_FAILURES_AFTER {
                    let message =
                        detail.unwrap_or_else(|| format!("rclone exited with code {exit_code}."));
                    announce(
                        env,
                        pair,
                        state,
                        NOTIFIED_FAILURES.to_owned(),
                        &Alert::Failing(message),
                    );
                }
                return Ok(());
            }
        };
        set_gate(state, gate.0, gate.1, finished);
        announce_gate(env, pair, state, gate.0);
        Ok(())
    }

    /// Conflict files in the local tree; the previous count when the tree
    /// cannot be scanned.
    async fn count_conflicts(&self, previous: u32) -> u32 {
        let root = self.pair.local.clone();
        let matcher = DirMatcher::new(&filters::render(self.pair));
        let scanned = tokio::task::spawn_blocking(move || find_conflicts(&root, &matcher))
            .await
            .unwrap_or_else(|join| Err(std::io::Error::other(join)));
        match scanned {
            Ok(found) => u32::try_from(found.len()).unwrap_or(u32::MAX),
            Err(err) => {
                log::warn!(
                    "cannot scan {} for conflicts: {err}",
                    self.pair.local.display()
                );
                previous
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    use super::*;

    fn script(dir: &Path, body: &str) -> Rclone {
        let path = dir.join("fake-rclone.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Rclone::new(path)
    }

    #[tokio::test]
    async fn execute_returns_the_exit_code_of_rclone() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = script(dir.path(), "exit 7");

        let code = execute(&rclone, vec![]).await.unwrap();

        assert_eq!(code, 7);
    }

    #[tokio::test]
    async fn execute_passes_sigint_to_rclone_and_waits_for_it_to_finish() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = script(
            dir.path(),
            "trap 'exit 42' INT\ntouch \"$0.ready\"\nwhile true; do sleep 0.05; done",
        );
        let ready = dir.path().join("fake-rclone.sh.ready");
        let interrupter = async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !ready.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let own = Pid::from_raw(i32::try_from(std::process::id()).unwrap()).unwrap();
            rustix::process::kill_process(own, Signal::INT).unwrap();
        };

        let (code, ()) = tokio::join!(execute(&rclone, vec![]), interrupter);

        assert_eq!(code.unwrap(), 42);
    }

    fn stamp_clock() -> Zoned {
        jiff::civil::date(2026, 1, 2)
            .at(15, 4, 5, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
    }

    #[test]
    fn fresh_start_keeps_the_clock_when_no_log_has_that_name() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            dir.path().join("h"),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("d"),
            dir.path().join("r"),
        );

        let now = fresh_start(&paths, "p", Mode::Normal, stamp_clock());

        assert_eq!(now, stamp_clock());
    }

    #[test]
    fn fresh_start_moves_one_second_on_for_each_log_already_taking_the_name() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            dir.path().join("h"),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("d"),
            dir.path().join("r"),
        );
        std::fs::create_dir_all(paths.runs_dir("p")).unwrap();
        for name in ["20260102-150405.jsonl", "20260102-150406.jsonl"] {
            std::fs::write(paths.runs_dir("p").join(name), "").unwrap();
        }

        let now = fresh_start(&paths, "p", Mode::Normal, stamp_clock());

        assert_eq!(now.strftime("%Y%m%d-%H%M%S").to_string(), "20260102-150407");
    }

    #[test]
    fn exit_code_is_2_for_config_problems_and_1_otherwise() {
        assert_eq!(RunError::UnknownPair("x".into()).exit_code(), 2);
        assert_eq!(
            RunError::Config(GlobalConfigError::UnsupportedVersion(2)).exit_code(),
            2
        );
        assert_eq!(RunError::Rclone(RcloneError::RcloneNotFound).exit_code(), 1);
    }
}
