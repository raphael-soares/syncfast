//! The New Sync sheet: the folder on this computer, the folder on Google
//! Drive that follows it, and what to keep, in one sheet. Filters and the
//! name wait under Options.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::text::{LayoutJob, TextFormat};
use egui::{Id, Rect, RichText, Sense, Ui, UiBuilder, Vec2};
use syncfast_core::behavior::SyncMode;
use syncfast_core::config::{filter_line_problem, remotes_overlap, unique_id};
use syncfast_core::filters::{DEFAULT_SYNC_PRESETS, preset_names};

use super::filters::{self as tab, FilterDraft, FilterTest};
use super::widgets::{
    self, ButtonKind, FolderNode, FolderTree, NodeState, SheetAction, SheetButtons, TreeAction,
};
use super::{Action, PairRow, RemoteRow};
use crate::icons::Icon;
use crate::theme::{self, radius, size, space};

/// A sync the sheet asks the backend to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSync {
    pub id: String,
    pub name: String,
    pub local: PathBuf,
    /// `<account>:<folder>`, for example `gdrive:` or `gdrive:Notes`.
    pub remote: String,
    pub presets: Vec<String>,
    /// Custom filter lines, each starting with `+ ` or `- `.
    pub filters: Vec<String>,
    pub mode: SyncMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddSyncAction {
    Cancel,
    /// Folder names inside this remote path (`gdrive:Notes`; `gdrive:` is
    /// the root) are needed. Emitted once per path until it is in
    /// [`AddSyncView::dirs`].
    LoadDirs(String),
    /// `rclone mkdir` of this remote path (`gdrive:Notes/New`).
    MakeDir(String),
    /// Does this text, as typed, name a folder on this computer?
    CheckLocal(String),
    /// Create this folder on this computer (the `Create Folder` button).
    CreateLocal(String),
    /// The `Choose…` button: the backend opens the portal's folder dialog.
    PickFolder,
    Create(NewSync),
}

/// What the backend knows about the folders inside one remote path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirListing {
    Loading,
    /// Folder names, as rclone lists them.
    Loaded(Vec<String>),
    /// The last line rclone printed.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalStatus {
    Exists,
    Missing,
    NotAFolder,
}

/// The answer to an [`AddSyncAction::CheckLocal`]: valid only while `path`
/// is still the text in the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFacts {
    pub path: String,
    pub status: LocalStatus,
}

/// What comes from outside the sheet: the app's facts. Immutable.
#[derive(Debug, Clone, Copy)]
pub struct AddSyncView<'a> {
    pub remotes: &'a [RemoteRow],
    /// The syncs that exist, for overlap checks and unique ids.
    pub existing: &'a [PairRow],
    /// Keyed by remote path: `gdrive:` for the root, `gdrive:Notes` below.
    pub dirs: &'a HashMap<String, DirListing>,
    pub local: Option<&'a LocalFacts>,
    /// The last error of `rclone mkdir`.
    pub mkdir_error: Option<&'a str>,
    /// Why the backend could not create the sync.
    pub create_error: Option<&'a str>,
    /// `Create Sync` was clicked and the backend has not answered.
    pub creating: bool,
    /// For `~` in the path.
    pub home: &'a Path,
    /// `$XDG_DATA_HOME/syncfast`'s parent as `Paths::data_dir`: a sync
    /// folder may not contain it.
    pub data_dir: &'a Path,
    /// Why `Create Folder` failed; shown under the folder field. It does not
    /// disable `Create Sync` by itself.
    pub local_error: Option<&'a str>,
    /// The last `Test Filters`, whichever sync or wizard it ran for.
    pub filter_test: Option<&'a FilterTest>,
}

/// What the sheet offers to keep, in words instead of sync modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SimpleMode {
    /// Both sides the same: changes and deletions go both ways.
    #[default]
    BothSame,
    /// A copy on Drive: files go up, nothing here is changed.
    CopyToDrive,
}

