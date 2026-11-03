use egui::{Align, Layout, RichText, Ui};

use crate::theme::{self, space};

use super::palette_of;

/// The detail toolbar: `title` (title2) and `subtitle` (callout, secondary)
/// on the left, `actions` on the right. Actions are laid out right to left,
/// so add the rightmost one first.
pub fn toolbar(ui: &mut Ui, title: &str, subtitle: &str, actions: impl FnOnce(&mut Ui)) {
    let palette = palette_of(ui);
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 20,
            right: 20,
            top: 16,
            bottom: 12,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(
                        RichText::new(title)
                            .font(theme::title2())
                            .color(palette.text),
                    );
                    ui.label(
                        RichText::new(subtitle)
                            .font(theme::callout())
                            .color(palette.secondary),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = space::S;
                    actions(ui);
                });
            });
        });
}
