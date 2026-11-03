//! The Activity tab: the rounds of the selected sync, newest first.

use egui::{Frame, Id, Margin, RichText, Ui, Vec2};
use syncfast_core::bisync_log::RunSummary;

use super::format::{duration, started};
use super::widgets::{BadgeSize, empty_state, list_row, status_badge};
use super::{Action, PairRow, ViewModel};
use crate::icons::Icon;
use crate::runs::RunEntry;
use crate::theme::{self, Palette, radius, size, space};

const WHEN_WIDTH: f32 = 140.0;
const DURATION_WIDTH: f32 = 60.0;
const RESULT_WIDTH: f32 = 130.0;
/// The expanded log scrolls past this height.
const LOG_MAX_HEIGHT: f32 = 220.0;

pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let (Some(pair), Some(runs)) = (vm.selected_pair(), vm.runs.as_ref()) else {
        return Vec::new();
    };
    if runs.is_empty() {
        empty_state(ui, Icon::History, "No activity yet", None, None);
        return Vec::new();
    }
    let palette = theme::palette(ui.ctx());
    for run in runs.iter() {
        run_row(ui, vm, pair, run, &palette);
    }
    Vec::new()
}

fn run_row(ui: &mut Ui, vm: &ViewModel, pair: &PairRow, run: &RunEntry, palette: &Palette) {
    let open_id = Id::new(("run-open", &pair.id, &run.id));
    let open = ui
        .data(|data| data.get_temp::<bool>(open_id))
        .unwrap_or(false);
    let response = list_row(ui, size::LIST_ROW_HEIGHT, |ui| {
        let chevron = if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        ui.add(chevron.image(palette.dim, size::ICON));
        cell(ui, WHEN_WIDTH, |ui| {
            let when = run
                .started
                .as_ref()
                .map_or_else(|| "Unknown".to_owned(), |at| started(vm.now, at));
            ui.label(RichText::new(when).font(theme::body()).color(palette.text));
        });
        cell(ui, DURATION_WIDTH, |ui| {
            let text = if run.running() {
                "running".to_owned()
            } else {
                run.seconds.map(duration).unwrap_or_default()
            };
            ui.label(
                RichText::new(text)
                    .font(theme::body())
                    .color(palette.secondary),
            );
        });
        cell(ui, RESULT_WIDTH, |ui| {
            status_badge(ui, run.status, BadgeSize::Regular);
        });
        let counters = counters(pair, run);
        ui.add(
            egui::Label::new(
                RichText::new(counters)
                    .font(theme::body())
                    .color(palette.secondary),
            )
            .truncate(),
        );
    });
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, open, "Show log")
    });
    if response.clicked() {
        ui.data_mut(|data| data.insert_temp(open_id, !open));
    }
    if open {
        log_view(ui, run, palette);
    }
}

fn cell(ui: &mut Ui, width: f32, content: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, size::CONTROL_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        content,
    );
}

/// `↑ <uploaded>  ↓ <downloaded>  ✕ <deletes>`, the reason of a failed
/// round, or how far the round in progress is.
fn counters(pair: &PairRow, run: &RunEntry) -> String {
    if run.running() {
        return match &pair.progress {
            Some(progress) if progress.total_transfers > 0 => format!(
                "{} of {} files",
                progress.transfers, progress.total_transfers
            ),
            _ => String::new(),
        };
    }
    if let Some(detail) = &run.detail {
        return detail.clone();
    }
    transfer_counts(&run.summary)
}

fn transfer_counts(summary: &RunSummary) -> String {
    if summary.uploaded.is_none() && summary.downloaded.is_none() {
        return String::new();
    }
    let deletes = summary.deleted_local.unwrap_or(0) + summary.deleted_remote.unwrap_or(0);
    format!(
        "↑ {}  ↓ {}  ✕ {deletes}",
        summary.uploaded.unwrap_or(0),
        summary.downloaded.unwrap_or(0),
    )
}

fn log_view(ui: &mut Ui, run: &RunEntry, palette: &Palette) {
    Frame::new()
        .fill(palette.surface)
        .corner_radius(radius::CARD)
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if run.messages.is_empty() {
                ui.label(
                    RichText::new("The log of this round is empty.")
                        .font(theme::callout())
                        .color(palette.secondary),
                );
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt(("run-log", &run.id))
                .max_height(LOG_MAX_HEIGHT)
                .show(ui, |ui| {
                    for message in &run.messages {
                        ui.label(
                            RichText::new(message)
                                .font(theme::monospace())
                                .color(palette.secondary),
                        );
                    }
                });
        });
    ui.add_space(space::S);
}
