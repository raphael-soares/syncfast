//! The Add Sync sheet: a five step assistant (account, folder in the
//! account, folder on this computer, filters, name).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::text::{LayoutJob, TextFormat};
use egui::{Id, Rect, RichText, Sense, Ui, UiBuilder, Vec2};
use syncfast_core::config::{filter_line_problem, remotes_overlap, unique_id};
use syncfast_core::filters::{self, PRESETS};

use super::widgets::{
    self, ButtonKind, FolderNode, FolderTree, NodeState, SheetAction, SheetButtons, TreeAction,
};
use super::{Action, PairRow, RemoteRow};
use crate::icons::Icon;
use crate::theme::{self, radius, size, space};

/// A sync the assistant asks the backend to create.
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
    /// disable `Continue` by itself.
    pub local_error: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Step {
    #[default]
    Account,
    RemoteFolder,
    LocalFolder,
    Filters,
    Name,
}

/// What the sheet itself remembers while it is open. The app owns it so it
/// can feed answers of the backend back in (see the methods).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddSyncState {
    pub step: Step,
    /// The chosen account (rclone remote name).
    pub account: Option<String>,
    /// The chosen folder in the account, relative to its root: `None` is no
    /// choice yet, `Some("")` is `Entire <account>`.
    pub remote_path: Option<String>,
    /// Folders of the tree that are open, by relative path.
    pub expanded: HashSet<String>,
    /// The `New Folder…` field while it is open.
    pub new_folder: Option<String>,
    /// The path as typed.
    pub local: String,
    pub system_junk: bool,
    pub dev_dirs: bool,
    /// Custom rules, one per line.
    pub rules: String,
    pub name: String,
    /// The user typed in the name field: stop following the folder name.
    pub name_edited: bool,
}

impl Default for AddSyncState {
    fn default() -> Self {
        Self {
            step: Step::Account,
            account: None,
            remote_path: None,
            expanded: HashSet::new(),
            new_folder: None,
            local: String::new(),
            system_junk: true,
            dev_dirs: true,
            rules: String::new(),
            name: String::new(),
            name_edited: false,
        }
    }
}

impl AddSyncState {
    /// The portal's dialog returned `path`.
    pub fn picked_local(&mut self, path: &Path) {
        self.local = path.display().to_string();
    }

    /// `rclone mkdir` made `<account>:<parent>/<child>` (`parent` is `""`
    /// at the root): the new folder becomes the choice.
    pub fn remote_folder_created(&mut self, parent: &str, child: &str) {
        let path = if parent.is_empty() {
            child.to_owned()
        } else {
            format!("{parent}/{child}")
        };
        if !parent.is_empty() {
            self.expanded.insert(parent.to_owned());
        }
        self.new_folder = None;
        self.remote_path = Some(path);
    }
}

/// Draws the sheet. `Cancel`, `Back` and `Continue` move the assistant
/// through `state`; everything else comes back as actions for the app.
#[must_use]
pub fn show(ctx: &egui::Context, view: &AddSyncView<'_>, state: &mut AddSyncState) -> Vec<Action> {
    if state.step == Step::Name && !state.name_edited {
        let name = default_name(view, state);
        if state.name != name {
            state.name = name;
        }
    }
    let title = step_title(state);
    let mut buttons = SheetButtons::new(if state.step == Step::Name {
        "Create Sync"
    } else {
        "Continue"
    });
    buttons.primary_enabled = step_valid(view, state) && !view.creating;
    buttons.back = state.step != Step::Account;

    let mut actions = Vec::new();
    let chosen = widgets::sheet(ctx, "add-sync", &title, &buttons, |ui| {
        body(ui, view, state, &mut actions);
    });
    match chosen {
        Some(SheetAction::Cancel) => emit(&mut actions, AddSyncAction::Cancel),
        Some(SheetAction::Back) => {
            state.step = previous(state.step);
            ctx.request_repaint();
        }
        Some(SheetAction::Primary) if step_valid(view, state) && !view.creating => {
            if state.step == Step::Name {
                if let Some(new) = new_sync(view, state) {
                    emit(&mut actions, AddSyncAction::Create(new));
                }
            } else {
                state.step = next(state.step);
                ctx.request_repaint();
            }
        }
        Some(SheetAction::Primary) | None => {}
    }
    step_requests(ctx, view, state, &mut actions);
    actions
}

fn emit(actions: &mut Vec<Action>, action: AddSyncAction) {
    actions.push(Action::AddSyncSheet(action));
}

fn body(ui: &mut Ui, view: &AddSyncView<'_>, state: &mut AddSyncState, actions: &mut Vec<Action>) {
    step_indicator(ui, state);
    match state.step {
        Step::Account => account_step(ui, view, state, actions),
        Step::RemoteFolder => remote_step(ui, view, state, actions),
        Step::LocalFolder => local_step(ui, view, state, actions),
        Step::Filters => filters_step(ui, state),
        Step::Name => name_step(ui, view, state),
    }
}

