//! The app that outlives its window: it owns the backend, the tray and the
//! state the screens show, and applies the actions they emit.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fastframe_shell::window::Geometry;
use fastframe_shell::{Closed, Headless, Resident, Waker};
use jiff::Timestamp;
use syncfast_core::paths::Paths;
use syncfast_core::state::LastOutcome;

use crate::backend::{Backend, Command, Event};
use crate::gui_state::{self, GuiState};
use crate::instance::{Inbox, Request};
use crate::runs::RunEntry;
use crate::sheets::{Sheets, Surroundings};
use crate::snapshot::Snapshot;
use crate::theme::{self, Palette, Themes};
use crate::tray::{IconLook, PairView, Summary, SyncTray, TrayView, action as tray_action};
use crate::ui::welcome::{self, WelcomeView};
use crate::ui::{self, Action, DetailTab, PairRow, ViewModel};

/// How long a hidden start waits for a panel to show the tray item before
/// it gives up and opens the window, since nothing else could bring it back.
const TRAY_WAIT: Duration = Duration::from_secs(10);
/// A change to the window's state is saved this long after the last one.
const SAVE_AFTER: Duration = Duration::from_secs(1);
/// The sidebar width is saved only when it changed by more than this.
const SIDEBAR_SLACK: f32 = 1.0;
/// Relative times in the list go stale; repaint at least this often.
const REPAINT_EVERY: Duration = Duration::from_secs(60);
/// A hidden app wakes by itself only for the tray wait; this is the idle
/// pause otherwise (the waker ends it early).
const HEADLESS_PAUSE: Duration = Duration::from_secs(30);

/// What the app means to do when the window closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseIntent {
    /// The user closed the window with its own button.
    Unplanned,
    Hide,
    /// Close only to open a fresh window at once.
    Reopen,
    Quit,
}

type RunsKey = (String, bool, Option<Timestamp>);

#[derive(Debug)]
pub struct App {
    backend: Backend,
    inbox: Inbox,
    gui: GuiState,
    gui_dirty_since: Option<Instant>,
    geometry: Geometry,
    themes: Themes,
    /// The desktop's colour scheme once the portal has answered.
    system_dark: Option<bool>,
    installed: Option<Palette>,
    snapshot: Snapshot,
    error: Option<String>,
    tray: Option<SyncTray>,
    tray_wait: Option<Instant>,
    window_hidden: bool,
    /// What the backend was last told about the window.
    reported_visible: Option<bool>,
    title: Option<String>,
    close: CloseIntent,
    wants_show: bool,
    wayland: bool,
    actions: Vec<Action>,
    tab: DetailTab,
    runs: Option<(String, Arc<[RunEntry]>)>,
    /// The sync, its running state and the end of its latest round, as of the
    /// last history request: the history is read again when any changes (a
    /// round that starts and ends between two polls included), not on every poll.
    runs_key: Option<RunsKey>,
    sheets: Sheets,
}

impl App {
    /// Starts the backend and, when `with_tray` is set, the tray item.
    /// `themes` is what the user's theme folder held at startup; later
    /// changes arrive from the backend.
    ///
    /// # Errors
    /// The backend thread cannot be created.
    pub fn new(
        paths: &Paths,
        waker: &Waker,
        inbox: Inbox,
        gui: GuiState,
        themes: Themes,
        with_tray: bool,
    ) -> std::io::Result<Self> {
        let backend = Backend::spawn(paths.clone(), waker.clone(), inbox.clone())?;
        let mut app = Self {
            backend,
            inbox,
            geometry: gui.window.into(),
            gui,
            gui_dirty_since: None,
            themes,
            system_dark: None,
            installed: None,
            snapshot: Snapshot::default(),
            error: None,
            tray: None,
            tray_wait: None,
            window_hidden: false,
            reported_visible: None,
            title: None,
            close: CloseIntent::Unplanned,
            wants_show: false,
            wayland: std::env::var_os("WAYLAND_DISPLAY").is_some(),
            actions: Vec::new(),
            tab: DetailTab::default(),
            runs: None,
            runs_key: None,
            sheets: Sheets::new(paths.home.clone(), paths.data_dir()),
        };
        if with_tray {
            app.tray = SyncTray::spawn(waker, app.tray_view());
            if app.tray.is_none() {
                log::warn!("no tray item: closing the window will minimize it");
            }
        }
        Ok(app)
    }

