//! The New Backup assistant (folders, destination, schedule, filters, name,
//! key) and the Add Existing Backup sheet (destination, key, name).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use egui::{RichText, Ui};
use syncfast_core::backup::config::{Every, Keep};
use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::KeyBackend;
use syncfast_core::backup::lifecycle::{ExistingBackup, NewBackup, backup_id};
use syncfast_core::behavior::Bandwidth;
use zeroize::Zeroizing;

use super::add_sync::{
    self, AddSyncAction, DirListing, account_row, children, expand_local, join_rel, listing_notes,
    new_folder_section, remote_key,
};
use super::backup_keys::{self, KeyFileOutcome};
use super::filters::{self as tab, FilterDraft, FilterTarget, FilterTest};
use super::widgets::{
    self, ButtonKind, FolderTree, NodeState, PrimaryKind, SheetAction, SheetButtons, TreeAction,
};
use super::{Action, RemoteRow};
use crate::icons::Icon;
use crate::theme::{self, size, space};

const FOLDERS_NOTE: &str =
    "Folders on another disk inside these folders are not included. Add them as their own folder.";
const DRIVE_HINT: &str =
    "Choose a folder only for this backup. Do not use a folder that a sync uses.";
const RETENTION_NOTE: &str = "Older snapshots are deleted from the destination after each backup.";
const MOUNTS_NOTE: &str =
    "Mounted drives, rclone mounts and Btrfs subvolumes inside these folders are backed up too.";
