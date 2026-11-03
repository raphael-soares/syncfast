//! Every screen is a function of an immutable [`ViewModel`] that returns the
//! [`Action`]s the user asked for. It never does I/O: `app.rs` applies the
//! actions after the frame.

pub mod account_sheet;
pub mod activity;
pub mod add_sync;
pub mod conflicts;
pub mod detail;
pub mod filters;
pub mod format;
pub mod relative;
pub mod review;
pub mod sidebar;
pub mod welcome;
pub mod widgets;

use std::sync::Arc;

use jiff::Timestamp;
use syncfast_core::bisync_log::Progress;
use syncfast_core::state::{DisplayStatus, GateKind};

use crate::runs::RunEntry;
use crate::snapshot::PairSnapshot;

pub use detail::DetailTab;

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
    /// What the gate says about itself; empty without a gate.
    pub gate_detail: String,
    pub consecutive_failures: u32,
}

impl PairRow {
    /// `Sync Now` is offered unless a round is running, a gate blocks it or
    /// the sync is paused (a round of a paused sync does nothing).
    #[must_use]
    pub fn can_sync(&self) -> bool {
        self.status != DisplayStatus::Syncing && self.gate.is_none() && self.enabled
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
            gate_detail: snapshot
                .state
                .gate
                .as_ref()
                .map(|gate| gate.detail.clone())
                .unwrap_or_default(),
            consecutive_failures: snapshot.state.consecutive_failures,
        }
    }
}

/// One rclone remote (an account) as the screens show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRow {
    pub name: String,
    /// The rclone type, for example `drive` or `local`.
    pub kind: String,
    /// Only meaningful for `drive`: the remote has its own `client_id`.
    pub own_client_id: bool,
}

impl RemoteRow {
    #[must_use]
    pub fn is_drive(&self) -> bool {
        self.kind == "drive"
    }

    /// A Drive remote on rclone's shared client ID, which stops working in
    /// 2026.
    #[must_use]
    pub fn shares_client_id(&self) -> bool {
        self.is_drive() && !self.own_client_id
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
    pub tab: DetailTab,
    /// The rounds of the selected sync; `None` until they are read.
    pub runs: Option<Arc<[RunEntry]>>,
    pub remotes: Vec<RemoteRow>,
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
    SelectTab(DetailTab),
    /// Pause (`false`) or resume (`true`) one sync.
    SetEnabled(String, bool),
    /// Opens the folder of the sync on this computer.
    OpenLocalFolder(String),
    /// Opens the folder that keeps the sync's saved versions.
    OpenVersionsFolder(String),
    /// Asks to remove a sync (the alert decides).
    RemoveSync(String),
    /// Opens Settings on Accounts.
    OpenAccounts,
    /// Opens the sheet that adds a Google Drive account.
    AddAccount,
    /// Opens the account sheet for an existing remote, to sign in again or
    /// to give it the user's own client ID.
    EditAccount(String),
    Review(review::ReviewAction),
    AddSyncSheet(add_sync::AddSyncAction),
    Account(account_sheet::AccountAction),
}

/// The whole window: sidebar and detail.
#[must_use]
pub fn root(ui: &mut egui::Ui, vm: &ViewModel) -> Vec<Action> {
    let mut actions = sidebar::show(ui, vm);
    actions.extend(detail::show(ui, vm));
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
