//! The Settings window: a separate window with a sidebar of panes. Its title
//! is the name of the pane it shows, and Esc closes it.

mod accounts;
mod appearance;

use egui::{Frame, Margin, RichText};

use super::widgets::{SidebarRow, sidebar_row};
use super::{Action, ViewModel};
use crate::gui_state::SettingsPane;
use crate::icons::Icon;
use crate::theme::{self, ThemeChoice, space};

const SIDEBAR_WIDTH: f32 = 180.0;
const DEFAULT_SIZE: [f32; 2] = [640.0, 420.0];
const MIN_SIZE: [f32; 2] = [520.0, 320.0];

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

/// Opens (or keeps open) the Settings window. Call it every frame while the
/// window should exist; it closes when the call stops.
#[must_use]
pub fn window(ctx: &egui::Context, vm: &ViewModel, app_id: &str) -> Vec<Action> {
    let mut actions = Vec::new();
    let builder = egui::ViewportBuilder::default()
        .with_title(vm.settings_pane.title())
        .with_app_id(app_id)
        .with_inner_size(DEFAULT_SIZE)
        .with_min_inner_size(MIN_SIZE);
    let id = egui::ViewportId::from_hash_of("syncfast-settings");
    ctx.show_viewport_immediate(id, builder, |ui, _class| {
        actions.extend(content(ui, vm));
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            actions.push(Action::CloseSettings);
        }
    });
    actions
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