impl SimpleMode {
    #[must_use]
    pub fn sync_mode(self) -> SyncMode {
        match self {
            Self::BothSame => SyncMode::Bisync,
            Self::CopyToDrive => SyncMode::Upload,
        }
    }
}

/// What the sheet itself remembers while it is open. The app owns it so it
/// can feed answers of the backend back in (see the methods).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddSyncState {
    /// The chosen account (rclone remote name).
    pub account: Option<String>,
    /// The folder on this computer, as typed.
    pub local: String,
    /// The folder in the account, relative to its root; empty is the whole
    /// account.
    pub remote_folder: String,
    /// The user typed or picked the Drive folder: stop following the local
    /// folder's name.
    pub remote_edited: bool,
    pub mode: SimpleMode,
    /// The Drive folder tree is open under the field.
    pub browsing: bool,
    /// Folders of the tree that are open, by relative path.
    pub expanded: HashSet<String>,
    /// The `New Folder…` field while it is open.
    pub new_folder: Option<String>,
    pub presets: Vec<String>,
    /// Custom rules, one per line.
    pub rules: String,
    pub name: String,
    /// The user typed in the name field: stop following the folder name.
    pub name_edited: bool,
    /// The text last sent to be checked; `None` sends the field again.
    pub checked_local: Option<String>,
}

impl Default for AddSyncState {
    fn default() -> Self {
        Self {
            account: None,
            local: String::new(),
            remote_folder: String::new(),
            remote_edited: false,
            mode: SimpleMode::default(),
            browsing: false,
            expanded: HashSet::new(),
            new_folder: None,
            presets: preset_names(&DEFAULT_SYNC_PRESETS),
            rules: String::new(),
            name: String::new(),
            name_edited: false,
            checked_local: None,
        }
    }
}

impl AddSyncState {
    /// The portal's dialog returned `path`, or a folder was dropped.
    pub fn picked_local(&mut self, path: &Path) {
        self.local = path.display().to_string();
    }

    /// `rclone mkdir` made `<account>:<parent>/<child>` (`parent` is `""`
    /// at the root): the new folder becomes the choice.
    pub fn remote_folder_created(&mut self, parent: &str, child: &str) {
        if !parent.is_empty() {
            self.expanded.insert(parent.to_owned());
        }
        self.new_folder = None;
        self.remote_folder = join_rel(parent, child);
        self.remote_edited = true;
    }
}

/// Draws the sheet. Everything the backend must do comes back as actions
/// for the app.
#[must_use]
pub fn show(ctx: &egui::Context, view: &AddSyncView<'_>, state: &mut AddSyncState) -> Vec<Action> {
    let dropped = ctx.input(|input| {
        input
            .raw
            .dropped_files
            .first()
            .map(|file| file.path().to_path_buf())
    });
    if let Some(path) = dropped {
        state.picked_local(&path);
    }
    if state.account.is_none()
        && let [only] = view.remotes
    {
        state.account = Some(only.name.clone());
    }
    follow_local_folder(view, state);
    let mut buttons = SheetButtons::new("Create Sync");
    buttons.primary_enabled = ready(view, state) && !view.creating;

    let mut actions = Vec::new();
    let chosen = widgets::sheet(ctx, "add-sync", "New Sync", &buttons, |ui| {
        body(ui, view, state, &mut actions);
    });
    match chosen {
        Some(SheetAction::Cancel) => emit(&mut actions, AddSyncAction::Cancel),
        Some(SheetAction::Primary) if ready(view, state) && !view.creating => {
            if let Some(new) = new_sync(view, state) {
                emit(&mut actions, AddSyncAction::Create(new));
            }
        }
        Some(SheetAction::Primary | SheetAction::Back) | None => {}
    }
    requests(view, state, &mut actions);
    actions
}

fn emit(actions: &mut Vec<Action>, action: AddSyncAction) {
    actions.push(Action::AddSyncSheet(action));
}

