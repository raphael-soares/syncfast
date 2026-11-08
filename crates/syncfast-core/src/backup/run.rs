//! One round of one backup: `syncfast backup-run <id>` (spec step 6). The
//! round decides whether it may run, runs `restic backup` and then `restic
//! forget --prune`, and turns how they ended into the backup's state and,
//! when the user must act, a notification.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Zoned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};

use crate::backup::classify::{BackupLine, BackupOutcome, classify_backup, parse_backup_line};
use crate::backup::config::{BackupBehavior, BackupConfig};
use crate::backup::destination::{Destination, DestinationError};
use crate::backup::effects::{
    BackupEvent, RunContext, SkipContext, apply_gate, apply_key_failure, apply_outcome,
    apply_retention_result, apply_skip, begin_attempt, clear_gate, retention_message,
};
use crate::backup::excludes::{render_excludes_live, write_excludes};
use crate::backup::keystore::KeyStore;
use crate::backup::run_decision::{
    BackupDecision, DecisionInput, SkipReason, decide_backup, source_check,
};
use crate::backup::state::{BackupState, BackupSummary};
use crate::backup::status::{BackupProgressFile, RunningStep};
use crate::bisync::{Fault, stamp};
use crate::config::{Config, GlobalConfigError};
use crate::network::Network;
use crate::notify::{Notifier, Policy, notify_backup};
use crate::paths::Paths;
use crate::rclone::RcloneError;
use crate::restic::{Repository, Restic, ResticError, backup_args, forget_args};
use crate::run::{Interrupts, forward_interrupt};
use crate::state::{GateKind, RUNS_KEPT, StateError, try_lock, write_json};

/// The exit code that tells the unit to try again soon.
const EXIT_TEMPFAIL: u8 = 75;
/// The first line of every run log: which step wrote it.
const LOG_HEADER: &str = r#"{"syncfast_step":"backup"}"#;
const PROGRESS_EVERY: Duration = Duration::from_secs(1);
pub const KEY_ABSENT: &str = "The key for this backup is not saved on this computer.";

pub struct BackupRunEnv<'a> {
    pub paths: &'a Paths,
    pub restic: &'a Restic,
    pub keys: &'a dyn KeyStore,
    pub network: &'a dyn Network,
    pub notifier: &'a dyn Notifier,
    pub clock: &'a (dyn Fn() -> Zoned + Send + Sync),
    /// SIGINT and SIGTERM of this process go to restic, and the round waits
    /// for it to release the repository lock.
    pub forward_signals: bool,
}

impl std::fmt::Debug for BackupRunEnv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupRunEnv")
            .field("paths", &self.paths)
            .field("forward_signals", &self.forward_signals)
            .finish_non_exhaustive()
    }
}

impl BackupRunEnv<'_> {
    fn now(&self) -> Zoned {
        (self.clock)()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackupRunError {
    #[error("{0}")]
    Config(#[from] GlobalConfigError),
    #[error("no backup has the id \"{0}\"")]
    UnknownBackup(String),
    #[error("{0}")]
    Destination(#[from] DestinationError),
    #[error("{0}")]
    Rclone(#[from] RcloneError),
    #[error("{0}")]
    State(#[from] StateError),
    #[error("{0}")]
    Restic(#[from] ResticError),
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl BackupRunError {
    fn io(context: &'static str) -> impl FnOnce(std::io::Error) -> Self {
        move |source| Self::Io { context, source }
    }

    /// 2 for a config the user must fix, 1 for anything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config(_) | Self::UnknownBackup(_) | Self::Destination(_) => 2,
            Self::Rclone(_) | Self::State(_) | Self::Restic(_) | Self::Io { .. } => 1,
        }
    }
}

/// How a round ended. Each is a normal end: the state says the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupRunReport {
    /// Another round of this backup holds the lock.
    Busy,
    ConfigInvalid,
    Skipped(SkipReason),
    /// A folder to back up is missing or empty.
    GateSet(Fault),
    /// The key is not in the key storage, or the storage cannot be read.
    KeyUnavailable,
    Ran(BackupOutcome),
}

impl BackupRunReport {
    /// 75 when the unit should try again soon, else 0.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Skipped(SkipReason::Offline | SkipReason::DestinationUnavailable) => {
                EXIT_TEMPFAIL
            }
            _ => 0,
        }
    }
}

struct Round<'a> {
    backup: &'a BackupConfig,
    behavior: &'a BackupBehavior,
    dest: &'a Destination,
    zone: jiff::tz::TimeZone,
    failures_after: u8,
}

