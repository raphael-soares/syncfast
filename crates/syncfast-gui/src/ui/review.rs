//! The Review sheet: what a first sync, a resync or a mass deletion would
//! change, shown before the user confirms it.

use std::collections::HashMap;

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
const ORDER: [ChangeKind; 7] = [
    ChangeKind::Upload,
    ChangeKind::Download,
    ChangeKind::OverwriteLocal,
    ChangeKind::OverwriteRemote,
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
        ReviewPhase::Ready(preview) => ready(ui, view.remote, preview, toggles_id),
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
    kind: ChangeKind,
    title: String,
    paths: Vec<&'a str>,
}

fn sections<'a>(changes: &'a [Change], remote: &str) -> Vec<Section<'a>> {
    ORDER
        .into_iter()
        .filter_map(|kind| {
            let paths: Vec<&str> = changes
                .iter()
                .filter(|change| change.kind == kind)
                .map(|change| change.path.as_str())
                .collect();
            (!paths.is_empty()).then(|| Section {
                kind,
                title: section_title(kind, remote),
                paths,
            })
        })
        .collect()
}

fn section_title(kind: ChangeKind, remote: &str) -> String {
    match kind {
        ChangeKind::Upload => format!("Would upload to {remote}"),
        ChangeKind::Download => "Would download to this computer".to_owned(),
        ChangeKind::OverwriteLocal => {
            "Would replace on this computer (old version kept for 30 days)".to_owned()
        }
        ChangeKind::OverwriteRemote => format!("Would replace on {remote}"),
        ChangeKind::DeleteLocal => "Would delete on this computer".to_owned(),
        ChangeKind::DeleteRemote => format!("Would delete on {remote}"),
        ChangeKind::Other => "Other changes".to_owned(),
    }
}

fn ready(ui: &mut Ui, remote: &str, preview: &Preview, toggles_id: Id) {
    let palette = theme::palette(ui.ctx());
    let changes = &preview.summary.changes;
    if changes.is_empty() {
        forget_toggles(ui, toggles_id);
        ui.label(
            RichText::new("Nothing to change.")
                .font(theme::body())
                .color(palette.text),
        );
        left_out_repos(ui, &palette, &preview.excluded_repos);
        return;
    }
    let mut toggles = ui
        .data(|data| data.get_temp::<Toggles>(toggles_id))
        .unwrap_or_default();
    let before = toggles.0.clone();
    for (index, section) in sections(changes, remote).iter().enumerate() {
        let default_open = index == 0 || is_delete(section.kind);
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

fn left_out_repos(ui: &mut Ui, palette: &Palette, repos: &[String]) {
    if repos.is_empty() {
        return;
    }
    ui.label(
        RichText::new(format!("Left out as Git repositories ({})", repos.len()))
            .font(theme::callout())
            .color(palette.secondary),
    );
    let paths: Vec<&str> = repos.iter().map(String::as_str).collect();
    list(ui, palette, &paths);
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
                list(ui, palette, &section.paths);
            }
        });
    clicked
}

fn header(ui: &mut Ui, palette: &Palette, section: &Section<'_>, open: bool) -> bool {
    let count = section.paths.len().to_string();
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

fn list(ui: &mut Ui, palette: &Palette, paths: &[&str]) {
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
            for path in paths.iter().take(MAX_LISTED) {
                ui.label(
                    RichText::new(*path)
                        .font(theme::monospace())
                        .color(palette.secondary),
                );
            }
            if paths.len() > MAX_LISTED {
                ui.label(
                    RichText::new(format!("and {} more", paths.len() - MAX_LISTED))
                        .font(theme::callout())
                        .color(palette.secondary),
                );
            }
        });
}
