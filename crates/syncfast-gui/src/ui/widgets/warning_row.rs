use egui::{Frame, Margin, RichText, Stroke, Ui};

use crate::icons::Icon;
use crate::theme::{self, radius, size, space};

use super::palette_of;

const FILL: f32 = 0.08;
const BORDER: f32 = 0.25;

/// A notice in line with the content: amber icon and text on a tinted card.
/// It warns without blocking; the user can read it and carry on.
pub fn warning_row(ui: &mut Ui, message: &str) -> egui::Response {
    let palette = palette_of(ui);
    Frame::new()
        .fill(palette.warning.gamma_multiply(FILL))
        .stroke(Stroke::new(1.0, palette.warning.gamma_multiply(BORDER)))
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
