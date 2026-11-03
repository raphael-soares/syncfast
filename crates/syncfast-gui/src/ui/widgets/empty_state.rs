use egui::{Align, Layout, RichText, Ui};

use crate::icons::Icon;
use crate::theme::{self, size, space};

use super::{palette_of, primary_button};

/// A centred placeholder: icon, title, optional explanation and optional
/// action. Returns whether the action was clicked.
pub fn empty_state(
    ui: &mut Ui,
    icon: Icon,
    title: &str,
    text: Option<&str>,
    action: Option<&str>,
) -> bool {
    let palette = palette_of(ui);
    let mut clicked = false;
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.add_space(space::XXL);
        ui.add(icon.image(palette.dim, size::ICON_LARGE));
        ui.add_space(space::S);
        ui.label(
            RichText::new(title)
                .font(theme::title3())
                .color(palette.text),
        );
        if let Some(text) = text {
            ui.label(
                RichText::new(text)
                    .font(theme::callout())
                    .color(palette.secondary),
            );
        }
        if let Some(action) = action {
            ui.add_space(space::S);
            clicked = primary_button(ui, action).clicked();
        }
        ui.add_space(space::XXL);
    });
    clicked
}
