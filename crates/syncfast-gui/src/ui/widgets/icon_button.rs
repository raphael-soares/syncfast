use egui::{Sense, Ui, Vec2};

use crate::icons::Icon;
use crate::theme::{radius, size};

use super::{focus_ring, paint_icon, palette_of};

const SIDE: f32 = 24.0;

/// A square icon-only button. The tooltip is mandatory: it is the hover text
/// and the accessible name.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str) -> egui::Response {
    let palette = palette_of(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SIDE), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tooltip)
    });
    if ui.is_rect_visible(rect) {
        if response.hovered() || response.is_pointer_button_down_on() {
            ui.painter()
                .rect_filled(rect, f32::from(radius::CONTROL), palette.surface_hover);
        }
        paint_icon(ui, icon, rect.center(), size::ICON, palette.secondary);
    }
    focus_ring(ui, &response, radius::CONTROL);
    response.on_hover_text(tooltip)
}