    /// What shows where the interface paints nothing: the window colour.
    #[must_use]
    pub fn window_colour(&self) -> egui::Color32 {
        self.palette().window
    }

    /// The window's remembered geometry, for the next window.
    #[must_use]
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn selected_id(&self) -> Option<String> {
        let wanted = self.gui.selected_pair.as_deref();
        let pairs = &self.snapshot.pairs;
        pairs
            .iter()
            .find(|pair| Some(pair.pair.id.as_str()) == wanted)
            .or_else(|| pairs.first())
            .map(|pair| pair.pair.id.clone())
    }

    fn view_model(&self) -> ViewModel {
        ViewModel {
            pairs: self.snapshot.pairs.iter().map(PairRow::from).collect(),
            selected: self.selected_id(),
            sidebar_width: self.gui.sidebar_width,
            error: self.snapshot.config_error.clone().or(self.error.clone()),
            now: Timestamp::now(),
            tab: self.tab,
            runs: self.runs_of_selected(),
            remotes: self.sheets.remotes().to_vec(),
        }
    }

    fn runs_of_selected(&self) -> Option<Arc<[RunEntry]>> {
        let (id, runs) = self.runs.as_ref()?;
        (self.selected_id().as_deref() == Some(id.as_str())).then(|| Arc::clone(runs))
    }

    fn runs_key(&self) -> Option<RunsKey> {
        let id = self.selected_id()?;
        let pair = self.snapshot.pairs.iter().find(|pair| pair.pair.id == id)?;
        let finished = pair.state.last_run.as_ref().map(|last| last.finished);
        Some((id, pair.running, finished))
    }

    /// Asks the backend for the history of the selected sync when Activity
    /// shows it and it changed: another sync, the tab just opened, or a
    /// round started or ended.
    fn load_runs_if_stale(&mut self) {
        if self.tab != DetailTab::Activity {
            self.runs_key = None;
            return;
        }
        let key = self.runs_key();
        if key == self.runs_key {
            return;
        }
        if let Some((id, ..)) = &key {
            self.backend.send(Command::LoadRuns(id.clone()));
        }
        self.runs_key = key;
    }

    fn tray_view(&self) -> TrayView {
        let pairs: Vec<PairView> = self
            .snapshot
            .pairs
            .iter()
            .map(|pair| PairView {
                status: pair.status,
                enabled: pair.pair.enabled,
            })
            .collect();
        let summary = Summary::of(&pairs);
        TrayView {
            look: IconLook::of(summary.worst, self.system_dark),
            summary,
            window_hidden: self.window_hidden,
        }
    }

    fn palette(&self) -> Palette {
        *self
            .themes
            .resolve(&self.gui.theme, self.system_dark.unwrap_or(true))
    }

    /// Records what the next window close means. A quit is never
    /// downgraded to a hide.
    fn intend(&mut self, intent: CloseIntent) {
        if self.close != CloseIntent::Quit {
            self.close = intent;
        }
    }

    fn mark_gui_dirty(&mut self) {
        self.gui_dirty_since.get_or_insert_with(Instant::now);
    }

    fn save_gui(&mut self) {
        if self.gui_dirty_since.take().is_some() {
            self.backend.send(Command::SaveGuiState(self.gui.clone()));
        }
    }

    fn tray_shown(&self) -> bool {
        self.tray.as_ref().is_some_and(SyncTray::is_shown)
    }

