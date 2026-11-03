use egui::{RichText, Ui};
use syncfast_core::state::DisplayStatus;

use crate::icons::status_icon;
use crate::theme::{self, size, space};

use super::palette_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadgeSize {
    /// Icon only; the status text is the tooltip.
    Compact,
    Regular,
    /// Larger icon and `title3` text, for the status card.
    Large,
}

/// Status icon and text in the status's tone. Colour is never the only signal.
pub fn status_badge(ui: &mut Ui, status: DisplayStatus, badge: BadgeSize) -> egui::Response {
    let palette = palette_of(ui);
    let color = theme::tone(&palette, status.tone());
    let text = status.text();
    let icon = status_icon(status);
    match badge {
        BadgeSize::Compact => {
            let response = ui.add(icon.image(color, size::ICON));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Image, ui.is_enabled(), &text)
            });
            response.on_hover_text(&text)
        }
        BadgeSize::Regular | BadgeSize::Large => {
            let (icon_size, font) = if badge == BadgeSize::Large {
                (20.0, theme::title3())
            } else {
                (size::ICON, theme::headline())
            };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S - 2.0;
                ui.add(icon.image(color, icon_size));
                ui.label(RichText::new(&text).font(font).color(color));
            })
            .response
        }
    }
}
