//! The restore as a job of its own (spec phase 3, step 9, ADR 0007): the
//! call that asks for it writes `restores/<id>.json` and starts
//! `syncfast restore-run <id>` detached, so the restore ends neither with
//! the window nor with the agent that asked. Readers learn how it stands
//! from the job file, the progress file and the lock.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, BufReader};
use tokio::process::Command;
use tokio::time::Instant;

use super::config::BackupConfig;
use super::key::Key;
use super::keystore::KeyStore;
use super::lifecycle::repository_of;
use super::restore::{
    BACKUP_BUSY, DESTINATION_EXISTS, RestoreForm, RestoreLine, RestoreProgress, RestoreSummary,
    check_snapshot_ref, classify_restore, parse_restore_line, restore_args, restore_form,
};
use super::run::{KEY_ABSENT, PROGRESS_EVERY};
use super::snapshots::{SnapshotView, parse_snapshots};
use crate::config::{Config, resolve};
use crate::ops::start_detached;
use crate::paths::Paths;
use crate::restic::{Repository, Restic, ResticError};
use crate::run::{Interrupts, forward_interrupt, wait_interrupt};
use crate::state::{is_locked, is_running, try_lock, write_json, write_json_or_warn};
use futures_core::future::BoxFuture;

/// How long a job may go without its lock before it counts as one that
/// never started or died.
const START_GRACE_SECONDS: i64 = 10;
const JOBS_KEPT: usize = 20;
const LONGEST_ID: usize = 40;
const UNIT_PREFIX: &str = "syncfast-restore-";
const STDERR_KEPT: usize = 64 * 1024;
const STOPPED: &str = "The restore stopped before it finished.";
const NO_SNAPSHOTS: &str = "This backup has no snapshots yet.";

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// The request is wrong; fix it and try again.
    #[error("{0}")]
    Invalid(String),
    #[error("no restore has the id \"{0}\"")]
    NotFound(String),
    /// The destination is inside the folder of a sync with a round running.
    #[error("this sync is running; try again when it finishes")]
    Busy,
    #[error("{0}")]
    Failed(String),
}

impl RestoreError {
    /// 2 for something the caller must fix, 3 when a round is running, 1
    /// for anything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Invalid(_) | Self::NotFound(_) => 2,
            Self::Busy => 3,
            Self::Failed(_) => 1,
        }
    }

    /// A stable name for the error, for JSON output.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::NotFound(_) => "not_found",
            Self::Busy => "busy",
            Self::Failed(_) => "failed",
        }
    }
}

fn failed(what: &str, err: impl std::fmt::Display) -> RestoreError {
    RestoreError::Failed(format!("{what}: {err}"))
}

fn from_restic(err: &ResticError) -> RestoreError {
    match err {
        ResticError::Locked { .. } => RestoreError::Failed(BACKUP_BUSY.to_owned()),
        other => RestoreError::Failed(other.to_string()),
    }
}

/// Starts `syncfast restore-run <id>` on its own.
pub trait RestoreStarter: Send + Sync {
    /// `Err` says why the restore could not be started.
    fn start<'a>(&'a self, job_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
}

/// A child process of its own session, or a transient unit on the host
/// inside the Flatpak sandbox.
#[derive(Debug, Clone)]
pub struct DetachedRestoreStarter {
    bin: PathBuf,
}

impl DetachedRestoreStarter {
    #[must_use]
    pub fn new(bin: PathBuf) -> Self {
        Self { bin }
    }
}

impl RestoreStarter for DetachedRestoreStarter {
    fn start<'a>(&'a self, job_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(start_detached(
            &self.bin,
            "restore-run",
            UNIT_PREFIX,
            job_id,
        ))
    }
}

/// What a restore borrows from.
pub struct RestoreEnv<'a> {
    pub paths: &'a Paths,
    pub restic: &'a Restic,
    pub keys: &'a dyn KeyStore,
}

impl std::fmt::Debug for RestoreEnv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreEnv")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