fn next(step: Step) -> Step {
    match step {
        Step::Account => Step::RemoteFolder,
        Step::RemoteFolder => Step::LocalFolder,
        Step::LocalFolder => Step::Filters,
        Step::Filters | Step::Name => Step::Name,
    }
}

fn previous(step: Step) -> Step {
    match step {
        Step::Account | Step::RemoteFolder => Step::Account,
        Step::LocalFolder => Step::RemoteFolder,
        Step::Filters => Step::LocalFolder,
        Step::Name => Step::Filters,
    }
}

fn step_title(state: &AddSyncState) -> String {
    match state.step {
        Step::Account => "Account".to_owned(),
        Step::RemoteFolder => remote_step_title(state),
        Step::LocalFolder => "Folder on This Computer".to_owned(),
        Step::Filters => "Filters".to_owned(),
        Step::Name => "Name".to_owned(),
    }
}

fn remote_step_title(state: &AddSyncState) -> String {
    format!(
        "Folder in {}",
        state.account.as_deref().unwrap_or("Account")
    )
}

fn step_indicator(ui: &mut Ui, state: &AddSyncState) {
    let palette = theme::palette(ui.ctx());
    let current = match state.step {
        Step::Account => 0,
        Step::RemoteFolder => 1,
        Step::LocalFolder => 2,
        Step::Filters => 3,
        Step::Name => 4,
    };
    let remote = remote_step_title(state);
    let names = [
        "Account",
        remote.as_str(),
        "Folder on This Computer",
        "Filters",
        "Name",
    ];
    let mut job = LayoutJob::default();
    for (index, name) in names.into_iter().enumerate() {
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

fn step_valid(view: &AddSyncView<'_>, state: &AddSyncState) -> bool {
    match state.step {
        Step::Account => state.account.is_some(),
        Step::RemoteFolder => state.remote_path.is_some() && remote_problem(view, state).is_none(),
        Step::LocalFolder => matches!(local_check(view, state), LocalCheck::Ready),
        Step::Filters => rule_problems(&state.rules).is_empty(),
        Step::Name => {
            name_problem(&state.name).is_none()
                && state.account.is_some()
                && state.remote_path.is_some()
                && local_target(view, state).is_some()
        }
    }
}

/// Requests that depend on the step being on screen, not on a click: the
/// root listing of the account and the check of the typed folder.
fn step_requests(
    ctx: &egui::Context,
    view: &AddSyncView<'_>,
    state: &AddSyncState,
    actions: &mut Vec<Action>,
) {
    if state.step == Step::RemoteFolder
        && let Some(account) = &state.account
    {
        let root = remote_key(account, "");
        if !view.dirs.contains_key(&root) {
            emit(actions, AddSyncAction::LoadDirs(root));
        }
    }
    let seen = Id::new("syncfast-add-sync-checked-local");
    if state.step != Step::LocalFolder {
        ctx.data_mut(|data| data.remove::<String>(seen));
        return;
    }
    let checked = ctx.data(|data| data.get_temp::<String>(seen));
    if checked.as_deref() != Some(state.local.as_str()) {
        ctx.data_mut(|data| data.insert_temp(seen, state.local.clone()));
        if !state.local.trim().is_empty() {
            emit(actions, AddSyncAction::CheckLocal(state.local.clone()));
        }
    }
}

fn remote_key(account: &str, rel: &str) -> String {
    format!("{account}:{rel}")
}

fn join_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn account_step(
    ui: &mut Ui,
    view: &AddSyncView<'_>,
    state: &mut AddSyncState,
    actions: &mut Vec<Action>,
) {
    if view.remotes.is_empty() {
        let palette = theme::palette(ui.ctx());
        ui.label(
            RichText::new("No accounts yet.")
                .font(theme::body())
                .color(palette.secondary),
        );
    }
    for remote in view.remotes {
        let selected = state.account.as_deref() == Some(remote.name.as_str());
        if account_row(ui, remote, selected) && !selected {
            state.account = Some(remote.name.clone());
            state.remote_path = None;
            state.expanded.clear();
            state.new_folder = None;
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
}

fn account_row(ui: &mut Ui, remote: &RemoteRow, selected: bool) -> bool {
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
            &remote.name,
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
                .layout_no_wrap(remote.name.clone(), theme::headline(), palette.text);
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
                RichText::new("Uses rclone's shared client ID")
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

fn remote_step(
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
    let root_label = format!("Entire {account}");
    let tree_actions = widgets::folder_tree(
        ui,
        &FolderTree {
            root_label: &root_label,
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
                        emit(actions, AddSyncAction::LoadDirs(key));
                    }
                    state.expanded.insert(path);
                }
            }
        }
    }
    listing_notes(ui, &account, view.dirs);
    new_folder_section(ui, view, state, &account, actions);
    if let Some(problem) = remote_problem(view, state) {
        widgets::inline_error(ui, &problem);
    }
}

fn children(account: &str, dirs: &HashMap<String, DirListing>, parent: &str) -> NodeState {
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
fn listing_notes(ui: &mut Ui, account: &str, dirs: &HashMap<String, DirListing>) {
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
    for (_, message) in failed {
        widgets::inline_error(ui, message);
    }
}

fn new_folder_section(
    ui: &mut Ui,
    view: &AddSyncView<'_>,
    state: &mut AddSyncState,
    account: &str,
    actions: &mut Vec<Action>,
) {
    let focus = Id::new("syncfast-add-sync-focus-new-folder");
    ui.add_space(space::XS);
    let open = state.new_folder.is_some();
    if widgets::button_with_icon(
        ui,
        ButtonKind::Secondary,
        Some(Icon::FolderPlus),
        "New Folder\u{2026}",
    )
    .clicked()
    {
        state.new_folder = if open { None } else { Some(String::new()) };
        ui.ctx().data_mut(|data| data.insert_temp(focus, !open));
        ui.ctx().request_repaint();
    }
    let parent = state.remote_path.as_deref().unwrap_or_default();
    let Some(name) = &mut state.new_folder else {
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
    if let Some(error) = view.mkdir_error {
        widgets::inline_error(ui, error);
    }
}

fn folder_name_ok(name: &str) -> bool {
    !name.trim().is_empty() && !name.contains('/')
}

fn remote_problem(view: &AddSyncView<'_>, state: &AddSyncState) -> Option<String> {
    let chosen = remote_key(state.account.as_deref()?, state.remote_path.as_deref()?);
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
fn expand_local(home: &Path, text: &str) -> PathBuf {
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

fn local_step(
    ui: &mut Ui,
    view: &AddSyncView<'_>,
    state: &mut AddSyncState,
    actions: &mut Vec<Action>,
) {
    let response = widgets::text_field(ui, "Folder", &mut state.local, "/home/you/Documents");
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

fn preset_enabled(state: &AddSyncState, name: &str) -> bool {
    match name {
        "system_junk" => state.system_junk,
        "dev_dirs" => state.dev_dirs,
        _ => false,
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

fn filters_step(ui: &mut Ui, state: &mut AddSyncState) {
    preset_row(
        ui,
        "System junk files",
        "system_junk",
        &mut state.system_junk,
    );
    preset_row(ui, "Developer folders", "dev_dirs", &mut state.dev_dirs);
    ui.add_space(space::S);
    if widgets::text_area(ui, "Custom rules", &mut state.rules, 5, "- *.bak").changed() {
        ui.ctx().request_repaint();
    }
    for problem in rule_problems(&state.rules) {
        widgets::inline_error(ui, &problem);
    }
}

fn preset_row(ui: &mut Ui, label: &str, preset: &str, on: &mut bool) {
    let palette = theme::palette(ui.ctx());
    if widgets::switch(ui, on, label).changed() {
        ui.ctx().request_repaint();
    }
    let rules = filters::preset(preset).map_or(&[][..], |preset| preset.rules);
    egui::CollapsingHeader::new(
        RichText::new("Show rules")
            .font(theme::callout())
            .color(palette.secondary),
    )
    .id_salt(preset)
    .show(ui, |ui| {
        for rule in rules {
            ui.label(
                RichText::new(*rule)
                    .font(theme::monospace())
                    .color(palette.text),
            );
        }
    });
}

fn default_name(view: &AddSyncView<'_>, state: &AddSyncState) -> String {
    local_target(view, state)
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Sync".to_owned())
}

fn name_problem(name: &str) -> Option<&'static str> {
    (name.trim().is_empty() || name.chars().any(char::is_control))
        .then_some("sync name must be one line of text")
}

fn name_step(ui: &mut Ui, view: &AddSyncView<'_>, state: &mut AddSyncState) {
    let palette = theme::palette(ui.ctx());
    if widgets::text_field(ui, "Name", &mut state.name, "Documents").changed() {
        state.name_edited = true;
        ui.ctx().request_repaint();
    }
    if let Some(problem) = name_problem(&state.name) {
        widgets::inline_error(ui, problem);
    } else {
        let id = unique_id(
            state.name.trim(),
            view.existing.iter().map(|pair| pair.id.as_str()),
        );
        ui.label(
            RichText::new(format!("Sync id: {id}"))
                .font(theme::footnote())
                .color(palette.secondary),
        );
    }
    if let Some(error) = view.create_error {
        widgets::inline_error(ui, error);
    }
}

fn new_sync(view: &AddSyncView<'_>, state: &AddSyncState) -> Option<NewSync> {
    let name = state.name.trim();
    Some(NewSync {
        id: unique_id(name, view.existing.iter().map(|pair| pair.id.as_str())),
        name: name.to_owned(),
        local: local_target(view, state)?,
        remote: remote_key(state.account.as_deref()?, state.remote_path.as_deref()?),
        presets: PRESETS
            .iter()
            .filter(|preset| preset_enabled(state, preset.name))
            .map(|preset| preset.name.to_owned())
            .collect(),
        filters: rule_lines(&state.rules),
    })
}
