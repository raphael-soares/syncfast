//! The thread that does all the I/O of the GUI: it polls the state of the
//! syncs, runs `apply`, starts rounds and writes the window's own state. The
//! UI talks to it with [`Command`] and hears back with [`Event`].

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fastframe_shell::Waker;
use syncfast_core::bisync::ReviewKind;
use syncfast_core::config::Config;
use syncfast_core::conflicts::{Conflict, Resolution};
use syncfast_core::filters::ExclusionSummary;
use syncfast_core::flatpak;
use syncfast_core::network::{Network as _, NetworkManager};
use syncfast_core::paths::Paths;
use syncfast_core::preview::Preview;
use syncfast_core::rclone::Rclone;
use syncfast_core::state::is_running;
use syncfast_core::units::{ApplyEnv, SystemdMode, UserSystemctl, apply};
use syncfast_core::watch::{Trigger, trigger_for};
use tokio::sync::{
    Notify, mpsc::UnboundedReceiver, mpsc::UnboundedSender, mpsc::unbounded_channel, oneshot,
};
use tokio::task::{AbortHandle, JoinSet};

use crate::gui_state::{self, GuiState};
use crate::instance::Inbox;
use crate::notices::{Notices, notice_for};
use crate::pair_tools;
use crate::sheet_io::{self, PreviewFailure};
use crate::snapshot::{self, Snapshot};
use crate::theme::{ThemeChoice, Themes};
use crate::ui::account_sheet::{AccountMode, ConnectRequest, Secret};
use crate::ui::add_sync::{LocalFacts, NewSync};
use crate::ui::{AccountStatus, RemoteRow};
use syncfast_core::runs::{self, RunEntry};

const POLL_ACTIVE: Duration = Duration::from_secs(1);
const POLL_VISIBLE: Duration = Duration::from_secs(2);
const POLL_HIDDEN: Duration = Duration::from_secs(10);
/// Time a started round needs to take its lock before the next poll.
const ROUND_START_WAIT: Duration = Duration::from_millis(600);
const CLI_NAME: &str = "syncfast";
const CLI_VAR: &str = "SYNCFAST_CLI";
const WORKER_STOPPED: &str = "syncfast's background worker stopped. Restart syncfast.";
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// Opens folders and links. In the Flatpak sandbox the runtime's `xdg-open`
/// goes through the `OpenURI` portal, which the toolkit's own opener cannot.
const OPENER: &str = "xdg-open";

/// What the UI asks of the backend.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// The window appeared or went away: poll faster or slower.
    WindowVisible(bool),
    SyncNow(String),
    /// A round for every enabled sync.
    SyncAll,
    /// Pause (`false`) or resume (`true`) every sync, then `apply`.
    SetAllEnabled(bool),
    /// Show the notification the `run` command asked for.
    Notify(String),
    SaveGuiState(GuiState),
    /// Read the theme files again, after `reload-themes`.
    ReloadThemes,
    /// Look for `.conflict<N>` files in the folder of a sync.
    ScanConflicts(String),
    /// Settle one conflict, look again and start a round.
    ResolveConflict {
        id: String,
        path: PathBuf,
        resolution: Resolution,
    },
    /// List what the rules would leave out of `local`.
    TestFilters {
        id: String,
        local: PathBuf,
        presets: Vec<String>,
        rules: Vec<String>,
    },
    /// Save the filters of a sync, `apply` and review the new filters.
    SaveFilters {
        id: String,
        presets: Vec<String>,
        rules: Vec<String>,
    },
    /// Take a sync out of the config and `apply`, deleting its saved
    /// versions when asked.
    RemoveSync {
        id: String,
        delete_versions: bool,
    },
    Shutdown,
    /// Read the rounds of a sync from its logs, for Activity.
    LoadRuns(String),
    /// Pause (`false`) or resume (`true`) one sync, then `apply`.
    SetEnabled(String, bool),
    OpenLocalFolder(PathBuf),
    /// The folder that keeps the saved versions of a sync.
    OpenVersionsFolder(String),
    /// A web link, handed to the desktop's opener.
    OpenUrl(String),
    /// Read the rclone remotes again.
    LoadRemotes,
    /// The folders inside a remote path (`gdrive:Notes`).
    ListDirs(String),
    /// `rclone mkdir` of a remote path.
    MakeRemoteDir(String),
    /// What a path typed in the Add Sync sheet names on this computer.
    CheckLocal(String),
    CreateLocalDir(String),
    /// Open the portal's folder dialog.
    PickFolder,
    /// Write a new sync into the config and `apply` it.
    CreateSync(NewSync),
    /// Dry run for the Review sheet. A new one replaces a running one.
    Preview {
        pair_id: String,
        kind: ReviewKind,
    },
    CancelPreview,
    /// The user confirmed the review: leave the marker and start a round.
    ConfirmReview {
        pair_id: String,
        kind: ReviewKind,
        filters_hash: String,
    },
    /// The saved client ID and secret of a Drive remote.
    LoadCredentials(String),
    ConnectAccount(ConnectRequest),
    /// Kill the sign-in in progress.
    CancelConnect,
    /// Check the sign-in of every account, one `AccountStatus` event each.
    CheckAccounts,
    /// Take an account out of rclone's config. Its syncs stay in the config
    /// and stop with the account missing.
    RemoveAccount(String),
}