/// The Drive folder and the name copy the local folder's name until the
/// user types their own.
fn follow_local_folder(view: &AddSyncView<'_>, state: &mut AddSyncState) {
    let local_name = local_target(view, state).and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    if !state.remote_edited {
        let folder = local_name.clone().unwrap_or_default();
        if state.remote_folder != folder {
            state.remote_folder = folder;
        }
    }
    if !state.name_edited {
        let name = local_name.unwrap_or_else(|| "Sync".to_owned());
        if state.name != name {
            state.name = name;
        }
    }
}

fn ready(view: &AddSyncView<'_>, state: &AddSyncState) -> bool {
    matches!(local_check(view, state), LocalCheck::Ready)
        && state.account.is_some()
        && remote_problem(view, state).is_none()
        && rule_problems(&state.rules).is_empty()
        && name_problem(&state.name).is_none()
}

/// Requests that depend on what the sheet shows, not on a click: the root
/// listing of the account and the check of the typed folder.
fn requests(view: &AddSyncView<'_>, state: &mut AddSyncState, actions: &mut Vec<Action>) {
    if let Some(account) = &state.account {
        let root = remote_key(account, "");
        if !view.dirs.contains_key(&root) {
            emit(actions, AddSyncAction::LoadDirs(root));
        }
    }
    if state.checked_local.as_deref() != Some(state.local.as_str()) {
        state.checked_local = Some(state.local.clone());
        if !state.local.trim().is_empty() {
            emit(actions, AddSyncAction::CheckLocal(state.local.clone()));
        }
    }
}

fn body(ui: &mut Ui, view: &AddSyncView<'_>, state: &mut AddSyncState, actions: &mut Vec<Action>) {
    local_section(ui, view, state, actions);
    ui.add_space(space::S);
    drive_section(ui, view, state, actions);
    ui.add_space(space::S);
    keep_section(ui, state);
    ui.add_space(space::S);
    options_section(ui, view, state, actions);
    if let Some(error) = view.create_error {
        widgets::inline_error(ui, error);
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

fn heading(ui: &mut Ui, text: &str) {
    let palette = theme::palette(ui.ctx());
    ui.label(
        RichText::new(text)
            .font(theme::headline())
            .color(palette.text),
    );
}

/// `A › B › C` with the step at `current` in bold.
pub(super) fn steps_header(ui: &mut Ui, names: &[&str], current: usize) {
    let palette = theme::palette(ui.ctx());
    let mut job = LayoutJob::default();
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            job.append(
                " \u{203a} ",
                0.0,
                TextFormat::simple(theme::callout(), palette.dim),
            );
        }
        let (font, color) = if index == current {
            (theme::headline(), palette.text)
        } else {
            (theme::callout(), palette.secondary)
        };
        job.append(name, 0.0, TextFormat::simple(font, color));
    }
    ui.label(job);
}

pub(super) fn remote_key(account: &str, rel: &str) -> String {
    format!("{account}:{rel}")
}

pub(super) fn join_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

pub(super) fn account_row(ui: &mut Ui, remote: &RemoteRow, selected: bool) -> bool {
    let palette = theme::palette(ui.ctx());
    let warn = remote.shares_client_id();
    let height = if warn {
        size::LIST_ROW_HEIGHT
    } else {
        size::SIDEBAR_ROW_HEIGHT
    };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            remote.label(),
        )
    });
    let text_x = rect.left() + space::S + size::ICON + space::S;
    if ui.is_rect_visible(rect) {
        if selected || response.hovered() {
            let fill = if selected {
                palette.surface_active
            } else {
                palette.surface_hover
            };
            ui.painter()
                .rect_filled(rect, f32::from(radius::CONTROL), fill);
        }
        let top = if warn {
            rect.top() + space::S
        } else {
            rect.center().y - size::ICON / 2.0
        };
        let icon = Rect::from_min_size(
            egui::pos2(rect.left() + space::S, top),
            Vec2::splat(size::ICON),
        );
        Icon::User
            .image(palette.secondary, size::ICON)
            .paint_at(ui, icon);
        let galley =
            ui.painter()
                .layout_no_wrap(remote.label().to_owned(), theme::headline(), palette.text);
        let name_y = icon.center().y - galley.size().y / 2.0;
        ui.painter()
            .galley(egui::pos2(text_x, name_y), galley, palette.text);
    }
    if warn {
        let area = Rect::from_min_max(
            egui::pos2(text_x, rect.top() + space::S + size::ICON),
            rect.right_bottom(),
        );
        let mut child = ui.new_child(UiBuilder::new().max_rect(area));
        child.add(
            egui::Label::new(
                RichText::new(SHARED_CLIENT_ID)
                    .font(theme::callout())
                    .color(palette.warning),
            )
            .selectable(false),
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            f32::from(radius::CONTROL),
            egui::Stroke::new(2.0, palette.accent),
            egui::StrokeKind::Outside,
        );
    }
    response.clicked()
}

