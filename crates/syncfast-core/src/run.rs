//! One round of one pair: `syncfast run <id>` (spec step 7). The round
//! decides whether it may run, runs `rclone bisync`, and turns how it ended
//! into the pair's state and, when the user must act, a notification.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use jiff::{SignedDuration, Timestamp, Zoned};
use rustix::process::{Pid, Signal};
use tokio::process::Child;
use tokio::signal::unix::{SignalKind, signal};

use crate::behavior::{Behavior, GoogleDocs, ReviewLevel, SyncMode};
use crate::bisync::{
    Fault, Outcome, RoundMode, bisync_args, classify, is_empty_prior_listing, log_file, stamp,
};
use crate::bisync_log::{LogLine, RunSummary, read_log, summarize};
use crate::config::{Config, GlobalConfigError, Pair, PairError, split_remote};
use crate::conflicts::{Naming, find_conflicts};
use crate::filters::{self, DirMatcher, FiltersError, FiltersFile};
use crate::network::Network;
use crate::notify::{Alert, Notice, Notifier, last_error_message, notice};
use crate::oneway::{
    self, classify_oneway, deleted_before_stop, files_at_destination, oneway_args, summarize_oneway,
};
use crate::ops::views::review_kind;
use crate::paths::Paths;
use crate::preview::{ChangeCounts, PreviewError, preview_locked};
use crate::rclone::{DRIVE_TYPE, Rclone, RcloneError};
use crate::run_decision::{
    AutoReview, DecideInput, Decision, SkipReason, auto_review, decide, may_confirm,
};
use crate::state::{
    AutoReviewed, ConfirmMarker, Gate, GateKind, LastOutcome, LastRun, NOTIFIED_FAILURES, State,
    StateError, idle_log_path, prune_runs, try_lock_pair,
};
use crate::versions;

/// Changes kept in `state.json`: the GUI rereads it every second or two, and
/// the full list stays in the run's log.
pub const STORED_CHANGES: usize = 100;
/// How long a series of transient failures may last before the user is
/// told: retrying has not fixed it.
const ESCALATE_AFTER: SignedDuration = SignedDuration::from_hours(6);

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
    /// Pass SIGINT and SIGTERM to rclone and wait for it, instead of
    /// leaving them to the process. Only the `run` command, which exists
    /// for one round, wants this: the handler cannot be removed again.
    pub forward_signals: bool,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunReport {
    /// Another round of the pair holds the lock.
    Busy,
    /// The pair is invalid; its gate is now `config_error`.
    ConfigInvalid,
    Skipped(SkipReason),
    GateSet(GateKind),
    Ran {
        mode: RoundMode,
        outcome: Outcome,
    },
}

pub(crate) enum PairLookup {
    Valid(Pair, Behavior),
    /// The pair as written in the config, and why it cannot run.
    Invalid(Pair, PairError, Behavior),
}

/// Loads the pair's state. A file that is not valid JSON is set aside as
/// `state.json.corrupt-<stamp>` and the pair starts over from its first
/// review, which is the safe way to lose its history.
async fn load_state(env: &RunEnv<'_>, pair_id: &str) -> Result<State, RunError> {
    match State::load(env.paths, pair_id).await {
        Err(StateError::Parse { path, source }) => {
            let now = env.now();
            let mut aside = path.clone().into_os_string();
            aside.push(format!(".corrupt-{}", stamp(&now)));
            log::warn!(
                "{} is not valid ({source}); keeping it as {} and starting over",
                path.display(),
                Path::new(&aside).display()
            );
            tokio::fs::rename(&path, &aside)
                .await
                .map_err(RunError::io("setting aside a corrupt state file"))?;
            Ok(State::first_run(now.timestamp()))
        }
        other => Ok(other?),
    }
}

pub(crate) async fn lookup_pair(env: &RunEnv<'_>, pair_id: &str) -> Result<PairLookup, RunError> {
    let config = Config::load(env.paths).await?;
    if !config.pairs().iter().any(|pair| pair.id == pair_id) {
        return Err(RunError::UnknownPair(pair_id.to_owned()));
    }
    let accounts = env.rclone.listremotes().await?;
    let validation = config.validate(env.paths, &accounts);
    let defaults = config.defaults();
    if let Some(pair) = validation.pairs.into_iter().find(|pair| pair.id == pair_id) {
        let behavior = Behavior::for_pair(&defaults, &pair);
        return Ok(PairLookup::Valid(pair, behavior));
    }
    let written = config.pairs().iter().find(|pair| pair.id == pair_id);
    let error = validation
        .errors
        .into_iter()
        .find(|error| error.pair_id == pair_id);
    match (written, error) {
        (Some(pair), Some(error)) => {
            let behavior = Behavior::for_pair(&defaults, pair);
            Ok(PairLookup::Invalid(pair.clone(), error, behavior))
        }
        _ => Err(RunError::UnknownPair(pair_id.to_owned())),
    }
}