    /// Ends a hidden start's wait for a panel: one showing the item keeps
    /// the window hidden, none by the deadline shows the window.
    fn check_tray_wait(&mut self, now: Instant) {
        let Some(deadline) = self.tray_wait else {
            return;
        };
        if self.tray_shown() {
            self.tray_wait = None;
        } else if now >= deadline {
            log::info!("no panel showed the tray item; showing the window");
            self.tray_wait = None;
            self.wants_show = true;
        }
    }

    /// The work every frame does, with or without a window: backend events,
    /// requests from other processes, the tray, saving.
    pub fn background_frame(&mut self, ctx: &egui::Context) {
        self.drain_backend();
        self.drain_inbox();
        self.sync_tray();
        let visible = !self.window_hidden;
        if self.reported_visible != Some(visible) {
            self.reported_visible = Some(visible);
            self.backend.send(Command::WindowVisible(visible));
        }
        if let Some(since) = self.gui_dirty_since {
            if since.elapsed() >= SAVE_AFTER {
                self.save_gui();
            } else {
                ctx.request_repaint_after(SAVE_AFTER);
            }
        }
        self.apply_actions(ctx);
    }

    fn drain_backend(&mut self) {
        for event in self.backend.events() {
            self.handle_event(event);
        }
    }

    /// Takes in what the backend reported.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Snapshot(snapshot) => {
                self.snapshot = snapshot;
                self.finish_welcome();
                self.load_runs_if_stale();
            }
            Event::Runs { pair, runs } => self.runs = Some((pair, runs)),
            Event::Themes(themes) => self.themes = themes,
            Event::SystemDark(dark) => self.system_dark = Some(dark),
            Event::Failed(text) => self.error = Some(text),
            other => {
                for command in self.sheets.handle_event(&other) {
                    self.backend.send(command);
                }
            }
        }
    }

    /// The first round that ended well confirms the first sync: the welcome
    /// screen never comes back.
    fn finish_welcome(&mut self) {
        let confirmed = self
            .snapshot
            .pairs
            .iter()
            .any(|pair| pair.state.last_outcome == Some(LastOutcome::Ok));
        if confirmed && !self.gui.welcome_done {
            self.gui.welcome_done = true;
            self.mark_gui_dirty();
        }
    }

    /// No syncs, none ever confirmed, and no config problem to show instead.
    fn welcome_visible(&self) -> bool {
        !self.gui.welcome_done
            && self.snapshot.pairs.is_empty()
            && self.snapshot.config_error.is_none()
    }

    fn pair_rows(&self) -> Vec<PairRow> {
        self.snapshot.pairs.iter().map(PairRow::from).collect()
    }

    fn drain_inbox(&mut self) {
        for request in self.inbox.drain() {
            match request {
                Request::Show => self.actions.push(Action::ShowWindow),
                Request::ReloadThemes => self.backend.send(Command::ReloadThemes),
                Request::Notify(id) => self.backend.send(Command::Notify(id)),
                Request::Opened(id) => {
                    self.actions.push(Action::ShowWindow);
                    self.actions.push(Action::SelectPair(id.clone()));
                    if self.pair_needs_review(&id) {
                        self.actions.push(Action::OpenReview(id));
                    }
                }
            }
        }
    }

    fn pair_needs_review(&self, id: &str) -> bool {
        self.snapshot
            .pairs
            .iter()
            .find(|pair| pair.pair.id == id)
            .and_then(|pair| pair.state.gate.as_ref())
            .is_some_and(|gate| gate.kind.is_needs_review() || gate.kind.is_force_review())
    }

    fn sync_tray(&mut self) {
        let view = self.tray_view();
        let Some(tray) = &mut self.tray else {
            return;
        };
        tray.sync(&view);
        for event in tray.events() {
            self.actions.extend(tray_action(&event, &view));
        }
    }

    /// Called from `eframe::App::logic`, which runs for every window state.
    pub fn window_logic(&mut self, ctx: &egui::Context) {
        if self.geometry.remember(ctx) {
            self.gui.window = self.geometry.into();
            self.mark_gui_dirty();
        }
        self.handle_close_request(ctx);
        self.background_frame(ctx);
        // After the events are in: a new system scheme or theme file must
        // reach this frame.
        self.sync_theme(ctx);
    }

    /// A window close keeps the app in the tray. Without a tray item to
    /// bring it back, the window minimizes instead.
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        match on_close_requested(self.close, self.tray_shown()) {
            CloseRequest::Proceed => {}
            CloseRequest::Hide => self.intend(CloseIntent::Hide),
            CloseRequest::Minimize => {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
    }

    /// Puts the palette the theme choice resolves to on the window, and
    /// asks for another frame when it changed.
    fn sync_theme(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        if self.installed == Some(palette) {
            return;
        }
        if self.installed.is_none() {
            theme::install(ctx, &palette);
        } else {
            theme::apply(ctx, &palette);
        }
        self.installed = Some(palette);
        ctx.request_repaint();
    }

    /// Draws the window and applies what the user asked for.
    pub fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let vm = self.view_model();
        let ctx = ui.ctx().clone();
        let welcome = self.welcome_visible();
        let title = if welcome {
            welcome::TITLE.to_owned()
        } else {
            vm.title()
        };
        if self.title.as_deref() != Some(title.as_str()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = Some(title);
        }
        let mut actions = Vec::new();
        if !self.sheets.is_open() {
            actions.extend(ui::shortcuts(&ctx, &vm));
        }
        if welcome {
            let view = WelcomeView {
                has_remote: !vm.remotes.is_empty(),
                has_sync: false,
                reviewed: false,
            };
            actions.extend(welcome::show(ui, &view));
        } else {
            actions.extend(ui::root(ui, &vm));
        }
        actions.extend(self.sheets.show(&ctx, &vm.pairs));
        let (commands, wait) = self.sheets.tick(Instant::now());
        for command in commands {
            self.backend.send(command);
        }
        if let Some(wait) = wait {
            ctx.request_repaint_after(wait);
        }
        self.actions.extend(actions);
        self.apply_actions(&ctx);
        ctx.request_repaint_after(REPAINT_EVERY);
    }

    /// A new window exists: it starts with nothing installed or titled.
    pub fn attach(&mut self, ctx: &egui::Context) {
        self.window_hidden = false;
        self.wants_show = false;
        self.close = CloseIntent::Unplanned;
        self.tray_wait = None;
        self.installed = None;
        self.title = None;
        self.reported_visible = None;
        // Before the first frame: egui applies new fonts at the start of
        // the next pass, and the interface asks for Inter's weights at once.
        self.sync_theme(ctx);
        theme::follow_text_rendering(ctx);
        if let Some(tray) = &mut self.tray {
            tray.attach();
        }
    }

    fn apply_actions(&mut self, ctx: &egui::Context) {
        for action in std::mem::take(&mut self.actions) {
            self.apply(action, ctx);
        }
    }

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::SelectPair(id) => {
                if self.gui.selected_pair.as_ref() != Some(&id) {
                    self.gui.selected_pair = Some(id);
                    self.mark_gui_dirty();
                    self.load_runs_if_stale();
                }
            }
            Action::ResizeSidebar(width) => {
                if (width - self.gui.sidebar_width).abs() > SIDEBAR_SLACK {
                    self.gui.sidebar_width =
                        width.clamp(gui_state::SIDEBAR_MIN_WIDTH, gui_state::SIDEBAR_MAX_WIDTH);
                    self.mark_gui_dirty();
                }
            }
            // TODO(#9): Settings (also Accounts) and the Remove alert.
            Action::OpenSettings | Action::RemoveSync(_) | Action::OpenAccounts => {}
            Action::AddSync
            | Action::OpenReview(_)
            | Action::AddAccount
            | Action::EditAccount(_)
            | Action::Review(_)
            | Action::AddSyncSheet(_)
            | Action::Account(_) => self.apply_to_sheets(&action),
            Action::SyncNow(id) => {
                self.error = None;
                self.backend.send(Command::SyncNow(id));
            }
            Action::SyncAll => {
                self.error = None;
                self.backend.send(Command::SyncAll);
            }
            Action::SetAllEnabled(enabled) => {
                self.error = None;
                self.backend.send(Command::SetAllEnabled(enabled));
            }
            Action::SelectTab(tab) => {
                self.tab = tab;
                self.load_runs_if_stale();
            }
            Action::SetEnabled(id, enabled) => {
                self.error = None;
                self.backend.send(Command::SetEnabled(id, enabled));
            }
            Action::OpenLocalFolder(id) => {
                self.error = None;
                let local = self
                    .snapshot
                    .pairs
                    .iter()
                    .find(|pair| pair.pair.id == id)
                    .map(|pair| pair.pair.local.clone());
                if let Some(local) = local {
                    self.backend.send(Command::OpenLocalFolder(local));
                }
            }
            Action::OpenVersionsFolder(id) => {
                self.error = None;
                self.backend.send(Command::OpenVersionsFolder(id));
            }
            Action::ShowWindow => self.show_window(ctx),
            Action::HideWindow => self.hide_window(ctx),
            Action::Quit => {
                self.intend(CloseIntent::Quit);
                if !self.window_hidden {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn apply_to_sheets(&mut self, action: &Action) {
        let pairs = self.pair_rows();
        let around = Surroundings {
            pairs: &pairs,
            welcome: self.welcome_visible(),
        };
        for command in self.sheets.apply(action, &around) {
            self.backend.send(command);
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        if self.window_hidden {
            // The shell's headless loop makes the window.
            self.wants_show = true;
        } else if self.wayland && fastframe_shell::window::raise_on_compositor() {
            // The compositor focused the existing window.
        } else if self.wayland {
            // Wayland drops a programmatic focus or unminimize request, so a
            // covered or minimized window cannot come forward that way:
            // close it and let the shell open a fresh one at once.
            self.intend(CloseIntent::Reopen);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        if self.window_hidden {
            return;
        }
        if self.tray_shown() {
            self.intend(CloseIntent::Hide);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }
}

/// What a close request of the window means, given what the app planned and
/// whether a panel shows the tray item to bring the window back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseRequest {
    /// The app already decided (hide, reopen or quit): let the window close.
    Proceed,
    Hide,
    /// No way back from the tray: keep the window and minimize it.
    Minimize,
}

fn on_close_requested(intent: CloseIntent, tray_shown: bool) -> CloseRequest {
    match (intent, tray_shown) {
        (CloseIntent::Unplanned, true) => CloseRequest::Hide,
        (CloseIntent::Unplanned, false) => CloseRequest::Minimize,
        _ => CloseRequest::Proceed,
    }
}

impl Resident for App {
    fn closed(&self) -> Closed {
        match self.close {
            CloseIntent::Hide => Closed::Hide,
            CloseIntent::Reopen => Closed::Reopen,
            CloseIntent::Unplanned | CloseIntent::Quit => Closed::Quit,
        }
    }

    fn window_gone(&mut self) {
        self.window_hidden = true;
        self.wants_show = false;
        self.close = CloseIntent::Unplanned;
        self.save_gui();
    }

    fn headless_frame(&mut self, ctx: &egui::Context) -> Headless {
        self.background_frame(ctx);
        self.check_tray_wait(Instant::now());
        if self.close == CloseIntent::Quit {
            Headless::Quit
        } else if self.wants_show {
            Headless::Show
        } else if self.tray_wait.is_some() {
            Headless::Wait
        } else {
            Headless::WaitFor(HEADLESS_PAUSE)
        }
    }

    /// Without a tray item there is no way back to a hidden window, so the
    /// window opens as usual.
    fn start_hidden(&mut self) -> bool {
        let Some(tray) = &mut self.tray else {
            return false;
        };
        if !tray.is_shown() {
            self.tray_wait = Some(Instant::now() + TRAY_WAIT);
        }
        tray.create_item();
        true
    }

    fn shutdown(&mut self) {
        self.save_gui();
        self.backend.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use syncfast_core::bisync_log::RunSummary;
    use syncfast_core::config::Pair;
    use syncfast_core::state::{DisplayStatus, Gate, GateKind, LastOutcome, LastRun, State};

    use super::*;
    use crate::snapshot::PairSnapshot;

    fn app() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        );
        let themes = Themes::load(&root.join("config/themes"));
        let app = App::new(
            &paths,
            &Waker::default(),
            Inbox::default(),
            GuiState::default(),
            themes,
            false,
        )
        .unwrap();
        (dir, app)
    }

    fn pair_with_gate(id: &str, gate: Option<GateKind>) -> PairSnapshot {
        PairSnapshot {
            pair: Pair {
                id: id.into(),
                name: id.into(),
                local: format!("/data/{id}").into(),
                remote: format!("r:{id}"),
                enabled: true,
                presets: Vec::new(),
                filters: Vec::new(),
            },
            state: State {
                gate: gate.map(|kind| Gate {
                    kind,
                    since: Timestamp::UNIX_EPOCH,
                    detail: String::new(),
                }),
                ..State::default()
            },
            running: false,
            status: DisplayStatus::UpToDate,
            progress: None,
        }
    }

    fn opened(app: &mut App, id: &str) -> Vec<Action> {
        app.inbox.push(Request::Opened(id.into()));
        app.drain_inbox();
        std::mem::take(&mut app.actions)
    }

    #[test]
    fn clicking_the_notification_of_a_sync_waiting_for_review_shows_the_window_selects_it_and_opens_the_review()
     {
        let (_dir, mut app) = app();
        for (id, gate) in [
            ("first", GateKind::NeedsReviewFirstRun),
            ("filters", GateKind::NeedsReviewFiltersChanged),
            ("crash", GateKind::NeedsReviewCriticalError),
            ("deletes", GateKind::TooManyDeletes),
        ] {
            app.snapshot.pairs = vec![pair_with_gate(id, Some(gate))];

            assert_eq!(
                opened(&mut app, id),
                [
                    Action::ShowWindow,
                    Action::SelectPair(id.into()),
                    Action::OpenReview(id.into())
                ],
                "{gate:?}"
            );
        }
    }

    #[test]
    fn clicking_a_notification_that_needs_no_review_only_shows_and_selects() {
        let (_dir, mut app) = app();
        app.snapshot.pairs = vec![
            pair_with_gate("auth", Some(GateKind::AuthError)),
            pair_with_gate("missing", Some(GateKind::LocalMissing)),
            pair_with_gate("fine", None),
        ];

        for id in ["auth", "missing", "fine", "gone"] {
            assert_eq!(
                opened(&mut app, id),
                [Action::ShowWindow, Action::SelectPair(id.into())],
                "{id}"
            );
        }
    }

    #[test]
    fn welcome_shows_only_without_syncs_and_without_a_config_problem_until_confirmed() {
        let (_dir, mut app) = app();
        assert!(app.welcome_visible());

        app.snapshot.config_error = Some("config.toml: broken".into());
        assert!(!app.welcome_visible());

        app.snapshot.config_error = None;
        app.snapshot.pairs = vec![pair_with_gate("a", None)];
        assert!(!app.welcome_visible());

        app.snapshot.pairs.clear();
        app.gui.welcome_done = true;
        assert!(!app.welcome_visible());
    }

    #[test]
    fn the_first_round_that_ends_well_ends_the_welcome_for_good_and_a_failed_one_does_not() {
        let (_dir, mut app) = app();
        let mut failed = pair_with_gate("a", None);
        failed.state.last_outcome = Some(LastOutcome::Error);
        let mut fine = pair_with_gate("b", None);
        fine.state.last_outcome = Some(LastOutcome::Ok);

        app.handle_event(Event::Snapshot(Snapshot {
            config_error: None,
            pairs: vec![failed],
        }));
        assert!(!app.gui.welcome_done);
        assert!(app.gui_dirty_since.is_none());

        app.handle_event(Event::Snapshot(Snapshot {
            config_error: None,
            pairs: vec![fine],
        }));
        assert!(app.gui.welcome_done);
        assert!(app.gui_dirty_since.is_some());

        app.handle_event(Event::Snapshot(Snapshot::default()));
        assert!(app.gui.welcome_done, "it never comes back");
    }

    #[test]
    fn selecting_a_sync_is_remembered_for_the_next_run() {
        let (_dir, mut app) = app();
        app.snapshot.pairs = vec![pair_with_gate("a", None), pair_with_gate("b", None)];

        app.apply(Action::SelectPair("b".into()), &egui::Context::default());

        assert_eq!(app.gui.selected_pair.as_deref(), Some("b"));
        assert!(app.gui_dirty_since.is_some());
        assert_eq!(app.selected_id().as_deref(), Some("b"));
    }

    #[test]
    fn the_history_is_read_again_when_a_round_ends_even_if_no_poll_saw_it_running() {
        let (_dir, mut app) = app();
        app.snapshot.pairs = vec![pair_with_gate("a", None)];
        app.load_runs_if_stale();
        let first = app.runs_key.clone();

        app.load_runs_if_stale();
        assert_eq!(app.runs_key, first, "a poll with nothing new reads nothing");

        let run = LastRun {
            started: Timestamp::UNIX_EPOCH,
            finished: "2026-11-03T12:00:00Z".parse().unwrap(),
            exit_code: 0,
            summary: RunSummary::default(),
            log_file: "a.jsonl".into(),
        };
        app.snapshot.pairs[0].state.last_run = Some(run);
        app.load_runs_if_stale();

        assert_ne!(app.runs_key, first);
        assert_eq!(
            app.runs_key.map(|key| key.2),
            Some(Some("2026-11-03T12:00:00Z".parse().unwrap()))
        );
    }

    #[test]
    fn the_first_sync_is_selected_when_none_was_chosen_or_the_chosen_one_is_gone() {
        let (_dir, mut app) = app();
        app.snapshot.pairs = vec![pair_with_gate("a", None), pair_with_gate("b", None)];

        assert_eq!(app.selected_id().as_deref(), Some("a"));
        app.gui.selected_pair = Some("removed".into());
        assert_eq!(app.selected_id().as_deref(), Some("a"));
    }

    #[test]
    fn closing_with_a_planned_hide_keeps_the_app_running_and_anything_else_ends_it() {
        let (_dir, mut app) = app();
        assert_eq!(app.closed(), Closed::Quit);

        app.intend(CloseIntent::Hide);
        assert_eq!(app.closed(), Closed::Hide);
        app.intend(CloseIntent::Reopen);
        assert_eq!(app.closed(), Closed::Reopen);
        app.intend(CloseIntent::Quit);
        assert_eq!(app.closed(), Closed::Quit);
    }

    #[test]
    fn a_quit_is_never_turned_into_a_hide() {
        let (_dir, mut app) = app();
        app.intend(CloseIntent::Quit);

        app.intend(CloseIntent::Hide);

        assert_eq!(app.closed(), Closed::Quit);
    }

    #[test]
    fn a_window_that_is_gone_forgets_what_the_last_one_meant_to_do() {
        let (_dir, mut app) = app();
        app.intend(CloseIntent::Hide);

        app.window_gone();

        assert!(app.window_hidden);
        assert_eq!(app.close, CloseIntent::Unplanned);
    }

    #[test]
    fn the_close_button_hides_when_a_panel_shows_the_tray_and_minimizes_when_none_does() {
        assert_eq!(
            on_close_requested(CloseIntent::Unplanned, true),
            CloseRequest::Hide
        );
        assert_eq!(
            on_close_requested(CloseIntent::Unplanned, false),
            CloseRequest::Minimize
        );
        for planned in [CloseIntent::Hide, CloseIntent::Reopen, CloseIntent::Quit] {
            assert_eq!(on_close_requested(planned, false), CloseRequest::Proceed);
        }
    }
}
