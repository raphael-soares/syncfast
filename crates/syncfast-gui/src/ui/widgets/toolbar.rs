use egui::{Align, Label, Layout, Rect, RichText, Sense, TextWrapMode, Ui, UiBuilder, Vec2};

use crate::theme::{self, space};

use super::palette_of;

/// The detail toolbar: `title` (title2) and `subtitle` (callout, secondary)
/// on the left, `actions` on the right. Actions are laid out right to left,
/// so add the rightmost one first. Title and subtitle take the width the
/// actions leave and are cut short with `…`; a cut text shows in full on
/// hover.
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
            let title_text = RichText::new(title)
                .font(theme::title2())
                .color(palette.text);
            let subtitle_text = RichText::new(subtitle)
                .font(theme::callout())
                .color(palette.secondary);
            let title_size = natural_size(ui, &title_text);
            let subtitle_size = natural_size(ui, &subtitle_text);
            let text_height = title_size.y + subtitle_size.y;
            let band = Rect::from_min_size(
                ui.available_rect_before_wrap().min,
                Vec2::new(ui.available_width(), text_height),
            );

            let mut controls = ui.new_child(
                UiBuilder::new()
                    .max_rect(band)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            controls.spacing_mut().item_spacing.x = space::S;
            actions(&mut controls);

            let text_width = (controls.min_rect().left() - space::M - band.left()).max(0.0);
            let mut text = ui.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        band.min,
                        Vec2::new(text_width, text_height),
                    ))
                    .layout(Layout::top_down(Align::Min)),
            );
            text.spacing_mut().item_spacing.y = 0.0;
            let shown = text.add(Label::new(title_text).truncate());
            if title_size.x > text_width {
                shown.on_hover_text(title);
            }
            let shown = text.add(Label::new(subtitle_text).truncate());
            if subtitle_size.x > text_width {
                shown.on_hover_text(subtitle);
            }

            ui.allocate_rect(band.union(controls.min_rect()), Sense::hover());
        });
}

fn natural_size(ui: &Ui, text: &RichText) -> Vec2 {
    egui::WidgetText::from(text.clone())
        .into_galley(
            ui,
            Some(TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Body,
        )
        .size()
}
