//! The list of syncs on the left.

use egui::{Frame, Margin, RichText};
use syncfast_core::behavior::SyncMode;

use super::menu;
use super::relative::synced;
use super::widgets::{SidebarRow, icon_button, sidebar_row};
use super::{Action, ViewModel, options};
use crate::gui_state::{SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH};
use crate::icons::Icon;
use crate::theme::{self, space};

/// Width changes smaller than this are layout rounding, not a drag.
const RESIZE_SLACK: f32 = 1.0;

pub fn show(ui: &mut egui::Ui, vm: &ViewModel) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    let panel = egui::Panel::left("sidebar")
        .resizable(true)
        .default_size(vm.sidebar_width)
        .size_range(SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(palette.panel)
                .inner_margin(Margin::same(12)),
        );
    let response = panel.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(super::TITLE)
                    .font(theme::title3())
                    .color(palette.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Plus, "Add Sync…").clicked() {
                    actions.push(Action::AddSync);
                }
                if icon_button(ui, Icon::Settings, "Settings…").clicked() {
                    actions.push(Action::OpenSettings);
                }
                actions.extend(menu::app_menu(ui, vm.app_menu()));
            });
        });
        ui.add_space(space::S);
        egui::ScrollArea::vertical().show(ui, |ui| {
            for pair in &vm.pairs {
                let synced = synced(vm.now, pair.last_success);
                let mode = pair.effective.behavior.mode;
                let subtitle = if mode == SyncMode::Bisync {
                    synced
                } else {
                    format!("{} \u{b7} {synced}", options::mode_label(mode.name()))
                };
                let row = SidebarRow {
                    icon: Icon::Folder,
                    title: &pair.name,
                    subtitle: Some(&subtitle),
                    selected: vm.selected.as_deref() == Some(pair.id.as_str()),
                    status: Some(pair.status),
                };
                if sidebar_row(ui, &row).clicked() {
                    actions.push(Action::SelectPair(pair.id.clone()));
                }
            }
        });
    });
    let width = response.response.rect.width();
    if (width - vm.sidebar_width).abs() > RESIZE_SLACK {
        actions.push(Action::ResizeSidebar(width));
    }
    actions
}
