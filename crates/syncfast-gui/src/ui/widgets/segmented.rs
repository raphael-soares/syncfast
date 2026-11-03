use egui::{Sense, Ui, Vec2};

use crate::theme::{self, radius, size, space};

use super::{focus_ring, palette_of};

/// A segmented control. Returns the index the user picked this frame.
pub fn segmented(ui: &mut Ui, selected: usize, labels: &[&str]) -> Option<usize> {
    let palette = palette_of(ui);
    let mut picked = None;
    egui::Frame::new()
        .fill(palette.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(2)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (index, label) in labels.iter().enumerate() {
                    let on = index == selected;
                    let color = if on { palette.text } else { palette.secondary };
                    let galley =
                        ui.painter()
                            .layout_no_wrap((*label).to_owned(), theme::headline(), color);
                    let desired =
                        Vec2::new(galley.size().x + space::M * 2.0, size::CONTROL_HEIGHT - 4.0);
                    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            ui.is_enabled(),
                            on,
                            *label,
                        )
                    });
                    if on || response.hovered() {
                        let fill = if on {
                            palette.surface_active
                        } else {
                            palette.surface_hover
                        };
                        ui.painter().rect_filled(rect, 5.0, fill);
                    }
                    let pos = egui::pos2(
                        rect.left() + space::M,
                        rect.center().y - galley.size().y / 2.0,
                    );
                    ui.painter().galley(pos, galley, color);
                    focus_ring(ui, &response, radius::CONTROL);
                    if response.clicked() && !on {
                        picked = Some(index);
                    }
                }
            });
        });
    picked
}
