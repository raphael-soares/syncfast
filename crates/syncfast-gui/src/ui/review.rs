//! The Review sheet: what a first sync, a resync or a mass deletion would
//! change, shown before the user confirms it.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use egui::epaint::CornerRadius;
use egui::text::LayoutJob;
use egui::{Id, RichText, Sense, Ui, Vec2};
use syncfast_core::bisync::ReviewKind;
use syncfast_core::bisync_log::{Change, ChangeKind};
use syncfast_core::preview::Preview;
use syncfast_core::state::GateKind;

use super::Action;
use super::widgets::{
    PrimaryKind, SheetAction, SheetButtons, inline_error, progress_bar, secondary_button, sheet,
};
use crate::icons::Icon;
use crate::theme::{self, Palette, radius, size, space};

const SHEET_ID: &str = "review";
const MAX_LISTED: usize = 100;
const NOTE: &str =
    "Preview from a dry run. A few listed deletions may not happen in the real sync.";

/// Sections in the order they are listed.
const ORDER: [ChangeKind; 9] = [
    ChangeKind::Upload,
    ChangeKind::Download,
    ChangeKind::OverwriteLocal,
    ChangeKind::OverwriteRemote,
    ChangeKind::RenameLocal,
    ChangeKind::RenameRemote,
    ChangeKind::DeleteLocal,
    ChangeKind::DeleteRemote,
    ChangeKind::Other,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
    Cancel,
    /// `Try Again` after a failed preview.
    Retry,
    /// `Sync` or `Delete and Sync`.
    Confirm,
}

/// Where the preview is. The backend runs it; the sheet only shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewPhase {
    /// The dry run is going.
    Checking,
    /// A round of the sync is running; the preview is asked again every 5 s.
    Busy,
    Ready(Preview),
    /// The last error line of the dry run.
    Failed(String),
}

#[derive(Debug, Clone, Copy)]
pub struct ReviewView<'a> {
    /// The sync's name, for the title.
    pub name: &'a str,
    /// The account part of the sync's remote (`gdrive` of `gdrive:Notes`),
    /// for the section titles.
    pub remote: &'a str,
    /// How many days a file this round replaces on this computer is kept;
    /// `None` when the sync keeps no old versions.
    pub kept_days: Option<u32>,
    /// Why the review is asked for; picks the subtitle.
    pub gate: GateKind,
    pub phase: &'a ReviewPhase,
}

/// The preview a gate asks for: `Resync` for every `needs_review_*`,
/// `Force` for `too_many_deletes` and `all_files_changed`.
#[must_use]
pub fn review_kind(gate: GateKind) -> ReviewKind {
    if gate.is_force_review() {
        ReviewKind::Force
    } else {
        ReviewKind::Resync
    }
}

/// The line under the title: why this review is asked for.
#[must_use]
pub fn subtitle(gate: GateKind) -> &'static str {
    match gate {
        GateKind::NeedsReviewFirstRun => "First sync",
        GateKind::NeedsReviewFiltersChanged => "Filters changed",
        GateKind::NeedsReviewCriticalError => "Sync stopped after an error",
        GateKind::NeedsReviewModeChanged => "Sync mode changed",
        GateKind::TooManyDeletes => "Many files would be deleted",
        GateKind::AllFilesChanged => "Every file changed",
        _ => "Review changes",
    }
}

/// Draws the sheet and returns the `Action::Review(..)` the user chose.
#[must_use]
pub fn show(ctx: &egui::Context, view: &ReviewView<'_>) -> Vec<Action> {
    let title = format!("Review Changes for {}", view.name);
    let deleting = matches!(view.phase, ReviewPhase::Ready(preview) if has_deletes(preview));
    let mut buttons = SheetButtons::new(if deleting { "Delete and Sync" } else { "Sync" });
    if deleting {
        buttons.primary_kind = PrimaryKind::Destructive;
    }
    buttons.primary_enabled = matches!(view.phase, ReviewPhase::Ready(_));

    let mut actions = Vec::new();
    let chosen = sheet(ctx, SHEET_ID, &title, &buttons, |ui| {
        body(ui, view, &mut actions);
    });
    match chosen {
        Some(SheetAction::Cancel) => actions.push(Action::Review(ReviewAction::Cancel)),
        Some(SheetAction::Primary) => actions.push(Action::Review(ReviewAction::Confirm)),
        Some(SheetAction::Back) | None => {}
    }
    actions
}

fn has_deletes(preview: &Preview) -> bool {
    preview
        .summary
        .changes
        .iter()
        .any(|change| is_delete(change.kind))
}

fn is_delete(kind: ChangeKind) -> bool {
    matches!(kind, ChangeKind::DeleteLocal | ChangeKind::DeleteRemote)
}

