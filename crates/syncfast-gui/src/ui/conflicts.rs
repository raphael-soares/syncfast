//! The Conflicts tab.

use egui::Ui;

use super::widgets::empty_state;
use super::{Action, ViewModel};
use crate::icons::Icon;

pub fn show(ui: &mut Ui, _vm: &ViewModel) -> Vec<Action> {
    empty_state(ui, Icon::Copy, "No conflicts", None, None);
    Vec::new()
}
