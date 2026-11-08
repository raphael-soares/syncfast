//! The syncs and the backups on the left.

use egui::{Frame, Margin, RichText};
use syncfast_core::behavior::SyncMode;

use super::menu;
use super::relative::{backed_up, synced};
use super::widgets::{SidebarRow, icon_button, secondary_button, sidebar_row};
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
        section_header(ui, super::TITLE, |ui| {
            if icon_button(ui, Icon::Plus, "Add Sync…").clicked() {
                actions.push(Action::AddSync);
            }
            if icon_button(ui, Icon::Settings, "Settings…").clicked() {
                actions.push(Action::OpenSettings);
            }
            actions.extend(menu::app_menu(ui, vm.app_menu()));
        });
        ui.add_space(space::S);
        egui::ScrollArea::vertical().show(ui, |ui| {
            if vm.pairs.is_empty()
                && !vm.backups.is_empty()
                && section_empty(ui, "No syncs yet", "Add Sync…")
            {
                actions.push(Action::AddSync);
            }
            for pair in &vm.pairs {
                let synced = synced(vm.now, pair.last_success);
                let mode = pair.effective.behavior.mode;
                let subtitle = if mode == SyncMode::Bisync {
                    synced
                } else {
                    format!("{synced} \u{b7} {}", options::mode_label(mode.name()))
                };
                let row = SidebarRow {
                    icon: Icon::Folder,
                    title: &pair.name,
                    subtitle: Some(&subtitle),
                    selected: vm.selected_backup.is_none()
                        && vm.selected.as_deref() == Some(pair.id.as_str()),
                    status: Some(pair.status.into()),
                };
                if sidebar_row(ui, &row).clicked() {
                    actions.push(Action::SelectPair(pair.id.clone()));
                }
            }
            backups(ui, vm, &mut actions);
        });
    });
    let width = response.response.rect.width();
    if (width - vm.sidebar_width).abs() > RESIZE_SLACK {
        actions.push(Action::ResizeSidebar(width));
    }
    actions
}

fn section_header(ui: &mut egui::Ui, title: &str, trailing: impl FnOnce(&mut egui::Ui)) {
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .font(theme::title3())
                .color(palette.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
    });
}

fn section_empty(ui: &mut egui::Ui, text: &str, button: &str) -> bool {
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.add_space(space::S);
        ui.label(
            RichText::new(text)
                .font(theme::callout())
                .color(palette.dim),
        );
    });
    ui.add_space(space::S);
    ui.horizontal(|ui| {
        ui.add_space(space::S);
        secondary_button(ui, button).clicked()
    })
    .inner
}

fn backups(ui: &mut egui::Ui, vm: &ViewModel, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    ui.add_space(space::M);
    section_header(ui, "Backups", |ui| {
        if icon_button(ui, Icon::Plus, "New Backup…").clicked() {
            actions.push(Action::NewBackup);
        }
        let more = icon_button(ui, Icon::Ellipsis, "Backup Actions");
        menu::popup(&more, &palette, |ui| {
            if menu::item(
                ui,
                &palette,
                Icon::Archive,
                "Add Existing Backup…",
                menu::Tone::Normal,
            ) {
                actions.push(Action::AddExistingBackup);
            }
        });
    });
    ui.add_space(space::S);
    if vm.backups.is_empty() {
        if section_empty(ui, "No backups yet", "New Backup…") {
            actions.push(Action::NewBackup);
        }
        return;
    }
    for backup in &vm.backups {
        let subtitle = backed_up(vm.now, backup.state.last_success);
        let row = SidebarRow {
            icon: Icon::Archive,
            title: &backup.config.name,
            subtitle: Some(&subtitle),
            selected: vm.selected_backup.as_deref() == Some(backup.config.id.as_str()),
            status: Some(backup.status.into()),
        };
        if sidebar_row(ui, &row).clicked() {
            actions.push(Action::SelectBackup(backup.config.id.clone()));
        }
    }
}