/// What the backend tells the UI.
#[derive(Debug, Clone)]
pub enum Event {
    Snapshot(Snapshot),
    /// The user's palette files, read in the backend.
    Themes(Themes),
    /// The theme `gui.json` names now: `syncfast theme` changed it from
    /// outside the window.
    Theme(ThemeChoice),
    SystemDark(bool),
    /// An action did not work; the text is shown near the sync list.
    Failed(String),
    /// The rounds of a sync, newest first.
    Runs {
        pair: String,
        runs: Arc<[RunEntry]>,
    },
    Remotes(Vec<RemoteRow>),
    Dirs {
        path: String,
        result: Result<Vec<String>, String>,
    },
    RemoteDirMade {
        path: String,
        result: Result<(), String>,
    },
    LocalChecked(LocalFacts),
    LocalCreated(Result<(), String>),
    /// `None`: the dialog was dismissed.
    FolderPicked(Option<PathBuf>),
    /// The id of the sync that was created, or why it was not.
    SyncCreated(Result<String, String>),
    Preview {
        pair_id: String,
        result: Result<Preview, PreviewFailure>,
    },
    Credentials {
        name: String,
        client_id: String,
        client_secret: Secret,
    },
    /// The sign-in ended: the account's name, or rclone's last error line.
    /// A cancelled sign-in sends nothing.
    Connected(Result<String, String>),
    /// What the check of an account found so far.
    AccountStatus {
        name: String,
        status: AccountStatus,
    },
    /// The conflicts found in the folder of a sync.
    Conflicts {
        id: String,
        found: Vec<Conflict>,
    },
    /// The outcome of `Test Filters` for exactly these rules.
    FilterTest {
        id: String,
        presets: Vec<String>,
        rules: Vec<String>,
        outcome: Result<ExclusionSummary, String>,
    },
    /// The filters of a sync were saved and applied.
    FiltersSaved(String),
}

#[derive(Clone)]
struct Emitter {
    events: mpsc::Sender<Event>,
    waker: Waker,
}

impl Emitter {
    fn send(&self, event: Event) {
        if self.events.send(event).is_ok() {
            self.waker.wake();
        }
    }
}

#[derive(Debug)]
pub struct Backend {
    commands: UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// A command found the thread gone; [`Backend::events`] reports it once.
    stopped: AtomicBool,
}

impl Backend {
    /// Starts the thread, which applies the config to systemd right away.
    ///
    /// # Errors
    /// The thread cannot be created.
    pub fn spawn(paths: Paths, waker: Waker, inbox: Inbox) -> std::io::Result<Self> {
        let (commands, receiver) = unbounded_channel();
        let (events_tx, events) = mpsc::channel();
        let emitter = Emitter {
            events: events_tx,
            waker: waker.clone(),
        };
        let notices = Notices::new(inbox, waker);
        let thread = std::thread::Builder::new()
            .name("syncfast-backend".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        log::error!("cannot start the backend runtime: {err}");
                        return;
                    }
                };
                runtime.block_on(run(Arc::new(paths), emitter, notices, receiver));
                runtime.shutdown_timeout(SHUTDOWN_GRACE);
            })?;
        Ok(Self {
            commands,
            events,
            thread: Some(thread),
            stopped: AtomicBool::new(false),
        })
    }

    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            log::warn!("the backend is not running");
            self.stopped.store(true, Ordering::Relaxed);
        }
    }

    /// What arrived since the last call, oldest first. A command sent to a
    /// thread that is gone ends the list with [`Event::Failed`].
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        let mut events: Vec<Event> = self.events.try_iter().collect();
        if self.stopped.swap(false, Ordering::Relaxed) {
            events.push(Event::Failed(WORKER_STOPPED.to_owned()));
        }
        events
    }

    /// Finishes what was already sent (a last save of the window's state
    /// among it) and stops the thread.
    pub fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("the backend thread panicked");
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// How long to wait before reading the state again: quickly while the
/// window shows a round, calmly while it only shows the list, rarely while
/// it is hidden.
#[must_use]
pub fn poll_interval(window_visible: bool, round_running: bool) -> Duration {
    match (window_visible, round_running) {
        (true, true) => POLL_ACTIVE,
        (true, false) => POLL_VISIBLE,
        (false, _) => POLL_HIDDEN,
    }
}

/// The `syncfast` command the units and dev-mode rounds call: `$SYNCFAST_CLI`
/// when set, else the one installed next to this executable.
fn cli_binary(var: Option<OsString>, exe: &Path) -> Result<PathBuf, String> {
    if let Some(path) = var {
        return Ok(PathBuf::from(path));
    }
    let sibling = exe.with_file_name(CLI_NAME);
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(format!(
            "cannot find the {CLI_NAME} command next to {}",
            exe.display()
        ))
    }
}