/// What to restore, from which backup, and into which new folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRequest {
    pub backup: String,
    /// `latest` or a snapshot id.
    pub snapshot: String,
    /// Absolute paths of the snapshot; none restores all of it.
    pub paths: Vec<String>,
    /// The folder to create: it must not exist, its parent must.
    pub destination: PathBuf,
    /// Say what would be restored and create nothing.
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RestoreReport {
    /// `None` for a dry run, which keeps no job.
    pub id: Option<String>,
    pub backup: String,
    pub snapshot: String,
    pub destination: String,
    pub files_restored: u64,
    pub bytes_restored: u64,
    pub total_files: u64,
    pub total_bytes: u64,
    /// `running`, `done`, `failed` or `dry_run`.
    pub status: &'static str,
    pub finished: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Running,
    Done,
    Failed,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    id: String,
    backup: String,
    /// The id the request resolved to: `latest` does not move under a
    /// running job.
    snapshot: String,
    destination: String,
    form: RestoreForm,
    phase: Phase,
    error: Option<String>,
    files_restored: u64,
    bytes_restored: u64,
    total_files: u64,
    total_bytes: u64,
    started: Timestamp,
    finished: Option<Timestamp>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= LONGEST_ID
        && id.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-')
}

fn unknown(id: &str) -> RestoreError {
    RestoreError::NotFound(id.to_owned())
}

/// The state of a job file as a reader sees it: one whose process is gone
/// without writing an outcome has failed.
fn effective(paths: &Paths, stored: &Stored) -> (Phase, Option<String>) {
    let gone = stored.phase == Phase::Running
        && !is_locked(&paths.restore_lock_file(&stored.id))
        && Timestamp::now().as_second() - stored.started.as_second() > START_GRACE_SECONDS;
    if gone {
        (Phase::Failed, Some(STOPPED.to_owned()))
    } else {
        (stored.phase, stored.error.clone())
    }
}

async fn read_progress(paths: &Paths, id: &str) -> Option<RestoreProgress> {
    let bytes = tokio::fs::read(paths.restore_progress_file(id))
        .await
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

impl Stored {
    async fn report(&self, paths: &Paths) -> RestoreReport {
        let (phase, error) = effective(paths, self);
        let (files_restored, bytes_restored, total_files, total_bytes) = match phase {
            Phase::Running => read_progress(paths, &self.id).await.map_or(
                (0, 0, self.total_files, self.total_bytes),
                |progress| {
                    (
                        progress.files_restored.min(progress.total_files),
                        progress.bytes_restored.min(progress.total_bytes),
                        progress.total_files,
                        progress.total_bytes,
                    )
                },
            ),
            _ => (
                self.files_restored,
                self.bytes_restored,
                self.total_files,
                self.total_bytes,
            ),
        };
        RestoreReport {
            id: Some(self.id.clone()),
            backup: self.backup.clone(),
            snapshot: self.snapshot.clone(),
            destination: self.destination.clone(),
            files_restored,
            bytes_restored,
            total_files,
            total_bytes,
            status: phase.name(),
            finished: phase != Phase::Running,
            error,
        }
    }
}

async fn load(paths: &Paths, id: &str) -> Result<Stored, RestoreError> {
    if !valid_id(id) {
        return Err(unknown(id));
    }
    let bytes = match tokio::fs::read(paths.restore_file(id)).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(unknown(id)),
        Err(err) => return Err(failed("cannot read the restore", err)),
    };
    serde_json::from_slice(&bytes).map_err(|err| failed("cannot read the restore", err))
}

async fn save(paths: &Paths, stored: &Stored) -> Result<(), RestoreError> {
    write_json(paths.restore_file(&stored.id), stored)
        .await
        .map_err(|err| failed("cannot save the restore", err))
}

/// A fresh id, from the clock: sortable, and safe in a file name.
async fn new_id(paths: &Paths) -> String {
    loop {
        let id = Timestamp::now().strftime("%Y%m%d-%H%M%S-%3f").to_string();
        if !tokio::fs::try_exists(paths.restore_file(&id))
            .await
            .unwrap_or(false)
        {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// The ids of the job files, oldest first.
async fn job_ids(paths: &Paths) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(paths.restores_dir()).await else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|id| valid_id(id))
        {
            ids.push(id.to_owned());
        }
    }
    ids.sort();
    ids
}

/// Deletes the oldest job files beyond [`JOBS_KEPT`], leaving the ones
/// still running.
async fn prune(paths: &Paths) {
    let ids = job_ids(paths).await;
    let excess = ids.len().saturating_sub(JOBS_KEPT);
    for id in ids.into_iter().take(excess) {
        if !is_locked(&paths.restore_lock_file(&id)) {
            let _ = tokio::fs::remove_file(paths.restore_file(&id)).await;
            let _ = tokio::fs::remove_file(paths.restore_progress_file(&id)).await;
        }
    }
}

