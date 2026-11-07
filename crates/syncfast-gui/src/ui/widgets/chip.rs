use egui::{Frame, Margin, RichText, Ui};

use crate::theme::{self, radius};

use super::palette_of;

const PADDING_X: i8 = 8;
const PADDING_Y: i8 = 1;

/// A small filled pill for a fact about what it sits next to, such as the
/// mode of a sync. It is a tag, not a control: no outline, smaller text than
/// a button and shorter than one. The inner label is its only accessible
/// node.
pub fn chip(ui: &mut Ui, text: &str) -> egui::Response {
    let palette = palette_of(ui);
    Frame::new()
        .fill(palette.surface_active)
        .corner_radius(radius::PILL)
        .inner_margin(Margin::symmetric(PADDING_X, PADDING_Y))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .font(theme::subheadline())
                    .color(palette.secondary),
            )
        })
        .response
}