fn current_cli() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|err| format!("cannot locate syncfast-gui: {err}"))?;
    cli_binary(std::env::var_os(CLI_VAR), &exe)
}

struct Context {
    paths: Arc<Paths>,
    emitter: Emitter,
    refresh: Arc<Notify>,
    tasks: Mutex<Tasks>,
}

/// The two jobs the user can cancel.
#[derive(Default)]
struct Tasks {
    preview: Option<AbortHandle>,
    connect: Option<oneshot::Sender<()>>,
}

impl Context {
    fn tasks(&self) -> MutexGuard<'_, Tasks> {
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn run(
    paths: Arc<Paths>,
    emitter: Emitter,
    mut notices: Notices,
    mut commands: UnboundedReceiver<Command>,
) {
    let context = Arc::new(Context {
        paths,
        emitter,
        refresh: Arc::new(Notify::new()),
        tasks: Mutex::new(Tasks::default()),
    });
    follow_appearance(&context);
    spawn_apply(&context);
    spawn_load_remotes(&context);
    let mut visible = true;
    let mut last: Option<Snapshot> = None;
    loop {
        let snapshot = snapshot::load(&context.paths).await;
        let running = snapshot.any_running();
        if last.as_ref() != Some(&snapshot) {
            context.emitter.send(Event::Snapshot(snapshot.clone()));
            last = Some(snapshot);
        }
        let wait = poll_interval(visible, running);
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    Command::Shutdown => break,
                    Command::WindowVisible(now) => visible = now,
                    Command::Notify(id) => {
                        if let Some(notice) = notice_for(&context.paths, &id).await {
                            notices.show(&notice);
                        }
                    }
                    Command::SaveGuiState(state) => save_gui_state(&context, state).await,
                    Command::ReloadThemes => spawn_reload_themes(&context),
                    Command::SyncNow(id) => spawn_sync(&context, vec![id]),
                    Command::SyncAll => spawn_sync_all(&context),
                    Command::SetAllEnabled(enabled) => spawn_set_all_enabled(&context, enabled),
                    Command::LoadRuns(id) => spawn_load_runs(&context, id),
                    Command::SetEnabled(id, enabled) => spawn_set_enabled(&context, id, enabled),
                    Command::OpenLocalFolder(path) => {
                        spawn_open_folder(&context, path, "The folder of this sync is missing.");
                    }
                    Command::OpenVersionsFolder(id) => {
                        let path = context.paths.versions_dir(&id);
                        spawn_open_folder(&context, path, "No versions are saved yet.");
                    }
                    Command::OpenUrl(url) => spawn_open_url(&context, url),
                    Command::LoadRemotes => spawn_load_remotes(&context),
                    Command::ListDirs(path) => spawn_list_dirs(&context, path),
                    Command::MakeRemoteDir(path) => spawn_make_remote_dir(&context, path),
                    Command::CheckLocal(typed) => spawn_check_local(&context, typed),
                    Command::CreateLocalDir(typed) => spawn_create_local(&context, typed),
                    Command::PickFolder => spawn_pick_folder(&context),
                    Command::CreateSync(new) => spawn_create_sync(&context, new),
                    Command::Preview { pair_id, kind } => spawn_preview(&context, pair_id, kind),
                    Command::CancelPreview => abort_preview(&context),
                    Command::ConfirmReview { pair_id, kind, filters_hash } => {
                        spawn_confirm_review(&context, pair_id, kind, filters_hash);
                    }
                    Command::LoadCredentials(name) => spawn_load_credentials(&context, name),
                    Command::ConnectAccount(request) => spawn_connect(&context, request),
                    Command::CheckAccounts => spawn_check_accounts(&context),
                    Command::RemoveAccount(name) => spawn_remove_account(&context, name),
                    Command::CancelConnect => cancel_connect(&context),
                    Command::ScanConflicts(id) => spawn_scan_conflicts(&context, id),
                    Command::ResolveConflict {
                        id,
                        path,
                        resolution,
                    } => spawn_resolve_conflict(&context, id, path, resolution),
                    Command::TestFilters {
                        id,
                        local,
                        presets,
                        rules,
                    } => spawn_test_filters(&context, id, local, presets, rules),
                    Command::SaveFilters { id, presets, rules } => {
                        spawn_save_filters(&context, id, presets, rules);
                    }
                    Command::RemoveSync {
                        id,
                        delete_versions,
                    } => spawn_remove_sync(&context, id, delete_versions),
                }
            }
            () = context.refresh.notified() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}

/// Spawns a command handler and, when it panics, tells the user instead of
/// leaving the window waiting for an event that will never come. Aborting
/// the returned handle aborts the handler too.
fn guarded(
    emitter: Emitter,
    what: &'static str,
    task: impl Future<Output = ()> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut set = JoinSet::new();
        set.spawn(task);
        if let Some(Err(err)) = set.join_next().await
            && err.is_panic()
        {
            log::error!("{what} panicked: {err}");
            emitter.send(Event::Failed(format!(
                "Something went wrong in syncfast: {what}. Try again."
            )));
        }
    })
}

