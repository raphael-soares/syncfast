use egui::{Frame, Margin, RichText, Stroke, Ui};

use crate::theme::{self, radius};

use super::palette_of;

const PADDING_X: i8 = 8;
const PADDING_Y: i8 = 2;

/// A short outlined label for a fact about what it sits next to, such as the
/// mode of a sync. The inner label is its only accessible node.
pub fn chip(ui: &mut Ui, text: &str) -> egui::Response {
    let palette = palette_of(ui);
    Frame::new()
        .stroke(Stroke::new(1.0, palette.secondary))
        .corner_radius(radius::CARD)
        .inner_margin(Margin::symmetric(PADDING_X, PADDING_Y))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .font(theme::callout())
                    .color(palette.secondary),
            )
        })
        .response
}
