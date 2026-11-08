//! Copying and moving a file or a folder between this computer and a
//! remote, in any of the four directions (ADR 0007). Like a round, a
//! transfer runs detached: the call waits up to a limit and `transfer`
//! reads the outcome later.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::Instant;

use super::explore::{Location, classify};
use super::{DEFAULT_WAIT_SECONDS, OpError, Ops};
use crate::config::{Pair, lexical_clean, remote_within, resolve, split_remote};
use crate::flatpak;
use crate::network::BoxFuture;
use crate::paths::Paths;
use crate::rclone::Rclone;
use crate::state::{is_locked, is_running, try_lock, write_json};

const POLL: Duration = Duration::from_millis(200);
/// How long a transfer may go without its lock before it counts as one
/// that never started or died.
const START_GRACE_SECONDS: i64 = 10;
/// Job files kept in the state folder.
const TRANSFERS_KEPT: usize = 20;
const LONGEST_ID: usize = 40;
const UNIT_PREFIX: &str = "syncfast-transfer-";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Copy,
    Move,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Move => "move",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    File,
    Folder,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferOptions {
    /// Report what would be transferred and change nothing.
    pub dry_run: bool,
    /// How long to wait for the transfer to end before reporting it as
    /// still running.
    pub wait: Duration,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            wait: Duration::from_secs(DEFAULT_WAIT_SECONDS),
        }
    }
}

/// A transfer, planned or running or over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TransferView {
    /// Pass this to `transfer`; none for a dry run, which starts nothing.
    pub id: Option<String>,
    /// `copy` or `move`.
    pub operation: &'static str,
    /// A path on this computer, or `account:folder`.
    pub source: String,
    pub destination: String,
    /// `file` or `folder`.
    pub kind: &'static str,
    pub files: u64,
    /// Files whose size the backend does not report count as 0 bytes.
    pub bytes: u64,
    /// Done by the backend without downloading and uploading again.
    pub server_side: bool,
    /// `dry_run`, `running`, `done` or `failed`.
    pub status: &'static str,
    pub error: Option<String>,
    pub started: Option<Timestamp>,
    pub finished: Option<Timestamp>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TransferReport {
    pub transfer: TransferView,
    /// The transfer is not running: it is done, it failed or it was only
    /// planned. `false` means it goes on: read it again with `transfer`.
    pub finished: bool,
}

/// What a detached transfer reads to know what to do, and what it writes
/// back when it ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    id: String,
    operation: Operation,
    source: String,
    destination: String,
    kind: Kind,
    files: u64,
    bytes: u64,
    server_side: bool,
    phase: Phase,
    error: Option<String>,
    started: Timestamp,
    finished: Option<Timestamp>,
}

/// Starts `syncfast transfer-run <id>` on its own, so the transfer ends
/// neither with the call that asked for it nor with the MCP client.
pub trait TransferStarter: Send + Sync {
    /// `Err` says why the transfer could not be started.
    fn start<'a>(&'a self, transfer_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
}

/// A child process of its own session, or a transient unit on the host
/// when running in the Flatpak sandbox, which ends with its main process.
#[derive(Debug, Clone)]
pub struct DetachedStarter {
    bin: PathBuf,
}

impl DetachedStarter {
    #[must_use]
    pub fn new(bin: PathBuf) -> Self {
        Self { bin }
    }
}

impl TransferStarter for DetachedStarter {
    fn start<'a>(&'a self, transfer_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(start_detached(
            &self.bin,
            "transfer-run",
            UNIT_PREFIX,
            transfer_id,
        ))
    }
}