fn follow_appearance(context: &Arc<Context>) {
    let emitter = context.emitter.clone();
    tokio::spawn(async move {
        let publish = |dark| emitter.send(Event::SystemDark(dark));
        if let Err(err) = crate::appearance::follow(publish).await {
            log::debug!("the desktop's colour scheme is unavailable: {err}");
        }
    });
}

fn spawn_reload_themes(context: &Arc<Context>) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "reloading the themes",
        async move {
            let dir = themes_dir(&context.paths);
            match tokio::task::spawn_blocking(move || Themes::load(&dir)).await {
                Ok(themes) => context.emitter.send(Event::Themes(themes)),
                Err(err) => log::warn!("cannot read the theme files: {err}"),
            }
            let file = gui_state::file(&context.paths);
            match tokio::task::spawn_blocking(move || GuiState::load(&file).theme).await {
                Ok(choice) => context.emitter.send(Event::Theme(choice)),
                Err(err) => log::warn!("cannot read the chosen theme: {err}"),
            }
        },
    );
}

/// Where the user's palette files live.
#[must_use]
pub fn themes_dir(paths: &Paths) -> PathBuf {
    paths.config_dir().join("themes")
}

async fn save_gui_state(context: &Context, state: GuiState) {
    let path = gui_state::file(&context.paths);
    let json = match state.to_json() {
        Ok(json) => json,
        Err(err) => {
            log::warn!("cannot encode the window state: {err}");
            return;
        }
    };
    let written = tokio::task::spawn_blocking(move || fastframe_file::write(&path, json)).await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(err)) => log::warn!("cannot save the window state: {err}"),
        Err(err) => log::warn!("saving the window state failed: {err}"),
    }
}

fn fail(context: &Context, text: String) {
    log::warn!("{text}");
    context.emitter.send(Event::Failed(text));
}

fn spawn_apply(context: &Arc<Context>) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "applying the config", async move {
        if let Err(text) = apply_config(&context.paths).await {
            fail(&context, text);
        }
        context.refresh.notify_one();
    });
}

/// `syncfast apply`, as a function of the core. Invalid syncs are not a
/// failure here: their own status already says so.
async fn apply_config(paths: &Paths) -> Result<(), String> {
    apply_deleting_versions(paths, &[]).await
}

/// [`apply_config`], also deleting the saved versions of the removed syncs
/// in `delete_versions`.
async fn apply_deleting_versions(paths: &Paths, delete_versions: &[String]) -> Result<(), String> {
    let rclone = Rclone::from_env().map_err(|err| err.to_string())?;
    let bin = current_cli()?;
    let systemctl = UserSystemctl::detect();
    let flatpak_id = flatpak::app_id();
    let env = ApplyEnv::new(paths, &rclone, &bin, SystemdMode::from_env(), &systemctl)
        .with_flatpak_id(flatpak_id.as_deref());
    let mut report = apply(&env, delete_versions).await;
    report.invalid_pairs.clear();
    if report.is_empty() {
        Ok(())
    } else {
        Err(report.to_string().trim().to_owned())
    }
}

fn spawn_scan_conflicts(context: &Arc<Context>, id: String) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "looking for conflicts",
        async move { scan_conflicts(&context, id).await },
    );
}

async fn scan_conflicts(context: &Context, id: String) {
    match pair_tools::scan_conflicts(&context.paths, &id).await {
        Ok(found) => context.emitter.send(Event::Conflicts { id, found }),
        Err(text) => fail(context, text),
    }
}

fn spawn_resolve_conflict(
    context: &Arc<Context>,
    id: String,
    path: PathBuf,
    resolution: Resolution,
) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "resolving a conflict",
        async move {
            match pair_tools::resolve_conflict(&context.paths, &id, path, resolution).await {
                Ok(()) => {
                    scan_conflicts(&context, id.clone()).await;
                    spawn_sync(&context, vec![id]);
                }
                Err(text) => fail(&context, text),
            }
        },
    );
}

fn spawn_test_filters(
    context: &Arc<Context>,
    id: String,
    local: PathBuf,
    presets: Vec<String>,
    rules: Vec<String>,
) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "testing the filters", async move {
        let outcome = pair_tools::test_filters(&context.paths, &local, &presets, &rules).await;
        context.emitter.send(Event::FilterTest {
            id,
            presets,
            rules,
            outcome,
        });
    });
}

fn spawn_save_filters(
    context: &Arc<Context>,
    id: String,
    presets: Vec<String>,
    rules: Vec<String>,
) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "saving the filters", async move {
        match save_filters(&context.paths, &id, &presets, &rules).await {
            Ok(was_clear) => {
                context.emitter.send(Event::FiltersSaved(id.clone()));
                if was_clear {
                    // A round only checks the new filters and holds the sync for
                    // review, so the status says so right away.
                    spawn_sync(&context, vec![id]);
                }
            }
            Err(text) => fail(&context, text),
        }
        context.refresh.notify_one();
    });
}

