//! Every screen is a function of an immutable [`ViewModel`] that returns the
//! [`Action`]s the user asked for. It never does I/O: `app.rs` applies the
//! actions after the frame.

pub mod relative;
pub mod sidebar;
pub mod widgets;

use jiff::Timestamp;
use syncfast_core::bisync_log::Progress;
use syncfast_core::state::{DisplayStatus, GateKind};

use crate::snapshot::PairSnapshot;
use crate::theme;

/// The window title when no sync is selected.
pub const TITLE: &str = "Syncs";

/// One sync as the screens show it.
#[derive(Debug, Clone, PartialEq)]
pub struct PairRow {
    pub id: String,
    pub name: String,
    pub local: std::path::PathBuf,
    pub remote: String,
    pub enabled: bool,
    pub status: DisplayStatus,
    pub gate: Option<GateKind>,
    pub last_success: Option<Timestamp>,
    /// Live numbers of the round in progress, if there is one.
    pub progress: Option<Progress>,
}

impl PairRow {
    /// `Sync Now` is offered unless a round is running or a gate blocks it.
    #[must_use]
    pub fn can_sync(&self) -> bool {
        self.status != DisplayStatus::Syncing && self.gate.is_none()
    }
}

impl From<&PairSnapshot> for PairRow {
    fn from(snapshot: &PairSnapshot) -> Self {
        Self {
            id: snapshot.pair.id.clone(),
            name: snapshot.pair.name.clone(),
            local: snapshot.pair.local.clone(),
            remote: snapshot.pair.remote.clone(),
            enabled: snapshot.pair.enabled,
            status: snapshot.status,
            gate: snapshot.state.gate.as_ref().map(|gate| gate.kind),
            last_success: snapshot.state.last_success,
            progress: snapshot.progress.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewModel {
    pub pairs: Vec<PairRow>,
    pub selected: Option<String>,
    pub sidebar_width: f32,
    /// A problem to show near the list: a broken config or a failed action.
    pub error: Option<String>,
    pub now: Timestamp,
}

impl ViewModel {
    #[must_use]
    pub fn selected_pair(&self) -> Option<&PairRow> {
        let id = self.selected.as_deref()?;
        self.pairs.iter().find(|pair| pair.id == id)
    }

    /// The name of the selected sync, or `Syncs`. Never the app's name.
    #[must_use]
    pub fn title(&self) -> String {
        self.selected_pair()
            .map_or_else(|| TITLE.to_owned(), |pair| pair.name.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    SelectPair(String),
    ResizeSidebar(f32),
    /// Opens the Add Sync sheet.
    AddSync,
    /// Opens the Settings window.
    OpenSettings,
    /// Opens the Review sheet of a sync (from a notification).
    OpenReview(String),
    SyncNow(String),
    SyncAll,
    /// Pause (`false`) or resume (`true`) every sync.
    SetAllEnabled(bool),
    ShowWindow,
    HideWindow,
    Quit,
}

/// The whole window: sidebar and detail.
#[must_use]
pub fn root(ui: &mut egui::Ui, vm: &ViewModel) -> Vec<Action> {
    let mut actions = sidebar::show(ui, vm);
    actions.extend(detail(ui, vm));
    actions
}

fn detail(ui: &mut egui::Ui, vm: &ViewModel) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(palette.window)
                .inner_margin(egui::Margin::same(20)),
        )
        .show(ui, |ui| {
            if let Some(error) = &vm.error {
                widgets::inline_error(ui, error);
                ui.add_space(theme::space::M);
            }
            match vm.selected_pair() {
                Some(pair) => {
                    ui.label(
                        egui::RichText::new(&pair.name)
                            .font(theme::title2())
                            .color(palette.text),
                    );
                    ui.label(
                        egui::RichText::new(format!("{} ↔ {}", pair.local.display(), pair.remote))
                            .font(theme::callout())
                            .color(palette.secondary),
                    );
                }
                None => {
                    if widgets::empty_state(
                        ui,
                        crate::icons::Icon::FolderSync,
                        "No syncs yet",
                        Some("Keep a folder on this computer and a folder in the cloud the same."),
                        Some("Add Sync…"),
                    ) {
                        actions.push(Action::AddSync);
                    }
                }
            }
        });
    actions
}

/// Keyboard shortcuts of the window. Nothing is drawn.
#[must_use]
pub fn shortcuts(ctx: &egui::Context, vm: &ViewModel) -> Vec<Action> {
    use egui::{Key, Modifiers};
    let pressed = |key| ctx.input_mut(|input| input.consume_key(Modifiers::CTRL, key));
    let mut actions = Vec::new();
    if pressed(Key::N) {
        actions.push(Action::AddSync);
    }
    if pressed(Key::R)
        && let Some(pair) = vm.selected_pair().filter(|pair| pair.can_sync())
    {
        actions.push(Action::SyncNow(pair.id.clone()));
    }
    if pressed(Key::Comma) {
        actions.push(Action::OpenSettings);
    }
    if pressed(Key::W) {
        actions.push(Action::HideWindow);
    }
    if pressed(Key::Q) {
        actions.push(Action::Quit);
    }
    actions
}