/// # Errors
/// The id is unknown, the global config is invalid, or syncfast itself fails
/// (state files, restic not startable).
pub async fn run_backup(
    env: &BackupRunEnv<'_>,
    backup_id: &str,
) -> Result<BackupRunReport, BackupRunError> {
    let config = Config::load(env.paths).await?;
    let backup = config
        .backups()
        .iter()
        .find(|backup| backup.id == backup_id)
        .cloned()
        .ok_or_else(|| BackupRunError::UnknownBackup(backup_id.to_owned()))?;
    let Some(lock) = try_lock(env.paths, env.paths.backup_lock_file(backup_id))? else {
        return Ok(BackupRunReport::Busy);
    };
    let mut state = load_state(env, backup_id).await?;
    let policy = config.defaults().notifications;
    let accounts = env.restic.rclone().listremotes().await?;
    let problem = config
        .validate_backups(env.paths, &accounts)
        .errors
        .into_iter()
        .find(|error| error.backup_id == backup_id)
        .map(|error| error.message);
    let behavior = match problem.map_or_else(|| backup.behavior(), Err) {
        Ok(behavior) => behavior,
        Err(message) => {
            let now = env.now().timestamp();
            let events = apply_gate(&mut state, now, GateKind::ConfigError, message, None);
            conclude(env, &backup, &policy, &state, lock, &events, true).await?;
            return Ok(BackupRunReport::ConfigInvalid);
        }
    };
    let mut dirty = false;
    if state
        .gate
        .as_ref()
        .is_some_and(|gate| gate.kind == GateKind::ConfigError)
    {
        clear_gate(&mut state);
        dirty = true;
    }
    let dest = Destination::parse(&backup.dest)?;
    let now = env.now();
    let round = Round {
        backup: &backup,
        behavior: &behavior,
        dest: &dest,
        zone: now.time_zone().clone(),
        failures_after: policy.failures_after,
    };
    let dest_is_remote = matches!(dest, Destination::Account { .. });
    let online = !dest_is_remote || env.network.is_online().await;
    let dest_available = match &dest {
        Destination::Local(path) => tokio::fs::try_exists(path.join("config"))
            .await
            .unwrap_or(false),
        Destination::Account { .. } => true,
    };
    let decision = decide_backup(&DecisionInput {
        enabled: backup.enabled,
        online,
        dest_is_remote,
        dest_available,
        gate: state.gate.as_ref(),
        source_check: source_check(&backup.paths),
    });
    let (events, report, save) = match decision {
        BackupDecision::Skip(reason) => {
            let events = apply_skip(
                &mut state,
                reason,
                &SkipContext {
                    now: now.timestamp(),
                    zone: round.zone.clone(),
                    every: behavior.every,
                    dest: &backup.dest,
                },
            );
            let save = dirty
                || matches!(
                    reason,
                    SkipReason::Offline | SkipReason::DestinationUnavailable
                );
            (events, BackupRunReport::Skipped(reason), save)
        }
        BackupDecision::SetGate {
            kind,
            fault,
            detail,
        } => {
            let events = apply_gate(&mut state, now.timestamp(), kind, detail, Some(fault));
            (events, BackupRunReport::GateSet(fault), true)
        }
        BackupDecision::Run => {
            let (events, report) = backup_round(env, &round, &now, &mut state).await?;
            (events, report, true)
        }
    };
    conclude(env, &backup, &policy, &state, lock, &events, save).await?;
    Ok(report)
}

/// Saves the state, frees the lock so a status read sees the backup idle,
/// then tells the user.
async fn conclude(
    env: &BackupRunEnv<'_>,
    backup: &BackupConfig,
    policy: &Policy,
    state: &BackupState,
    lock: std::fs::File,
    events: &[BackupEvent],
    save: bool,
) -> Result<(), BackupRunError> {
    if save {
        state.save(env.paths, &backup.id).await?;
    }
    drop(lock);
    notify_backup(env.notifier, policy, backup, events);
    Ok(())
}

async fn load_state(
    env: &BackupRunEnv<'_>,
    backup_id: &str,
) -> Result<BackupState, BackupRunError> {
    match BackupState::load(env.paths, backup_id).await {
        Err(StateError::Parse { path, source }) => {
            let mut aside = path.clone().into_os_string();
            aside.push(format!(".corrupt-{}", stamp(&env.now())));
            log::warn!(
                "{} is not valid ({source}); keeping it as {} and starting over",
                path.display(),
                Path::new(&aside).display()
            );
            tokio::fs::rename(&path, &aside)
                .await
                .map_err(BackupRunError::io("setting aside a corrupt state file"))?;
            Ok(BackupState::default())
        }
        other => Ok(other?),
    }
}

