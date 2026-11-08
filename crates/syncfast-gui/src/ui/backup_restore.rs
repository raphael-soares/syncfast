//! The sheets of a restore: the files of a snapshot to choose from, the
//! question before it starts, its progress and its end.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use egui::{CursorIcon, RichText, Sense, Ui};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use syncfast_core::backup::files::{LIST_LIMIT, Listing, NodeKind, NodeView};
use syncfast_core::backup::restore_job::RestoreReport;

use super::Action;
use super::backup_keys::wrap_labels;
use super::backup_wizard::BackupSheetAction;
use super::format::bytes;
use super::widgets::{
    self, AlertButton, ButtonKind, SheetAction, SheetButtons, alert, inline_error, list_row,
    progress_bar, sheet,
};
use crate::icons::Icon;
use crate::theme::{self, size, space};

const LIST_HEIGHT: f32 = 260.0;
const CHECK_WIDTH: f32 = 22.0;
const SIZE_WIDTH: f32 = 72.0;
const DATE_WIDTH: f32 = 116.0;
const ROW_HEIGHT: f32 = 36.0;

pub const READING: &str = "Reading the snapshot\u{2026}";
pub const NEW_FOLDER_NOTE: &str = "Files are restored into a new folder. Nothing is overwritten.";
pub const BUSY_SYNC: &str = "This sync is running. Try again when it finishes.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreAction {
    /// Shows a folder of the snapshot, reading it first when needed.
    Open(String),
    Toggle(String),
    /// `Restore To…` of the file list.
    RestoreTo,
    /// `Restore` of the confirmation.
    Confirm,
    TryAgain,
    OpenFolder,
    Done,
}

