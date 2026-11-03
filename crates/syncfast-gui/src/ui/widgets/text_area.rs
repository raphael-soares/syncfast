use egui::{RichText, TextEdit, Ui};

use crate::theme::{self, radius, space};

use super::palette_of;

/// A multi-line monospace field with its label above it, `rows` lines tall.
pub fn text_area(
    ui: &mut Ui,
    label: &str,
    value: &mut String,
    rows: usize,
    hint: &str,
) -> egui::Response {
    let palette = palette_of(ui);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = space::XS;
        let label_id = ui
            .label(
                RichText::new(label)
                    .font(theme::headline())
                    .color(palette.text),
            )
            .id;
        let mut frame = egui::Frame::new()
            .fill(palette.surface)
            .corner_radius(radius::CONTROL)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .begin(ui);
        let response = frame.content_ui.add(
            TextEdit::multiline(value)
                .hint_text(hint)
                .font(egui::TextStyle::Monospace)
                .frame(egui::Frame::NONE)
                .desired_rows(rows)
                .desired_width(f32::INFINITY),
        );
        let stroke = if response.has_focus() {
            palette.accent
        } else {
            palette.outline
        };
        frame.frame.stroke = egui::Stroke::new(1.0, stroke);
        frame.end(ui);
        response.labelled_by(label_id)
    })
    .inner
}