/// Records `kind` as the gate, keeping `since` when the pair was already
/// gated for the same reason.
pub(crate) fn set_gate(
    state: &mut State,
    kind: GateKind,
    detail: String,
    fault: Option<Fault>,
    now: Timestamp,
) {
    let since = state
        .gate
        .as_ref()
        .filter(|gate| gate.kind == kind)
        .map_or(now, |gate| gate.since);
    state.gate = Some(Gate {
        kind,
        since,
        detail,
        fault,
        auto_checked: None,
    });
}

/// Counts a failed round; the first of a series starts the clock that
/// [`ESCALATE_AFTER`] reads.
fn record_failure(state: &mut State, finished: Timestamp) {
    state.last_outcome = Some(LastOutcome::Error);
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    state.failing_since.get_or_insert(finished);
}

/// Records one notification per transition: the key stays in
/// `state.notified` until a round succeeds (or the gate is cleared). The
/// notice is returned, not sent: callers send it after saving the state, so
/// whoever is told can already read why.
fn announce(
    pair: &Pair,
    behavior: &Behavior,
    state: &mut State,
    key: String,
    alert: &Alert,
) -> Option<Notice> {
    if state.notified.contains(&key) {
        return None;
    }
    state.notified.push(key);
    behavior
        .notifications
        .allows(alert)
        .then(|| notice(pair, behavior, alert))
}

fn announce_gate(
    pair: &Pair,
    behavior: &Behavior,
    state: &mut State,
    kind: GateKind,
) -> Option<Notice> {
    let (detail, fault) = state
        .gate
        .as_ref()
        .map(|gate| (gate.detail.clone(), gate.fault))
        .unwrap_or_default();
    announce(
        pair,
        behavior,
        state,
        kind.notified_key(),
        &Alert::Gate {
            kind,
            detail,
            fault,
        },
    )
}

fn send(env: &RunEnv<'_>, notices: impl IntoIterator<Item = Notice>) {
    for notice in notices {
        env.notifier.notify(&notice);
    }
}

pub(crate) fn forward_interrupt(child: &Child, program: &str) {
    let Some(pid) = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
    else {
        return;
    };
    if let Err(err) = rustix::process::kill_process(pid, Signal::INT) {
        log::warn!("cannot interrupt {program}: {err}");
    }
}

