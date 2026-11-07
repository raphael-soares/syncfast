//! Settings > Appearance: which palette the window uses.

use egui::Ui;

use crate::theme::{ThemeChoice, display_name};
use crate::ui::widgets::{inline_error, setting_row};
use crate::ui::{Action, ViewModel};

fn label(choice: &ThemeChoice) -> String {
    match choice {
        ThemeChoice::System => "System".to_owned(),
        ThemeChoice::Light => "Light".to_owned(),
        ThemeChoice::Dark => "Dark".to_owned(),
        ThemeChoice::Named(file) => display_name(file).to_owned(),
    }
}

/// `System`, `Light`, `Dark`, then the user's themes.
fn choices(vm: &ViewModel) -> Vec<ThemeChoice> {
    let mut choices = vec![ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];
    choices.extend(vm.appearance.themes.iter().cloned().map(ThemeChoice::Named));
    choices
}

pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let mut actions = Vec::new();
    setting_row(ui, "Theme", None, |ui| {
        egui::ComboBox::from_id_salt("settings-theme")
            .selected_text(label(&vm.appearance.choice))
            .show_ui(ui, |ui| {
                for choice in choices(vm) {
                    let selected = choice == vm.appearance.choice;
                    if ui.selectable_label(selected, label(&choice)).clicked() && !selected {
                        actions.push(Action::SetTheme(choice));
                    }
                }
            });
    });
    for error in &vm.appearance.errors {
        inline_error(ui, error);
    }
    actions
}
