//! The Settings dialog: a modal over the main window with a sidebar of
//! panes, grouped. Close, a click outside and Esc dismiss it.

mod accounts;
mod appearance;

use egui::{RichText, Ui};

use super::widgets::{self, SidebarRow, sidebar_row};
use super::{Action, ViewModel, options};
use crate::gui_state::{SettingsGroup, SettingsPane};
use crate::icons::Icon;
use crate::theme::{self, ThemeChoice, space};

/// What the Theme selector offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppearanceView {
    pub choice: ThemeChoice,
    /// File names of the user's themes.
    pub themes: Vec<String>,
    /// Theme files that could not be used, one line each.
    pub errors: Vec<String>,
}

fn pane_icon(pane: SettingsPane) -> Icon {
    match pane {
        SettingsPane::Accounts => Icon::User,
        SettingsPane::Defaults => Icon::Settings,
        SettingsPane::Appearance => Icon::Palette,
    }
}

/// Draws the dialog over the window. `modal_above` is set while a sheet or
/// an alert covers it: the dialog then ignores Esc and clicks outside.
#[must_use]
pub fn dialog(ctx: &egui::Context, vm: &ViewModel, modal_above: bool) -> Vec<Action> {
    let mut navigation = Vec::new();
    let mut shown = Vec::new();
    let close = widgets::dialog(
        ctx,
        "settings",
        "Settings",
        modal_above,
        |ui| navigation = sidebar(ui, vm),
        |ui| shown = pane(ui, vm),
    );
    let mut actions = navigation;
    actions.extend(shown);
    if close {
        actions.push(Action::CloseSettings);
    }
    actions
}

fn sidebar(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    let mut first = true;
    for group in SettingsGroup::ALL {
        let panes = SettingsPane::ALL
            .into_iter()
            .filter(|pane| pane.group() == group);
        if panes.clone().next().is_none() {
            continue;
        }
        if !first {
            ui.add_space(space::M);
        }
        first = false;
        ui.horizontal(|ui| {
            ui.add_space(space::S);
            ui.label(
                RichText::new(group.title())
                    .font(theme::subheadline())
                    .color(palette.secondary),
            );
        });
        for pane in panes {
            let row = SidebarRow {
                icon: pane_icon(pane),
                title: pane.title(),
                subtitle: None,
                selected: vm.settings_pane == pane,
                status: None,
            };
            if sidebar_row(ui, &row).clicked() {
                actions.push(Action::SetSettingsPane(pane));
            }
        }
    }
    actions
}

fn pane(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    ui.label(
        RichText::new(vm.settings_pane.title())
            .font(theme::title3())
            .color(palette.text),
    );
    match vm.settings_pane {
        SettingsPane::Accounts => accounts::show(ui, vm),
        SettingsPane::Defaults => options::defaults(ui, vm),
        SettingsPane::Appearance => appearance::show(ui, vm),
    }
}
