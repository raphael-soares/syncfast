use egui::{Frame, Margin, RichText, Stroke, Ui};

use crate::icons::Icon;
use crate::theme::{self, radius, size, space};

use super::palette_of;

/// A notice in line with the content: amber icon and text on a tinted card.
/// It warns without blocking; the user can read it and carry on.
pub fn warning_row(ui: &mut Ui, message: &str) -> egui::Response {
    let palette = palette_of(ui);
    Frame::new()
        .fill(palette.warning_fill())
        .stroke(Stroke::new(1.0, palette.warning_border()))
        .corner_radius(radius::CARD)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                ui.add(Icon::TriangleAlert.image(palette.warning, size::ICON));
                ui.label(
                    RichText::new(message)
                        .font(theme::callout())
                        .color(palette.text),
                );
            });
        })
        .response
}