const SHARED_CLIENT_ID: &str = "Uses rclone's shared client ID";

fn local_section(
    ui: &mut Ui,
    view: &AddSyncView<'_>,
    state: &mut AddSyncState,
    actions: &mut Vec<Action>,
) {
    let response = widgets::text_field(
        ui,
        "Folder on this computer",
        &mut state.local,
        "/home/you/Documents",
    );
    if response.changed() {
        ui.ctx().request_repaint();
    }
    if widgets::secondary_button(ui, "Choose\u{2026}").clicked() {
        emit(actions, AddSyncAction::PickFolder);
    }
    match local_check(view, state) {
        LocalCheck::Empty | LocalCheck::Pending | LocalCheck::Ready => {}
        LocalCheck::Problem(message) => {
            widgets::inline_error(ui, &message);
        }
        LocalCheck::Missing(path) => {
            ui.horizontal(|ui| {
                widgets::inline_error(ui, "This folder does not exist.");
                if widgets::secondary_button(ui, "Create Folder").clicked() {
                    emit(
                        actions,
                        AddSyncAction::CreateLocal(path.to_string_lossy().into_owned()),
                    );
                }
            });
        }
    }
    if let Some(error) = view.local_error {
        widgets::inline_error(ui, error);
    }
}

fn drive_section(
    ui: &mut Ui,
    view: &AddSyncView<'_>,
    state: &mut AddSyncState,
    actions: &mut Vec<Action>,
) {
    if view.remotes.is_empty() {
        heading(ui, "Google Drive");
        note(ui, "No Google Drive account is connected yet.");
        if widgets::button_with_icon(
            ui,
            ButtonKind::Secondary,
            Some(Icon::Plus),
            "Connect Google Drive\u{2026}",
        )
        .clicked()
        {
            actions.push(Action::AddAccount);
        }
        return;
    }
    if view.remotes.len() > 1 {
        account_choice(ui, view, state);
    }
    if widgets::text_field(
        ui,
        "Folder on Google Drive",
        &mut state.remote_folder,
        "Entire Drive",
    )
    .changed()
    {
        state.remote_edited = true;
        ui.ctx().request_repaint();
    }
    let chosen = state
        .account
        .as_deref()
        .and_then(|account| view.remotes.iter().find(|remote| remote.name == account));
    if let Some(remote) = chosen {
        if view.remotes.len() == 1 {
            note(ui, &format!("In {}", remote.label()));
        }
        if remote.shares_client_id() {
            let palette = theme::palette(ui.ctx());
            ui.label(
                RichText::new(SHARED_CLIENT_ID)
                    .font(theme::callout())
                    .color(palette.warning),
            );
        }
    }
    if let Some(problem) = remote_problem(view, state) {
        widgets::inline_error(ui, &problem);
    }
    if widgets::secondary_button(ui, "Browse\u{2026}").clicked() {
        state.browsing = !state.browsing;
        ui.ctx().request_repaint();
    }
    if state.browsing {
        browse(ui, view, state, actions);
    }
}