async fn find_backup(paths: &Paths, backup_id: &str) -> Result<BackupConfig, RestoreError> {
    let config = Config::load(paths)
        .await
        .map_err(|err| RestoreError::Invalid(err.to_string()))?;
    config
        .backups()
        .iter()
        .find(|backup| backup.id == backup_id)
        .cloned()
        .ok_or_else(|| RestoreError::Invalid(format!("no backup has the id \"{backup_id}\"")))
}

/// A sync with a round running holds the folder: nothing new goes inside.
async fn check_not_in_running_sync(paths: &Paths, destination: &Path) -> Result<(), RestoreError> {
    let config = Config::load(paths)
        .await
        .map_err(|err| RestoreError::Invalid(err.to_string()))?;
    let target = resolve(destination);
    for pair in config.pairs() {
        if target.starts_with(resolve(&pair.local)) && is_running(paths, &pair.id) {
            return Err(RestoreError::Busy);
        }
    }
    Ok(())
}

async fn resolve_snapshot(
    env: &RestoreEnv<'_>,
    repository: &Repository,
    key: &Key,
    wanted: &str,
) -> Result<SnapshotView, RestoreError> {
    let mut found = if wanted == "latest" {
        let output = env
            .restic
            .run(
                repository,
                key,
                &["snapshots", "--json", "--latest", "1"],
                false,
            )
            .await
            .map_err(|err| from_restic(&err))?;
        parse_snapshots(&output.stdout).map_err(|_| {
            RestoreError::Failed("restic gave output syncfast cannot read".to_owned())
        })?
    } else {
        let wanted = wanted.to_ascii_lowercase();
        env.restic
            .snapshots(repository, key)
            .await
            .map_err(|err| from_restic(&err))?
            .into_iter()
            .filter(|snapshot| snapshot.id.starts_with(&wanted))
            .collect()
    };
    found.sort_unstable_by_key(|snapshot| std::cmp::Reverse(snapshot.time));
    match (wanted, found.len()) {
        ("latest", 0) => Err(RestoreError::Invalid(NO_SNAPSHOTS.to_owned())),
        (_, 0) => Err(RestoreError::Invalid(format!(
            "this backup has no snapshot \"{wanted}\""
        ))),
        ("latest", _) | (_, 1) => Ok(found.swap_remove(0)),
        _ => Err(RestoreError::Invalid(format!(
            "\"{wanted}\" matches more than one snapshot; use more of the id"
        ))),
    }
}

/// Checks the request and, unless it is a dry run, makes the new folder,
/// writes the job and starts it. The report says how the job stands: the
/// restore itself goes on in the process the starter made.
///
/// # Errors
/// The snapshot, the destination or a path is refused
/// ([`RestoreError::Invalid`]), the destination is in a sync with a round
/// running ([`RestoreError::Busy`]), or restic or the job cannot be run.
pub async fn start_restore(
    env: &RestoreEnv<'_>,
    starter: &dyn RestoreStarter,
    request: &RestoreRequest,
) -> Result<RestoreReport, RestoreError> {
    check_snapshot_ref(&request.snapshot).map_err(RestoreError::Invalid)?;
    let target = request
        .destination
        .to_str()
        .filter(|target| request.destination.is_absolute() && !target.contains('\0'))
        .ok_or_else(|| {
            RestoreError::Invalid("The destination must be an absolute path.".to_owned())
        })?;
    if tokio::fs::symlink_metadata(&request.destination)
        .await
        .is_ok()
    {
        return Err(RestoreError::Invalid(DESTINATION_EXISTS.to_owned()));
    }
    let backup = find_backup(env.paths, &request.backup).await?;
    check_not_in_running_sync(env.paths, &request.destination).await?;
    let key = env
        .keys
        .get(&backup.id)
        .await
        .map_err(|err| RestoreError::Failed(err.to_string()))?
        .ok_or_else(|| RestoreError::Failed(KEY_ABSENT.to_owned()))?;
    let repository =
        repository_of(env.paths, &backup).map_err(|err| RestoreError::Invalid(err.to_string()))?;
    let snapshot = resolve_snapshot(env, &repository, &key, &request.snapshot).await?;
    let form = restore_form(&snapshot.paths, &request.paths).map_err(RestoreError::Invalid)?;

    if request.dry_run {
        return dry_run(env, &repository, &key, &snapshot, &form, target, &backup).await;
    }
    create_destination(&request.destination).await?;
    let stored = Stored {
        id: new_id(env.paths).await,
        backup: backup.id,
        snapshot: snapshot.id,
        destination: target.to_owned(),
        form,
        phase: Phase::Running,
        error: None,
        files_restored: 0,
        bytes_restored: 0,
        total_files: 0,
        total_bytes: 0,
        started: Timestamp::now(),
        finished: None,
    };
    if let Err(err) = save(env.paths, &stored).await {
        let _ = tokio::fs::remove_dir(&request.destination).await;
        return Err(err);
    }
    prune(env.paths).await;
    if let Err(reason) = starter.start(&stored.id).await {
        let _ = tokio::fs::remove_dir(&request.destination).await;
        let _ = tokio::fs::remove_file(env.paths.restore_file(&stored.id)).await;
        return Err(RestoreError::Failed(reason));
    }
    restore_status(env.paths, &stored.id).await
}

