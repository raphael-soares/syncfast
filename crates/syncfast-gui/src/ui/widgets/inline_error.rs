use egui::{RichText, Ui};

use crate::icons::Icon;
use crate::theme::{self, size};

use super::palette_of;

/// An error shown right below the field it concerns.
pub fn inline_error(ui: &mut Ui, message: &str) -> egui::Response {
    let palette = palette_of(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add(Icon::CircleAlert.image(palette.danger, size::ICON));
        ui.label(
            RichText::new(message)
                .font(theme::callout())
                .color(palette.danger),
        );
    })
    .response
}