async fn save_filters(
    paths: &Paths,
    id: &str,
    presets: &[String],
    rules: &[String],
) -> Result<bool, String> {
    pair_tools::write_filters(paths, id, presets, rules).await?;
    let was_clear = pair_tools::after_filters_saved(paths, id).await?;
    apply_config(paths).await?;
    Ok(was_clear)
}

fn spawn_remove_sync(context: &Arc<Context>, id: String, delete_versions: bool) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "removing the sync", async move {
        if let Err(text) = remove_sync(&context.paths, &id, delete_versions).await {
            fail(&context, text);
        }
        context.refresh.notify_one();
    });
}

async fn remove_sync(paths: &Paths, id: &str, delete_versions: bool) -> Result<(), String> {
    pair_tools::remove_from_config(paths, id).await?;
    let versions = if delete_versions {
        vec![id.to_owned()]
    } else {
        Vec::new()
    };
    apply_deleting_versions(paths, &versions).await
}

fn spawn_sync(context: &Arc<Context>, ids: Vec<String>) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "starting a sync", async move {
        match start_rounds(&ids).await {
            Ok(()) => tokio::time::sleep(ROUND_START_WAIT).await,
            Err(text) => fail(&context, text),
        }
        context.refresh.notify_one();
    });
}

/// Starts a round of each sync; the error joins the reason of every one
/// that could not start.
async fn start_rounds(ids: &[String]) -> Result<(), String> {
    let trigger: Arc<dyn Trigger> = trigger_for(
        SystemdMode::from_env(),
        current_cli()?,
        Arc::new(UserSystemctl::detect()),
    );
    let mut failures = Vec::new();
    for id in ids {
        if let Err(reason) = trigger.start(id).await {
            failures.push(reason);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn spawn_sync_all(context: &Arc<Context>) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "starting every sync", async move {
        let ids = match Config::load(&context.paths).await {
            Ok(config) => config
                .pairs()
                .iter()
                .filter(|pair| pair.enabled)
                .map(|pair| pair.id.clone())
                .collect::<Vec<_>>(),
            Err(err) => {
                fail(&context, err.to_string());
                return;
            }
        };
        spawn_sync(&context, ids);
    });
}

fn spawn_set_all_enabled(context: &Arc<Context>, enabled: bool) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "pausing or resuming the syncs",
        async move {
            if let Err(text) = set_all_enabled(&context.paths, enabled).await {
                fail(&context, text);
            }
            context.refresh.notify_one();
        },
    );
}

async fn set_all_enabled(paths: &Paths, enabled: bool) -> Result<(), String> {
    write_enabled(paths, enabled).await?;
    apply_config(paths).await
}

async fn write_enabled(paths: &Paths, enabled: bool) -> Result<(), String> {
    Config::edit(paths, |config| {
        let ids: Vec<String> = config.pairs().iter().map(|pair| pair.id.clone()).collect();
        for id in &ids {
            config.set_enabled(id, enabled);
        }
    })
    .await
    .map_err(|err| err.to_string())
}

fn spawn_load_runs(context: &Arc<Context>, id: String) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "loading the run history",
        async move {
            let running = is_running(&context.paths, &id);
            let runs = runs::load(&context.paths, &id, running).await;
            context.emitter.send(Event::Runs { pair: id, runs });
        },
    );
}

fn spawn_set_enabled(context: &Arc<Context>, id: String, enabled: bool) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "pausing or resuming the sync",
        async move {
            if let Err(text) = set_enabled(&context.paths, &id, enabled).await {
                fail(&context, text);
            }
            context.refresh.notify_one();
        },
    );
}

async fn set_enabled(paths: &Paths, id: &str, enabled: bool) -> Result<(), String> {
    let found = Config::edit(paths, |config| config.set_enabled(id, enabled))
        .await
        .map_err(|err| err.to_string())?;
    if !found {
        return Err(format!("There is no sync named \"{id}\"."));
    }
    apply_config(paths).await
}

fn spawn_open_folder(context: &Arc<Context>, path: PathBuf, missing: &'static str) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "opening a folder", async move {
        if let Err(text) = open_folder(OPENER.as_ref(), &path, missing).await {
            fail(&context, text);
        }
    });
}

/// Hands a folder to the desktop's opener. A folder that does not exist
/// is reported with `missing` instead of whatever the opener would say.
async fn open_folder(opener: &std::ffi::OsStr, path: &Path, missing: &str) -> Result<(), String> {
    if !tokio::fs::metadata(path)
        .await
        .is_ok_and(|meta| meta.is_dir())
    {
        return Err(missing.to_owned());
    }
    run_opener(opener, path.as_os_str()).await
}