const HOST_KEYRING_NOTE: &str = "Using the host keyring. See docs/guide/backup.md.";
const EVERY: [(&str, Every); 3] = [
    ("Every hour", Every::Hourly),
    ("Every day", Every::Daily),
    ("Every week", Every::Weekly),
];
const KEEP_NAMES: [&str; 4] = ["daily", "weekly", "monthly", "yearly"];
const KEEP_MAX: u16 = 999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFacts {
    pub id: String,
    pub name: String,
    pub dest: String,
    pub key_unsaved: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackupSheetAction {
    Cancel,
    /// `Add Folder…` or the `Choose…` of a destination on this computer.
    PickFolder(PickTarget),
    Check(CheckRequest),
    TestFilters {
        paths: Vec<PathBuf>,
        presets: Vec<String>,
        rules: Vec<String>,
    },
    Create(Box<NewBackup>, Key),
    AddExisting(ExistingBackup, Key),
    SaveKeyToFile(Key),
    /// The `Show Key` button of the confirmation alert.
    ConfirmShowKey,
    /// `I Saved the Key` of a backup whose key was never saved.
    MarkKeySaved,
    SaveEnteredKey(Key),
    ConfirmRemove {
        delete_key: bool,
    },
    Restore(super::backup_restore::RestoreAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickTarget {
    Folder,
    Destination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckPart {
    Folders,
    Destination,
    ExistingDestination,
}

/// A question for the backend about what the user typed. The answer comes
/// back with the same `token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRequest {
    pub token: String,
    pub part: CheckPart,
    pub folders: Vec<PathBuf>,
    pub dest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStoreFacts {
    Probing,
    Ready(KeyBackend),
    /// The text of `KeyError::Unavailable`.
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepError {
    pub step: Step,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    New,
    Existing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Folders,
    Destination,
    Schedule,
    Filters,
    Name,
    Key,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestKind {
    Drive,
    Computer,
}

/// A typed backup key. Zeroed when dropped; `Debug` hides it.
#[derive(Clone, Default)]
pub struct TypedKey(Zeroizing<String>);

impl TypedKey {
    pub fn buffer(&mut self) -> &mut String {
        &mut self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// # Errors
    /// The text is not a backup key.
    pub fn parse(&self) -> Result<Key, syncfast_core::backup::key::KeyError> {
        Key::parse(&self.0)
    }
}

impl fmt::Debug for TypedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TypedKey(..)")
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // independent form switches
pub struct WizardState {
    pub flow: Flow,
    pub step: Step,
    pub folders: Vec<PathBuf>,
    pub kind: DestKind,
    pub account: Option<String>,
    pub remote_path: Option<String>,
    pub expanded: HashSet<String>,
    pub new_folder: Option<String>,
    pub local: String,
    local_editing: bool,
    pub every: Every,
    pub customize: bool,
    pub keep: [String; 4],
    pub one_file_system: bool,
    pub limit_upload: String,
    pub limit_download: String,
    pub presets: Vec<String>,
    pub rules: String,
    pub name: String,
    pub key: Option<Key>,
    pub key_error: Option<String>,
    pub typed_key: TypedKey,
    pub show_typed_key: bool,
    answers: HashMap<String, Option<String>>,
    asked: HashSet<String>,
    pub busy: bool,
    pub error: Option<StepError>,
}

impl WizardState {
    #[must_use]
    pub fn new(flow: Flow) -> Self {
        let defaults =
            NewBackup::with_defaults(String::new(), String::new(), Vec::new(), String::new());
        Self {
            flow,
            step: sequence(flow)[0],
            folders: Vec::new(),
            kind: DestKind::Drive,
            account: None,
            remote_path: None,
            expanded: HashSet::new(),
            new_folder: None,
            local: String::new(),
            local_editing: false,
            every: defaults.every,
            customize: false,
            keep: [
                defaults.keep.daily.to_string(),
                defaults.keep.weekly.to_string(),
                defaults.keep.monthly.to_string(),
                defaults.keep.yearly.to_string(),
            ],
            one_file_system: defaults.one_file_system,
            limit_upload: String::new(),
            limit_download: String::new(),
            presets: defaults.presets,
            rules: String::new(),
            name: String::new(),
            key: None,
            key_error: None,
            typed_key: TypedKey::default(),
            show_typed_key: false,
            answers: HashMap::new(),
            asked: HashSet::new(),
            busy: false,
            error: None,
        }
    }

    pub fn add_folder(&mut self, path: PathBuf) {
        if !self.folders.contains(&path) {
            self.folders.push(path);
        }
    }

    pub fn picked_local(&mut self, path: &Path) {
        self.local = path.display().to_string();
    }

    pub fn select_account(&mut self, name: &str) {
        self.kind = DestKind::Drive;
        self.account = Some(name.to_owned());
        self.remote_path = None;
        self.expanded.clear();
        self.new_folder = None;
    }

    pub fn remote_folder_created(&mut self, parent: &str, child: &str) {
        if !parent.is_empty() {
            self.expanded.insert(parent.to_owned());
        }
        self.new_folder = None;
        self.remote_path = Some(join_rel(parent, child));
    }

    pub fn record(&mut self, token: String, problem: Option<String>) {
        self.answers.insert(token, problem);
    }

    #[must_use]
    pub fn sequence(&self) -> &'static [Step] {
        sequence(self.flow)
    }

    fn go(&mut self, step: Step) {
        self.step = step;
        self.answers.clear();
        self.asked.clear();
        self.error = None;
    }

    pub fn start_call(&mut self) {
        self.busy = true;
        self.error = None;
    }

    pub fn fail(&mut self, step: Step, text: String) {
        self.busy = false;
        self.error = Some(StepError { step, text });
    }

    /// Like [`Self::fail`], and goes back to `step` to fix it.
    pub fn fail_at(&mut self, step: Step, text: String) {
        self.go(step);
        self.fail(step, text);
    }

    fn advance(&mut self) {
        let steps = self.sequence();
        if let Some(index) = steps.iter().position(|step| *step == self.step)
            && let Some(next) = steps.get(index + 1)
        {
            self.go(*next);
        }
    }

    fn back(&mut self) {
        let steps = self.sequence();
        if let Some(index) = steps.iter().position(|step| *step == self.step)
            && index > 0
        {
            self.go(steps[index - 1]);
        }
    }

    fn enter_key_step(&mut self) {
        match Key::generate() {
            Ok(key) => {
                self.key = Some(key);
                self.key_error = None;
            }
            Err(err) => self.key_error = Some(err.to_string()),
        }
        self.advance();
    }

    /// The destination as `backup.dest` stores it. `Err(None)` while the
    /// user has not chosen one yet.
    fn destination(&self, home: &Path) -> Result<String, Option<String>> {
        match self.kind {
            DestKind::Drive => match (&self.account, &self.remote_path) {
                (Some(account), Some(path)) if !path.is_empty() => Ok(format!("{account}:{path}")),
                _ => Err(None),
            },
            DestKind::Computer => {
                let typed = self.local.trim();
                if typed.is_empty() {
                    return Err(None);
                }
                let path = expand_local(home, typed);
                if path.is_absolute() {
                    Ok(path.to_string_lossy().into_owned())
                } else {
                    Err(Some("Enter the full path of the folder.".to_owned()))
                }
            }
        }
    }

    fn keep(&self) -> Result<Keep, String> {
        let mut values = [0_u16; 4];
        for ((slot, text), name) in values.iter_mut().zip(&self.keep).zip(KEEP_NAMES) {
            let text = text.trim();
            *slot = if text.is_empty() {
                0
            } else {
                text.parse::<u16>()
                    .ok()
                    .filter(|value| *value <= KEEP_MAX)
                    .ok_or_else(|| format!("keep.{name} must be between 0 and {KEEP_MAX}"))?
            };
        }
        if values.iter().all(|value| *value == 0) {
            return Err("keep must retain at least one snapshot".to_owned());
        }
        let [daily, weekly, monthly, yearly] = values;
        Ok(Keep {
            daily,
            weekly,
            monthly,
            yearly,
        })
    }

    fn limit(text: &str, name: &str) -> Result<Bandwidth, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Bandwidth::Off);
        }
        Bandwidth::parse(text).ok_or_else(|| {
            format!(
                "{name} must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M"
            )
        })
    }

    fn schedule_problem(&self) -> Option<String> {
        self.keep()
            .err()
            .or_else(|| Self::limit(&self.limit_upload, "limit_upload").err())
            .or_else(|| Self::limit(&self.limit_download, "limit_download").err())
    }

    fn filters(&self) -> FilterDraft {
        FilterDraft {
            presets: self.presets.clone(),
            rules: self.rules.clone(),
        }
    }

    fn default_name(&self) -> String {
        let source = match self.flow {
            Flow::New => self.folders.first().cloned(),
            Flow::Existing => None,
        };
        source
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| {
                let dest = match (self.kind, &self.remote_path, self.local.trim()) {
                    (DestKind::Drive, Some(path), _) => path.clone(),
                    (DestKind::Computer, _, local) => local.to_owned(),
                    _ => String::new(),
                };
                Path::new(dest.trim_end_matches('/'))
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Backup".to_owned())
    }

    fn effective_name(&self) -> String {
        let typed = self.name.trim();
        if typed.is_empty() {
            self.default_name()
        } else {
            typed.to_owned()
        }
    }

    #[must_use]
    pub fn backup_id(&self, taken: &[BackupFacts]) -> String {
        backup_id(
            &self.effective_name(),
            taken.iter().map(|backup| backup.id.as_str()),
        )
    }

    fn new_backup(&self, home: &Path, taken: &[BackupFacts]) -> Option<NewBackup> {
        let dest = self.destination(home).ok()?;
        let keep = self.keep().ok()?;
        Some(NewBackup {
            name: self.effective_name(),
            id: self.backup_id(taken),
            paths: self.folders.clone(),
            dest,
            every: self.every,
            keep,
            presets: self.presets.clone(),
            filters: self.filters().custom_rules(),
            one_file_system: self.one_file_system,
            limit_upload: Self::limit(&self.limit_upload, "limit_upload").ok()?,
            limit_download: Self::limit(&self.limit_download, "limit_download").ok()?,
        })
    }

    fn existing_backup(&self, home: &Path, taken: &[BackupFacts]) -> Option<ExistingBackup> {
        Some(ExistingBackup {
            name: self.effective_name(),
            id: self.backup_id(taken),
            dest: self.destination(home).ok()?,
        })
    }

    #[must_use]
    pub fn filter_request(&self) -> BackupSheetAction {
        BackupSheetAction::TestFilters {
            paths: self.folders.clone(),
            presets: self.presets.clone(),
            rules: self.filters().custom_rules(),
        }
    }
}

#[must_use]
pub fn sequence(flow: Flow) -> &'static [Step] {
    match flow {
        Flow::New => &[
            Step::Folders,
            Step::Destination,
            Step::Schedule,
            Step::Filters,
            Step::Name,
            Step::Key,
        ],
        Flow::Existing => &[Step::Destination, Step::Key, Step::Name],
    }
}

fn step_title(flow: Flow, step: Step) -> &'static str {
    match step {
        Step::Folders => "Folders",
        Step::Destination => "Destination",
        Step::Schedule => "Schedule",
        Step::Filters => "Filters",
        Step::Name => "Name",
        Step::Key if flow == Flow::New => "Your Backup Key",
        Step::Key => "Key",
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WizardView<'a> {
    pub remotes: &'a [RemoteRow],
    pub backups: &'a [BackupFacts],
    pub dirs: &'a HashMap<String, DirListing>,
    pub mkdir_error: Option<&'a str>,
    pub home: &'a Path,
    pub key_store: &'a KeyStoreFacts,
    pub filter_test: Option<&'a FilterTest>,
    pub key_file: Option<&'a KeyFileOutcome>,
}

struct Gate {
    ready: bool,
    problem: Option<String>,
}

impl Gate {
    fn ready() -> Self {
        Self {
            ready: true,
            problem: None,
        }
    }

    fn waiting() -> Self {
        Self {
            ready: false,
            problem: None,
        }
    }

    fn blocked(problem: String) -> Self {
        Self {
            ready: false,
            problem: Some(problem),
        }
    }

    fn of(ready: bool, problem: Option<String>) -> Self {
        match problem {
            Some(text) => Self::blocked(text),
            None => Self {
                ready,
                problem: None,
            },
        }
    }
}

fn gate(view: &WizardView<'_>, state: &WizardState) -> Gate {
    if state.step == state.sequence()[0] {
        match view.key_store {
            KeyStoreFacts::Unavailable(text) => return Gate::blocked(text.clone()),
            KeyStoreFacts::Probing => return Gate::waiting(),
            KeyStoreFacts::Ready(_) => {}
        }
    }
    match state.step {
        Step::Folders => {
            if state.folders.is_empty() {
                return Gate::waiting();
            }
            answer(state, &folders_token(state))
        }
        Step::Destination => match state.destination(view.home) {
            Err(None) => Gate::waiting(),
            Err(Some(text)) => Gate::blocked(text),
            Ok(dest) => answer(state, &destination_token(state, &dest)),
        },
        Step::Schedule => Gate::of(true, state.schedule_problem()),
        Step::Filters => Gate {
            ready: state
                .filters()
                .problems_for(FilterTarget::Backup)
                .is_empty(),
            problem: None,
        },
        Step::Name => Gate::ready(),
        Step::Key => match state.flow {
            Flow::New => match (&state.key, &state.key_error) {
                (_, Some(text)) => Gate::blocked(text.clone()),
                (Some(_), None) => Gate::ready(),
                (None, None) => Gate::waiting(),
            },
            Flow::Existing => match state.typed_key.parse() {
                Ok(_) => Gate::ready(),
                Err(_) if state.typed_key.is_empty() => Gate::waiting(),
                Err(err) => Gate::blocked(err.to_string()),
            },
        },
    }
}

fn answer(state: &WizardState, token: &str) -> Gate {
    match state.answers.get(token) {
        None => Gate::waiting(),
        Some(Some(problem)) => Gate::blocked(problem.clone()),
        Some(None) => Gate::ready(),
    }
}

fn folders_token(state: &WizardState) -> String {
    state
        .folders
        .iter()
        .map(|folder| folder.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n")
}

fn destination_token(state: &WizardState, dest: &str) -> String {
    match state.flow {
        Flow::New => format!("{dest}\n{}", folders_token(state)),
        Flow::Existing => format!("existing\n{dest}"),
    }
}

fn check_for(view: &WizardView<'_>, state: &WizardState) -> Option<CheckRequest> {
    match state.step {
        Step::Folders if !state.folders.is_empty() => Some(CheckRequest {
            token: folders_token(state),
            part: CheckPart::Folders,
            folders: state.folders.clone(),
            dest: String::new(),
        }),
        Step::Destination => {
            let dest = state.destination(view.home).ok()?;
            let part = match state.flow {
                Flow::New => CheckPart::Destination,
                Flow::Existing => CheckPart::ExistingDestination,
            };
            Some(CheckRequest {
                token: destination_token(state, &dest),
                part,
                folders: state.folders.clone(),
                dest,
            })
        }
        _ => None,
    }
}

fn emit(actions: &mut Vec<Action>, action: BackupSheetAction) {
    actions.push(Action::BackupSheet(action));
}

fn emit_dirs(actions: &mut Vec<Action>, action: AddSyncAction) {
    actions.push(Action::AddSyncSheet(action));
}

pub fn show(ctx: &egui::Context, view: &WizardView<'_>, state: &mut WizardState) -> Vec<Action> {
    let flow = state.flow;
    let step = state.step;
    let gate = gate(view, state);
    let last = *state.sequence().last().unwrap_or(&step);
    let primary = match (flow, step) {
        (Flow::New, Step::Key) => "I Saved the Key",
        (Flow::Existing, Step::Name) => "Add Backup",
        _ => "Continue",
    };
    let buttons = SheetButtons {
        primary,
        primary_kind: PrimaryKind::Normal,
        primary_enabled: gate.ready && !state.busy,
        back: step != state.sequence()[0] && !(flow == Flow::New && step == last),
        cancel: true,
    };
    let names: Vec<&str> = state
        .sequence()
        .iter()
        .map(|step| step_title(flow, *step))
        .collect();
    let current = state
        .sequence()
        .iter()
        .position(|candidate| *candidate == step)
        .unwrap_or(0);
    let id = match flow {
        Flow::New => "new-backup",
        Flow::Existing => "add-existing-backup",
    };
    let mut actions = Vec::new();
    let chosen = widgets::sheet_with_header(
        ctx,
        id,
        step_title(flow, step),
        &buttons,
        |ui| add_sync::steps_header(ui, &names, current),
        |ui| body(ui, view, state, &gate, &mut actions),
    );
    match chosen {
        Some(SheetAction::Cancel) => emit(&mut actions, BackupSheetAction::Cancel),
        Some(SheetAction::Back) => {
            state.back();
            ctx.request_repaint();
        }
        Some(SheetAction::Primary) if gate.ready && !state.busy => {
            primary_pressed(view, state, &mut actions);
            ctx.request_repaint();
        }
        Some(SheetAction::Primary) | None => {}
    }
    request_check(view, state, &mut actions);
    actions
}

fn primary_pressed(view: &WizardView<'_>, state: &mut WizardState, actions: &mut Vec<Action>) {
    match (state.flow, state.step) {
        (Flow::New, Step::Name) => state.enter_key_step(),
        (Flow::New, Step::Key) => create(view, state, actions),
        (Flow::Existing, Step::Name) => {
            if let (Some(draft), Ok(key)) = (
                state.existing_backup(view.home, view.backups),
                state.typed_key.parse(),
            ) {
                emit(actions, BackupSheetAction::AddExisting(draft, key));
            }
        }
        _ => state.advance(),
    }
}

fn create(view: &WizardView<'_>, state: &WizardState, actions: &mut Vec<Action>) {
    if let (Some(draft), Some(key)) = (state.new_backup(view.home, view.backups), &state.key) {
        emit(
            actions,
            BackupSheetAction::Create(Box::new(draft), key.clone()),
        );
    }
}

fn request_check(view: &WizardView<'_>, state: &mut WizardState, actions: &mut Vec<Action>) {
    if state.local_editing {
        return;
    }
    if let Some(request) = check_for(view, state)
        && !state.answers.contains_key(&request.token)
        && state.asked.insert(request.token.clone())
    {
        emit(actions, BackupSheetAction::Check(request));
    }
}

fn body(
    ui: &mut Ui,
    view: &WizardView<'_>,
    state: &mut WizardState,
    gate: &Gate,
    actions: &mut Vec<Action>,
) {
    backup_keys::wrap_labels(ui);
    match state.step {
        Step::Folders => folders_step(ui, state, actions),
        Step::Destination => destination_step(ui, view, state, actions),
        Step::Schedule => schedule_step(ui, state),
        Step::Filters => filters_step(ui, view, state, actions),
        Step::Name => name_step(ui, view, state),
        Step::Key => key_step(ui, view, state, actions),
    }
    if let Some(problem) = &gate.problem {
        widgets::inline_error(ui, problem);
    }
    if let KeyStoreFacts::Ready(KeyBackend::HostKeyring) = view.key_store
        && state.step == state.sequence()[0]
    {
        note(ui, HOST_KEYRING_NOTE);
    }
    if let Some(error) = &state.error
        && error.step == state.step
    {
        widgets::inline_error(ui, &error.text);
    }
}

fn note(ui: &mut Ui, text: &str) {
    let palette = theme::palette(ui.ctx());
    ui.label(
        RichText::new(text)
            .font(theme::callout())
            .color(palette.secondary),
    );
}

fn folders_step(ui: &mut Ui, state: &mut WizardState, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    let mut remove = None;
    for (index, folder) in state.folders.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_button(ui, Icon::X, "Remove Folder").clicked() {
                    remove = Some(index);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(Icon::Folder.image(palette.secondary, size::ICON));
                    ui.label(
                        RichText::new(folder.display().to_string())
                            .font(theme::body())
                            .color(palette.text),
                    );
                });
            });
        });
    }
    if let Some(index) = remove {
        state.folders.remove(index);
        ui.ctx().request_repaint();
    }
    add_folder_button(ui, actions);
    note(ui, FOLDERS_NOTE);
}