async fn create_destination(destination: &Path) -> Result<(), RestoreError> {
    tokio::fs::create_dir(destination)
        .await
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::AlreadyExists => {
                RestoreError::Invalid(DESTINATION_EXISTS.to_owned())
            }
            std::io::ErrorKind::NotFound => {
                RestoreError::Invalid("The folder to restore into does not exist.".to_owned())
            }
            std::io::ErrorKind::PermissionDenied => RestoreError::Invalid(format!(
                "syncfast cannot write in {}.",
                destination.parent().unwrap_or(destination).display()
            )),
            _ => failed("cannot create the destination", err),
        })
}

async fn dry_run(
    env: &RestoreEnv<'_>,
    repository: &Repository,
    key: &Key,
    snapshot: &SnapshotView,
    form: &RestoreForm,
    target: &str,
    backup: &BackupConfig,
) -> Result<RestoreReport, RestoreError> {
    let args = restore_args(&snapshot.id, form, target, true);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = env
        .restic
        .run(repository, key, &args, false)
        .await
        .map_err(|err| from_restic(&err))?;
    let summary = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| match parse_restore_line(line) {
            RestoreLine::Summary(summary) => Some(summary),
            _ => None,
        })
        .next_back();
    let summary =
        classify_restore(Some(0), summary.as_ref(), b"").map_err(RestoreError::Invalid)?;
    Ok(RestoreReport {
        id: None,
        backup: backup.id.clone(),
        snapshot: snapshot.id.clone(),
        destination: target.to_owned(),
        files_restored: 0,
        bytes_restored: 0,
        total_files: summary.total_files,
        total_bytes: summary.total_bytes,
        status: "dry_run",
        finished: true,
        error: None,
    })
}

/// How a restore stands.
///
/// # Errors
/// No such job, or its file cannot be read.
pub async fn restore_status(paths: &Paths, id: &str) -> Result<RestoreReport, RestoreError> {
    Ok(load(paths, id).await?.report(paths).await)
}

/// The restores that are running now, oldest first: what the backup
/// screens show as `Restoring…`.
pub async fn running_restores(paths: &Paths) -> Vec<RestoreReport> {
    let mut running = Vec::new();
    for id in job_ids(paths).await {
        let Ok(stored) = load(paths, &id).await else {
            continue;
        };
        let report = stored.report(paths).await;
        if !report.finished {
            running.push(report);
        }
    }
    running
}

fn host_kill_command(app_unit: &str) -> (&'static str, Vec<String>) {
    (
        "flatpak-spawn",
        [
            "--host",
            "systemctl",
            "--user",
            "kill",
            "--signal=SIGINT",
            app_unit,
        ]
        .map(str::to_owned)
        .into(),
    )
}

