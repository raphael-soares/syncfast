//! The thread that does all the I/O of the GUI: it polls the state of the
//! syncs, runs `apply`, starts rounds and writes the window's own state. The
//! UI talks to it with [`Command`] and hears back with [`Event`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use fastframe_shell::Waker;
use syncfast_core::config::Config;
use syncfast_core::paths::Paths;
use syncfast_core::rclone::Rclone;
use syncfast_core::state::is_running;
use syncfast_core::units::{ApplyEnv, SystemdMode, UserSystemctl, apply};
use syncfast_core::watch::{Trigger, trigger_for};
use tokio::sync::{
    Notify, mpsc::UnboundedReceiver, mpsc::UnboundedSender, mpsc::unbounded_channel,
};

use crate::gui_state::{self, GuiState};
use crate::instance::Inbox;
use crate::notices::{Notices, notice_for};
use crate::runs::{self, RunEntry};
use crate::snapshot::{self, Snapshot};
use crate::theme::Themes;

const POLL_ACTIVE: Duration = Duration::from_secs(1);
const POLL_VISIBLE: Duration = Duration::from_secs(2);
const POLL_HIDDEN: Duration = Duration::from_secs(10);
/// Time a started round needs to take its lock before the next poll.
const ROUND_START_WAIT: Duration = Duration::from_millis(600);
const CLI_NAME: &str = "syncfast";
const CLI_VAR: &str = "SYNCFAST_CLI";
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const FOLDER_OPENER: &str = "xdg-open";

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
    Shutdown,
    /// Read the rounds of a sync from its logs, for Activity.
    LoadRuns(String),
    /// Pause (`false`) or resume (`true`) one sync, then `apply`.
    SetEnabled(String, bool),
    OpenLocalFolder(PathBuf),
    /// The folder that keeps the saved versions of a sync.
    OpenVersionsFolder(String),
}

/// What the backend tells the UI.
#[derive(Debug, Clone)]
pub enum Event {
    Snapshot(Snapshot),
    /// The user's palette files, read in the backend.
    Themes(Themes),
    SystemDark(bool),
    /// An action did not work; the text is shown near the sync list.
    Failed(String),
    /// The rounds of a sync, newest first.
    Runs {
        pair: String,
        runs: Arc<[RunEntry]>,
    },
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
        })
    }

    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            log::warn!("the backend is not running");
        }
    }

    /// What arrived since the last call, oldest first.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
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
    });
    follow_appearance(&context);
    spawn_apply(&context);
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
                }
            }
            () = context.refresh.notified() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
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
    tokio::spawn(async move {
        let dir = themes_dir(&context.paths);
        match tokio::task::spawn_blocking(move || Themes::load(&dir)).await {
            Ok(themes) => context.emitter.send(Event::Themes(themes)),
            Err(err) => log::warn!("cannot read the theme files: {err}"),
        }
    });
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
    tokio::spawn(async move {
        if let Err(text) = apply_config(&context.paths).await {
            fail(&context, text);
        }
        context.refresh.notify_one();
    });
}

/// `syncfast apply`, as a function of the core. Invalid syncs are not a
/// failure here: their own status already says so.
async fn apply_config(paths: &Paths) -> Result<(), String> {
    let rclone = Rclone::from_env().map_err(|err| err.to_string())?;
    let bin = current_cli()?;
    let systemctl = UserSystemctl;
    let env = ApplyEnv::new(paths, &rclone, &bin, SystemdMode::from_env(), &systemctl);
    let mut report = apply(&env, &[]).await;
    report.invalid_pairs.clear();
    if report.is_empty() {
        Ok(())
    } else {
        Err(report.to_string().trim().to_owned())
    }
}

fn spawn_sync(context: &Arc<Context>, ids: Vec<String>) {
    let context = Arc::clone(context);
    tokio::spawn(async move {
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
        Arc::new(UserSystemctl),
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
    tokio::spawn(async move {
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
    tokio::spawn(async move {
        if let Err(text) = set_all_enabled(&context.paths, enabled).await {
            fail(&context, text);
        }
        context.refresh.notify_one();
    });
}

async fn set_all_enabled(paths: &Paths, enabled: bool) -> Result<(), String> {
    write_enabled(paths, enabled).await?;
    apply_config(paths).await
}

async fn write_enabled(paths: &Paths, enabled: bool) -> Result<(), String> {
    let mut config = Config::load(paths).await.map_err(|err| err.to_string())?;
    let ids: Vec<String> = config.pairs().iter().map(|pair| pair.id.clone()).collect();
    for id in &ids {
        config.set_enabled(id, enabled);
    }
    config.save(paths).await.map_err(|err| err.to_string())
}

fn spawn_load_runs(context: &Arc<Context>, id: String) {
    let context = Arc::clone(context);
    tokio::spawn(async move {
        let running = is_running(&context.paths, &id);
        let runs = runs::load(&context.paths, &id, running).await;
        context.emitter.send(Event::Runs { pair: id, runs });
    });
}

fn spawn_set_enabled(context: &Arc<Context>, id: String, enabled: bool) {
    let context = Arc::clone(context);
    tokio::spawn(async move {
        if let Err(text) = set_enabled(&context.paths, &id, enabled).await {
            fail(&context, text);
        }
        context.refresh.notify_one();
    });
}

async fn set_enabled(paths: &Paths, id: &str, enabled: bool) -> Result<(), String> {
    let mut config = Config::load(paths).await.map_err(|err| err.to_string())?;
    if !config.set_enabled(id, enabled) {
        return Err(format!("There is no sync named \"{id}\"."));
    }
    config.save(paths).await.map_err(|err| err.to_string())?;
    apply_config(paths).await
}

fn spawn_open_folder(context: &Arc<Context>, path: PathBuf, missing: &'static str) {
    let context = Arc::clone(context);
    tokio::spawn(async move {
        if let Err(text) = open_folder(FOLDER_OPENER.as_ref(), &path, missing).await {
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
    let status = tokio::process::Command::new(opener)
        .arg(path)
        .status()
        .await
        .map_err(|err| format!("Cannot run {}: {err}", opener.to_string_lossy()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Cannot open {}.", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