async fn backup_round(
    env: &BackupRunEnv<'_>,
    round: &Round<'_>,
    started: &Zoned,
    state: &mut BackupState,
) -> Result<(Vec<BackupEvent>, BackupRunReport), BackupRunError> {
    let backup = round.backup;
    let key = match env.keys.get(&backup.id).await {
        Ok(Some(key)) => key,
        Ok(None) => return Ok(key_unavailable(state, KEY_ABSENT.to_owned(), started)),
        Err(err) => return Ok(key_unavailable(state, err.to_string(), started)),
    };
    begin_attempt(state, started.timestamp());
    let state_dir = env.paths.backup_state_dir(&backup.id);
    tokio::fs::create_dir_all(&state_dir)
        .await
        .map_err(BackupRunError::io("creating the backup's state folder"))?;
    let excludes_file = env.paths.backup_excludes_file(&backup.id);
    write_excludes(&excludes_file, &render_excludes_live(backup).await)
        .map_err(BackupRunError::io("writing the excludes file"))?;
    let repository = Repository::new(round.dest.clone(), state_dir)
        .with_limits(round.behavior.limit_upload, round.behavior.limit_download);
    let args = backup_args(
        &backup.paths,
        round.behavior.one_file_system,
        &excludes_file,
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let runs_dir = env.paths.backup_runs_dir(&backup.id);
    tokio::fs::create_dir_all(&runs_dir)
        .await
        .map_err(BackupRunError::io("creating the runs folder"))?;
    let (mut log, log_file) = RunLog::create(&runs_dir, started)
        .await
        .map_err(BackupRunError::io("creating the run log"))?;
    write_progress(
        env.paths,
        &backup.id,
        &BackupProgressFile::of_step(RunningStep::Backup),
    )
    .await;

    let streamed = stream_backup(env, &backup.id, &repository, &key, &args, &mut log).await;
    let streamed = match streamed {
        Ok(streamed) => streamed,
        Err(err) => {
            remove_progress(env.paths, &backup.id).await;
            return Err(err);
        }
    };
    let outcome = classify_backup(
        streamed.exit_code,
        streamed.summary.as_ref(),
        &streamed.stderr,
    );
    let finished = env.now();
    let ctx = RunContext {
        started: started.timestamp(),
        finished: finished.timestamp(),
        zone: round.zone.clone(),
        every: round.behavior.every,
        failures_after: round.failures_after,
        exit_code: streamed.exit_code,
        summary: streamed.summary.clone(),
        log_file,
    };
    let mut events = apply_outcome(state, &outcome, &ctx);
    let stored = streamed
        .summary
        .as_ref()
        .is_some_and(|summary| summary.total_files_processed > 0);
    if stored
        && matches!(
            outcome,
            BackupOutcome::Ok | BackupOutcome::OkWithWarnings { .. }
        )
    {
        let result = forget(env, round, &repository, &key, &mut log).await;
        events.extend(apply_retention_result(state, result, env.now().timestamp()));
    }
    remove_progress(env.paths, &backup.id).await;
    if let Err(err) = prune_run_logs(&runs_dir).await {
        log::warn!("cannot prune {}: {err}", runs_dir.display());
    }
    Ok((events, BackupRunReport::Ran(outcome)))
}

fn key_unavailable(
    state: &mut BackupState,
    detail: String,
    now: &Zoned,
) -> (Vec<BackupEvent>, BackupRunReport) {
    (
        apply_key_failure(state, detail, now.timestamp()),
        BackupRunReport::KeyUnavailable,
    )
}

/// `restic forget --prune`. A failure here never changes the backup's own
/// result: it is counted by [`apply_retention_result`].
async fn forget(
    env: &BackupRunEnv<'_>,
    round: &Round<'_>,
    repository: &Repository,
    key: &crate::backup::key::Key,
    log: &mut RunLog,
) -> Result<(), String> {
    let args = forget_args(round.behavior.keep);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match env
        .restic
        .run(repository, key, &args, env.forward_signals)
        .await
    {
        Ok(output) => {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                log.line(line).await;
            }
            Ok(())
        }
        Err(err) => {
            let message = err.to_string();
            log.line(&retention_message(&message)).await;
            Err(message)
        }
    }
}

struct Streamed {
    exit_code: Option<i32>,
    summary: Option<BackupSummary>,
    stderr: Vec<String>,
}