/// Sends SIGINT to a running restore: restic stops, the job ends as failed
/// with [`super::restore::CANCELLED`] and what it had restored stays in the
/// folder. Inside the Flatpak sandbox the job is a unit on the host, so the
/// unit is signalled.
///
/// # Errors
/// No such job, it is not running, or the signal cannot be sent.
pub async fn cancel_restore(paths: &Paths, id: &str, in_flatpak: bool) -> Result<(), RestoreError> {
    let stored = load(paths, id).await?;
    if !is_locked(&paths.restore_lock_file(&stored.id)) {
        return Err(RestoreError::Invalid(
            "That restore is not running.".to_owned(),
        ));
    }
    if in_flatpak {
        let unit = format!("{UNIT_PREFIX}{}.service", stored.id);
        let (program, args) = host_kill_command(&unit);
        let status = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|err| failed("cannot run flatpak-spawn", err))?;
        return if status.success() {
            Ok(())
        } else {
            Err(RestoreError::Failed(format!(
                "cannot stop the restore: {status}"
            )))
        };
    }
    let pid = tokio::fs::read_to_string(paths.restore_lock_file(&stored.id))
        .await
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| {
            RestoreError::Failed("cannot tell which process runs the restore".to_owned())
        })?;
    rustix::process::kill_process(pid, rustix::process::Signal::INT)
        .map_err(|err| failed("cannot stop the restore", err))
}

/// What the restore did, read from restic.
struct Streamed {
    exit_code: Option<i32>,
    summary: Option<RestoreSummary>,
    stderr: Vec<u8>,
}

async fn stream_restore(
    env: &RestoreEnv<'_>,
    stored: &Stored,
    repository: &Repository,
    key: &Key,
    forward_signals: bool,
) -> Result<Streamed, String> {
    let mut interrupts = if forward_signals {
        Some(Interrupts::install().map_err(|err| format!("cannot listen for signals: {err}"))?)
    } else {
        None
    };
    let args = restore_args(&stored.snapshot, &stored.form, &stored.destination, false);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut child = env
        .restic
        .spawn(repository, key, &args)
        .await
        .map_err(|err| err.to_string())?;
    let (Some(stdout_pipe), Some(mut stderr_pipe)) = (child.stdout.take(), child.stderr.take())
    else {
        return Err("cannot read the output of restic".to_owned());
    };
    let mut lines = BufReader::new(stdout_pipe).lines();
    let mut stderr = Vec::new();
    let mut summary = None;
    let mut last_progress: Option<Instant> = None;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut chunk = vec![0_u8; 4096];
    while stdout_open || stderr_open {
        tokio::select! {
            line = async { lines.next_line().await }, if stdout_open => {
                match line {
                    Ok(Some(line)) => match parse_restore_line(&line) {
                        RestoreLine::Status(progress) => {
                            if last_progress.is_none_or(|at| at.elapsed() >= PROGRESS_EVERY) {
                                last_progress = Some(Instant::now());
                                write_progress(env.paths, &stored.id, &progress).await;
                            }
                        }
                        RestoreLine::Summary(found) => summary = Some(found),
                        RestoreLine::Other => {}
                    },
                    _ => stdout_open = false,
                }
            }
            read = async { stderr_pipe.read(&mut chunk).await }, if stderr_open => {
                match read {
                    Ok(0) | Err(_) => stderr_open = false,
                    Ok(count) => {
                        stderr.extend_from_slice(&chunk[..count]);
                        if stderr.len() > STDERR_KEPT {
                            let excess = stderr.len() - STDERR_KEPT;
                            stderr.drain(..excess);
                        }
                    }
                }
            }
            () = wait_interrupt(&mut interrupts) => forward_interrupt(&child, "restic"),
        }
    }
    let status = loop {
        tokio::select! {
            status = child.wait() => break status.map_err(|err| format!("cannot wait for restic: {err}"))?,
            () = wait_interrupt(&mut interrupts) => forward_interrupt(&child, "restic"),
        }
    };
    Ok(Streamed {
        exit_code: status.code(),
        summary,
        stderr,
    })
}

async fn write_progress(paths: &Paths, id: &str, progress: &RestoreProgress) {
    write_json_or_warn(paths.restore_progress_file(id), progress).await;
}

async fn execute(
    env: &RestoreEnv<'_>,
    stored: &Stored,
    forward_signals: bool,
) -> Result<RestoreSummary, String> {
    let backup = find_backup(env.paths, &stored.backup)
        .await
        .map_err(|err| err.to_string())?;
    let key = env
        .keys
        .get(&backup.id)
        .await
        .map_err(|err| err.to_string())?
        .ok_or_else(|| KEY_ABSENT.to_owned())?;
    let repository = repository_of(env.paths, &backup).map_err(|err| err.to_string())?;
    let streamed = stream_restore(env, stored, &repository, &key, forward_signals).await?;
    classify_restore(
        streamed.exit_code,
        streamed.summary.as_ref(),
        &streamed.stderr,
    )
}

