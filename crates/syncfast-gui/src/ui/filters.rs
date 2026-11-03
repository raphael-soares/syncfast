//! The Filters tab.

use egui::Ui;

use super::widgets::empty_state;
use super::{Action, ViewModel};
use crate::icons::Icon;

pub fn show(ui: &mut Ui, _vm: &ViewModel) -> Vec<Action> {
    empty_state(
        ui,
        Icon::Filter,
        "Filters",
        Some("Choose what stays out of this sync."),
        None,
    );
    Vec::new()
}