fn emit(actions: &mut Vec<Action>, action: RestoreAction) {
    actions.push(Action::BackupSheet(BackupSheetAction::Restore(action)));
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirState {
    Loading,
    Loaded(Listing),
    Failed(String),
}

/// Where a person is in the files of a snapshot and what they marked.
#[derive(Debug, Default)]
pub struct Browse {
    dir: String,
    dirs: HashMap<String, DirState>,
    selected: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub path: String,
}

fn is_inside(path: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    path.len() > folder.len() && path.starts_with(folder) && path[folder.len()..].starts_with('/')
}

impl Browse {
    /// Starts inside the folder a snapshot of one folder holds; a snapshot
    /// of several starts at the list of them.
    #[must_use]
    pub fn starting_in(roots: &[String]) -> Self {
        Self {
            dir: match roots {
                [only] => only.clone(),
                _ => String::new(),
            },
            ..Self::default()
        }
    }

    #[must_use]
    pub fn dir(&self) -> &str {
        &self.dir
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.selected.len()
    }

    #[must_use]
    pub fn selection(&self) -> Vec<String> {
        self.selected.iter().cloned().collect()
    }

    /// The row is marked, by itself or because a folder around it is.
    #[must_use]
    pub fn is_marked(&self, path: &str) -> bool {
        self.selected.contains(path) || self.is_inherited(path)
    }

    /// A folder around the row is marked: the row cannot be left out of it.
    #[must_use]
    pub fn is_inherited(&self, path: &str) -> bool {
        self.selected.iter().any(|folder| is_inside(path, folder))
    }

    /// Marking a folder takes in what is inside it, so the marks inside it
    /// go; a row inside a marked folder stays as it is.
    pub fn toggle(&mut self, path: &str) {
        if self.is_inherited(path) {
            return;
        }
        if !self.selected.remove(path) {
            self.selected.retain(|inside| !is_inside(inside, path));
            self.selected.insert(path.to_owned());
        }
    }

    /// Goes to `dir`; `true` when it has to be read.
    pub fn open(&mut self, dir: &str) -> bool {
        dir.clone_into(&mut self.dir);
        let missing = !matches!(
            self.dirs.get(dir),
            Some(DirState::Loading | DirState::Loaded(_))
        );
        if missing {
            self.dirs.insert(dir.to_owned(), DirState::Loading);
        }
        missing
    }

    pub fn loaded(&mut self, dir: &str, result: &Result<Listing, String>) {
        let state = match result {
            Ok(listing) => DirState::Loaded(listing.clone()),
            Err(text) => DirState::Failed(text.clone()),
        };
        self.dirs.insert(dir.to_owned(), state);
    }

    #[must_use]
    pub fn shown(&self) -> Option<&DirState> {
        self.dirs.get(&self.dir)
    }

    /// The way from the snapshot to the folder shown.
    #[must_use]
    pub fn crumbs(&self, roots: &[String]) -> Vec<Crumb> {
        let mut crumbs = vec![Crumb {
            label: "Snapshot".to_owned(),
            path: String::new(),
        }];
        if self.dir.is_empty() {
            return crumbs;
        }
        let root = roots
            .iter()
            .filter(|root| self.dir == **root || is_inside(&self.dir, root))
            .max_by_key(|root| root.len());
        let mut path = root.cloned().unwrap_or_default();
        if root.is_some() {
            crumbs.push(Crumb {
                label: path.clone(),
                path: path.clone(),
            });
        }
        let rest = &self.dir[path.trim_end_matches('/').len()..];
        for part in rest.split('/').filter(|part| !part.is_empty()) {
            path = format!("{}/{part}", path.trim_end_matches('/'));
            crumbs.push(Crumb {
                label: part.to_owned(),
                path: path.clone(),
            });
        }
        crumbs
    }
}

#[must_use]
pub fn date(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%b %-d, %Y %H:%M")
        .to_string()
}

fn modified(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

fn items(count: usize) -> String {
    if count == 1 {
        "1 item selected".to_owned()
    } else {
        format!("{count} items selected")
    }
}

fn crumb_row(ui: &mut Ui, browse: &Browse, roots: &[String], actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    let crumbs = browse.crumbs(roots);
    let last = crumbs.len() - 1;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        for (index, crumb) in crumbs.iter().enumerate() {
            if index == last {
                ui.label(
                    RichText::new(&crumb.label)
                        .font(theme::callout())
                        .color(palette.text),
                );
                continue;
            }
            let link = ui
                .add(
                    egui::Label::new(
                        RichText::new(&crumb.label)
                            .font(theme::callout())
                            .color(palette.accent),
                    )
                    .sense(Sense::click()),
                )
                .on_hover_cursor(CursorIcon::PointingHand);
            if link.clicked() {
                emit(actions, RestoreAction::Open(crumb.path.clone()));
            }
            ui.add(Icon::ChevronRight.image(palette.dim, size::ICON));
        }
    });
}

fn sorted(listing: &Listing) -> Vec<&NodeView> {
    let mut nodes: Vec<&NodeView> = listing.nodes.iter().collect();
    nodes.sort_by_cached_key(|node| (node.kind != NodeKind::Dir, node.name.to_lowercase()));
    nodes
}

fn node_row(ui: &mut Ui, browse: &Browse, node: &NodeView, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    let folder = node.kind == NodeKind::Dir;
    let mut marked = browse.is_marked(&node.path);
    let inherited = browse.is_inherited(&node.path);
    let mut opened = false;
    let row = list_row(ui, ROW_HEIGHT, |ui| {
        ui.add_enabled_ui(!inherited, |ui| {
            let outline = egui::Stroke::new(1.0, palette.dim);
            ui.visuals_mut().widgets.inactive.bg_stroke = outline;
            let response = ui.checkbox(&mut marked, "");
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    !inherited,
                    marked,
                    format!("Select {}", node.name),
                )
            });
            if response.changed() {
                emit(actions, RestoreAction::Toggle(node.path.clone()));
            }
        });
        let icon = if folder { Icon::Folder } else { Icon::File };
        ui.add(icon.image(palette.secondary, size::ICON));
        let name_width = (ui.available_width() - SIZE_WIDTH - DATE_WIDTH - space::M * 2.0)
            .max(CHECK_WIDTH * 2.0);
        ui.allocate_ui_with_layout(
            egui::vec2(name_width, ROW_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let name = ui
                    .add(
                        egui::Label::new(
                            RichText::new(&node.name)
                                .font(theme::body())
                                .color(palette.text),
                        )
                        .truncate()
                        .sense(if folder {
                            Sense::click()
                        } else {
                            Sense::hover()
                        }),
                    )
                    .on_hover_text(&node.name);
                opened = folder && name.clicked();
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(SIZE_WIDTH, ROW_HEIGHT),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if let (false, Some(total)) = (folder, node.size) {
                    ui.label(
                        RichText::new(bytes(total))
                            .font(theme::callout())
                            .color(palette.secondary),
                    );
                }
            },
        );
        if let Some(at) = node.mtime {
            ui.label(
                RichText::new(modified(at))
                    .font(theme::callout())
                    .color(palette.secondary),
            );
        }
    });
    if folder && (opened || row.clicked()) {
        emit(actions, RestoreAction::Open(node.path.clone()));
    }
}

