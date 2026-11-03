use egui::{Sense, Ui, Vec2};

use crate::icons::Icon;
use crate::theme::{self, radius, size, space};

use super::{focus_ring, paint_icon, palette_of};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// Accent fill: the one action the user most likely wants.
    Primary,
    Secondary,
    /// Danger text on a neutral fill; never the default action.
    Destructive,
}

const ICON_GAP: f32 = 6.0;

/// A button with an optional leading icon. The label is its accessible name.
pub fn button_with_icon(
    ui: &mut Ui,
    kind: ButtonKind,
    icon: Option<Icon>,
    label: &str,
) -> egui::Response {
    let palette = palette_of(ui);
    let (fill, hover, text) = match kind {
        ButtonKind::Primary => (palette.accent, palette.accent_hover, palette.on_accent),
        ButtonKind::Secondary => (palette.surface, palette.surface_hover, palette.text),
        ButtonKind::Destructive => (palette.surface, palette.surface_hover, palette.danger),
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), theme::headline(), text);
    let icon_width = if icon.is_some() {
        size::ICON + ICON_GAP
    } else {
        0.0
    };
    let desired = Vec2::new(
        galley.size().x + icon_width + space::M * 2.0,
        size::CONTROL_HEIGHT,
    );
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if ui.is_rect_visible(rect) {
        let fill = if response.is_pointer_button_down_on() || response.hovered() {
            hover
        } else {
            fill
        };
        ui.painter()
            .rect_filled(rect, f32::from(radius::CONTROL), fill);
        let mut x = rect.left() + space::M;
        if let Some(icon) = icon {
            paint_icon(
                ui,
                icon,
                egui::pos2(x + size::ICON / 2.0, rect.center().y),
                size::ICON,
                text,
            );
            x += icon_width;
        }
        let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, text);
    }
    focus_ring(ui, &response, radius::CONTROL);
    response
}

pub fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    button_with_icon(ui, ButtonKind::Primary, None, label)
}

pub fn secondary_button(ui: &mut Ui, label: &str) -> egui::Response {
    button_with_icon(ui, ButtonKind::Secondary, None, label)
}

pub fn destructive_button(ui: &mut Ui, label: &str) -> egui::Response {
    button_with_icon(ui, ButtonKind::Destructive, None, label)
}