fn add_folder_button(ui: &mut Ui, actions: &mut Vec<Action>) {
    if widgets::button_with_icon(
        ui,
        ButtonKind::Secondary,
        Some(Icon::Plus),
        "Add Folder\u{2026}",
    )
    .clicked()
    {
        emit(actions, BackupSheetAction::PickFolder(PickTarget::Folder));
    }
}

fn destination_step(
    ui: &mut Ui,
    view: &WizardView<'_>,
    state: &mut WizardState,
    actions: &mut Vec<Action>,
) {
    let selected = match state.kind {
        DestKind::Drive => 0,
        DestKind::Computer => 1,
    };
    if let Some(index) = widgets::segmented(ui, selected, &["Google Drive", "This Computer"]) {
        state.kind = if index == 0 {
            DestKind::Drive
        } else {
            DestKind::Computer
        };
        ui.ctx().request_repaint();
    }
    match state.kind {
        DestKind::Drive => drive_pane(ui, view, state, actions),
        DestKind::Computer => computer_pane(ui, state, actions),
    }
}

fn drive_pane(
    ui: &mut Ui,
    view: &WizardView<'_>,
    state: &mut WizardState,
    actions: &mut Vec<Action>,
) {
    if view.remotes.is_empty() {
        note(ui, "No accounts yet.");
    }
    for remote in view.remotes {
        let selected = state.account.as_deref() == Some(remote.name.as_str());
        if account_row(ui, remote, selected) && !selected {
            state.select_account(&remote.name);
            ui.ctx().request_repaint();
        }
    }
    ui.add_space(space::S);
    if widgets::button_with_icon(
        ui,
        ButtonKind::Secondary,
        Some(Icon::Plus),
        "Add Google Drive Account\u{2026}",
    )
    .clicked()
    {
        actions.push(Action::AddAccount);
    }
    let Some(account) = state.account.clone() else {
        return;
    };
    let root = remote_key(&account, "");
    if !view.dirs.contains_key(&root) {
        emit_dirs(actions, AddSyncAction::LoadDirs(root));
    }
    ui.add_space(space::S);
    let nodes = match children(&account, view.dirs, "") {
        NodeState::Loaded(nodes) => nodes,
        NodeState::Unloaded | NodeState::Loading => Vec::new(),
    };
    let tree_actions = widgets::folder_tree(
        ui,
        &FolderTree {
            root_label: "",
            nodes: &nodes,
            selected: state.remote_path.as_deref(),
            expanded: &state.expanded,
        },
    );
    for action in tree_actions {
        ui.ctx().request_repaint();
        match action {
            TreeAction::Select(path) => state.remote_path = Some(path),
            TreeAction::Toggle(path) => {
                if !state.expanded.remove(&path) {
                    let key = remote_key(&account, &path);
                    if !view.dirs.contains_key(&key) {
                        emit_dirs(actions, AddSyncAction::LoadDirs(key));
                    }
                    state.expanded.insert(path);
                }
            }
        }
    }
    listing_notes(ui, &account, view.dirs, actions);
    new_folder_section(
        ui,
        &mut state.new_folder,
        state.remote_path.as_deref().unwrap_or_default(),
        view.mkdir_error,
        &account,
        &mut *actions,
    );
    note(ui, DRIVE_HINT);
}

