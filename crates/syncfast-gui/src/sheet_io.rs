//! The I/O behind the sheets (Review, Add Sync, account): rclone calls,
//! folder checks and the files the confirmation leaves for the next round.
//! The backend runs these and turns the results into events.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use jiff::Zoned;
use syncfast_core::behavior::{Behavior, ReviewLevel, SyncMode};
use syncfast_core::bisync::{Fault, ReviewKind, fault_of};
use syncfast_core::config::{Config, Pair, expand_tilde, split_remote};
use syncfast_core::network::NetworkManager;
use syncfast_core::notify::NativeNotifier;
use syncfast_core::paths::Paths;
use syncfast_core::preview::{Preview, PreviewError, preview};
use syncfast_core::rclone::{Rclone, RcloneError};
use syncfast_core::run::RunEnv;
use syncfast_core::state::{ConfirmMarker, Gate, GateKind, State, lock_pair};
use tokio::sync::oneshot;

use crate::ui::account_sheet::{AccountMode, ConnectRequest, Secret};
use crate::ui::add_sync::{LocalFacts, LocalStatus, NewSync};
use crate::ui::{AccountStatus, RemoteRow};

/// Why a preview did not come out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewFailure {
    /// A round of the sync is running.
    Busy,
    /// The last error line of the dry run.
    Failed(String),
}

/// Every remote with its type and whether a Drive one has its own client ID.
///
/// # Errors
/// rclone cannot list the remotes or read its config.
pub async fn load_remotes(rclone: &Rclone) -> Result<Vec<RemoteRow>, String> {
    let kinds = rclone.remote_types().await.map_err(|e| e.to_string())?;
    let shared = rclone
        .drive_remotes_without_own_client_id()
        .await
        .map_err(|e| e.to_string())?;
    Ok(kinds
        .into_iter()
        .map(|(name, kind)| RemoteRow {
            own_client_id: !shared.contains(&name),
            name,
            kind,
        })
        .collect())
}

/// The folder names directly inside `remote_path`, sorted.
///
/// # Errors
/// The last line rclone printed.
pub async fn list_dirs(rclone: &Rclone, remote_path: &str) -> Result<Vec<String>, String> {
    let mut dirs = rclone
        .list_dirs(remote_path)
        .await
        .map_err(|e| e.to_string())?;
    dirs.sort_by_key(|name| name.to_lowercase());
    Ok(dirs)
}

/// `rclone mkdir`.
///
/// # Errors
/// The last line rclone printed.
pub async fn make_remote_dir(rclone: &Rclone, remote_path: &str) -> Result<(), String> {
    rclone.mkdir(remote_path).await.map_err(|e| e.to_string())
}

/// What `typed` names on this computer. `~` stands for `home`.
pub async fn check_local(home: &Path, typed: &str) -> LocalFacts {
    let path = expand_tilde(Path::new(typed), home);
    let status = match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_dir() => LocalStatus::Exists,
        Ok(_) => LocalStatus::NotAFolder,
        Err(_) => LocalStatus::Missing,
    };
    LocalFacts {
        path: typed.to_owned(),
        status,
    }
}

/// Creates the folder `typed` names and the missing ones above it.
///
/// # Errors
/// The folder cannot be created.
pub async fn create_local(home: &Path, typed: &str) -> Result<(), String> {
    let path = expand_tilde(Path::new(typed), home);
    tokio::fs::create_dir_all(&path)
        .await
        .map_err(|err| format!("cannot create {}: {err}", path.display()))
}

/// Writes the new sync into the config, unless the config would not accept
/// it: then nothing is written and the error is the config's own message.
/// Answers how often the new sync asks for a review.
///
/// # Errors
/// The config cannot be read or written, or rejects the sync.
pub async fn create_sync(
    paths: &Paths,
    rclone: &Rclone,
    new: NewSync,
) -> Result<ReviewLevel, String> {
    let mut config = Config::load(paths).await.map_err(|e| e.to_string())?;
    let id = new.id.clone();
    let pair = Pair {
        id: new.id,
        name: new.name,
        local: new.local,
        remote: new.remote,
        enabled: true,
        presets: new.presets,
        filters: new.filters,
        overrides: syncfast_core::settings::Overrides::default(),
    };
    config.add_pair(pair.clone()).map_err(|e| e.to_string())?;
    let accounts = rclone.listremotes().await.map_err(|e| e.to_string())?;
    let validation = config.validate(paths, &accounts);
    if let Some(error) = validation.errors.iter().find(|error| error.pair_id == id) {
        return Err(error.message.clone());
    }
    let review = Behavior::for_pair(&config.defaults(), &pair).review;
    Config::edit(paths, |config| config.add_pair(pair))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(review)
}

/// The dry run behind the Review sheet.
///
/// # Errors
/// [`PreviewFailure`].
pub async fn run_preview(
    paths: &Paths,
    rclone: &Rclone,
    pair_id: &str,
    kind: ReviewKind,
) -> Result<Preview, PreviewFailure> {
    let clock = Zoned::now;
    let env = RunEnv {
        paths,
        rclone,
        network: &NetworkManager,
        notifier: &NativeNotifier,
        clock: &clock,
        forward_signals: false,
    };
    preview(&env, pair_id, kind).await.map_err(|err| match err {
        PreviewError::Busy => PreviewFailure::Busy,
        other => PreviewFailure::Failed(other.to_string()),
    })
}

