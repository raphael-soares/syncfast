//! The I/O behind the sheets (Review, Add Sync, account): rclone calls,
//! folder checks and the files the confirmation leaves for the next round.
//! The backend runs these and turns the results into events.

use std::path::Path;

use jiff::Zoned;
use syncfast_core::bisync::ReviewKind;
use syncfast_core::config::{Config, Pair, expand_tilde, split_remote};
use syncfast_core::network::NetworkManager;
use syncfast_core::notify::NativeNotifier;
use syncfast_core::paths::Paths;
use syncfast_core::preview::{Preview, PreviewError, preview};
use syncfast_core::rclone::Rclone;
use syncfast_core::run::RunEnv;
use syncfast_core::state::{ConfirmMarker, GateKind, State};
use tokio::sync::oneshot;

use crate::ui::RemoteRow;
use crate::ui::account_sheet::{AccountMode, ConnectRequest, Secret};
use crate::ui::add_sync::{LocalFacts, LocalStatus, NewSync};

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
///
/// # Errors
/// The config cannot be read or written, or rejects the sync.
pub async fn create_sync(paths: &Paths, rclone: &Rclone, new: NewSync) -> Result<(), String> {
    let mut config = Config::load(paths).await.map_err(|e| e.to_string())?;
    let id = new.id.clone();
    config
        .add_pair(Pair {
            id: new.id,
            name: new.name,
            local: new.local,
            remote: new.remote,
            enabled: true,
            presets: new.presets,
            filters: new.filters,
        })
        .map_err(|e| e.to_string())?;
    let accounts = rclone.listremotes().await.map_err(|e| e.to_string())?;
    let validation = config.validate(paths, &accounts);
    if let Some(error) = validation.errors.iter().find(|error| error.pair_id == id) {
        return Err(error.message.clone());
    }
    config.save(paths).await.map_err(|e| e.to_string())
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
    filters_hash: String,
) -> Result<(), String> {
    ConfirmMarker { kind, filters_hash }
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
        AccountMode::Add => rclone.create_drive_remote(&request.name, &request.client_id, secret),
        AccountMode::Update { name } => {
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

        confirm_review(&paths, "notes", ReviewKind::Force, "abc".into())
            .await
            .unwrap();

        let marker = ConfirmMarker::load(&paths, "notes").await.unwrap();
        assert_eq!(
            marker,
            Some(ConfirmMarker {
                kind: ReviewKind::Force,
                filters_hash: "abc".into()
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

        create_sync(&paths, &rclone, new_sync("dev", local.clone(), "dev:"))
            .await
            .unwrap();

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
}