/// What the detached `syncfast restore-run <id>` does: runs the restore
/// that [`start_restore`] planned and writes how it ended into its job
/// file. A restore that fails is not an error here: the job file says so.
/// With `forward_signals`, SIGINT and SIGTERM go to restic and the job
/// waits for it.
///
/// # Errors
/// No such job, or its file cannot be read or written.
pub async fn run_restore(
    env: &RestoreEnv<'_>,
    id: &str,
    forward_signals: bool,
) -> Result<(), RestoreError> {
    let mut stored = load(env.paths, id).await?;
    let lock_path = env.paths.restore_lock_file(id);
    let Some(mut lock) = try_lock(env.paths, lock_path.clone())
        .map_err(|err| failed("cannot lock the restore", err))?
    else {
        return Ok(());
    };
    if stored.phase != Phase::Running {
        return Ok(());
    }
    if let Err(err) = lock
        .set_len(0)
        .and_then(|()| write!(lock, "{}", std::process::id()))
    {
        log::warn!("cannot record the process of restore {id}: {err}");
    }
    match execute(env, &stored, forward_signals).await {
        Ok(summary) => {
            stored.phase = Phase::Done;
            stored.files_restored = summary.files_restored;
            stored.bytes_restored = summary.bytes_restored;
            stored.total_files = summary.total_files;
            stored.total_bytes = summary.total_bytes;
        }
        Err(message) => {
            stored.phase = Phase::Failed;
            stored.error = Some(message);
            if let Some(progress) = read_progress(env.paths, id).await {
                stored.files_restored = progress.files_restored.min(progress.total_files);
                stored.bytes_restored = progress.bytes_restored.min(progress.total_bytes);
                stored.total_files = progress.total_files;
                stored.total_bytes = progress.total_bytes;
            }
            // Only when nothing was written: a cancelled restore keeps
            // what it restored.
            let _ = tokio::fs::remove_dir(&stored.destination).await;
        }
    }
    stored.finished = Some(Timestamp::now());
    save(env.paths, &stored).await?;
    drop(lock);
    let _ = tokio::fs::remove_file(&lock_path).await;
    let _ = tokio::fs::remove_file(env.paths.restore_progress_file(id)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> Paths {
        Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        )
    }

    fn stored(id: &str, phase: Phase, started: Timestamp) -> Stored {
        Stored {
            id: id.to_owned(),
            backup: "docs".to_owned(),
            snapshot: "abcd1234".to_owned(),
            destination: "/tmp/out".to_owned(),
            form: RestoreForm::Whole { root: None },
            phase,
            error: None,
            files_restored: 0,
            bytes_restored: 0,
            total_files: 0,
            total_bytes: 0,
            started,
            finished: None,
        }
    }

    fn ago(seconds: i64) -> Timestamp {
        Timestamp::from_second(Timestamp::now().as_second() - seconds).unwrap()
    }

    #[test]
    fn exit_codes_follow_the_other_operations() {
        assert_eq!(RestoreError::Invalid(String::new()).exit_code(), 2);
        assert_eq!(RestoreError::NotFound(String::new()).exit_code(), 2);
        assert_eq!(RestoreError::Busy.exit_code(), 3);
        assert_eq!(RestoreError::Failed(String::new()).exit_code(), 1);
        assert_eq!(RestoreError::Invalid(String::new()).code(), "invalid");
        assert_eq!(RestoreError::NotFound(String::new()).code(), "not_found");
        assert_eq!(RestoreError::Busy.code(), "busy");
        assert_eq!(RestoreError::Failed(String::new()).code(), "failed");
    }

    #[test]
    fn ids_are_digits_and_dashes_only() {
        assert!(valid_id("20261108-143000-123"));
        for bad in ["", "../x", "a", &"1".repeat(41)] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_running_job_without_a_lock_fails_once_the_start_grace_is_over() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());

        let fresh = stored("1", Phase::Running, ago(2)).report(&paths).await;
        let old = stored("2", Phase::Running, ago(60)).report(&paths).await;

        assert_eq!((fresh.status, fresh.finished), ("running", false));
        assert_eq!((old.status, old.finished), ("failed", true));
        assert_eq!(old.error.as_deref(), Some(STOPPED));
    }

    #[tokio::test]
    async fn a_running_job_with_its_lock_keeps_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let _lock = try_lock(&paths, paths.restore_lock_file("3"))
            .unwrap()
            .unwrap();

        let report = stored("3", Phase::Running, ago(600)).report(&paths).await;

        assert_eq!(report.status, "running");
        assert_eq!(report.error, None);
    }

    #[tokio::test]
    async fn a_job_that_ended_keeps_its_own_answer() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let mut done = stored("4", Phase::Done, ago(600));
        done.files_restored = 5;
        done.total_files = 5;

        let report = done.report(&paths).await;

        assert_eq!(
            (report.status, report.finished, report.files_restored),
            ("done", true, 5)
        );
    }

    #[tokio::test]
    async fn a_running_job_shows_the_progress_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        tokio::fs::create_dir_all(paths.restores_dir())
            .await
            .unwrap();
        write_progress(
            &paths,
            "5",
            &RestoreProgress {
                percent_done: Some(0.5),
                files_restored: 9,
                total_files: 4,
                bytes_restored: 50,
                total_bytes: 100,
            },
        )
        .await;

        let report = stored("5", Phase::Running, ago(2)).report(&paths).await;

        assert_eq!(
            (
                report.files_restored,
                report.total_files,
                report.bytes_restored,
                report.total_bytes
            ),
            (4, 4, 50, 100)
        );
    }

    #[tokio::test]
    async fn only_running_jobs_are_listed_as_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        save(
            &paths,
            &stored("20260101-000000-000", Phase::Done, ago(600)),
        )
        .await
        .unwrap();
        save(
            &paths,
            &stored("20260101-000001-000", Phase::Running, ago(2)),
        )
        .await
        .unwrap();
        save(
            &paths,
            &stored("20260101-000002-000", Phase::Running, ago(600)),
        )
        .await
        .unwrap();
        write_progress(
            &paths,
            "20260101-000001-000",
            &RestoreProgress {
                percent_done: None,
                files_restored: 1,
                total_files: 2,
                bytes_restored: 1,
                total_bytes: 2,
            },
        )
        .await;

        let running = running_restores(&paths).await;

        assert_eq!(
            running
                .iter()
                .filter_map(|r| r.id.as_deref())
                .collect::<Vec<_>>(),
            ["20260101-000001-000"]
        );
    }

    #[tokio::test]
    async fn a_job_that_does_not_exist_is_not_found() {
        let dir = tempfile::tempdir().unwrap();

        assert!(matches!(
            restore_status(&paths(dir.path()), "123").await,
            Err(RestoreError::NotFound(_))
        ));
        assert!(matches!(
            restore_status(&paths(dir.path()), "../x").await,
            Err(RestoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn the_oldest_finished_jobs_beyond_the_limit_are_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        for number in 0..=JOBS_KEPT {
            let id = format!("20260101-0000{number:02}-000");
            save(&paths, &stored(&id, Phase::Done, ago(600)))
                .await
                .unwrap();
        }

        prune(&paths).await;

        assert_eq!(job_ids(&paths).await.len(), JOBS_KEPT);
        assert!(!paths.restore_file("20260101-000000-000").exists());
    }

    #[tokio::test]
    async fn a_job_that_is_running_is_not_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        for number in 0..=JOBS_KEPT {
            let id = format!("20260101-0000{number:02}-000");
            save(&paths, &stored(&id, Phase::Done, ago(600)))
                .await
                .unwrap();
        }
        let _lock = try_lock(&paths, paths.restore_lock_file("20260101-000000-000"))
            .unwrap()
            .unwrap();

        prune(&paths).await;

        assert!(paths.restore_file("20260101-000000-000").exists());
    }

    #[test]
    fn the_sandbox_stops_a_restore_through_its_unit_on_the_host() {
        assert_eq!(
            host_kill_command("syncfast-restore-1.service"),
            (
                "flatpak-spawn",
                vec![
                    "--host".to_owned(),
                    "systemctl".to_owned(),
                    "--user".to_owned(),
                    "kill".to_owned(),
                    "--signal=SIGINT".to_owned(),
                    "syncfast-restore-1.service".to_owned(),
                ]
            )
        );
    }

    #[test]
    fn a_locked_repository_reads_as_a_busy_backup() {
        let err = from_restic(&ResticError::Locked {
            message: "x".to_owned(),
        });

        assert_eq!(err.to_string(), BACKUP_BUSY);
    }
}
