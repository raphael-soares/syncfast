//! The Advanced pane of Settings: the rounds of one sync, picked here.

use egui::Ui;

use crate::icons::Icon;
use crate::theme::size;
use crate::ui::widgets::{empty_state, setting_row};
use crate::ui::{Action, ViewModel, activity};

#[must_use]
pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    if vm.pairs.is_empty() {
        empty_state(ui, Icon::History, "No syncs yet", None, None);
        return Vec::new();
    }
    let mut actions = Vec::new();
    let shown = vm.selected_pair().map_or("", |pair| pair.name.as_str());
    setting_row(ui, "Sync", None, |ui| {
        let combo = egui::ComboBox::from_id_salt("advanced-sync")
            .selected_text(shown)
            .width(size::SETTING_COMBO)
            .show_ui(ui, |ui| {
                for pair in &vm.pairs {
                    let selected = vm.selected_pair().is_some_and(|open| open.id == pair.id);
                    if ui.selectable_label(selected, &pair.name).clicked() && !selected {
                        actions.push(Action::SelectPair(pair.id.clone()));
                    }
                }
            });
        combo.response.widget_info(|| {
            let mut info = egui::WidgetInfo::new(egui::WidgetType::ComboBox);
            info.label = Some("Sync".to_owned());
            info.current_text_value = Some(shown.to_owned());
            info
        });
    });
    actions.extend(activity::show(ui, vm));
    actions
}