fn computer_pane(ui: &mut Ui, state: &mut WizardState, actions: &mut Vec<Action>) {
    let response = widgets::text_field(ui, "Folder", &mut state.local, "/mnt/backup/syncfast");
    state.local_editing = response.has_focus();
    if response.changed() {
        ui.ctx().request_repaint();
    }
    if widgets::secondary_button(ui, "Choose\u{2026}").clicked() {
        emit(
            actions,
            BackupSheetAction::PickFolder(PickTarget::Destination),
        );
    }
}

#[must_use]
fn keep_summary(keep: Keep) -> String {
    let mut parts = vec![
        format!("{} daily", keep.daily),
        format!("{} weekly", keep.weekly),
        format!("{} monthly", keep.monthly),
    ];
    if keep.yearly > 0 {
        parts.push(format!("{} yearly", keep.yearly));
    }
    let last = parts.pop().unwrap_or_default();
    format!("Keep {} and {last} snapshots.", parts.join(", "))
}

fn schedule_step(ui: &mut Ui, state: &mut WizardState) {
    let shown = EVERY
        .iter()
        .find(|(_, every)| *every == state.every)
        .map_or("Every day", |(label, _)| label);
    widgets::setting_row(ui, "Back up", None, |ui| {
        let combo = egui::ComboBox::from_id_salt("backup-wizard-every")
            .selected_text(shown)
            .width(size::SETTING_COMBO)
            .show_ui(ui, |ui| {
                for (label, every) in EVERY {
                    if ui.selectable_label(state.every == every, label).clicked() {
                        state.every = every;
                    }
                }
            });
        combo.response.widget_info(|| {
            let mut info = egui::WidgetInfo::new(egui::WidgetType::ComboBox);
            info.label = Some("Back up".to_owned());
            info.current_text_value = Some(shown.to_owned());
            info
        });
    });
    ui.horizontal(|ui| {
        if let Ok(keep) = state.keep() {
            note(ui, &keep_summary(keep));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::secondary_button(ui, "Customize\u{2026}").clicked() {
                state.customize = !state.customize;
            }
        });
    });
    if state.customize {
        for (index, label) in ["Daily", "Weekly", "Monthly", "Yearly"]
            .into_iter()
            .enumerate()
        {
            widgets::setting_row(ui, label, None, |ui| {
                widgets::inline_field(ui, label, &mut state.keep[index], "0", size::SETTING_FIELD);
            });
        }
    }
    note(ui, RETENTION_NOTE);
    egui::CollapsingHeader::new(
        RichText::new("Advanced")
            .font(theme::headline())
            .color(theme::palette(ui.ctx()).text),
    )
    .id_salt("backup-wizard-advanced")
    .default_open(false)
    .show(ui, |ui| {
        widgets::switch(ui, &mut state.one_file_system, "Stay on one file system");
        if !state.one_file_system {
            note(ui, MOUNTS_NOTE);
        }
        widgets::setting_row(ui, "Upload limit", None, |ui| {
            widgets::inline_field(
                ui,
                "Upload limit",
                &mut state.limit_upload,
                "Unlimited",
                size::SETTING_FIELD,
            );
        });
        widgets::setting_row(ui, "Download limit", None, |ui| {
            widgets::inline_field(
                ui,
                "Download limit",
                &mut state.limit_download,
                "Unlimited",
                size::SETTING_FIELD,
            );
        });
    });
}