/// Starts `restic backup` and follows it to its end: progress to
/// `progress.json`, everything else to the log.
async fn stream_backup(
    env: &BackupRunEnv<'_>,
    backup_id: &str,
    repository: &Repository,
    key: &crate::backup::key::Key,
    args: &[&str],
    log: &mut RunLog,
) -> Result<Streamed, BackupRunError> {
    let mut interrupts = if env.forward_signals {
        Some(Interrupts::install().map_err(BackupRunError::io("listening for signals"))?)
    } else {
        None
    };
    let mut child = env.restic.spawn(repository, key, args).await?;
    let mut stdout = child.stdout.take().map(|pipe| BufReader::new(pipe).lines());
    let mut stderr = child.stderr.take().map(|pipe| BufReader::new(pipe).lines());
    let mut throttle = Throttle::new(PROGRESS_EVERY);
    let mut summary = None;
    let mut stderr_lines = Vec::new();
    while stdout.is_some() || stderr.is_some() {
        tokio::select! {
            line = next_line(&mut stdout), if stdout.is_some() => match line {
                Ok(Some(line)) => match parse_backup_line(&line) {
                    BackupLine::Status(status) => {
                        if throttle.allows(Instant::now()) {
                            write_progress(
                                env.paths,
                                backup_id,
                                &BackupProgressFile::from_status(&status),
                            )
                            .await;
                        }
                    }
                    BackupLine::Summary(found) => {
                        log.line(&line).await;
                        summary = Some(found);
                    }
                    _ => log.line(&line).await,
                },
                _ => stdout = None,
            },
            line = next_line(&mut stderr), if stderr.is_some() => match line {
                Ok(Some(line)) => {
                    log.line(&line).await;
                    stderr_lines.push(line);
                }
                _ => stderr = None,
            },
            () = wait_interrupt(&mut interrupts) => forward_interrupt(&child, "restic"),
        }
    }
    let status = loop {
        tokio::select! {
            status = child.wait() => break status.map_err(BackupRunError::io("waiting for restic"))?,
            () = wait_interrupt(&mut interrupts) => forward_interrupt(&child, "restic"),
        }
    };
    Ok(Streamed {
        exit_code: status.code(),
        summary,
        stderr: stderr_lines,
    })
}

async fn next_line<R: AsyncBufRead + Unpin>(
    lines: &mut Option<Lines<R>>,
) -> std::io::Result<Option<String>> {
    match lines {
        Some(lines) => lines.next_line().await,
        None => Ok(None),
    }
}

async fn wait_interrupt(interrupts: &mut Option<Interrupts>) {
    match interrupts {
        Some(interrupts) => interrupts.next().await,
        None => std::future::pending().await,
    }
}

struct Throttle {
    every: Duration,
    last: Option<Instant>,
}

impl Throttle {
    fn new(every: Duration) -> Self {
        Self { every, last: None }
    }

    fn allows(&mut self, now: Instant) -> bool {
        let due = self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= self.every);
        if due {
            self.last = Some(now);
        }
        due
    }
}

async fn write_progress(paths: &Paths, backup_id: &str, progress: &BackupProgressFile) {
    if let Err(err) = write_json(paths.backup_progress_file(backup_id), progress).await {
        log::warn!("{err}");
    }
}

async fn remove_progress(paths: &Paths, backup_id: &str) {
    let file = paths.backup_progress_file(backup_id);
    match tokio::fs::remove_file(&file).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => log::warn!("cannot delete {}: {err}", file.display()),
    }
}

/// The run's log: `runs/<stamp>.jsonl`. A line that cannot be written is a
/// warning, not a stop: the backup matters more than its log.
struct RunLog {
    file: tokio::fs::File,
}

impl RunLog {
    /// Creates the log named for `now`, or for the first second after it
    /// that is free.
    async fn create(dir: &Path, now: &Zoned) -> std::io::Result<(Self, PathBuf)> {
        let mut at = now.clone();
        loop {
            let path = dir.join(format!("{}.jsonl", stamp(&at)));
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
            {
                Ok(file) => {
                    let mut log = Self { file };
                    log.line(LOG_HEADER).await;
                    return Ok((log, path));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    at = at
                        .checked_add(SignedDuration::from_secs(1))
                        .map_err(std::io::Error::other)?;
                }
                Err(err) => return Err(err),
            }
        }
    }