fn account_choice(ui: &mut Ui, view: &AddSyncView<'_>, state: &mut AddSyncState) {
    heading(ui, "Google Drive account");
    let shown = state
        .account
        .as_deref()
        .and_then(|account| view.remotes.iter().find(|remote| remote.name == account))
        .map_or("Choose an account", RemoteRow::label)
        .to_owned();
    let combo = egui::ComboBox::from_id_salt("add-sync-account")
        .selected_text(&shown)
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for remote in view.remotes {
                let selected = state.account.as_deref() == Some(remote.name.as_str());
                if ui.selectable_label(selected, remote.label()).clicked() && !selected {
                    state.account = Some(remote.name.clone());
                    state.expanded.clear();
                    state.new_folder = None;
                }
            }
        });
    combo.response.widget_info(|| {
        let mut info = egui::WidgetInfo::new(egui::WidgetType::ComboBox);
        info.label = Some("Google Drive account".to_owned());
        info.current_text_value = Some(shown.clone());
        info
    });
}

fn browse(
    ui: &mut Ui,
    view: &AddSyncView<'_>,
    state: &mut AddSyncState,
    actions: &mut Vec<Action>,
) {
    let Some(account) = state.account.clone() else {
        return;
    };
    let nodes = match children(&account, view.dirs, "") {
        NodeState::Loaded(nodes) => nodes,
        NodeState::Unloaded | NodeState::Loading => Vec::new(),
    };
    let selected = remote_rel(state).to_owned();
    let tree_actions = widgets::folder_tree(
        ui,
        &FolderTree {
            root_label: "Entire Drive",
            nodes: &nodes,
            selected: Some(&selected),
            expanded: &state.expanded,
        },
    );
    for action in tree_actions {
        ui.ctx().request_repaint();
        match action {
            TreeAction::Select(path) => {
                state.remote_folder = path;
                state.remote_edited = true;
            }
            TreeAction::Toggle(path) => {
                if !state.expanded.remove(&path) {
                    let key = remote_key(&account, &path);
                    if !view.dirs.contains_key(&key) {
                        emit(actions, AddSyncAction::LoadDirs(key));
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
        &selected,
        view.mkdir_error,
        &account,
        actions,
    );
}

fn keep_section(ui: &mut Ui, state: &mut AddSyncState) {
    heading(ui, "Keep");
    let choices = [
        (
            SimpleMode::BothSame,
            "Both the same",
            "Changes and deletions go both ways.",
        ),
        (
            SimpleMode::CopyToDrive,
            "A copy on Google Drive",
            "Files go up to Drive. Nothing on this computer is changed or deleted.",
        ),
    ];
    for (mode, label, caption) in choices {
        if ui.radio(state.mode == mode, label).clicked() && state.mode != mode {
            state.mode = mode;
            ui.ctx().request_repaint();
        }
        ui.horizontal(|ui| {
            ui.add_space(space::XL);
            note(ui, caption);
        });
    }
}

fn options_section(
    ui: &mut Ui,
    view: &AddSyncView<'_>,
    state: &mut AddSyncState,
    actions: &mut Vec<Action>,
) {
    let palette = theme::palette(ui.ctx());
    egui::CollapsingHeader::new(
        RichText::new("Options")
            .font(theme::headline())
            .color(palette.text),
    )
    .id_salt("add-sync-options")
    .default_open(false)
    .show(ui, |ui| {
        if widgets::text_field(ui, "Name", &mut state.name, "Documents").changed() {
            state.name_edited = true;
            ui.ctx().request_repaint();
        }
        if let Some(problem) = name_problem(&state.name) {
            widgets::inline_error(ui, problem);
        }
        ui.add_space(space::S);
        filters_form(ui, view, state, actions);
    });
    if name_problem(&state.name).is_some() || !rule_problems(&state.rules).is_empty() {
        widgets::inline_error(ui, "Fix the name or the filters under Options.");
    }
}

pub(super) fn children(
    account: &str,
    dirs: &HashMap<String, DirListing>,
    parent: &str,
) -> NodeState {
    match dirs.get(&remote_key(account, parent)) {
        None => NodeState::Unloaded,
        Some(DirListing::Loading) => NodeState::Loading,
        Some(DirListing::Failed(_)) => NodeState::Loaded(Vec::new()),
        Some(DirListing::Loaded(names)) => {
            let mut names: Vec<&String> = names.iter().collect();
            names.sort_by_cached_key(|name| name.to_lowercase());
            NodeState::Loaded(
                names
                    .into_iter()
                    .map(|name| {
                        let path = join_rel(parent, name);
                        let children = children(account, dirs, &path);
                        FolderNode {
                            path,
                            name: name.clone(),
                            children,
                        }
                    })
                    .collect(),
            )
        }
    }
}

/// The root loading line and the listings that failed.
pub(super) fn listing_notes(
    ui: &mut Ui,
    account: &str,
    dirs: &HashMap<String, DirListing>,
    actions: &mut Vec<Action>,
) {
    let palette = theme::palette(ui.ctx());
    if matches!(
        dirs.get(&remote_key(account, "")),
        Some(DirListing::Loading)
    ) {
        ui.label(
            RichText::new("Loading folders\u{2026}")
                .font(theme::callout())
                .color(palette.dim),
        );
    }
    let mut failed: Vec<(&String, &String)> = dirs
        .iter()
        .filter(|(key, _)| {
            key.strip_prefix(account)
                .is_some_and(|rest| rest.starts_with(':'))
        })
        .filter_map(|(key, listing)| match listing {
            DirListing::Failed(message) => Some((key, message)),
            _ => None,
        })
        .collect();
    failed.sort();
    for (key, message) in failed {
        widgets::error_with_details(ui, message);
        if widgets::secondary_button(ui, "Try Again").clicked() {
            emit(actions, AddSyncAction::LoadDirs(key.clone()));
        }
    }
}

/// `parent` is the folder (relative to the account root) that is chosen:
/// the new folder is made inside it.
pub(super) fn new_folder_section(
    ui: &mut Ui,
    new_folder: &mut Option<String>,
    parent: &str,
    mkdir_error: Option<&str>,
    account: &str,
    actions: &mut Vec<Action>,
) {
    let focus = Id::new("syncfast-add-sync-focus-new-folder");
    ui.add_space(space::XS);
    let open = new_folder.is_some();
    if widgets::button_with_icon(
        ui,
        ButtonKind::Secondary,
        Some(Icon::FolderPlus),
        "New Folder\u{2026}",
    )
    .clicked()
    {
        *new_folder = if open { None } else { Some(String::new()) };
        ui.ctx().data_mut(|data| data.insert_temp(focus, !open));
        ui.ctx().request_repaint();
    }
    let Some(name) = new_folder else {
        return;
    };
    let response = widgets::text_field(ui, "Folder name", name, "");
    if ui
        .ctx()
        .data_mut(|data| data.remove_temp::<bool>(focus))
        .unwrap_or(false)
    {
        response.request_focus();
    }
    let valid = folder_name_ok(name);
    let enter = response.lost_focus()
        && ui
            .ctx()
            .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    let mut create = enter;
    ui.add_enabled_ui(valid, |ui| {
        create |= widgets::secondary_button(ui, "Create").clicked();
    });
    if create && valid {
        let path = join_rel(parent, name.trim());
        emit(actions, AddSyncAction::MakeDir(remote_key(account, &path)));
    }
    if let Some(error) = mkdir_error {
        widgets::inline_error(ui, error);
    }
}

fn folder_name_ok(name: &str) -> bool {
    !name.trim().is_empty() && !name.contains('/')
}

/// The Drive folder as rclone takes it: no surrounding blanks or trailing
/// `/`.
fn remote_rel(state: &AddSyncState) -> &str {
    state.remote_folder.trim().trim_end_matches('/')
}

fn remote_problem(view: &AddSyncView<'_>, state: &AddSyncState) -> Option<String> {
    let rel = remote_rel(state);
    if rel.starts_with('/') {
        return Some("Write the Drive folder without a leading \"/\".".to_owned());
    }
    let chosen = remote_key(state.account.as_deref()?, rel);
    view.existing
        .iter()
        .find(|pair| remotes_overlap(&chosen, &pair.remote))
        .map(|pair| format!("remote folder overlaps \"{}\"", pair.name))
}

enum LocalCheck {
    Empty,
    /// The app has not answered for this text yet.
    Pending,
    Problem(String),
    Missing(PathBuf),
    Ready,
}

/// `~` and `~/x` are the home folder; the result has no `.` or trailing
/// separator.
pub(super) fn expand_local(home: &Path, text: &str) -> PathBuf {
    let path = if text == "~" {
        home.to_path_buf()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home.join(rest.trim_start_matches('/'))
    } else {
        PathBuf::from(text)
    };
    path.components().collect()
}

/// The typed folder as an absolute path, when it is one.
fn local_target(view: &AddSyncView<'_>, state: &AddSyncState) -> Option<PathBuf> {
    if state.local.trim().is_empty() {
        return None;
    }
    Some(expand_local(view.home, &state.local)).filter(|path| path.is_absolute())
}

fn local_check(view: &AddSyncView<'_>, state: &AddSyncState) -> LocalCheck {
    if state.local.trim().is_empty() {
        return LocalCheck::Empty;
    }
    let Some(path) = local_target(view, state) else {
        return LocalCheck::Problem("local folder must be an absolute path".to_owned());
    };
    if view.data_dir.starts_with(&path) {
        return LocalCheck::Problem(
            "local folder cannot contain syncfast's versions folder".to_owned(),
        );
    }
    if let Some(other) = view
        .existing
        .iter()
        .find(|pair| path.starts_with(&pair.local) || pair.local.starts_with(&path))
    {
        return LocalCheck::Problem(format!("local folder overlaps \"{}\"", other.name));
    }
    match view
        .local
        .filter(|facts| facts.path == state.local)
        .map(|facts| facts.status)
    {
        None => LocalCheck::Pending,
        Some(LocalStatus::Missing) => LocalCheck::Missing(path),
        Some(LocalStatus::NotAFolder) => {
            LocalCheck::Problem("This path is not a folder.".to_owned())
        }
        Some(LocalStatus::Exists) => LocalCheck::Ready,
    }
}

/// One entry per non-blank line, numbered as the user sees the lines.
fn rule_problems(rules: &str) -> Vec<String> {
    rules
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(index, line)| filter_line_problem(index + 1, line))
        .collect()
}

fn rule_lines(rules: &str) -> Vec<String> {
    rules
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn filters_form(
    ui: &mut Ui,
    view: &AddSyncView<'_>,
    state: &mut AddSyncState,
    actions: &mut Vec<Action>,
) {
    let mut draft = FilterDraft {
        presets: state.presets.clone(),
        rules: state.rules.clone(),
    };
    let before = draft.clone();
    let tested = view.filter_test.filter(|test| {
        test.id == tab::NEW_SYNC
            && test.rules.presets == draft.presets
            && test.rules.custom == draft.custom_rules()
    });
    let test_clicked = tab::form(ui, "add-sync", &mut draft, tested, |_| {});
    if draft != before {
        state.presets.clone_from(&draft.presets);
        state.rules.clone_from(&draft.rules);
        ui.ctx().request_repaint();
    }
    if test_clicked && let Some(local) = local_target(view, state) {
        actions.push(Action::TestFilters {
            id: tab::NEW_SYNC.to_owned(),
            local,
            presets: draft.presets.clone(),
            rules: draft.custom_rules(),
        });
    }
}

fn name_problem(name: &str) -> Option<&'static str> {
    (name.trim().is_empty() || name.chars().any(char::is_control))
        .then_some("sync name must be one line of text")
}

fn new_sync(view: &AddSyncView<'_>, state: &AddSyncState) -> Option<NewSync> {
    let name = state.name.trim();
    Some(NewSync {
        id: unique_id(name, view.existing.iter().map(|pair| pair.id.as_str())),
        name: name.to_owned(),
        local: local_target(view, state)?,
        remote: remote_key(state.account.as_deref()?, remote_rel(state)),
        presets: state.presets.clone(),
        filters: rule_lines(&state.rules),
        mode: state.mode.sync_mode(),
    })
}
