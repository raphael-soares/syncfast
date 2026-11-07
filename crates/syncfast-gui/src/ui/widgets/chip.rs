use egui::{Sense, Ui, Vec2, WidgetInfo, WidgetType};

use crate::theme::{self, radius};

use super::palette_of;

const PADDING_X: f32 = 8.0;
const PADDING_Y: f32 = 3.0;

/// A small filled pill for a fact about what it sits next to, such as the
/// mode of a sync. It is a tag, not a control: no outline, smaller text than
/// a button and shorter than one. Drawn at its text's size, because a frame
/// inside a centred horizontal layout stretches to the row's height.
pub fn chip(ui: &mut Ui, text: &str) -> egui::Response {
    let palette = palette_of(ui);
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), theme::subheadline(), palette.secondary);
    let size = galley.size() + Vec2::new(2.0 * PADDING_X, 2.0 * PADDING_Y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, radius::PILL, palette.surface_active);
        painter.galley(
            rect.min + Vec2::new(PADDING_X, PADDING_Y),
            galley,
            palette.secondary,
        );
    }
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    response
}