/// SIGINT and SIGTERM of this process. tokio never undoes a signal
/// handler, so only a process that exists for rounds installs them.
pub(crate) struct Interrupts {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

impl Interrupts {
    pub(crate) fn install() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    pub(crate) async fn next(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
    }
}

/// Resolves when a signal arrives; never, when there is no handler.
pub(crate) async fn wait_interrupt(interrupts: &mut Option<Interrupts>) {
    match interrupts {
        Some(interrupts) => interrupts.next().await,
        None => std::future::pending().await,
    }
}

/// Runs rclone with `args` and waits for it. rclone's own output is
/// dropped (its log goes to `--log-file`). With `forward_signals`, SIGINT
/// and SIGTERM do not stop the round: they are passed to rclone as SIGINT,
/// which makes bisync shut down cleanly, and the round waits for it.
///
/// # Errors
/// rclone cannot be started or waited for.
pub(crate) async fn execute(
    rclone: &Rclone,
    args: Vec<OsString>,
    forward_signals: bool,
) -> Result<i32, RunError> {
    let mut interrupts = if forward_signals {
        Some(Interrupts::install().map_err(RunError::io("signal handler"))?)
    } else {
        None
    };
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
            () = async {
                match interrupts.as_mut() {
                    Some(interrupts) => interrupts.next().await,
                    None => std::future::pending().await,
                }
            } => forward_interrupt(&child, "rclone"),
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
/// share a log file, because rclone appends to an existing one, nor a stamp,
/// which ties a log to its `.changes` report (an idle round's log has
/// another name but keeps its stamp).
pub(crate) fn fresh_start(paths: &Paths, pair_id: &str, mode: RoundMode, mut now: Zoned) -> Zoned {
    let taken = |at: &Zoned| {
        let log = log_file(paths, pair_id, mode, at);
        log.exists() || idle_log_path(&log).exists()
    };
    while taken(&now) {
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
    let (pair, behavior) = match lookup_pair(env, pair_id).await? {
        PairLookup::Valid(pair, behavior) => (pair, behavior),
        PairLookup::Invalid(pair, error, behavior) => {
            return report_config_invalid(env, pair_id, &pair, error, &behavior).await;
        }
    };
    let Some(_lock) = try_lock_pair(env.paths, pair_id)? else {
        return Ok(RunReport::Busy);
    };
    let filters_file = filters::write_if_changed(env.paths, &pair).await?;
    let mut state = load_state(env, pair_id).await?;
    let mut marker = ConfirmMarker::load(env.paths, pair_id).await?;
    let round = Round {
        env,
        pair: &pair,
        behavior: &behavior,
        filters: &filters_file,
        online: env.network.is_online().await,
        local_exists: tokio::fs::metadata(&pair.local)
            .await
            .is_ok_and(|meta| meta.is_dir()),
    };
    let gate_kind = state.gate.as_ref().map(|gate| gate.kind);
    let mut decision = round.decide(&state, marker.as_ref());
    if marker.is_some() && decision.discards_marker() {
        ConfirmMarker::remove(env.paths, pair_id).await?;
    }
    let mut dirty = false;
    // These gates end by themselves once the cause is gone; the next
    // announcement of the same gate must notify again.
    let mode_is_back = gate_kind == Some(GateKind::NeedsReviewModeChanged)
        && state.synced_mode() == Some(behavior.mode);
    if let Some(kind) = gate_kind
        && (matches!(kind, GateKind::LocalMissing | GateKind::ConfigError) && round.local_exists
            || mode_is_back)
        && decision != Decision::Skip(SkipReason::Paused)
    {
        state.gate = None;
        state.notified.retain(|key| *key != kind.notified_key());
        dirty = true;
    }
    let level = behavior.review;
    let mut held_at = None;
    let mut auto_reviewed = None;
    if round.online
        && round.local_exists
        && let Some(kind) = reviewable_gate(decision, &state)
        && may_confirm(level, kind)
        && !already_checked(&state, kind, level)
    {
        match round.review(&mut state, kind, level).await? {
            Review::Confirmed { saved, reviewed } => {
                marker = Some(saved);
                decision = round.decide(&state, marker.as_ref());
                auto_reviewed = Some(reviewed);
            }
            Review::Held => held_at = Some(level),
            Review::Failed => dirty = false,
        }
    }
    loop {
        match decision {
            Decision::Skip(SkipReason::Paused) => {
                return Ok(RunReport::Skipped(SkipReason::Paused));
            }
            Decision::Skip(SkipReason::Offline) => {
                state.offline = true;
                state.failing_since = None;
                state.save(env.paths, pair_id).await?;
                return Ok(RunReport::Skipped(SkipReason::Offline));
            }
            Decision::Skip(SkipReason::Gated) => {
                return finish_gated(&round, &mut state, held_at, dirty).await;
            }
            Decision::SetGate(kind) => {
                return finish_gate_set(&round, &mut state, kind, held_at).await;
            }
            Decision::Run(mode) => match round.run(&mut state, mode, auto_reviewed).await? {
                Settled::Report(report) => return Ok(report),
                Settled::Confirmed { saved, reviewed } => {
                    marker = Some(saved);
                    auto_reviewed = Some(reviewed);
                    decision = round.decide(&state, marker.as_ref());
                }
            },
        }
    }
}

async fn report_config_invalid(
    env: &RunEnv<'_>,
    pair_id: &str,
    pair: &Pair,
    error: PairError,
    behavior: &Behavior,
) -> Result<RunReport, RunError> {
    let mut state = load_state(env, pair_id).await?;
    set_gate(
        &mut state,
        GateKind::ConfigError,
        error.message,
        None,
        env.now().timestamp(),
    );
    let notice = announce_gate(pair, behavior, &mut state, GateKind::ConfigError);
    state.save(env.paths, pair_id).await?;
    send(env, notice);
    Ok(RunReport::ConfigInvalid)
}

async fn record_failed_check(
    round: &Round<'_>,
    state: &mut State,
    exit_code: i32,
    message: String,
) -> Result<(), RunError> {
    let env = round.env;
    let finished = env.now().timestamp();
    let notice = round
        .apply_outcome(
            state,
            &Outcome::Transient,
            Ended {
                exit_code,
                alerts: Vec::new(),
                detail: Some(message),
                finished,
                dest_files: None,
            },
        )
        .await;
    state.save(env.paths, &round.pair.id).await?;
    send(env, notice);
    Ok(())
}

/// Announces the gate in `state` (once), notes the level that found it
/// held, saves what changed and sends the notice.
async fn settle_gate(
    round: &Round<'_>,
    state: &mut State,
    held_at: Option<ReviewLevel>,
    mut dirty: bool,
) -> Result<(), RunError> {
    // A gate nobody wrote through this code path (first run, or one
    // set by the GUI) is announced the first time a round sees it.
    let notice = state
        .gate
        .as_ref()
        .map(|gate| gate.kind)
        .and_then(|kind| announce_gate(round.pair, round.behavior, state, kind));
    dirty |= notice.is_some();
    if let (Some(level), Some(gate)) = (held_at, state.gate.as_mut()) {
        gate.auto_checked = Some(level);
        dirty = true;
    }
    if dirty {
        state.save(round.env.paths, &round.pair.id).await?;
    }
    send(round.env, notice);
    Ok(())
}

async fn finish_gated(
    round: &Round<'_>,
    state: &mut State,
    held_at: Option<ReviewLevel>,
    dirty: bool,
) -> Result<RunReport, RunError> {
    settle_gate(round, state, held_at, dirty).await?;
    Ok(RunReport::Skipped(SkipReason::Gated))
}

async fn finish_gate_set(
    round: &Round<'_>,
    state: &mut State,
    kind: GateKind,
    held_at: Option<ReviewLevel>,
) -> Result<RunReport, RunError> {
    let unchanged = state.gate.as_ref().is_some_and(|gate| gate.kind == kind);
    if !unchanged {
        set_gate(
            state,
            kind,
            String::new(),
            None,
            round.env.now().timestamp(),
        );
    }
    settle_gate(round, state, held_at, true).await?;
    Ok(RunReport::GateSet(kind))
}

fn reviewable_gate(decision: Decision, state: &State) -> Option<GateKind> {
    match decision {
        Decision::SetGate(kind) => Some(kind),
        Decision::Skip(SkipReason::Gated) => state.gate.as_ref().map(|gate| gate.kind),
        _ => None,
    }
}

/// A dry run at this level already found `kind` held.
fn already_checked(state: &State, kind: GateKind, level: ReviewLevel) -> bool {
    state
        .gate
        .as_ref()
        .is_some_and(|gate| gate.kind == kind && gate.auto_checked == Some(level))
}

/// How [`Round::run`] ended.
enum Settled {
    Report(RunReport),
    /// The round ended at a cap and the level confirmed it: the marker is
    /// on disk and the caller decides the round that follows.
    Confirmed {
        saved: ConfirmMarker,
        reviewed: AutoReviewed,
    },
}

/// What [`Round::review`] left behind; a failed check is already recorded.
enum Review {
    Confirmed {
        saved: ConfirmMarker,
        reviewed: AutoReviewed,
    },
    Held,
    Failed,
}

enum Checked {
    /// `saved` is on disk, where the next `decide` finds it.
    Confirmed {
        saved: ConfirmMarker,
        reviewed: AutoReviewed,
    },
    Held,
    Failed {
        exit_code: i32,
        message: String,
    },
}

/// Dry-runs the review `kind` waits for and, when `level` confirms it,
/// leaves the same marker the GUI leaves.
async fn check_review(
    round: &Round<'_>,
    kind: GateKind,
    level: ReviewLevel,
) -> Result<Checked, RunError> {
    let Some(review) = review_kind(kind) else {
        return Ok(Checked::Held);
    };
    let (env, pair) = (round.env, round.pair);
    let preview = match preview_locked(env, pair, round.behavior, round.filters, review).await {
        Ok(preview) => preview,
        Err(PreviewError::Run(err)) => return Err(err),
        Err(PreviewError::Failed { exit_code, message }) => {
            return Ok(Checked::Failed { exit_code, message });
        }
        Err(other) => {
            return Ok(Checked::Failed {
                exit_code: 1,
                message: other.to_string(),
            });
        }
    };
    let counts = ChangeCounts::of(&preview.summary.changes);
    let recoverable =
        counts.deletes() && deletions_recoverable(env, pair, round.behavior, &counts).await;
    if auto_review(level, kind, &counts, recoverable) == AutoReview::Hold {
        return Ok(Checked::Held);
    }
    let saved = ConfirmMarker {
        kind: review,
        mode: preview.mode,
        filters_hash: preview.filters_hash,
    };
    saved.save(env.paths, &pair.id).await?;
    Ok(Checked::Confirmed {
        saved,
        reviewed: AutoReviewed {
            gate: kind,
            level,
            counts,
            recoverable,
        },
    })
}

/// Whether every file `counts` deletes can come back: local deletions from
/// the versions folder, remote ones from the Drive trash or, in `mirror_up`,
/// from the versions folder on the account. An account whose type cannot be
/// read counts as one without a trash.
async fn deletions_recoverable(
    env: &RunEnv<'_>,
    pair: &Pair,
    behavior: &Behavior,
    counts: &ChangeCounts,
) -> bool {
    if counts.delete_local > 0 && !behavior.versions {
        return false;
    }
    if counts.delete_remote == 0 || (behavior.mode == SyncMode::MirrorUp && behavior.versions) {
        return true;
    }
    let Some((account, _)) = split_remote(&pair.remote) else {
        return false;
    };
    match env.rclone.remote_types().await {
        Ok(types) => types
            .iter()
            .any(|(name, kind)| name == account && kind == DRIVE_TYPE),
        Err(err) => {
            log::warn!(
                "{}: cannot tell whether {account} has a trash: {err}",
                pair.id
            );
            false
        }
    }
}

struct Attempt {
    exit_code: i32,
    log_path: PathBuf,
    lines: Vec<LogLine>,
    /// The `--combined` report of a one-way round; empty for two-way.
    changes: String,
}

/// What a round learned by ending, for [`Round::apply_outcome`].
struct Ended {
    exit_code: i32,
    alerts: Vec<Alert>,
    detail: Option<String>,
    finished: Timestamp,
    /// Files the destination of a mirror holds afterwards.
    dest_files: Option<u64>,
}

/// A round that succeeded with nothing to report: no change and no new
/// conflict.
fn is_idle(outcome: &Outcome, summary: &RunSummary) -> bool {
    *outcome == Outcome::Ok
        && summary.changes.is_empty()
        && summary.conflicts_new.is_none_or(|count| count == 0)
}

/// Renames the log of an idle round so Activity and the pruning can tell it
/// from the rounds that did something. A log that cannot be renamed stays
/// where it is.
async fn rename_to_idle(log: PathBuf) -> PathBuf {
    let idle = idle_log_path(&log);
    match tokio::fs::rename(&log, &idle).await {
        Ok(()) => idle,
        Err(err) => {
            log::warn!("cannot rename {}: {err}", log.display());
            log
        }
    }
}

/// Reads the `--combined` report of a one-way round; a report that was
/// never written reads as empty.
pub(crate) async fn read_changes(path: &Path) -> String {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                log::warn!("cannot read {}: {err}", path.display());
            }
            String::new()
        }
    }
}

