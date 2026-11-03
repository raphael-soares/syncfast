use egui::{RichText, TextEdit, Ui};

use crate::theme::{self, radius, size, space};

use super::palette_of;

/// A single-line field with its label above it. Pair it with `inline_error`
/// right below to show a validation problem.
pub fn text_field(ui: &mut Ui, label: &str, value: &mut String, hint: &str) -> egui::Response {
    field(ui, label, value, hint, false)
}

/// Like [`text_field`], with the text masked.
pub fn secret_field(ui: &mut Ui, label: &str, value: &mut String) -> egui::Response {
    field(ui, label, value, "", true)
}

fn field(ui: &mut Ui, label: &str, value: &mut String, hint: &str, secret: bool) -> egui::Response {
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
            .inner_margin(egui::Margin::symmetric(8, 0))
            .begin(ui);
        frame.content_ui.set_min_height(size::CONTROL_HEIGHT - 2.0);
        let response = frame.content_ui.add(
            TextEdit::singleline(value)
                .hint_text(hint)
                .password(secret)
                .frame(egui::Frame::NONE)
                .vertical_align(egui::Align::Center)
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