fn filters_step(
    ui: &mut Ui,
    view: &WizardView<'_>,
    state: &mut WizardState,
    actions: &mut Vec<Action>,
) {
    let mut draft = state.filters();
    let before = draft.clone();
    let tested = view.filter_test.filter(|test| {
        test.id == tab::NEW_SYNC
            && test.rules.presets == draft.presets
            && test.rules.custom == draft.custom_rules()
    });
    let test_clicked = tab::form_for(
        ui,
        "new-backup",
        &mut draft,
        tested,
        FilterTarget::Backup,
        |_| {},
    );
    if draft != before {
        state.presets.clone_from(&draft.presets);
        state.rules.clone_from(&draft.rules);
        ui.ctx().request_repaint();
    }
    if test_clicked {
        emit(actions, state.filter_request());
    }
}

fn name_step(ui: &mut Ui, view: &WizardView<'_>, state: &mut WizardState) {
    let palette = theme::palette(ui.ctx());
    let hint = state.default_name();
    if widgets::text_field(ui, "Name", &mut state.name, &hint).changed() {
        ui.ctx().request_repaint();
    }
    ui.label(
        RichText::new(format!("Backup id: {}", state.backup_id(view.backups)))
            .font(theme::footnote())
            .color(palette.secondary),
    );
}

fn key_step(
    ui: &mut Ui,
    view: &WizardView<'_>,
    state: &mut WizardState,
    actions: &mut Vec<Action>,
) {
    match state.flow {
        Flow::New => {
            if let Some(key) = &state.key {
                backup_keys::key_body(ui, key, view.key_file, actions);
            }
            if state
                .error
                .as_ref()
                .is_some_and(|error| error.step == Step::Key)
                && !state.busy
                && widgets::secondary_button(ui, "Try Again").clicked()
            {
                create(view, state, actions);
            }
        }
        Flow::Existing => {
            let shown = state.show_typed_key;
            let changed = if shown {
                widgets::text_field(ui, "Backup key", state.typed_key.buffer(), "").changed()
            } else {
                widgets::secret_field(ui, "Backup key", state.typed_key.buffer()).changed()
            };
            if changed {
                ui.ctx().request_repaint();
            }
            widgets::switch(ui, &mut state.show_typed_key, "Show");
        }
    }
}