fn body(ui: &mut Ui, view: &ReviewView<'_>, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    ui.label(
        RichText::new(subtitle(view.gate))
            .font(theme::callout())
            .color(palette.secondary),
    );
    let toggles_id = Id::new((SHEET_ID, view.name, "open-sections"));
    match view.phase {
        ReviewPhase::Checking => {
            forget_toggles(ui, toggles_id);
            progress_bar(ui, "Checking", None);
            text(ui, &palette, "Checking what would change…");
        }
        ReviewPhase::Busy => {
            forget_toggles(ui, toggles_id);
            progress_bar(ui, "Waiting for the running sync", None);
            text(
                ui,
                &palette,
                "This sync is running. The review opens when it finishes.",
            );
        }
        ReviewPhase::Failed(message) => {
            forget_toggles(ui, toggles_id);
            inline_error(ui, message);
            if secondary_button(ui, "Try Again").clicked() {
                actions.push(Action::Review(ReviewAction::Retry));
            }
        }
        ReviewPhase::Ready(preview) => {
            ready(ui, view.remote, view.kept_days, preview, toggles_id);
        }
    }
}

fn text(ui: &mut Ui, palette: &Palette, message: &str) {
    ui.label(
        RichText::new(message)
            .font(theme::body())
            .color(palette.secondary),
    );
}

/// Which sections the user toggled, by title. Kept in egui memory only while
/// the sheet shows a preview, so each opening starts from the defaults.
#[derive(Clone, Default)]
struct Toggles(HashMap<String, bool>);

fn forget_toggles(ui: &Ui, id: Id) {
    ui.data_mut(|data| data.remove::<Toggles>(id));
}

struct Section<'a> {
    title: String,
    rows: Vec<Cow<'a, str>>,
    open_by_default: bool,
}

fn row(change: &Change) -> Cow<'_, str> {
    match &change.from {
        Some(from) => Cow::Owned(format!("{from} → {}", change.path)),
        None => Cow::Borrowed(change.path.as_str()),
    }
}

fn sections<'a>(preview: &'a Preview, remote: &str, kept_days: Option<u32>) -> Vec<Section<'a>> {
    let coming_back: HashSet<(ChangeKind, &str)> = preview
        .returning
        .iter()
        .map(|change| (change.kind, change.path.as_str()))
        .collect();
    let back = (!preview.returning.is_empty()).then(|| Section {
        title: "Would come back".to_owned(),
        rows: preview.returning.iter().map(row).collect(),
        open_by_default: true,
    });
    let by_kind = ORDER.into_iter().filter_map(|kind| {
        let rows: Vec<Cow<'_, str>> = preview
            .summary
            .changes
            .iter()
            .filter(|change| change.kind == kind)
            .filter(|change| !coming_back.contains(&(change.kind, change.path.as_str())))
            .map(row)
            .collect();
        (!rows.is_empty()).then(|| Section {
            title: section_title(kind, remote, kept_days),
            rows,
            open_by_default: is_delete(kind),
        })
    });
    back.into_iter().chain(by_kind).collect()
}

fn section_title(kind: ChangeKind, remote: &str, kept_days: Option<u32>) -> String {
    match kind {
        ChangeKind::Upload => format!("Would upload to {remote}"),
        ChangeKind::Download => "Would download to this computer".to_owned(),
        ChangeKind::OverwriteLocal => format!(
            "Would replace on this computer ({})",
            match kept_days {
                None => "old version not kept".to_owned(),
                Some(1) => "old version kept for 1 day".to_owned(),
                Some(days) => format!("old version kept for {days} days"),
            }
        ),
        ChangeKind::OverwriteRemote => format!("Would replace on {remote}"),
        ChangeKind::RenameLocal => "Would rename on this computer".to_owned(),
        ChangeKind::RenameRemote => format!("Would rename on {remote}"),
        ChangeKind::DeleteLocal => "Would delete on this computer".to_owned(),
        ChangeKind::DeleteRemote => format!("Would delete on {remote}"),
        ChangeKind::Other => "Other changes".to_owned(),
    }
}

fn ready(ui: &mut Ui, remote: &str, kept_days: Option<u32>, preview: &Preview, toggles_id: Id) {
    let palette = theme::palette(ui.ctx());
    if preview.summary.changes.is_empty() {
        forget_toggles(ui, toggles_id);
        ui.label(
            RichText::new("Nothing to change.")
                .font(theme::body())
                .color(palette.text),
        );
        left_out_repos(ui, &palette, &preview.excluded_repos);
        return;
    }
    coming_back_notice(ui, &palette, preview.returning.len());
    let mut toggles = ui
        .data(|data| data.get_temp::<Toggles>(toggles_id))
        .unwrap_or_default();
    let before = toggles.0.clone();
    for (index, section) in sections(preview, remote, kept_days).iter().enumerate() {
        let default_open = index == 0 || section.open_by_default;
        let open = toggles
            .0
            .get(&section.title)
            .copied()
            .unwrap_or(default_open);
        if group(ui, &palette, section, open) {
            toggles.0.insert(section.title.clone(), !open);
        }
    }
    if toggles.0 != before {
        ui.data_mut(|data| data.insert_temp(toggles_id, toggles));
    }
    left_out_repos(ui, &palette, &preview.excluded_repos);
    ui.label(
        RichText::new(NOTE)
            .font(theme::footnote())
            .color(palette.dim),
    );
}

