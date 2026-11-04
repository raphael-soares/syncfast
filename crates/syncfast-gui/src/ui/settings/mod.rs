//! The Settings screen: it replaces the main window's content with a sidebar
//! of panes. A back button and Esc return to the syncs.

mod accounts;
mod appearance;

use egui::{Frame, Margin, RichText};

use super::widgets::{SidebarRow, icon_button, sidebar_row};
use super::{Action, ViewModel};
use crate::gui_state::SettingsPane;
use crate::icons::Icon;
use crate::theme::{self, ThemeChoice, space};

/// The tooltip and accessible name of the button that leaves Settings.
const BACK: &str = "Back";

const SIDEBAR_WIDTH: f32 = 180.0;

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
        SettingsPane::Appearance => Icon::Palette,
    }
}

/// The panes and the one that is selected.
#[must_use]
pub fn content(ui: &mut egui::Ui, vm: &ViewModel) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    // An open selector takes the first Esc to close itself.
    let popup_open = egui::Popup::is_any_open(ui.ctx());
    egui::Panel::left("settings-sidebar")
        .resizable(false)
        .exact_size(SIDEBAR_WIDTH)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(palette.panel)
                .inner_margin(Margin::same(12)),
        )
        .show(ui, |ui| {
            if icon_button(ui, Icon::ChevronLeft, BACK).clicked() {
                actions.push(Action::CloseSettings);
            }
            ui.add_space(space::S);
            for pane in SettingsPane::ALL {
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
        });
    egui::CentralPanel::default()
        .frame(
            Frame::new()
                .fill(palette.window)
                .inner_margin(Margin::same(20)),
        )
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = space::M;
            ui.label(
                RichText::new(vm.settings_pane.title())
                    .font(theme::title3())
                    .color(palette.text),
            );
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match vm.settings_pane {
                    SettingsPane::Accounts => actions.extend(accounts::show(ui, vm)),
                    SettingsPane::Appearance => actions.extend(appearance::show(ui, vm)),
                });
        });
    let escape = ui
        .ctx()
        .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if escape && !popup_open {
        actions.push(Action::CloseSettings);
    }
    actions
}