fn listing_body(ui: &mut Ui, browse: &Browse, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    match browse.shown() {
        None | Some(DirState::Loading) => {
            progress_bar(ui, READING, None);
            ui.label(
                RichText::new(READING)
                    .font(theme::callout())
                    .color(palette.secondary),
            );
        }
        Some(DirState::Failed(text)) => {
            inline_error(ui, text);
            if widgets::secondary_button(ui, "Try Again").clicked() {
                emit(actions, RestoreAction::Open(browse.dir().to_owned()));
            }
        }
        Some(DirState::Loaded(listing)) if listing.nodes.is_empty() => {
            ui.label(
                RichText::new("This folder is empty.")
                    .font(theme::callout())
                    .color(palette.secondary),
            );
        }
        Some(DirState::Loaded(listing)) => {
            egui::ScrollArea::vertical()
                .max_height(LIST_HEIGHT)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for node in sorted(listing) {
                        node_row(ui, browse, node, actions);
                    }
                });
            if listing.truncated {
                ui.label(
                    RichText::new(format!(
                        "Only the first {LIST_LIMIT} items of this folder are shown."
                    ))
                    .font(theme::footnote())
                    .color(palette.secondary),
                );
            }
        }
    }
}

/// The files of one snapshot, to mark what to restore.
#[must_use]
pub fn browse(
    ctx: &egui::Context,
    taken: Timestamp,
    roots: &[String],
    state: &Browse,
) -> Vec<Action> {
    let palette = theme::palette(ctx);
    let mut buttons = SheetButtons::new("Restore To\u{2026}");
    buttons.primary_enabled = state.count() > 0;
    let mut actions = Vec::new();
    let title = format!("Files in {} snapshot", date(taken));
    let chosen = sheet(ctx, "restore-browse", &title, &buttons, |ui| {
        crumb_row(ui, state, roots, &mut actions);
        listing_body(ui, state, &mut actions);
        ui.label(
            RichText::new(items(state.count()))
                .font(theme::callout())
                .color(palette.secondary),
        );
    });
    match chosen {
        Some(SheetAction::Cancel) => actions.push(Action::BackupSheet(BackupSheetAction::Cancel)),
        Some(SheetAction::Primary) => emit(&mut actions, RestoreAction::RestoreTo),
        _ => {}
    }
    actions
}