/// [`read_changes`] for a report nobody reads again: a preview's.
pub(crate) async fn take_changes(path: &Path) -> String {
    let text = read_changes(path).await;
    if let Err(err) = tokio::fs::remove_file(path).await
        && err.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("cannot delete {}: {err}", path.display());
    }
    text
}

/// The gate a round that ended in `outcome` waits at, when `level` may
/// confirm it without a sheet.
fn confirmable_cap(outcome: &Outcome, level: ReviewLevel) -> Option<GateKind> {
    let kind = match outcome {
        Outcome::TooManyDeletes => GateKind::TooManyDeletes,
        Outcome::AllFilesChanged => GateKind::AllFilesChanged,
        _ => return None,
    };
    may_confirm(level, kind).then_some(kind)
}

struct Round<'a> {
    env: &'a RunEnv<'a>,
    pair: &'a Pair,
    behavior: &'a Behavior,
    filters: &'a FiltersFile,
    online: bool,
    local_exists: bool,
}

impl Round<'_> {
    fn decide(&self, state: &State, marker: Option<&ConfirmMarker>) -> Decision {
        decide(&DecideInput {
            pair_enabled: self.pair.enabled,
            local_exists: self.local_exists,
            online: self.online,
            gate: state.gate.as_ref().map(|gate| gate.kind),
            applied_filters_hash: state.applied_filters_hash.as_deref(),
            current_filters_hash: &self.filters.hash,
            mode: self.behavior.mode,
            applied_mode: state.synced_mode(),
            marker,
        })
    }

    /// Dry-runs the review `kind` waits for; a confirmed one leaves the
    /// gate in `state` and the marker on disk, a failed one is recorded.
    async fn review(
        &self,
        state: &mut State,
        kind: GateKind,
        level: ReviewLevel,
    ) -> Result<Review, RunError> {
        match check_review(self, kind, level).await? {
            Checked::Confirmed { saved, reviewed } => {
                if state.gate.as_ref().map(|gate| gate.kind) != Some(kind) {
                    let now = self.env.now().timestamp();
                    set_gate(state, kind, String::new(), None, now);
                }
                Ok(Review::Confirmed { saved, reviewed })
            }
            Checked::Held => Ok(Review::Held),
            Checked::Failed { exit_code, message } => {
                record_failed_check(self, state, exit_code, message).await?;
                Ok(Review::Failed)
            }
        }
    }

    /// [`Round::execute`], and when the round ends at a cap the level may
    /// confirm, the review in the same call: the user is not told about a
    /// gate that syncfast is about to open.
    async fn run(
        &self,
        state: &mut State,
        mode: RoundMode,
        auto_reviewed: Option<AutoReviewed>,
    ) -> Result<Settled, RunError> {
        let level = self.behavior.review;
        let defer_at = auto_reviewed.is_none().then_some(level);
        let (mode, outcome) = self.execute(state, mode, auto_reviewed, defer_at).await?;
        let report = Settled::Report(RunReport::Ran {
            mode,
            outcome: outcome.clone(),
        });
        let Some(kind) = defer_at.and_then(|level| confirmable_cap(&outcome, level)) else {
            return Ok(report);
        };
        let held_at = match self.review(state, kind, level).await? {
            Review::Confirmed { saved, reviewed } => {
                return Ok(Settled::Confirmed { saved, reviewed });
            }
            Review::Held => Some(level),
            Review::Failed => None,
        };
        settle_gate(self, state, held_at, true).await?;
        Ok(report)
    }

    async fn execute(
        &self,
        state: &mut State,
        mut mode: RoundMode,
        auto_reviewed: Option<AutoReviewed>,
        defer_at: Option<ReviewLevel>,
    ) -> Result<(RoundMode, Outcome), RunError> {
        let (env, pair) = (self.env, self.pair);
        let id = pair.id.as_str();
        let started = env.now();
        state.last_attempt = Some(started.timestamp());
        state.offline = false;
        state.save(env.paths, id).await?;

        if let Some((outcome, detail)) = self.source_stops_the_round().await {
            let ended = Ended {
                exit_code: 1,
                alerts: Vec::new(),
                detail,
                finished: env.now().timestamp(),
                dest_files: None,
            };
            let notices = self.apply_outcome(state, &outcome, ended).await;
            state.save(env.paths, id).await?;
            send(env, notices);
            return Ok((mode, outcome));
        }

        tokio::fs::create_dir_all(env.paths.runs_dir(id))
            .await
            .map_err(RunError::io("creating the runs folder"))?;
        let dest_files = state.dest_files.unwrap_or(0);
        let mut attempt = self.attempt(mode, started.clone(), dest_files).await?;
        if mode == RoundMode::Normal && is_empty_prior_listing(attempt.exit_code, &attempt.lines) {
            mode = RoundMode::Resync;
            let refused = attempt.log_path;
            attempt = self.attempt(mode, env.now(), dest_files).await?;
            if let Err(err) = tokio::fs::remove_file(&refused).await {
                log::warn!("cannot delete {}: {err}", refused.display());
            }
        }
        let Attempt {
            exit_code,
            mut log_path,
            lines,
            changes,
        } = attempt;
        let finished = env.now().timestamp();
        let behavior = self.behavior;
        let (outcome, mut summary) = self.judge(exit_code, &lines, &changes);
        if mode == RoundMode::Normal && auto_reviewed.is_none() && is_idle(&outcome, &summary) {
            log_path = rename_to_idle(log_path).await;
        }
        let alerts: Vec<Alert> = summary
            .conflicts_new
            .filter(|count| *count > 0)
            .map(Alert::NewConflicts)
            .into_iter()
            .chain(Alert::synced(&summary))
            .chain(
                auto_reviewed
                    .filter(|reviewed| reviewed.counts.deletes())
                    .map(|reviewed| Alert::AutoReviewed {
                        deleted: reviewed.counts.deleted(),
                        recoverable: reviewed.recoverable,
                    }),
            )
            .collect();
        summary.changes.truncate(STORED_CHANGES);
        state.last_run = Some(LastRun {
            started: started.timestamp(),
            finished,
            exit_code,
            summary,
            log_file: log_path,
            auto_reviewed,
        });
        let ended = Ended {
            exit_code,
            alerts,
            detail: if matches!(outcome, Outcome::TooManyDeletes) {
                Some(deleted_before_stop(
                    &changes,
                    behavior.mode,
                    behavior.versions,
                ))
            } else {
                last_error_message(&lines).map(str::to_owned)
            },
            finished,
            dest_files: behavior
                .mode
                .is_mirror()
                .then(|| files_at_destination(&changes)),
        };
        let mut notices = self.apply_outcome(state, &outcome, ended).await;
        if let Some(kind) = defer_at.and_then(|level| confirmable_cap(&outcome, level)) {
            notices.clear();
            state.notified.retain(|key| *key != kind.notified_key());
        }
        state.save(env.paths, id).await?;
        send(env, notices);

        self.prune().await;
        if !matches!(outcome, Outcome::Transient | Outcome::NeedsAttention { .. }) {
            ConfirmMarker::remove(env.paths, id).await?;
        }
        Ok((mode, outcome))
    }

    fn judge(&self, exit_code: i32, lines: &[LogLine], changes: &str) -> (Outcome, RunSummary) {
        let mode = self.behavior.mode;
        if mode.is_one_way() {
            (
                classify_oneway(exit_code, lines),
                summarize_oneway(changes, lines, mode, exit_code),
            )
        } else {
            (
                classify(
                    exit_code,
                    lines,
                    &self.env.paths.bisync_workdir(&self.pair.id),
                ),
                summarize(lines),
            )
        }
    }

    /// The floor of the one-way modes: a source with no file to send is
    /// never mirrored, whatever the review said.
    async fn source_stops_the_round(&self) -> Option<(Outcome, Option<String>)> {
        let (env, pair, mode) = (self.env, self.pair, self.behavior.mode);
        if !mode.is_one_way() {
            return None;
        }
        let local = mode.local_is_source();
        let source = if local {
            pair.local.to_string_lossy().into_owned()
        } else {
            pair.remote.clone()
        };
        let skip_gdocs = self.behavior.google_docs == GoogleDocs::Skip;
        match env
            .rclone
            .has_files(&source, &env.paths.filters_file(&pair.id), skip_gdocs)
            .await
        {
            Ok(true) => None,
            Ok(false) => {
                let fault = if local {
                    Fault::LocalEmpty
                } else {
                    Fault::RemoteEmpty
                };
                Some((
                    Outcome::NeedsAttention {
                        fault,
                        detail: "The source folder is empty, so nothing was changed.".into(),
                    },
                    None,
                ))
            }
            Err(err) => Some((Outcome::Transient, Some(err.to_string()))),
        }
    }

    /// Old versions and run logs go after every round, whatever its
    /// outcome; a failure here never becomes the round's.
    async fn prune(&self) {
        let (env, id) = (self.env, self.pair.id.as_str());
        let keep_days = self.behavior.versions_keep_days;
        match versions::prune(env.paths, id, keep_days, &env.now()).await {
            Ok(0) => {}
            Ok(failed) => log::warn!("{id}: {failed} old version folders could not be pruned"),
            Err(err) => log::warn!("{id}: cannot prune old versions: {err}"),
        }
        if self.behavior.mode == SyncMode::MirrorUp && self.behavior.versions {
            let pruned =
                oneway::prune_remote_versions(env.rclone, self.pair, keep_days, &env.now()).await;
            if let Err(err) = pruned {
                log::warn!("{id}: cannot prune old versions on the account: {err}");
            }
        }
        if let Err(err) = prune_runs(env.paths, id).await {
            log::warn!("{id}: {err}");
        }
    }

    async fn attempt(
        &self,
        mode: RoundMode,
        at: Zoned,
        dest_files: u64,
    ) -> Result<Attempt, RunError> {
        let (env, pair) = (self.env, self.pair);
        let stamp = fresh_start(env.paths, &pair.id, mode, at);
        let log_path = log_file(env.paths, &pair.id, mode, &stamp);
        let one_way = self.behavior.mode.is_one_way();
        let args = if one_way {
            oneway_args(pair, self.behavior, env.paths, mode, dest_files, &stamp)
        } else {
            bisync_args(pair, self.behavior, env.paths, mode, &stamp)
        };
        let exit_code = execute(env.rclone, args, env.forward_signals).await?;
        let lines = read_log_or_empty(&log_path).await;
        let changes = if one_way {
            read_changes(&oneway::changes_file(
                env.paths,
                &pair.id,
                mode,
                self.behavior.mode,
                &stamp,
            ))
            .await
        } else {
            String::new()
        };
        Ok(Attempt {
            exit_code,
            log_path,
            lines,
            changes,
        })
    }

    async fn apply_outcome(
        &self,
        state: &mut State,
        outcome: &Outcome,
        ended: Ended,
    ) -> Vec<Notice> {
        let Ended {
            exit_code,
            alerts,
            detail,
            finished,
            dest_files,
        } = ended;
        let pair = self.pair;
        if !matches!(outcome, Outcome::Transient | Outcome::NeedsAttention { .. }) {
            state.failing_since = None;
        }
        let gate = match outcome {
            Outcome::Ok => {
                state.gate = None;
                state.last_outcome = Some(LastOutcome::Ok);
                state.consecutive_failures = 0;
                state.applied_filters_hash = Some(self.filters.hash.clone());
                state.last_success = Some(finished);
                state
                    .notified
                    .retain(|key| key != NOTIFIED_FAILURES && !key.starts_with("gate:"));
                let leaving_bisync = state.synced_mode() == Some(SyncMode::Bisync)
                    && self.behavior.mode != SyncMode::Bisync;
                state.applied_mode = Some(self.behavior.mode);
                state.dest_files = dest_files;
                if leaving_bisync {
                    self.forget_bisync_listings().await;
                }
                state.conflicts_pending = if self.behavior.mode == SyncMode::Bisync {
                    self.count_conflicts(state.conflicts_pending).await
                } else {
                    0
                };
                return alerts
                    .iter()
                    .filter(|alert| self.behavior.notifications.allows(alert))
                    .map(|alert| notice(pair, self.behavior, alert))
                    .collect();
            }
            Outcome::TooManyDeletes => (GateKind::TooManyDeletes, detail.unwrap_or_default(), None),
            Outcome::AllFilesChanged => (GateKind::AllFilesChanged, String::new(), None),
            Outcome::NeedsReview(_) => (
                GateKind::NeedsReviewCriticalError,
                detail.unwrap_or_default(),
                None,
            ),
            Outcome::AuthError => (GateKind::AuthError, detail.unwrap_or_default(), None),
            Outcome::ConfigError => (
                GateKind::ConfigError,
                format!(
                    "rclone rejected its arguments: {}",
                    detail.unwrap_or_else(|| format!("exit code {exit_code}"))
                ),
                None,
            ),
            Outcome::NeedsAttention { fault, detail } => {
                record_failure(state, finished);
                (GateKind::NeedsAttention, detail.clone(), Some(*fault))
            }
            Outcome::Transient => {
                record_failure(state, finished);
                let message =
                    detail.unwrap_or_else(|| format!("rclone exited with code {exit_code}."));
                let stalled = state.gate.is_none()
                    && state
                        .failing_since
                        .is_some_and(|since| finished.duration_since(since) > ESCALATE_AFTER);
                if stalled {
                    let hours = ESCALATE_AFTER.as_hours();
                    (
                        GateKind::NeedsAttention,
                        format!("Syncing has failed for over {hours} hours: {message}"),
                        None,
                    )
                } else if state.consecutive_failures >= self.behavior.notifications.failing_after()
                {
                    return announce(
                        pair,
                        self.behavior,
                        state,
                        NOTIFIED_FAILURES.to_owned(),
                        &Alert::Failing(message),
                    )
                    .into_iter()
                    .collect();
                } else {
                    return Vec::new();
                }
            }
        };
        set_gate(state, gate.0, gate.1, gate.2, finished);
        announce_gate(pair, self.behavior, state, gate.0)
            .into_iter()
            .collect()
    }

    /// The listings of two-way rounds are stale once another mode ran;
    /// coming back starts from a confirmed `--resync`.
    async fn forget_bisync_listings(&self) {
        let workdir = self.env.paths.bisync_workdir(&self.pair.id);
        match tokio::fs::remove_dir_all(&workdir).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => log::warn!("cannot delete {}: {err}", workdir.display()),
        }
    }

    /// Conflict files in the local tree; the previous count when the tree
    /// cannot be scanned.
    async fn count_conflicts(&self, previous: u32) -> u32 {
        let root = self.pair.local.clone();
        let matcher = DirMatcher::new(&self.filters.rendered);
        let naming = Naming::from(self.behavior.conflict_loser);
        let scanned = tokio::task::spawn_blocking(move || find_conflicts(&root, &matcher, naming))
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
    use std::path::PathBuf;
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

        let code = execute(&rclone, vec![], true).await.unwrap();

        assert_eq!(code, 7);
    }

    const SIGNAL_CHILD: &str = "SYNCFAST_TEST_SIGNAL_CHILD";

    /// Runs as a child of the test below: a process that executed a round
    /// without forwarding signals, then waits.
    async fn signal_child(ready: PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let rclone = script(dir.path(), "exit 0");
        execute(&rclone, vec![], false).await.unwrap();
        std::fs::write(ready, "").unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }

    #[tokio::test]
    async fn a_process_that_ran_a_round_without_forwarding_still_dies_of_sigterm() {
        if let Some(ready) = std::env::var_os(SIGNAL_CHILD) {
            signal_child(PathBuf::from(ready)).await;
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "run::tests::a_process_that_ran_a_round_without_forwarding_still_dies_of_sigterm",
                "--nocapture",
            ])
            .env(SIGNAL_CHILD, &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while !ready.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        rustix::process::kill_process(pid, Signal::TERM).unwrap();
        let status = child.wait().unwrap();

        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(15)
        );
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

        let (code, ()) = tokio::join!(execute(&rclone, vec![], true), interrupter);

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

        let now = fresh_start(&paths, "p", RoundMode::Normal, stamp_clock());

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

        let now = fresh_start(&paths, "p", RoundMode::Normal, stamp_clock());

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