fn spawn_open_url(context: &Arc<Context>, url: String) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "opening a link", async move {
        if let Err(text) = run_opener(OPENER.as_ref(), url.as_ref()).await {
            fail(&context, text);
        }
    });
}

async fn run_opener(opener: &std::ffi::OsStr, target: &std::ffi::OsStr) -> Result<(), String> {
    let status = tokio::process::Command::new(opener)
        .arg(target)
        .status()
        .await
        .map_err(|err| format!("Cannot run {}: {err}", opener.to_string_lossy()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Cannot open {}.", target.to_string_lossy()))
    }
}

fn rclone() -> Result<Rclone, String> {
    Rclone::from_env().map_err(|err| err.to_string())
}

async fn publish_remotes(context: &Context) {
    let loaded = match rclone() {
        Ok(rclone) => sheet_io::load_remotes(&rclone).await,
        Err(text) => Err(text),
    };
    match loaded {
        Ok(remotes) => context.emitter.send(Event::Remotes(remotes)),
        Err(text) => fail(context, text),
    }
}

fn spawn_load_remotes(context: &Arc<Context>) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "listing the accounts",
        async move { publish_remotes(&context).await },
    );
}

fn spawn_list_dirs(context: &Arc<Context>, path: String) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "listing the folders", async move {
        let result = match rclone() {
            Ok(rclone) => sheet_io::list_dirs(&rclone, &path).await,
            Err(text) => Err(text),
        };
        context.emitter.send(Event::Dirs { path, result });
    });
}

fn spawn_make_remote_dir(context: &Arc<Context>, path: String) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "creating the remote folder",
        async move {
            let result = match rclone() {
                Ok(rclone) => sheet_io::make_remote_dir(&rclone, &path).await,
                Err(text) => Err(text),
            };
            context.emitter.send(Event::RemoteDirMade { path, result });
        },
    );
}

fn spawn_check_local(context: &Arc<Context>, typed: String) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "checking the folder", async move {
        let facts = sheet_io::check_local(&context.paths.home, &typed).await;
        context.emitter.send(Event::LocalChecked(facts));
    });
}

fn spawn_create_local(context: &Arc<Context>, typed: String) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "creating the folder", async move {
        let result = sheet_io::create_local(&context.paths.home, &typed).await;
        context.emitter.send(Event::LocalCreated(result));
    });
}

fn spawn_pick_folder(context: &Arc<Context>) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "choosing a folder", async move {
        let picked = rfd::AsyncFileDialog::new()
            .set_title("Choose Folder")
            .pick_folder()
            .await;
        let path = picked.map(|handle| handle.path().to_path_buf());
        context.emitter.send(Event::FolderPicked(path));
    });
}

fn spawn_create_sync(context: &Arc<Context>, new: NewSync) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "creating the sync", async move {
        let id = new.id.clone();
        let created = match rclone() {
            Ok(rclone) => sheet_io::create_sync(&context.paths, &rclone, new).await,
            Err(text) => Err(text),
        };
        let result = match created {
            Ok(()) => {
                if let Err(text) = apply_config(&context.paths).await {
                    fail(&context, text);
                }
                Ok(id)
            }
            Err(text) => Err(text),
        };
        context.emitter.send(Event::SyncCreated(result));
        context.refresh.notify_one();
    });
}

fn abort_preview(context: &Context) {
    if let Some(running) = context.tasks().preview.take() {
        running.abort();
    }
}

fn spawn_preview(context: &Arc<Context>, pair_id: String, kind: ReviewKind) {
    abort_preview(context);
    let task_context = Arc::clone(context);
    let job = guarded(
        context.emitter.clone(),
        "previewing the changes",
        async move {
            let result = match rclone() {
                Ok(rclone) => {
                    sheet_io::run_preview(&task_context.paths, &rclone, &pair_id, kind).await
                }
                Err(text) => Err(PreviewFailure::Failed(text)),
            };
            task_context
                .emitter
                .send(Event::Preview { pair_id, result });
        },
    );
    context.tasks().preview = Some(job.abort_handle());
}

fn spawn_confirm_review(
    context: &Arc<Context>,
    pair_id: String,
    kind: ReviewKind,
    filters_hash: String,
) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "confirming the review",
        async move {
            match sheet_io::confirm_review(&context.paths, &pair_id, kind, filters_hash).await {
                Ok(()) => spawn_sync(&context, vec![pair_id]),
                Err(text) => fail(&context, text),
            }
        },
    );
}

fn spawn_load_credentials(context: &Arc<Context>, name: String) {
    let context = Arc::clone(context);
    guarded(
        context.emitter.clone(),
        "loading the credentials",
        async move {
            let loaded = match rclone() {
                Ok(rclone) => sheet_io::load_credentials(&rclone, &name).await,
                Err(text) => Err(text),
            };
            match loaded {
                Ok((client_id, client_secret)) => context.emitter.send(Event::Credentials {
                    name,
                    client_id,
                    client_secret,
                }),
                Err(text) => fail(&context, text),
            }
        },
    );
}