fn coming_back_notice(ui: &mut Ui, palette: &Palette, count: usize) {
    let message = match count {
        0 => return,
        1 => "1 file deleted or renamed since the last sync will come back.".to_owned(),
        n => format!("{n} files deleted or renamed since the last sync will come back."),
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        ui.add(Icon::CircleAlert.image(palette.warning, size::ICON));
        ui.label(
            RichText::new(message)
                .font(theme::callout())
                .color(palette.warning),
        );
    });
}

fn left_out_repos(ui: &mut Ui, palette: &Palette, repos: &[String]) {
    if repos.is_empty() {
        return;
    }
    ui.label(
        RichText::new(format!("Left out as Git repositories ({})", repos.len()))
            .font(theme::callout())
            .color(palette.secondary),
    );
    let rows: Vec<Cow<'_, str>> = repos
        .iter()
        .map(|repo| Cow::Borrowed(repo.as_str()))
        .collect();
    list(ui, palette, &rows);
}

/// One collapsible section. Returns `true` when its header was clicked.
fn group(ui: &mut Ui, palette: &Palette, section: &Section<'_>, open: bool) -> bool {
    let mut clicked = false;
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, palette.outline))
        .corner_radius(radius::CARD)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            clicked = header(ui, palette, section, open);
            if open != clicked {
                list(ui, palette, &section.rows);
            }
        });
    clicked
}

fn header(ui: &mut Ui, palette: &Palette, section: &Section<'_>, open: bool) -> bool {
    let count = section.rows.len().to_string();
    let label = format!("{} ({count})", section.title);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), size::SIDEBAR_ROW_HEIGHT),
        Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::CollapsingHeader,
            ui.is_enabled(),
            open,
            &label,
        )
    });
    let clicked = response.clicked();
    let open = open != clicked;
    if !ui.is_rect_visible(rect) {
        return clicked;
    }
    let card = radius::CARD;
    if response.hovered() {
        let corners = if open {
            CornerRadius {
                nw: card,
                ne: card,
                sw: 0,
                se: 0,
            }
        } else {
            CornerRadius::same(card)
        };
        ui.painter()
            .rect_filled(rect, corners, palette.surface_hover);
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            f32::from(card),
            egui::Stroke::new(2.0, palette.accent),
            egui::StrokeKind::Inside,
        );
    }
    let inner = rect.shrink2(Vec2::new(space::M, 0.0));
    let chevron = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    chevron.image(palette.dim, size::ICON).paint_at(
        ui,
        egui::Rect::from_center_size(
            egui::pos2(inner.left() + size::ICON / 2.0, rect.center().y),
            Vec2::splat(size::ICON),
        ),
    );

    let pill = ui
        .painter()
        .layout_no_wrap(count, theme::callout(), palette.secondary);
    let pill_size = Vec2::new(pill.size().x + space::M, pill.size().y + space::XS);
    let pill_rect = egui::Rect::from_min_size(
        egui::pos2(
            inner.right() - pill_size.x,
            rect.center().y - pill_size.y / 2.0,
        ),
        pill_size,
    );
    ui.painter()
        .rect_filled(pill_rect, pill_rect.height() / 2.0, palette.surface_active);
    ui.painter().galley(
        egui::pos2(
            pill_rect.center().x - pill.size().x / 2.0,
            pill_rect.center().y - pill.size().y / 2.0,
        ),
        pill,
        palette.secondary,
    );

    let title_x = inner.left() + size::ICON + space::S;
    let title_width = (pill_rect.left() - space::S - title_x).max(0.0);
    let mut job = LayoutJob::simple(
        section.title.clone(),
        theme::headline(),
        palette.text,
        title_width,
    );
    job.wrap.max_rows = 1;
    let title = ui.painter().layout_job(job);
    ui.painter().galley(
        egui::pos2(title_x, rect.center().y - title.size().y / 2.0),
        title,
        palette.text,
    );
    clicked
}

fn list(ui: &mut Ui, palette: &Palette, rows: &[Cow<'_, str>]) {
    let top = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        top,
        egui::Stroke::new(1.0, palette.outline),
    );
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 34,
            right: 12,
            top: 6,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            for path in rows.iter().take(MAX_LISTED) {
                ui.label(
                    RichText::new(path.as_ref())
                        .font(theme::monospace())
                        .color(palette.secondary),
                );
            }
            if rows.len() > MAX_LISTED {
                ui.label(
                    RichText::new(format!("and {} more", rows.len() - MAX_LISTED))
                        .font(theme::callout())
                        .color(palette.secondary),
                );
            }
        });
}