/// Starts `bin <subcommand> <job_id>` on its own: in a session of its
/// own, or as the transient unit `<unit_prefix><job_id>` on the host
/// inside the Flatpak sandbox.
pub(crate) async fn start_detached(
    bin: &Path,
    subcommand: &str,
    unit_prefix: &str,
    job_id: &str,
) -> Result<(), String> {
    let args = [subcommand, job_id];
    if let Some(app_id) = flatpak::app_id() {
        let unit = format!("{unit_prefix}{job_id}");
        let (program, full) = flatpak::transient_command(&app_id, &unit, &args);
        let status = Command::new(program)
            .args(full)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|err| format!("cannot run {program}: {err}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!("cannot start {subcommand}: {program} {status}"))
        };
    }
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|err| {
            format!(
                "cannot start {} {subcommand} {job_id}: {err}",
                bin.display()
            )
        })?;
    // Reaps the child; its outcome is in the job file.
    let subcommand = subcommand.to_owned();
    tokio::spawn(async move {
        if let Err(err) = child.wait().await {
            log::warn!("cannot wait for {subcommand}: {err}");
        }
    });
    Ok(())
}

/// Where a path lives, as rclone is told about it.
#[derive(Debug, Clone)]
struct Place {
    /// An absolute path, or `account:folder`.
    text: String,
    location: Location,
}

impl Place {
    fn local(path: &Path) -> Self {
        Self {
            text: path.to_string_lossy().into_owned(),
            location: Location::Local,
        }
    }

    fn remote(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            location: Location::Remote,
        }
    }

    fn account(&self) -> Option<&str> {
        match self.location {
            Location::Local => None,
            Location::Remote => split_remote(&self.text).map(|(account, _)| account),
        }
    }

    /// The root of an account, or of this computer's file system.
    fn is_root(&self) -> bool {
        match self.location {
            Location::Local => Path::new(&self.text).parent().is_none(),
            Location::Remote => {
                split_remote(&self.text).is_some_and(|(_, path)| path.split('/').all(str::is_empty))
            }
        }
    }

    /// This place is `outer` or inside it.
    fn within(&self, outer: &Self) -> bool {
        match (self.location, outer.location) {
            (Location::Local, Location::Local) => {
                resolve(Path::new(&self.text)).starts_with(resolve(Path::new(&outer.text)))
            }
            (Location::Remote, Location::Remote) => remote_within(&self.text, &outer.text),
            _ => false,
        }
    }
}

/// The folders a sync keeps in step.
fn pair_places(pair: &Pair) -> [Place; 2] {
    [Place::local(&pair.local), Place::remote(&pair.remote)]
}

struct Plan {
    operation: Operation,
    source: Place,
    destination: Place,
    kind: Kind,
    files: u64,
    bytes: u64,
    server_side: bool,
}

impl Plan {
    fn stored(&self, id: String) -> Stored {
        Stored {
            id,
            operation: self.operation,
            source: self.source.text.clone(),
            destination: self.destination.text.clone(),
            kind: self.kind,
            files: self.files,
            bytes: self.bytes,
            server_side: self.server_side,
            phase: Phase::Running,
            error: None,
            started: Timestamp::now(),
            finished: None,
        }
    }

    fn dry_run_view(&self) -> TransferView {
        TransferView {
            id: None,
            operation: self.operation.name(),
            source: self.source.text.clone(),
            destination: self.destination.text.clone(),
            kind: self.kind.name(),
            files: self.files,
            bytes: self.bytes,
            server_side: self.server_side,
            status: "dry_run",
            error: None,
            started: None,
            finished: None,
            note: "Nothing was changed. Run it again without dry_run to transfer.".to_owned(),
        }
    }
}