fn cancel_connect(context: &Context) {
    if let Some(cancel) = context.tasks().connect.take() {
        let _ = cancel.send(());
    }
}

fn spawn_connect(context: &Arc<Context>, request: ConnectRequest) {
    let (cancel, cancelled) = oneshot::channel();
    context.tasks().connect = Some(cancel);
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "signing in", async move {
        let account = request
            .mode
            .remote()
            .map_or_else(|| request.name.clone(), str::to_owned);
        let signed_in = match rclone() {
            Ok(rclone) => sheet_io::sign_in(&rclone, &request, cancelled).await,
            Err(text) => Err(text),
        };
        match (signed_in, &request.mode) {
            (Ok(true), AccountMode::Rename { from }) => {
                renamed(&context, from, &request.name).await;
            }
            (Ok(true), _) => reconnected(&context, account).await,
            (Ok(false), _) => {}
            (Err(text), _) => context.emitter.send(Event::Connected(Err(text))),
        }
    });
}

/// A sign-in worked: the remote list changed, and syncs that waited for
/// access can run.
async fn reconnected(context: &Arc<Context>, account: String) {
    publish_remotes(context).await;
    match sheet_io::clear_auth_gates(&context.paths, &account).await {
        Ok(ids) if ids.is_empty() => {}
        Ok(ids) => spawn_sync(context, ids),
        Err(text) => fail(context, text),
    }
    context.emitter.send(Event::Connected(Ok(account)));
    spawn_check_accounts(context);
}

/// The new account is signed in: the syncs of `from` move to it, held for a
/// review, and `from` goes. A failure before the move leaves both accounts
/// for the user to sort out.
async fn renamed(context: &Arc<Context>, from: &str, to: &str) {
    if let Err(text) = sheet_io::rename_account(&context.paths, from, to).await {
        context.emitter.send(Event::Connected(Err(text)));
        return;
    }
    let removed = match rclone() {
        Ok(rclone) => rclone
            .delete_remote(from)
            .await
            .map_err(|err| err.to_string()),
        Err(text) => Err(text),
    };
    if let Err(text) = removed {
        fail(
            context,
            format!("Renamed, but the old account \"{from}\" could not be removed: {text}"),
        );
    }
    publish_remotes(context).await;
    if let Err(text) = apply_config(&context.paths).await {
        fail(context, text);
    }
    context.emitter.send(Event::Connected(Ok(to.to_owned())));
    spawn_check_accounts(context);
    context.refresh.notify_one();
}

fn spawn_check_accounts(context: &Arc<Context>) {
    let context = Arc::clone(context);
    tokio::spawn(async move {
        let rclone = match rclone() {
            Ok(rclone) => rclone,
            Err(text) => return fail(&context, text),
        };
        let names = match rclone.listremotes().await {
            Ok(names) => names,
            Err(err) => return fail(&context, err.to_string()),
        };
        let online = NetworkManager.is_online().await;
        for name in names {
            context.emitter.send(Event::AccountStatus {
                name: name.clone(),
                status: AccountStatus::Checking,
            });
            let context = Arc::clone(&context);
            let rclone = rclone.clone();
            tokio::spawn(async move {
                let status = sheet_io::check_account(&rclone, online, &name).await;
                context.emitter.send(Event::AccountStatus { name, status });
            });
        }
    });
}