    async fn line(&mut self, text: &str) {
        let mut bytes = Vec::with_capacity(text.len() + 1);
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(b'\n');
        let written = match self.file.write_all(&bytes).await {
            Ok(()) => self.file.flush().await,
            Err(err) => Err(err),
        };
        if let Err(err) = written {
            log::warn!("cannot write the run log: {err}");
        }
    }
}

/// The logs to delete so that [`RUNS_KEPT`] remain: the oldest, by name
/// (names are timestamps).
fn logs_to_delete(mut names: Vec<String>, kept: usize) -> Vec<String> {
    names.retain(|name| {
        Path::new(name)
            .extension()
            .is_some_and(|ext| ext == "jsonl")
    });
    names.sort();
    let excess = names.len().saturating_sub(kept);
    names.truncate(excess);
    names
}

async fn prune_run_logs(dir: &Path) -> std::io::Result<()> {
    let mut names = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    for name in logs_to_delete(names, RUNS_KEPT) {
        if let Err(err) = tokio::fs::remove_file(dir.join(&name)).await {
            log::warn!("cannot delete {name}: {err}");
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_is_told_to_retry_only_when_the_destination_was_out_of_reach() {
        let code = |report: BackupRunReport| report.exit_code();

        assert_eq!(code(BackupRunReport::Skipped(SkipReason::Offline)), 75);
        assert_eq!(
            code(BackupRunReport::Skipped(SkipReason::DestinationUnavailable)),
            75
        );
        assert_eq!(code(BackupRunReport::Skipped(SkipReason::Paused)), 0);
        assert_eq!(code(BackupRunReport::Skipped(SkipReason::Gated)), 0);
        assert_eq!(code(BackupRunReport::Busy), 0);
        assert_eq!(code(BackupRunReport::ConfigInvalid), 0);
        assert_eq!(code(BackupRunReport::KeyUnavailable), 0);
        assert_eq!(code(BackupRunReport::GateSet(Fault::SourceEmpty)), 0);
        assert_eq!(code(BackupRunReport::Ran(BackupOutcome::Cancelled)), 0);
    }

    #[test]
    fn errors_that_the_user_fixes_exit_2_and_the_rest_exit_1() {
        assert_eq!(BackupRunError::UnknownBackup("x".into()).exit_code(), 2);
        assert_eq!(
            BackupRunError::Config(GlobalConfigError::Toml("bad".into())).exit_code(),
            2
        );
        assert_eq!(
            BackupRunError::Io {
                context: "x",
                source: std::io::Error::other("y"),
            }
            .exit_code(),
            1
        );
    }

    #[test]
    fn progress_is_let_through_once_a_second() {
        let start = Instant::now();
        let mut throttle = Throttle::new(Duration::from_secs(1));

        assert!(throttle.allows(start));
        assert!(!throttle.allows(start + Duration::from_millis(400)));
        assert!(!throttle.allows(start + Duration::from_millis(999)));
        assert!(throttle.allows(start + Duration::from_secs(1)));
        assert!(!throttle.allows(start + Duration::from_millis(1500)));
        assert!(throttle.allows(start + Duration::from_secs(2)));
    }

    #[test]
    fn the_oldest_logs_beyond_the_kept_ones_go_and_other_files_stay() {
        let names: Vec<String> = [
            "20260103-100000.jsonl",
            "20260101-100000.jsonl",
            "notes.txt",
            "20260102-100000.jsonl",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        assert_eq!(logs_to_delete(names.clone(), 2), ["20260101-100000.jsonl"]);
        assert!(logs_to_delete(names.clone(), 3).is_empty());
        assert_eq!(logs_to_delete(names, 0).len(), 3);
    }

    #[tokio::test]
    async fn two_runs_in_the_same_second_get_two_logs_and_each_starts_with_the_step_header() {
        let dir = tempfile::tempdir().unwrap();
        let now: Zoned = "2026-05-01T10:00:00[UTC]".parse().unwrap();

        let (mut first, first_path) = RunLog::create(dir.path(), &now).await.unwrap();
        first.line("one").await;
        let (_second, second_path) = RunLog::create(dir.path(), &now).await.unwrap();

        assert_eq!(first_path.file_name().unwrap(), "20260501-100000.jsonl");
        assert_eq!(second_path.file_name().unwrap(), "20260501-100001.jsonl");
        assert_eq!(
            std::fs::read_to_string(&first_path).unwrap(),
            format!("{LOG_HEADER}\none\n")
        );
        assert_eq!(
            std::fs::read_to_string(&second_path).unwrap(),
            format!("{LOG_HEADER}\n")
        );
    }
}
