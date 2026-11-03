use egui::{Align, Layout, Sense, Ui, UiBuilder, Vec2};

use crate::theme::{radius, space};

use super::{focus_ring, palette_of};

/// A list line with a bottom divider and hover fill. `content` is laid out
/// left to right, centred vertically, inside a row of `height` points (44 for
/// a two-line row, see `theme::size::LIST_ROW_HEIGHT`). The response senses
/// clicks on the row; widgets inside keep their own clicks.
pub fn list_row(ui: &mut Ui, height: f32, content: impl FnOnce(&mut Ui)) -> egui::Response {
    let palette = palette_of(ui);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, f32::from(radius::CARD), palette.surface_hover);
        }
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, palette.outline),
        );
    }
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(space::XS, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = space::M;
    content(&mut child);
    focus_ring(ui, &response, radius::CARD);
    response
}