fn spawn_remove_account(context: &Arc<Context>, name: String) {
    let context = Arc::clone(context);
    tokio::spawn(async move {
        let removed = match rclone() {
            Ok(rclone) => sheet_io::remove_account(&context.paths, &rclone, &name).await,
            Err(text) => Err(text),
        };
        match removed {
            Ok(ids) => {
                publish_remotes(&context).await;
                if !ids.is_empty() {
                    spawn_sync(&context, ids);
                }
            }
            Err(text) => fail(&context, text),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emitter() -> (Emitter, mpsc::Receiver<Event>) {
        let (events, received) = mpsc::channel();
        let emitter = Emitter {
            events,
            waker: Waker::default(),
        };
        (emitter, received)
    }

    fn failures(events: impl IntoIterator<Item = Event>) -> Vec<String> {
        events
            .into_iter()
            .filter_map(|event| match event {
                Event::Failed(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_handler_that_panics_tells_the_user_what_to_retry() {
        let (emitter, received) = emitter();

        guarded(emitter, "saving the filters", async {
            panic!("boom");
        })
        .await
        .unwrap();

        assert_eq!(
            failures(received.try_iter()),
            ["Something went wrong in syncfast: saving the filters. Try again."]
        );
    }

    #[tokio::test]
    async fn a_handler_that_finishes_emits_nothing_of_its_own() {
        let (emitter, received) = emitter();

        guarded(emitter, "x", async {}).await.unwrap();

        assert!(received.try_iter().next().is_none());
    }

    #[tokio::test]
    async fn aborting_a_guarded_handler_stops_it_without_an_error() {
        let (emitter, received) = emitter();
        let (finished_tx, mut finished) = oneshot::channel::<()>();
        let job = guarded(emitter, "x", async move {
            let _keep = finished_tx;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;

        job.abort();
        let _ = job.await;

        assert!(matches!(
            finished.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        assert!(received.try_iter().next().is_none());
    }

    #[test]
    fn sending_to_a_stopped_backend_reports_the_worker_stopped_once() {
        let (commands, receiver) = unbounded_channel();
        let (_events_tx, events) = mpsc::channel();
        drop(receiver);
        let backend = Backend {
            commands,
            events,
            thread: None,
            stopped: AtomicBool::new(false),
        };

        backend.send(Command::SyncAll);

        assert_eq!(
            failures(backend.events()),
            ["syncfast's background worker stopped. Restart syncfast."]
        );
        assert!(backend.events().is_empty());
    }

    #[test]
    fn poll_is_fast_with_a_visible_round_calm_when_visible_and_rare_when_hidden() {
        assert_eq!(poll_interval(true, true), Duration::from_secs(1));
        assert_eq!(poll_interval(true, false), Duration::from_secs(2));
        assert_eq!(poll_interval(false, true), Duration::from_secs(10));
        assert_eq!(poll_interval(false, false), Duration::from_secs(10));
    }

    #[test]
    fn cli_variable_wins_over_the_installed_one() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("syncfast-gui");
        std::fs::write(dir.path().join("syncfast"), "").unwrap();

        let cli = cli_binary(Some("/dev/syncfast".into()), &exe).unwrap();

        assert_eq!(cli, PathBuf::from("/dev/syncfast"));
    }

    #[test]
    fn without_the_variable_the_cli_next_to_the_gui_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("syncfast-gui");
        std::fs::write(dir.path().join("syncfast"), "").unwrap();

        let cli = cli_binary(None, &exe).unwrap();

        assert_eq!(cli, dir.path().join("syncfast"));
    }

    #[test]
    fn missing_cli_is_an_error_that_names_where_it_looked() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("syncfast-gui");

        let err = cli_binary(None, &exe).unwrap_err();

        assert!(err.contains("cannot find the syncfast command"), "{err}");
    }

    #[tokio::test]
    async fn pausing_all_syncs_writes_enabled_false_for_each_and_resuming_undoes_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("state"),
            dir.path().join("data"),
            dir.path().join("run"),
        );
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "version = 1\n\n[[pair]]\nid = \"a\"\nname = \"A\"\nlocal = \"/data/a\"\nremote = \"r:a\"\n\n[[pair]]\nid = \"b\"\nname = \"B\"\nlocal = \"/data/b\"\nremote = \"r:b\"\n",
        )
        .unwrap();

        write_enabled(&paths, false).await.unwrap();

        let reloaded = Config::load(&paths).await.unwrap();

        assert!(reloaded.pairs().iter().all(|pair| !pair.enabled));

        write_enabled(&paths, true).await.unwrap();

        let resumed = Config::load(&paths).await.unwrap();
        assert!(resumed.pairs().iter().all(|pair| pair.enabled));
    }

    fn paths_with_pairs(root: &Path) -> Paths {
        let paths = Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        );
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "version = 1\n\n[[pair]]\nid = \"a\"\nname = \"A\"\nlocal = \"/data/a\"\nremote = \"r:a\"\n\n[[pair]]\nid = \"b\"\nname = \"B\"\nlocal = \"/data/b\"\nremote = \"r:b\"\n",
        )
        .unwrap();
        paths
    }

    #[tokio::test]
    async fn setting_enabled_on_an_unknown_sync_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_with_pairs(dir.path());

        let err = set_enabled(&paths, "gone", false).await.unwrap_err();

        assert_eq!(err, "There is no sync named \"gone\".");
    }

    #[tokio::test]
    async fn opening_a_folder_that_does_not_exist_reports_the_given_reason_without_running_the_opener()
     {
        let dir = tempfile::tempdir().unwrap();

        let err = open_folder("false".as_ref(), &dir.path().join("none"), "Nothing here.")
            .await
            .unwrap_err();

        assert_eq!(err, "Nothing here.");
    }

    #[tokio::test]
    async fn opening_a_folder_succeeds_when_the_opener_does_and_fails_when_it_does_not() {
        let dir = tempfile::tempdir().unwrap();

        let opened = open_folder("true".as_ref(), dir.path(), "x").await;
        let refused = open_folder("false".as_ref(), dir.path(), "x").await;

        assert!(opened.is_ok());
        assert_eq!(
            refused.unwrap_err(),
            format!("Cannot open {}.", dir.path().display())
        );
    }

    #[tokio::test]
    async fn opening_a_link_reports_the_target_when_the_opener_refuses() {
        let opened = run_opener("true".as_ref(), "https://example.org".as_ref()).await;
        let refused = run_opener("false".as_ref(), "https://example.org".as_ref()).await;

        assert!(opened.is_ok());
        assert_eq!(
            refused.unwrap_err(),
            "Cannot open https://example.org.".to_owned()
        );
    }
}
