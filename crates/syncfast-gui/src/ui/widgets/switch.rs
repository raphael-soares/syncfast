use egui::{Sense, Ui, Vec2};

use crate::theme::{self, size};

use super::{focus_ring, palette_of};

/// A labelled on/off row: `label` on the left, a 40x22 switch on the right.
/// The whole row toggles. The response reports `changed()` when it flips.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> egui::Response {
    let palette = palette_of(ui);
    let [track_width, track_height] = size::SWITCH;
    let (rect, mut response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), size::CONTROL_HEIGHT),
        Sense::click(),
    );
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let value = *on;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), value, label)
    });
    if ui.is_rect_visible(rect) {
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_owned(), theme::body(), palette.text);
        ui.painter().galley(
            egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
            galley,
            palette.text,
        );
        let track = egui::Rect::from_center_size(
            egui::pos2(rect.right() - track_width / 2.0, rect.center().y),
            Vec2::new(track_width, track_height),
        );
        let amount = ui.ctx().animate_bool_responsive(response.id, value);
        let off_fill = palette.surface_active;
        let fill = off_fill.lerp_to_gamma(palette.accent, amount);
        ui.painter().rect_filled(track, track_height / 2.0, fill);
        let knob_radius = track_height / 2.0 - 2.0;
        let travel = track_width - track_height;
        let knob = egui::pos2(
            track.left() + track_height / 2.0 + travel * amount,
            track.center().y,
        );
        let knob_fill = if value {
            palette.on_accent
        } else {
            palette.text
        };
        ui.painter().circle_filled(knob, knob_radius, knob_fill);
    }
    focus_ring(ui, &response, 11);
    response
}