/// Leaves the marker the next round of `pair_id` consumes.
///
/// # Errors
/// The marker cannot be written.
pub async fn confirm_review(
    paths: &Paths,
    pair_id: &str,
    kind: ReviewKind,
    mode: SyncMode,
    filters_hash: String,
) -> Result<(), String> {
    ConfirmMarker {
        kind,
        filters_hash,
        mode,
    }
    .save(paths, pair_id)
    .await
    .map_err(|e| e.to_string())
}

/// The saved `client_id` and `client_secret` of `name`, to start the sign-in
/// again from.
///
/// # Errors
/// rclone cannot read its config.
pub async fn load_credentials(rclone: &Rclone, name: &str) -> Result<(String, Secret), String> {
    let found = rclone
        .drive_client_credentials(name)
        .await
        .map_err(|e| e.to_string())?;
    let (id, secret) = found.unwrap_or_default();
    Ok((id, Secret::new(secret)))
}

/// Runs the sign-in and waits for it. Dropping `cancel`'s sender is not a
/// cancel; sending on it is, and the rclone child is killed.
///
/// # Errors
/// The last line of rclone's stderr, or the reason it could not start.
/// `Ok(false)` is a cancel.
pub async fn connect(
    rclone: &Rclone,
    request: &ConnectRequest,
    cancel: oneshot::Receiver<()>,
) -> Result<bool, String> {
    let secret = request.client_secret.expose();
    let child = match &request.mode {
        AccountMode::Add | AccountMode::Rename { .. } => {
            rclone.create_drive_remote(&request.name, &request.client_id, secret)
        }
        AccountMode::Update { name } | AccountMode::ChangeClientId { name } => {
            rclone.update_drive_credentials(name, &request.client_id, secret)
        }
    }
    .map_err(|e| e.to_string())?;
    let finished = child.wait_with_output();
    tokio::pin!(finished);
    let output = tokio::select! {
        output = &mut finished => output.map_err(|e| e.to_string())?,
        Ok(()) = cancel => return Ok(false),
    };
    if output.status.success() {
        return Ok(true);
    }
    Err(
        last_line(&output.stderr).unwrap_or_else(|| match output.status.code() {
            Some(code) => format!("rclone exited with code {code}"),
            None => "rclone was stopped by a signal".to_owned(),
        }),
    )
}

fn last_line(stderr: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_owned)
}

/// After a sign-in to `account`: the syncs on it that were stopped for
/// lack of access can run again. Returns their ids.
///
/// # Errors
/// The config or a state file cannot be read or written.
pub async fn clear_auth_gates(paths: &Paths, account: &str) -> Result<Vec<String>, String> {
    let config = Config::load(paths).await.map_err(|e| e.to_string())?;
    let mut cleared = Vec::new();
    for pair in config.pairs() {
        if split_remote(&pair.remote).map(|(name, _)| name) != Some(account) {
            continue;
        }
        let mut state = State::load(paths, &pair.id)
            .await
            .map_err(|e| e.to_string())?;
        if state.gate.as_ref().map(|gate| gate.kind) != Some(GateKind::AuthError) {
            continue;
        }
        state.gate = None;
        let announced = GateKind::AuthError.notified_key();
        state.notified.retain(|key| *key != announced);
        state
            .save(paths, &pair.id)
            .await
            .map_err(|e| e.to_string())?;
        cleared.push(pair.id.clone());
    }
    Ok(cleared)
}

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const NO_ABOUT: &str = "doesn't support about";

/// What `rclone about` says of the sign-in of `name`. `online` tells why a
/// failure that is not a lost sign-in happened: no network, or something
/// the user has to read.
pub async fn check_account(rclone: &Rclone, online: bool, name: &str) -> AccountStatus {
    let detail = match tokio::time::timeout(CHECK_TIMEOUT, rclone.check_remote(name)).await {
        Ok(Ok(())) => return AccountStatus::SignedIn,
        Ok(Err(RcloneError::Failed { detail, .. })) => detail,
        Ok(Err(other)) => other.to_string(),
        Err(_) => {
            return AccountStatus::Problem(format!(
                "No answer from the account within {} seconds.",
                CHECK_TIMEOUT.as_secs()
            ));
        }
    };
    if detail.contains(NO_ABOUT) {
        return AccountStatus::SignedIn;
    }
    if fault_of(&detail) == Some(Fault::Auth) {
        AccountStatus::SignedOut
    } else if online {
        AccountStatus::Problem(detail)
    } else {
        AccountStatus::Offline
    }
}

/// How long removing or renaming an account waits for a running round of
/// its syncs.
pub const ACCOUNT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const SYNC_RUNNING: &str = "A sync on this account is running. Try again when it finishes.";

/// The locks of `ids`, so no round runs while their states change.
async fn lock_syncs(paths: &Paths, ids: &[String], timeout: Duration) -> Result<Vec<File>, String> {
    let mut locks = Vec::with_capacity(ids.len());
    for id in ids {
        match lock_pair(paths, id, timeout).await {
            Ok(Some(lock)) => locks.push(lock),
            Ok(None) => return Err(SYNC_RUNNING.to_owned()),
            Err(err) => return Err(err.to_string()),
        }
    }
    Ok(locks)
}