fn folder_name(destination: &Path) -> String {
    destination
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn location(destination: &Path) -> String {
    destination
        .parent()
        .map_or_else(String::new, |parent| parent.display().to_string())
}

/// `Restore to <new folder>?`, before anything is written.
#[must_use]
pub fn confirm(ctx: &egui::Context, destination: &Path) -> Vec<Action> {
    let palette = theme::palette(ctx);
    let cancel = AlertButton {
        label: "Cancel",
        kind: ButtonKind::Secondary,
        is_default: false,
        is_cancel: true,
    };
    let restore = AlertButton {
        label: "Restore",
        kind: ButtonKind::Primary,
        is_default: true,
        is_cancel: false,
    };
    let chosen = alert(
        ctx,
        "restore-confirm",
        &format!("Restore to {}?", folder_name(destination)),
        NEW_FOLDER_NOTE,
        &[cancel, restore],
        |ui| {
            ui.label(
                RichText::new(format!("In {}", location(destination)))
                    .font(theme::footnote())
                    .color(palette.secondary),
            );
        },
    );
    match chosen {
        Some(0) => vec![Action::BackupSheet(BackupSheetAction::Cancel)],
        Some(1) => vec![Action::BackupSheet(BackupSheetAction::Restore(
            RestoreAction::Confirm,
        ))],
        _ => Vec::new(),
    }
}

#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // a share of a bar
pub fn fraction(report: &RestoreReport) -> Option<f32> {
    let (done, total) = if report.total_bytes > 0 {
        (report.bytes_restored, report.total_bytes)
    } else {
        (report.files_restored, report.total_files)
    };
    (total > 0).then(|| (done as f64 / total as f64).clamp(0.0, 1.0) as f32)
}

/// `<n> of <total> files · <bytes>`.
#[must_use]
pub fn restore_line(report: &RestoreReport) -> String {
    format!(
        "{} of {} files \u{b7} {}",
        report.files_restored,
        report.total_files,
        bytes(report.bytes_restored)
    )
}

/// The progress of the restore that runs; `Cancel` interrupts it.
#[must_use]
pub fn progress(
    ctx: &egui::Context,
    destination: &Path,
    report: Option<&RestoreReport>,
) -> Vec<Action> {
    let palette = theme::palette(ctx);
    let cancel = AlertButton {
        label: "Cancel",
        kind: ButtonKind::Secondary,
        is_default: false,
        is_cancel: true,
    };
    let chosen = alert(
        ctx,
        "restore-progress",
        &format!("Restoring to {}", folder_name(destination)),
        "",
        &[cancel],
        |ui| {
            progress_bar(ui, "Restore progress", report.and_then(fraction));
            let line = report
                .filter(|report| report.total_files > 0)
                .map_or_else(|| "Starting\u{2026}".to_owned(), restore_line);
            ui.label(
                RichText::new(line)
                    .font(theme::callout())
                    .color(palette.secondary),
            );
        },
    );
    match chosen {
        Some(0) => vec![Action::BackupSheet(BackupSheetAction::Cancel)],
        _ => Vec::new(),
    }
}

/// `Restored <n> files to <folder>.`
#[must_use]
pub fn done(ctx: &egui::Context, destination: &Path, report: &RestoreReport) -> Vec<Action> {
    let palette = theme::palette(ctx);
    let mut buttons = SheetButtons::new("Done");
    buttons.cancel = false;
    let mut actions = Vec::new();
    let chosen = sheet(ctx, "restore-done", "Restore Complete", &buttons, |ui| {
        wrap_labels(ui);
        let noun = if report.files_restored == 1 {
            "file"
        } else {
            "files"
        };
        ui.label(
            RichText::new(format!(
                "Restored {} {noun} to {}.",
                report.files_restored,
                destination.display()
            ))
            .font(theme::callout())
            .color(palette.text),
        );
        if widgets::button_with_icon(
            ui,
            ButtonKind::Secondary,
            Some(Icon::ExternalLink),
            "Open Folder",
        )
        .clicked()
        {
            emit(&mut actions, RestoreAction::OpenFolder);
        }
    });
    if matches!(chosen, Some(SheetAction::Primary | SheetAction::Cancel)) {
        emit(&mut actions, RestoreAction::Done);
    }
    actions
}

/// A restore that did not happen, or did not finish.
#[must_use]
pub fn failed(ctx: &egui::Context, text: &str) -> Vec<Action> {
    let buttons = SheetButtons::new("Try Again");
    let mut actions = Vec::new();
    let chosen = sheet(ctx, "restore-failed", "Restore Failed", &buttons, |ui| {
        wrap_labels(ui);
        inline_error(ui, text);
    });
    match chosen {
        Some(SheetAction::Cancel) => actions.push(Action::BackupSheet(BackupSheetAction::Cancel)),
        Some(SheetAction::Primary) => emit(&mut actions, RestoreAction::TryAgain),
        _ => {}
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_marked_folder_marks_what_is_inside_it() {
        let mut browse = Browse::default();

        browse.toggle("/home/ana/docs");

        assert!(browse.is_marked("/home/ana/docs/notes/a.txt"));
        assert!(browse.is_inherited("/home/ana/docs/notes"));
        assert!(!browse.is_inherited("/home/ana/docs"));
    }

    #[test]
    fn a_folder_does_not_mark_a_sibling_that_shares_its_prefix() {
        let mut browse = Browse::default();

        browse.toggle("/home/ana/docs");

        assert!(!browse.is_marked("/home/ana/docs-old"));
        assert!(!browse.is_marked("/home/ana/docs-old/a.txt"));
    }

    #[test]
    fn a_row_inside_a_marked_folder_cannot_be_left_out() {
        let mut browse = Browse::default();
        browse.toggle("/d");

        browse.toggle("/d/a.txt");

        assert_eq!(browse.selection(), ["/d"]);
    }

    #[test]
    fn marking_a_folder_replaces_the_marks_inside_it() {
        let mut browse = Browse::default();
        browse.toggle("/d/a.txt");
        browse.toggle("/d/sub/b.txt");
        browse.toggle("/other.txt");

        browse.toggle("/d");

        assert_eq!(browse.selection(), ["/d", "/other.txt"]);
        assert_eq!(browse.count(), 2);
    }

    #[test]
    fn unmarking_a_row_removes_it() {
        let mut browse = Browse::default();
        browse.toggle("/d/a.txt");

        browse.toggle("/d/a.txt");

        assert_eq!(browse.count(), 0);
    }

    #[test]
    fn a_snapshot_of_one_folder_starts_inside_it() {
        assert_eq!(
            Browse::starting_in(&roots(&["/home/ana/docs"])).dir(),
            "/home/ana/docs"
        );
        assert_eq!(Browse::starting_in(&roots(&["/a", "/b"])).dir(), "");
        assert_eq!(Browse::starting_in(&[]).dir(), "");
    }

    #[test]
    fn a_folder_is_read_once_unless_the_reading_failed() {
        let mut browse = Browse::default();

        assert!(browse.open("/d"));
        assert!(!browse.open("/d"));
        browse.loaded("/d", &Err("no".to_owned()));
        assert!(browse.open("/d"));
        browse.loaded(
            "/d",
            &Ok(Listing {
                nodes: Vec::new(),
                truncated: false,
            }),
        );
        assert!(!browse.open("/d"));
        assert!(!browse.open("/d"));
    }

    #[test]
    fn the_breadcrumb_goes_from_the_snapshot_through_its_folder() {
        let mut browse = Browse::default();
        browse.open("/home/ana/docs/notes/2024");

        let crumbs = browse.crumbs(&roots(&["/home/ana/docs", "/etc"]));

        let shown: Vec<(&str, &str)> = crumbs
            .iter()
            .map(|crumb| (crumb.label.as_str(), crumb.path.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("Snapshot", ""),
                ("/home/ana/docs", "/home/ana/docs"),
                ("notes", "/home/ana/docs/notes"),
                ("2024", "/home/ana/docs/notes/2024"),
            ]
        );
    }

    #[test]
    fn the_breadcrumb_of_the_list_of_folders_is_the_snapshot() {
        let browse = Browse::default();

        let crumbs = browse.crumbs(&roots(&["/a", "/b"]));

        assert_eq!(crumbs.len(), 1);
        assert_eq!(crumbs[0].path, "");
    }

    #[test]
    fn the_breadcrumb_of_a_folder_itself_ends_there() {
        let mut browse = Browse::default();
        browse.open("/a");

        let crumbs = browse.crumbs(&roots(&["/a", "/b"]));

        assert_eq!(crumbs.len(), 2);
        assert_eq!(crumbs[1].path, "/a");
    }

    #[test]
    fn the_count_says_item_for_one() {
        assert_eq!(items(1), "1 item selected");
        assert_eq!(items(0), "0 items selected");
        assert_eq!(items(3), "3 items selected");
    }
}