/// The rclone command and flags that do `operation` on a `kind`. A folder
/// moves with `moveto` inside one account (a rename on the server) and
/// with `move` between accounts, which also carries empty folders over and
/// removes the ones left behind.
fn rclone_command(
    operation: Operation,
    kind: Kind,
    server_side: bool,
) -> (&'static str, &'static [&'static str]) {
    match (operation, kind, server_side) {
        (Operation::Copy, Kind::File, _) => ("copyto", &[]),
        (Operation::Copy, Kind::Folder, _) => ("copy", &["--create-empty-src-dirs"]),
        (Operation::Move, Kind::File, _) | (Operation::Move, Kind::Folder, true) => ("moveto", &[]),
        (Operation::Move, Kind::Folder, false) => (
            "move",
            &["--create-empty-src-dirs", "--delete-empty-src-dirs"],
        ),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= LONGEST_ID
        && id.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-')
}

fn unknown(id: &str) -> OpError {
    OpError::Invalid(format!("no transfer has the id \"{id}\""))
}

/// The state of a job file as a reader sees it: one whose process is gone
/// without writing an outcome has failed.
fn effective(paths: &Paths, stored: &Stored) -> (Phase, Option<String>) {
    let gone = stored.phase == Phase::Running
        && !is_locked(&paths.transfer_lock_file(&stored.id))
        && Timestamp::now().as_second() - stored.started.as_second() > START_GRACE_SECONDS;
    if gone {
        (
            Phase::Failed,
            Some("the transfer stopped before it finished".to_owned()),
        )
    } else {
        (stored.phase, stored.error.clone())
    }
}

fn describe(stored: &Stored, phase: Phase) -> String {
    match (phase, stored.operation) {
        (Phase::Running, _) => format!(
            "Still running: read it again with `transfer {}` (or the show_transfer tool).",
            stored.id
        ),
        (Phase::Done, Operation::Copy) => "Copied. The source is untouched.".to_owned(),
        (Phase::Done, Operation::Move) => "Moved. The source is gone.".to_owned(),
        (Phase::Failed, Operation::Copy) => "The copy failed; the source is untouched.".to_owned(),
        (Phase::Failed, Operation::Move) => {
            "The move failed; what was not transferred is still at the source.".to_owned()
        }
    }
}

impl Stored {
    fn view(&self, paths: &Paths) -> TransferView {
        let (phase, error) = effective(paths, self);
        TransferView {
            id: Some(self.id.clone()),
            operation: self.operation.name(),
            source: self.source.clone(),
            destination: self.destination.clone(),
            kind: self.kind.name(),
            files: self.files,
            bytes: self.bytes,
            server_side: self.server_side,
            status: phase.name(),
            error,
            started: Some(self.started),
            finished: self.finished,
            note: describe(self, phase),
        }
    }
}

async fn load(paths: &Paths, id: &str) -> Result<Stored, OpError> {
    if !valid_id(id) {
        return Err(unknown(id));
    }
    let bytes = match tokio::fs::read(paths.transfer_file(id)).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(unknown(id)),
        Err(err) => return Err(OpError::Failed(format!("cannot read the transfer: {err}"))),
    };
    serde_json::from_slice(&bytes)
        .map_err(|err| OpError::Failed(format!("cannot read the transfer: {err}")))
}

/// A fresh id, from the clock: sortable, and safe in a file name.
async fn new_id(paths: &Paths) -> String {
    loop {
        let id = Timestamp::now().strftime("%Y%m%d-%H%M%S-%3f").to_string();
        if !tokio::fs::try_exists(paths.transfer_file(&id))
            .await
            .unwrap_or(false)
        {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Deletes the oldest job files beyond [`TRANSFERS_KEPT`], leaving the
/// ones still running.
async fn prune(paths: &Paths) {
    let Ok(mut entries) = tokio::fs::read_dir(paths.transfers_dir()).await else {
        return;
    };
    let mut ids = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
        {
            ids.push(id.to_owned());
        }
    }
    ids.sort();
    let excess = ids.len().saturating_sub(TRANSFERS_KEPT);
    for id in ids.into_iter().take(excess) {
        if !is_locked(&paths.transfer_lock_file(&id)) {
            let _ = tokio::fs::remove_file(paths.transfer_file(&id)).await;
        }
    }
}

/// What the detached `syncfast transfer-run <id>` does: runs the transfer
/// that `copy` or `move` planned and writes how it ended into its job file.
/// A transfer that fails is not an error here: the job file says so.
///
/// # Errors
/// No such transfer, or its job file cannot be read or written.
pub async fn run_transfer(paths: &Paths, rclone: &Rclone, id: &str) -> Result<(), OpError> {
    let mut stored = load(paths, id).await?;
    let Some(lock) = try_lock(paths, paths.transfer_lock_file(id))? else {
        return Ok(());
    };
    if stored.phase != Phase::Running {
        return Ok(());
    }
    let (command, flags) = rclone_command(stored.operation, stored.kind, stored.server_side);
    let result = rclone
        .transfer(command, flags, &stored.source, &stored.destination)
        .await;
    // `--delete-empty-src-dirs` keeps the folder it moved out of.
    if result.is_ok()
        && command == "move"
        && let Err(err) = rclone.rmdir(&stored.source).await
    {
        log::warn!("cannot remove the moved folder {}: {err}", stored.source);
    }
    stored.finished = Some(Timestamp::now());
    match result {
        Ok(()) => stored.phase = Phase::Done,
        Err(err) => {
            stored.phase = Phase::Failed;
            stored.error = Some(err.to_string());
        }
    }
    write_json(paths.transfer_file(id), &stored).await?;
    drop(lock);
    let _ = tokio::fs::remove_file(paths.transfer_lock_file(id)).await;
    Ok(())
}

impl Ops {
    /// Copies a file or a folder to a destination that does not exist yet.
    /// `source` and `destination` are each a path on this computer (`~`
    /// is the home folder) or `account:path`; the destination is the new
    /// file or folder itself, not the folder it goes into.
    ///
    /// The transfer runs detached and the call waits up to
    /// `options.wait`; when it ends in time its result is returned, and
    /// when it does not, the report has `finished: false` and
    /// [`Ops::transfer_status`] reads it later. With `options.dry_run` it
    /// only reports the files and bytes and changes nothing.
    ///
    /// Refused with [`OpError::Invalid`] when the destination exists (it
    /// is never overwritten or merged), the source does not exist, the
    /// source is the root of an account or is or contains the folder of a
    /// sync, or the destination is inside the source; with
    /// [`OpError::Busy`] when the source or the destination is inside the
    /// folder of a sync that is running.
    ///
    /// # Errors
    /// The above, an account that is not set up, rclone failing to read
    /// the source, the transfer failing within the wait, or the transfer
    /// not starting.
    pub async fn copy(
        &self,
        source: &str,
        destination: &str,
        options: TransferOptions,
    ) -> Result<TransferReport, OpError> {
        self.transfer_with(Operation::Copy, source, destination, options)
            .await
    }

    /// [`Ops::copy`], then the source is gone: rclone deletes what it
    /// transferred only after it transferred it without error, and a move
    /// inside one account is a rename on the server. A move that fails
    /// leaves the source where it is.
    ///
    /// # Errors
    /// Those of [`Ops::copy`].
    pub async fn r#move(
        &self,
        source: &str,
        destination: &str,
        options: TransferOptions,
    ) -> Result<TransferReport, OpError> {
        self.transfer_with(Operation::Move, source, destination, options)
            .await
    }

    /// A transfer started by `copy` or `move`, waiting up to `wait` for it
    /// to end. A transfer that failed is reported with its `error`.
    ///
    /// # Errors
    /// No transfer has this id.
    pub async fn transfer_status(
        &self,
        id: &str,
        wait: Duration,
    ) -> Result<TransferReport, OpError> {
        load(&self.paths, id).await?;
        let view = self.await_transfer(id, Instant::now() + wait).await?;
        Ok(report(view))
    }

    async fn transfer_with(
        &self,
        operation: Operation,
        source: &str,
        destination: &str,
        options: TransferOptions,
    ) -> Result<TransferReport, OpError> {
        let plan = self.plan(operation, source, destination).await?;
        if options.dry_run {
            return Ok(report(plan.dry_run_view()));
        }
        prune(&self.paths).await;
        let id = new_id(&self.paths).await;
        let deadline = Instant::now() + options.wait;
        write_json(self.paths.transfer_file(&id), &plan.stored(id.clone())).await?;
        if let Err(message) = self.transfers.start(&id).await {
            let _ = tokio::fs::remove_file(self.paths.transfer_file(&id)).await;
            return Err(OpError::Failed(message));
        }
        let view = self.await_transfer(&id, deadline).await?;
        if view.status == "failed" {
            return Err(OpError::Failed(view.error.unwrap_or_default()));
        }
        Ok(report(view))
    }

    async fn await_transfer(&self, id: &str, deadline: Instant) -> Result<TransferView, OpError> {
        loop {
            let view = load(&self.paths, id).await?.view(&self.paths);
            if view.status != "running" || Instant::now() >= deadline {
                return Ok(view);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    fn place(&self, text: &str) -> Result<Place, OpError> {
        let cwd = std::env::current_dir()
            .map_err(|err| OpError::Failed(format!("cannot read the current folder: {err}")))?;
        let (text, location) = classify(text, &self.paths.home, &cwd)?;
        Ok(match location {
            Location::Local => Place::local(&lexical_clean(Path::new(&text))),
            Location::Remote => Place::remote(&text),
        })
    }

    /// Everything that is checked before anything is touched.
    async fn plan(
        &self,
        operation: Operation,
        source: &str,
        destination: &str,
    ) -> Result<Plan, OpError> {
        let source = self.place(source)?;
        let destination = self.place(destination)?;
        self.check_accounts(&[&source, &destination]).await?;
        if source.is_root() {
            return Err(OpError::Invalid(format!(
                "{} is the root of an account or of this computer; transfer a folder inside it",
                source.text
            )));
        }
        self.check_syncs(&source, &destination).await?;
        if destination.is_root() {
            return Err(exists(&destination));
        }
        let found = self.rclone.stat(&source.text).await?.ok_or_else(|| {
            OpError::Invalid(format!("the source {} does not exist", source.text))
        })?;
        if destination.within(&source) {
            return Err(OpError::Invalid(format!(
                "the destination {} is inside the source {}",
                destination.text, source.text
            )));
        }
        if self.rclone.stat(&destination.text).await?.is_some() {
            return Err(exists(&destination));
        }
        let (kind, files, bytes) = if found.is_dir {
            let (files, bytes) = self.folder_size(&source.text).await?;
            (Kind::Folder, files, bytes)
        } else {
            (Kind::File, 1, u64::try_from(found.size).unwrap_or(0))
        };
        let server_side = match (source.location, destination.location) {
            (Location::Local, Location::Local) => true,
            _ => source.account().is_some() && source.account() == destination.account(),
        };
        Ok(Plan {
            operation,
            source,
            destination,
            kind,
            files,
            bytes,
            server_side,
        })
    }

    async fn check_accounts(&self, places: &[&Place]) -> Result<(), OpError> {
        let mut accounts = None;
        for place in places {
            let Some(account) = place.account() else {
                continue;
            };
            if accounts.is_none() {
                accounts = Some(self.rclone.listremotes().await?);
            }
            if accounts
                .as_ref()
                .is_some_and(|known| !known.iter().any(|name| name == account))
            {
                return Err(OpError::Invalid(format!(
                    "account \"{account}\" is not set up in rclone"
                )));
            }
        }
        Ok(())
    }

    /// Syncs and transfers: nothing may take the folder of a sync with it
    /// (that breaks the pair), and nothing may touch a folder a round is
    /// working on.
    async fn check_syncs(&self, source: &Place, destination: &Place) -> Result<(), OpError> {
        let config = self.load_config().await?;
        for pair in config.pairs() {
            let places = pair_places(pair);
            if places.iter().any(|folder| folder.within(source)) {
                return Err(OpError::Invalid(format!(
                    "{} is or contains the folder of the sync \"{}\"; moving it breaks the pair. Remove the sync first",
                    source.text, pair.id
                )));
            }
        }
        for pair in config.pairs() {
            let touched = pair_places(pair)
                .iter()
                .any(|folder| source.within(folder) || destination.within(folder));
            if touched && is_running(&self.paths, &pair.id) {
                return Err(OpError::Busy);
            }
        }
        Ok(())
    }

    /// Files and bytes under a folder, from `lsjson -R`.
    async fn folder_size(&self, path: &str) -> Result<(u64, u64), OpError> {
        let entries = self.rclone.list_tree(path).await?;
        let mut files = 0;
        let mut bytes = 0;
        for entry in entries.iter().filter(|entry| !entry.is_dir) {
            files += 1;
            bytes += u64::try_from(entry.size).unwrap_or(0);
        }
        Ok((files, bytes))
    }
}

fn exists(destination: &Place) -> OpError {
    OpError::Invalid(format!(
        "the destination {} already exists; nothing is overwritten or merged. Choose a new name",
        destination.text
    ))
}

fn report(transfer: TransferView) -> TransferReport {
    TransferReport {
        finished: transfer.status != "running",
        transfer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_moves_in_place_within_an_account_and_with_move_between_accounts() {
        assert_eq!(
            rclone_command(Operation::Move, Kind::Folder, true).0,
            "moveto"
        );
        let (command, flags) = rclone_command(Operation::Move, Kind::Folder, false);
        assert_eq!(command, "move");
        assert!(flags.contains(&"--delete-empty-src-dirs"));
        assert_eq!(
            rclone_command(Operation::Move, Kind::File, false).0,
            "moveto"
        );
        assert_eq!(
            rclone_command(Operation::Copy, Kind::File, true).0,
            "copyto"
        );
        assert_eq!(
            rclone_command(Operation::Copy, Kind::Folder, true).0,
            "copy"
        );
    }

    #[test]
    fn only_ids_made_of_digits_and_dashes_name_a_transfer() {
        assert!(valid_id("20261103-101500-123"));
        assert!(!valid_id(""));
        assert!(!valid_id("../state"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id(&"1".repeat(LONGEST_ID + 1)));
    }

    #[test]
    fn the_root_of_an_account_and_of_the_computer_are_roots() {
        assert!(Place::remote("gdrive:").is_root());
        assert!(Place::remote("gdrive:/").is_root());
        assert!(!Place::remote("gdrive:Notes").is_root());
        assert!(Place::local(Path::new("/")).is_root());
        assert!(!Place::local(Path::new("/home")).is_root());
    }

    #[test]
    fn a_remote_place_is_inside_its_folder_and_not_inside_a_sibling_with_a_longer_name() {
        let notes = Place::remote("gdrive:Notes");

        assert!(Place::remote("gdrive:Notes/2026").within(&notes));
        assert!(Place::remote("gdrive:Notes").within(&notes));
        assert!(!Place::remote("gdrive:Notes2").within(&notes));
        assert!(!Place::remote("other:Notes").within(&notes));
        assert!(!Place::local(Path::new("/Notes")).within(&notes));
    }

    #[test]
    fn a_job_whose_process_is_gone_reads_as_failed_but_not_right_after_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("state"),
            dir.path().join("data"),
            dir.path().join("run"),
        );
        let plan = Plan {
            operation: Operation::Move,
            source: Place::local(Path::new("/a")),
            destination: Place::local(Path::new("/b")),
            kind: Kind::Folder,
            files: 1,
            bytes: 1,
            server_side: true,
        };
        let fresh = plan.stored("20261103-101500-123".to_owned());
        let mut old = fresh.clone();
        old.started = Timestamp::from_second(fresh.started.as_second() - 60).unwrap();

        assert_eq!(effective(&paths, &fresh).0, Phase::Running);
        let (phase, error) = effective(&paths, &old);
        assert_eq!(phase, Phase::Failed);
        assert!(error.is_some_and(|message| message.contains("stopped")));
    }
}