/// Takes `name` out of rclone's config; returns its syncs' ids, which owe a
/// reviewed first run if an account of that name returns.
///
/// # Errors
/// A sync is running, rclone fails, or a config or state file fails.
pub async fn remove_account(
    paths: &Paths,
    rclone: &Rclone,
    name: &str,
    lock_timeout: Duration,
) -> Result<Vec<String>, String> {
    let config = Config::load(paths).await.map_err(|e| e.to_string())?;
    let ids = pair_ids_on(&config, name);
    let _locks = lock_syncs(paths, &ids, lock_timeout).await?;
    rclone
        .delete_remote(name)
        .await
        .map_err(|e| e.to_string())?;
    for id in &ids {
        let mut state = State::load(paths, id).await.map_err(|e| e.to_string())?;
        state.applied_filters_hash = None;
        state.save(paths, id).await.map_err(|e| e.to_string())?;
        ConfirmMarker::remove(paths, id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(ids)
}

fn pair_ids_on(config: &Config, account: &str) -> Vec<String> {
    config
        .pairs()
        .iter()
        .filter(|pair| split_remote(&pair.remote).map(|(name, _)| name) == Some(account))
        .map(|pair| pair.id.clone())
        .collect()
}

/// Moves the syncs of `from` to `to` and holds each for a reviewed first run,
/// as bisync's listings are named after the remote. Returns their ids.
///
/// # Errors
/// A sync on `from` is running (nothing changes), or a file fails.
pub async fn rename_account(
    paths: &Paths,
    from: &str,
    to: &str,
    lock_timeout: Duration,
) -> Result<Vec<String>, String> {
    let config = Config::load(paths).await.map_err(|e| e.to_string())?;
    let _locks = lock_syncs(paths, &pair_ids_on(&config, from), lock_timeout).await?;
    let moved = Config::edit(paths, |config| config.rename_account(from, to))
        .await
        .map_err(|e| e.to_string())?;
    let held = GateKind::NeedsReviewFirstRun;
    for id in &moved {
        let mut state = State::load(paths, id).await.map_err(|e| e.to_string())?;
        state.gate = Some(Gate {
            kind: held,
            since: jiff::Timestamp::now(),
            detail: format!("Account renamed to {to}."),
            fault: None,
            auto_checked: None,
        });
        state.notified.retain(|key| *key != held.notified_key());
        state.save(paths, id).await.map_err(|e| e.to_string())?;
        ConfirmMarker::remove(paths, id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(moved)
}

/// Signs in. A new account that fails or is cancelled leaves no remote
/// behind; changed credentials are checked with `rclone about`.
///
/// # Errors
/// As [`connect`], or Google Drive refuses the account.
pub async fn sign_in(
    rclone: &Rclone,
    request: &ConnectRequest,
    cancel: oneshot::Receiver<()>,
) -> Result<bool, String> {
    match &request.mode {
        AccountMode::Add | AccountMode::Rename { .. } => {
            let before = rclone.listremotes().await.ok();
            let result = connect(rclone, request, cancel).await;
            if !matches!(result, Ok(true)) {
                discard_new_remote(rclone, &request.name, before.as_deref()).await;
            }
            result
        }
        AccountMode::Update { name } | AccountMode::ChangeClientId { name } => {
            let signed_in = connect(rclone, request, cancel).await?;
            if signed_in {
                verify_signed_in(rclone, name).await?;
            }
            Ok(signed_in)
        }
    }
}

/// Deletes `name` when it exists now and `before` shows it did not exist
/// before the sign-in started. Without `before` nothing is known: it stays.
async fn discard_new_remote(rclone: &Rclone, name: &str, before: Option<&[String]>) {
    let Some(before) = before else {
        return;
    };
    if before.iter().any(|existing| existing == name) {
        return;
    }
    let exists = rclone
        .listremotes()
        .await
        .is_ok_and(|now| now.iter().any(|existing| existing == name));
    if exists && let Err(err) = rclone.delete_remote(name).await {
        log::warn!("cannot remove the unfinished account \"{name}\": {err}");
    }
}

/// After a sign-in with new credentials: an account Google Drive refuses
/// (the API is off in the project, the client is not allowed) is reported
/// with Google's message. The new credentials and token stay: they are a
/// consistent pair, and work once the refusal is fixed on Google's side. A
/// lost sign-in or no answer in time is not a refusal.
async fn verify_signed_in(rclone: &Rclone, name: &str) -> Result<(), String> {
    let detail = match tokio::time::timeout(CHECK_TIMEOUT, rclone.check_remote(name)).await {
        Ok(Ok(())) | Err(_) => return Ok(()),
        Ok(Err(RcloneError::Failed { detail, .. })) => detail,
        Ok(Err(other)) => other.to_string(),
    };
    if fault_of(&detail) == Some(Fault::Auth) {
        return Ok(());
    }
    Err(format!(
        "Signed in, but Google Drive refused the account: {detail}"
    ))
}

#[cfg(test)]
#[allow(clippy::print_stderr)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    use syncfast_core::state::{Gate, try_lock_pair};

    use super::*;

    fn paths_in(root: &Path) -> Paths {
        Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        )
    }

    fn write_config(paths: &Paths, text: &str) {
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn pair_toml(id: &str, local: &Path, remote: &str) -> String {
        format!(
            "\n[[pair]]\nid = \"{id}\"\nname = \"{}\"\nlocal = \"{}\"\nremote = \"{remote}\"\n",
            id.to_uppercase(),
            local.display()
        )
    }

    /// The real rclone with a config of local remotes, or `None` with a
    /// warning when it is not installed.
    fn rclone_with(root: &Path, config: &str) -> Option<Rclone> {
        let Ok(rclone) = Rclone::from_env() else {
            eprintln!("skipping: rclone is not installed");
            return None;
        };
        let file = root.join("rclone.conf");
        std::fs::write(&file, config).unwrap();
        Some(rclone.with_config(file))
    }

    /// A fake `rclone` that appends its arguments to `args.txt` and then
    /// runs `body`.
    fn fake_rclone(root: &Path, body: &str) -> Rclone {
        let script = root.join("rclone");
        let log = root.join("args.txt");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\n{body}\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Rclone::new(script)
    }

    fn request(mode: AccountMode, name: &str) -> ConnectRequest {
        ConnectRequest {
            mode,
            name: name.into(),
            client_id: "id.apps".into(),
            client_secret: Secret::new("s3cret".into()),
        }
    }

    fn new_sync(id: &str, local: PathBuf, remote: &str) -> NewSync {
        NewSync {
            id: id.into(),
            name: id.to_uppercase(),
            local,
            remote: remote.into(),
            presets: vec!["system_junk".into()],
            filters: vec!["- *.bak".into()],
        }
    }

    #[tokio::test]
    async fn typed_path_is_reported_as_a_folder_a_file_or_missing_and_tilde_means_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join("docs")).unwrap();
        std::fs::write(home.join("note.txt"), "x").unwrap();

        let folder = check_local(&home, "~/docs").await;
        let file = check_local(&home, "~/note.txt").await;
        let missing = check_local(&home, "~/nope").await;

        assert_eq!(folder.status, LocalStatus::Exists);
        assert_eq!(
            folder.path, "~/docs",
            "the answer carries the text as typed"
        );
        assert_eq!(file.status, LocalStatus::NotAFolder);
        assert_eq!(missing.status, LocalStatus::Missing);
    }

    #[tokio::test]
    async fn creating_a_local_folder_makes_the_missing_parents_too() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");

        create_local(&home, "~/a/b/c").await.unwrap();

        assert!(home.join("a/b/c").is_dir());
    }

    #[tokio::test]
    async fn confirming_a_review_leaves_the_marker_the_next_round_reads() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());

        confirm_review(
            &paths,
            "notes",
            ReviewKind::Force,
            SyncMode::MirrorUp,
            "abc".into(),
        )
        .await
        .unwrap();

        let marker = ConfirmMarker::load(&paths, "notes").await.unwrap();
        assert_eq!(
            marker,
            Some(ConfirmMarker {
                kind: ReviewKind::Force,
                filters_hash: "abc".into(),
                mode: SyncMode::MirrorUp,
            })
        );
    }

    #[tokio::test]
    async fn after_a_sign_in_only_syncs_of_that_account_stopped_by_auth_run_again() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let local = dir.path().join("l");
        write_config(
            &paths,
            &format!(
                "version = 1\n{}{}{}",
                pair_toml("mine", &local.join("a"), "gdrive:a"),
                pair_toml("other", &local.join("b"), "work:b"),
                pair_toml("review", &local.join("c"), "gdrive:c"),
            ),
        );
        let gate = |kind| {
            Some(Gate {
                kind,
                since: jiff::Timestamp::UNIX_EPOCH,
                detail: String::new(),
                fault: None,
                auto_checked: None,
            })
        };
        for (id, kind) in [
            ("mine", GateKind::AuthError),
            ("other", GateKind::AuthError),
            ("review", GateKind::NeedsReviewFirstRun),
        ] {
            State {
                gate: gate(kind),
                notified: vec![GateKind::AuthError.notified_key(), "failures".into()],
                ..State::default()
            }
            .save(&paths, id)
            .await
            .unwrap();
        }

        let cleared = clear_auth_gates(&paths, "gdrive").await.unwrap();

        assert_eq!(cleared, ["mine"]);
        let mine = State::load(&paths, "mine").await.unwrap();
        assert_eq!(mine.gate, None);
        assert_eq!(mine.notified, ["failures"]);
        let other = State::load(&paths, "other").await.unwrap();
        assert_eq!(other.gate.map(|g| g.kind), Some(GateKind::AuthError));
        let review = State::load(&paths, "review").await.unwrap();
        assert_eq!(
            review.gate.map(|g| g.kind),
            Some(GateKind::NeedsReviewFirstRun)
        );
    }

    #[tokio::test]
    async fn adding_an_account_runs_config_create_with_the_users_client() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(dir.path(), "exit 0");
        let (_keep, cancel) = oneshot::channel();

        let done = connect(&rclone, &request(AccountMode::Add, "trabalho"), cancel)
            .await
            .unwrap();

        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert!(done);
        assert_eq!(
            args.trim(),
            "config create trabalho drive client_id=id.apps client_secret=s3cret scope=drive config_is_local=true"
        );
    }

    #[tokio::test]
    async fn reconnecting_runs_config_update_on_the_existing_remote() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(dir.path(), "exit 0");
        let (_keep, cancel) = oneshot::channel();
        let mode = AccountMode::Update {
            name: "gdrive".into(),
        };

        connect(&rclone, &request(mode, "gdrive"), cancel)
            .await
            .unwrap();

        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert_eq!(
            args.trim(),
            "config update gdrive client_id=id.apps client_secret=s3cret"
        );
    }

    #[tokio::test]
    async fn a_failed_sign_in_reports_the_last_line_rclone_printed_on_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(
            dir.path(),
            "echo 'NOTICE: opening browser' >&2\necho 'Failed to configure token: access_denied' >&2\necho >&2\nexit 1",
        );
        let (_keep, cancel) = oneshot::channel();

        let error = connect(&rclone, &request(AccountMode::Add, "x"), cancel)
            .await
            .unwrap_err();

        assert_eq!(error, "Failed to configure token: access_denied");
    }

    #[tokio::test]
    async fn cancelling_a_sign_in_kills_the_rclone_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let rclone = fake_rclone(
            dir.path(),
            &format!("echo $$ > '{}'\nexec sleep 60", pid_file.display()),
        );
        let (cancel_tx, cancel) = oneshot::channel();
        let request = request(AccountMode::Add, "x");
        let job = tokio::spawn(async move { connect(&rclone, &request, cancel).await });
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };

        cancel_tx.send(()).unwrap();
        let outcome = job.await.unwrap();

        assert_eq!(outcome, Ok(false));
        let alive = || Path::new(&format!("/proc/{pid}")).exists();
        for _ in 0..100 {
            if !alive() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!alive(), "the rclone child must be gone");
    }

    #[tokio::test]
    async fn remotes_come_with_their_type_and_drive_remotes_say_whether_the_client_is_their_own() {
        let dir = tempfile::tempdir().unwrap();
        let config = "[dev]\ntype = local\n\n[shared]\ntype = drive\n\n[own]\ntype = drive\nclient_id = 1.apps\nclient_secret = x\n";
        let Some(rclone) = rclone_with(dir.path(), config) else {
            return;
        };

        let remotes = load_remotes(&rclone).await.unwrap();

        let row = |name: &str| remotes.iter().find(|r| r.name == name).unwrap().clone();
        assert_eq!(remotes.len(), 3);
        assert!(row("dev").kind == "local" && !row("dev").shares_client_id());
        assert!(row("shared").shares_client_id());
        assert!(row("own").is_drive() && !row("own").shares_client_id());
    }

    #[tokio::test]
    async fn folders_of_a_remote_come_sorted_ignoring_case() {
        let dir = tempfile::tempdir().unwrap();
        let Some(rclone) = rclone_with(dir.path(), "[dev]\ntype = local\n") else {
            return;
        };
        let root = dir.path().join("root");
        for name in ["beta", "Alpha", "gamma"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }

        let dirs = list_dirs(&rclone, &format!("dev:{}", root.display()))
            .await
            .unwrap();

        assert_eq!(dirs, ["Alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn a_valid_new_sync_is_saved_with_its_filters_and_presets() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let Some(rclone) = rclone_with(dir.path(), "[dev]\ntype = local\n") else {
            return;
        };
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();

        let review = create_sync(&paths, &rclone, new_sync("dev", local.clone(), "dev:"))
            .await
            .unwrap();

        assert_eq!(review, ReviewLevel::Deletions);

        let config = Config::load(&paths).await.unwrap();
        let saved = &config.pairs()[0];
        assert_eq!(
            (
                saved.id.as_str(),
                saved.remote.as_str(),
                saved.local.clone()
            ),
            ("dev", "dev:", local)
        );
        assert_eq!(saved.presets, ["system_junk"]);
        assert_eq!(saved.filters, ["- *.bak"]);
    }

    #[tokio::test]
    async fn a_new_sync_reports_the_review_level_of_the_defaults_it_inherits() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let Some(rclone) = rclone_with(dir.path(), "[dev]\ntype = local\n") else {
            return;
        };
        write_config(
            &paths,
            "version = 2\n\n[defaults]\nreview = \"automatic\"\n",
        );
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();

        let review = create_sync(&paths, &rclone, new_sync("dev", local, "dev:"))
            .await
            .unwrap();

        assert_eq!(review, ReviewLevel::Automatic);
    }

    #[tokio::test]
    async fn a_new_sync_that_overlaps_another_is_refused_with_the_configs_message_and_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let Some(rclone) = rclone_with(dir.path(), "[dev]\ntype = local\n") else {
            return;
        };
        let first = dir.path().join("first");
        std::fs::create_dir_all(&first).unwrap();
        write_config(
            &paths,
            &format!("version = 1\n{}", pair_toml("notes", &first, "dev:notes")),
        );
        let second = dir.path().join("second");

        let error = create_sync(
            &paths,
            &rclone,
            new_sync("second", second, "dev:notes/deep"),
        )
        .await
        .unwrap_err();

        assert_eq!(error, "remote folder overlaps \"NOTES\"");
        assert_eq!(Config::load(&paths).await.unwrap().pairs().len(), 1);
    }

    #[tokio::test]
    async fn a_new_sync_on_an_account_rclone_does_not_have_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let Some(rclone) = rclone_with(dir.path(), "[dev]\ntype = local\n") else {
            return;
        };

        let error = create_sync(
            &paths,
            &rclone,
            new_sync("x", dir.path().join("l"), "ghost:"),
        )
        .await
        .unwrap_err();

        assert_eq!(error, "account \"ghost\" is not set up in rclone");
    }

    #[tokio::test]
    async fn a_preview_while_a_round_holds_the_lock_is_busy() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let Some(rclone) = rclone_with(dir.path(), "[dev]\ntype = local\n") else {
            return;
        };
        let local = dir.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        write_config(
            &paths,
            &format!("version = 1\n{}", pair_toml("dev", &local, "dev:")),
        );
        let _round = try_lock_pair(&paths, "dev").unwrap().unwrap();

        let result = run_preview(&paths, &rclone, "dev", ReviewKind::Resync).await;

        assert_eq!(result, Err(PreviewFailure::Busy));
    }

    fn args_logged(root: &Path) -> Vec<String> {
        std::fs::read_to_string(root.join("args.txt"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[tokio::test]
    async fn removing_an_account_deletes_the_remote_and_names_the_syncs_that_were_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let local = dir.path().join("l");
        write_config(
            &paths,
            &format!(
                "version = 1\n{}{}{}",
                pair_toml("a", &local.join("a"), "gdrive:a"),
                pair_toml("b", &local.join("b"), "work:b"),
                pair_toml("c", &local.join("c"), "gdrive:"),
            ),
        );
        let rclone = fake_rclone(dir.path(), "exit 0");

        let ids = remove_account(&paths, &rclone, "gdrive", ACCOUNT_LOCK_TIMEOUT)
            .await
            .unwrap();

        assert_eq!(ids, ["a", "c"]);
        assert_eq!(args_logged(dir.path()), ["config delete gdrive"]);
    }

    #[tokio::test]
    async fn a_failed_removal_names_nothing_and_reports_rclone() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        write_config(&paths, "version = 1\n");
        let rclone = fake_rclone(dir.path(), "echo nope >&2; exit 1");

        let error = remove_account(&paths, &rclone, "gdrive", ACCOUNT_LOCK_TIMEOUT)
            .await
            .unwrap_err();

        assert_eq!(error, "rclone config delete failed: nope");
    }

    #[tokio::test]
    async fn removing_an_account_forgets_what_its_syncs_applied_and_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let local = dir.path().join("l");
        write_config(
            &paths,
            &format!(
                "version = 1\n{}{}",
                pair_toml("a", &local.join("a"), "gdrive:a"),
                pair_toml("b", &local.join("b"), "work:b"),
            ),
        );
        for id in ["a", "b"] {
            State {
                applied_filters_hash: Some("applied".into()),
                ..State::default()
            }
            .save(&paths, id)
            .await
            .unwrap();
            confirm_review(
                &paths,
                id,
                ReviewKind::Resync,
                SyncMode::Bisync,
                "hash".into(),
            )
            .await
            .unwrap();
        }
        let rclone = fake_rclone(dir.path(), "exit 0");

        remove_account(&paths, &rclone, "gdrive", ACCOUNT_LOCK_TIMEOUT)
            .await
            .unwrap();

        let removed = State::load(&paths, "a").await.unwrap();
        assert_eq!(removed.applied_filters_hash, None);
        assert_eq!(ConfirmMarker::load(&paths, "a").await.unwrap(), None);
        let other = State::load(&paths, "b").await.unwrap();
        assert_eq!(other.applied_filters_hash.as_deref(), Some("applied"));
        assert!(ConfirmMarker::load(&paths, "b").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn removing_an_account_with_a_running_sync_leaves_the_remote_and_the_states() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let local = dir.path().join("l");
        write_config(
            &paths,
            &format!(
                "version = 1\n{}",
                pair_toml("a", &local.join("a"), "gdrive:a")
            ),
        );
        State {
            applied_filters_hash: Some("applied".into()),
            ..State::default()
        }
        .save(&paths, "a")
        .await
        .unwrap();
        let _round = try_lock_pair(&paths, "a").unwrap().unwrap();
        let rclone = fake_rclone(dir.path(), "exit 0");

        let error = remove_account(&paths, &rclone, "gdrive", Duration::from_millis(150))
            .await
            .unwrap_err();

        assert_eq!(
            error,
            "A sync on this account is running. Try again when it finishes."
        );
        assert!(args_logged(dir.path()).is_empty());
        let state = State::load(&paths, "a").await.unwrap();
        assert_eq!(state.applied_filters_hash.as_deref(), Some("applied"));
    }

    #[tokio::test]
    async fn renaming_signs_in_with_config_create_under_the_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(dir.path(), "exit 0");
        let (_keep, cancel) = oneshot::channel();
        let mode = AccountMode::Rename {
            from: "gdrive".into(),
        };

        let done = connect(&rclone, &request(mode, "personal"), cancel)
            .await
            .unwrap();

        assert!(done);
        assert_eq!(
            args_logged(dir.path()),
            [
                "config create personal drive client_id=id.apps client_secret=s3cret scope=drive config_is_local=true"
            ]
        );
    }

    #[tokio::test]
    async fn a_check_that_works_is_signed_in_and_asks_about_the_remote() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(dir.path(), "exit 0");

        let status = check_account(&rclone, true, "gdrive").await;

        assert_eq!(status, AccountStatus::SignedIn);
        assert_eq!(args_logged(dir.path()), ["about gdrive: --json"]);
    }

    #[tokio::test]
    async fn a_lost_sign_in_is_signed_out_whether_or_not_the_computer_is_online() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(dir.path(), "echo 'oauth2: cannot fetch token' >&2; exit 1");

        for online in [true, false] {
            let status = check_account(&rclone, online, "gdrive").await;
            assert_eq!(status, AccountStatus::SignedOut, "online={online}");
        }
    }

    #[tokio::test]
    async fn another_failure_is_offline_without_a_network_and_a_problem_with_one() {
        let dir = tempfile::tempdir().unwrap();
        let detail = "Error 403: Google Drive API has not been used in project 1";
        let rclone = fake_rclone(dir.path(), &format!("echo '{detail}' >&2; exit 1"));

        let offline = check_account(&rclone, false, "gdrive").await;
        let online = check_account(&rclone, true, "gdrive").await;

        assert_eq!(offline, AccountStatus::Offline);
        assert_eq!(online, AccountStatus::Problem(detail.to_owned()));
    }

    #[tokio::test]
    async fn a_remote_type_without_about_counts_as_signed_in() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(
            dir.path(),
            "echo \"NOTICE: Failed to about: http://x/ doesn't support about\" >&2; exit 1",
        );

        let status = check_account(&rclone, true, "web").await;

        assert_eq!(status, AccountStatus::SignedIn);
    }

    #[tokio::test]
    async fn renaming_an_account_moves_its_syncs_holds_them_for_review_and_drops_their_markers() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let local = dir.path().join("l");
        write_config(
            &paths,
            &format!(
                "version = 1\n{}{}{}",
                pair_toml("a", &local.join("a"), "gdrive:a"),
                pair_toml("b", &local.join("b"), "work:b"),
                pair_toml("c", &local.join("c"), "gdrive:"),
            ),
        );
        for (id, kind) in [
            ("a", GateKind::AuthError),
            ("b", GateKind::AuthError),
            ("c", GateKind::NeedsReviewFirstRun),
        ] {
            State {
                gate: Some(Gate {
                    kind,
                    since: jiff::Timestamp::UNIX_EPOCH,
                    detail: String::new(),
                    fault: None,
                    auto_checked: None,
                }),
                notified: vec![
                    GateKind::NeedsReviewFirstRun.notified_key(),
                    "failures".into(),
                ],
                ..State::default()
            }
            .save(&paths, id)
            .await
            .unwrap();
            confirm_review(
                &paths,
                id,
                ReviewKind::Resync,
                SyncMode::Bisync,
                "hash".into(),
            )
            .await
            .unwrap();
        }

        let moved = rename_account(&paths, "gdrive", "personal", ACCOUNT_LOCK_TIMEOUT)
            .await
            .unwrap();

        assert_eq!(moved, ["a", "c"]);
        let config = Config::load(&paths).await.unwrap();
        let remotes: Vec<_> = config.pairs().iter().map(|p| p.remote.as_str()).collect();
        assert_eq!(remotes, ["personal:a", "work:b", "personal:"]);
        for id in ["a", "c"] {
            let state = State::load(&paths, id).await.unwrap();
            let gate = state.gate.unwrap();
            assert_eq!(gate.kind, GateKind::NeedsReviewFirstRun, "{id}");
            assert_eq!(gate.detail, "Account renamed to personal.");
            assert_eq!(state.notified, ["failures"], "{id}");
            assert_eq!(ConfirmMarker::load(&paths, id).await.unwrap(), None, "{id}");
        }
        let untouched = State::load(&paths, "b").await.unwrap();
        assert_eq!(untouched.gate.map(|g| g.kind), Some(GateKind::AuthError));
        assert!(ConfirmMarker::load(&paths, "b").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn renaming_an_account_with_a_running_sync_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let local = dir.path().join("l");
        let config = format!(
            "version = 1\n{}{}",
            pair_toml("a", &local.join("a"), "gdrive:a"),
            pair_toml("b", &local.join("b"), "gdrive:b"),
        );
        write_config(&paths, &config);
        State::default().save(&paths, "a").await.unwrap();
        let _round = try_lock_pair(&paths, "b").unwrap().unwrap();

        let error = rename_account(&paths, "gdrive", "personal", Duration::from_millis(150))
            .await
            .unwrap_err();

        assert_eq!(
            error,
            "A sync on this account is running. Try again when it finishes."
        );
        assert_eq!(Config::load(&paths).await.unwrap().to_toml(), config);
        assert_eq!(State::load(&paths, "a").await.unwrap().gate, None);
    }

    const REFUSED: &str = "Error 403: Google Drive API has not been used in project 1";

    /// A Drive account whose `about` runs `about`.
    fn drive_account(root: &Path, about: &str) -> Rclone {
        fake_rclone(
            root,
            &format!("case \"$1 $2\" in\n  \"about gdrive:\") {about} ;;\nesac"),
        )
    }

    async fn sign_in_once(rclone: &Rclone, mode: AccountMode) -> Result<bool, String> {
        let (_keep, cancel) = oneshot::channel();
        sign_in(rclone, &request(mode, "gdrive"), cancel).await
    }

    fn update() -> AccountMode {
        AccountMode::Update {
            name: "gdrive".into(),
        }
    }

    const SIGN_IN: &str = "config update gdrive client_id=id.apps client_secret=s3cret";

    #[tokio::test]
    async fn credentials_google_drive_refuses_are_reported_and_kept_without_a_second_update() {
        for mode in [
            update(),
            AccountMode::ChangeClientId {
                name: "gdrive".into(),
            },
        ] {
            let dir = tempfile::tempdir().unwrap();
            let rclone = drive_account(dir.path(), &format!("echo '{REFUSED}' >&2; exit 1"));

            let error = sign_in_once(&rclone, mode).await.unwrap_err();

            assert_eq!(
                error,
                format!("Signed in, but Google Drive refused the account: {REFUSED}")
            );
            assert_eq!(
                args_logged(dir.path()),
                [SIGN_IN, "about gdrive: --json"],
                "only the sign-in writes credentials"
            );
        }
    }

    #[tokio::test]
    async fn an_account_that_checks_out_after_the_sign_in_is_connected() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = drive_account(dir.path(), "exit 0");

        let done = sign_in_once(&rclone, update()).await.unwrap();

        assert!(done);
        assert_eq!(args_logged(dir.path()), [SIGN_IN, "about gdrive: --json"]);
    }

    #[tokio::test]
    async fn a_lost_sign_in_after_the_sign_in_is_not_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = drive_account(
            dir.path(),
            "echo \"couldn't fetch token: invalid_client\" >&2; exit 1",
        );

        let done = sign_in_once(&rclone, update()).await.unwrap();

        assert!(done);
        assert_eq!(args_logged(dir.path()).len(), 2);
    }

    #[tokio::test]
    async fn a_failed_sign_in_never_runs_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(dir.path(), "echo denied >&2; exit 1");

        let error = sign_in_once(&rclone, update()).await.unwrap_err();

        assert_eq!(error, "denied");
        assert_eq!(args_logged(dir.path()), [SIGN_IN]);
    }

    #[tokio::test]
    async fn adding_an_account_is_not_checked_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(dir.path(), "exit 0");
        let (_keep, cancel) = oneshot::channel();

        let done = sign_in(&rclone, &request(AccountMode::Add, "trabalho"), cancel)
            .await
            .unwrap();

        assert!(done);
        assert_eq!(
            args_logged(dir.path()),
            [
                "listremotes",
                "config create trabalho drive client_id=id.apps client_secret=s3cret scope=drive config_is_local=true",
            ]
        );
    }

    /// A rclone that lists `gdrive` and, once `config create` ran,
    /// `trabalho`; `create` runs `on_create` after leaving the mark.
    fn creating_rclone(root: &Path, on_create: &str) -> Rclone {
        let mark = root.join("created");
        fake_rclone(
            root,
            &format!(
                "case \"$1\" in\n  listremotes) if [ -f '{mark}' ]; then printf 'gdrive:\\ntrabalho:\\n'; else printf 'gdrive:\\n'; fi ;;\n  config) case \"$2\" in\n    create) touch '{mark}'; {on_create} ;;\n    delete) rm -f '{mark}' ;;\n  esac ;;\nesac",
                mark = mark.display()
            ),
        )
    }

    #[tokio::test]
    async fn a_failed_add_removes_the_remote_it_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = creating_rclone(dir.path(), "echo boom >&2; exit 1");
        let (_keep, cancel) = oneshot::channel();

        let error = sign_in(&rclone, &request(AccountMode::Add, "trabalho"), cancel)
            .await
            .unwrap_err();

        assert_eq!(error, "boom");
        let logged = args_logged(dir.path());
        assert_eq!(logged.first().map(String::as_str), Some("listremotes"));
        assert_eq!(
            logged.last().map(String::as_str),
            Some("config delete trabalho")
        );
        assert!(!dir.path().join("created").exists());
    }

    #[tokio::test]
    async fn a_failed_rename_removes_the_new_remote_and_keeps_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = creating_rclone(dir.path(), "echo boom >&2; exit 1");
        let (_keep, cancel) = oneshot::channel();
        let mode = AccountMode::Rename {
            from: "gdrive".into(),
        };

        let error = sign_in(&rclone, &request(mode, "trabalho"), cancel)
            .await
            .unwrap_err();

        assert_eq!(error, "boom");
        let logged = args_logged(dir.path());
        assert!(logged.contains(&"config delete trabalho".to_owned()));
        assert!(!logged.contains(&"config delete gdrive".to_owned()));
    }

    #[tokio::test]
    async fn a_cancelled_add_removes_the_remote_it_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = creating_rclone(dir.path(), "exec sleep 30");
        let (cancel_tx, cancel) = oneshot::channel();
        let request = request(AccountMode::Add, "trabalho");
        let job = tokio::spawn(async move { sign_in(&rclone, &request, cancel).await });
        while !dir.path().join("created").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        cancel_tx.send(()).unwrap();
        let done = job.await.unwrap().unwrap();

        assert!(!done);
        assert_eq!(
            args_logged(dir.path()).last().map(String::as_str),
            Some("config delete trabalho")
        );
    }

    #[tokio::test]
    async fn a_failed_add_never_deletes_a_remote_that_existed_before() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(
            dir.path(),
            "case \"$1\" in listremotes) printf 'gdrive:\\ntrabalho:\\n' ;; config) echo boom >&2; exit 1 ;; esac",
        );
        let (_keep, cancel) = oneshot::channel();

        let error = sign_in(&rclone, &request(AccountMode::Add, "trabalho"), cancel)
            .await
            .unwrap_err();

        assert_eq!(error, "boom");
        assert!(
            args_logged(dir.path())
                .iter()
                .all(|line| !line.starts_with("config delete"))
        );
    }
}
