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

const PADDING: i8 = 8;

/// A single-line field `width` wide with no visible label, for a row that
/// draws its own (a `setting_row`). `label` is the accessible name.
pub fn inline_field(ui: &mut Ui, label: &str, value: &mut String, width: f32) -> egui::Response {
    let outer = egui::vec2(width + 2.0 * f32::from(PADDING) + 2.0, size::CONTROL_HEIGHT);
    let response = ui
        .allocate_ui_with_layout(
            outer,
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| edit_box(ui, value, "", false, width),
        )
        .inner;
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::text_edit(ui.is_enabled(), "", value.as_str(), "");
        info.label = Some(label.to_owned());
        info
    });
    response
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
        edit_box(ui, value, hint, secret, f32::INFINITY).labelled_by(label_id)
    })
    .inner
}

fn edit_box(
    ui: &mut Ui,
    value: &mut String,
    hint: &str,
    secret: bool,
    width: f32,
) -> egui::Response {
    let palette = palette_of(ui);
    let mut frame = egui::Frame::new()
        .fill(palette.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(egui::Margin::symmetric(PADDING, 0))
        .begin(ui);
    frame.content_ui.set_min_height(size::CONTROL_HEIGHT - 2.0);
    let response = frame.content_ui.add(
        TextEdit::singleline(value)
            .hint_text(hint)
            .password(secret)
            .frame(egui::Frame::NONE)
            .vertical_align(egui::Align::Center)
            .desired_width(width),
    );
    let stroke = if response.has_focus() {
        palette.accent
    } else {
        palette.outline
    };
    frame.frame.stroke = egui::Stroke::new(1.0, stroke);
    frame.end(ui);
    response
}
