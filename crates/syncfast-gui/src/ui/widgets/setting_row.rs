use egui::{Align, Layout, Pos2, Rect, RichText, Sense, Stroke, TextWrapMode, Ui, UiBuilder, Vec2};

use crate::theme::{self, size, space};

use super::palette_of;

const LINE_GAP: f32 = 2.0;

/// One line of an options list: the label and an optional caption on the
/// left, the control on the right, a divider below. The row grows when the
/// caption wraps; the control stays centered in the first `LIST_ROW_HEIGHT`.
/// Returns what `control` returns.
pub fn setting_row<R>(
    ui: &mut Ui,
    label: &str,
    caption: Option<&str>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    let palette = palette_of(ui);
    let band = Rect::from_min_size(
        ui.available_rect_before_wrap().min,
        Vec2::new(ui.available_width(), size::LIST_ROW_HEIGHT),
    );

    let mut controls = ui.new_child(
        UiBuilder::new()
            .max_rect(band)
            .layout(Layout::right_to_left(Align::Center)),
    );
    controls.spacing_mut().item_spacing.x = space::M;
    let result = control(&mut controls);

    let label = RichText::new(label).font(theme::body()).color(palette.text);
    let caption = caption.map(|text| {
        RichText::new(text)
            .font(theme::callout())
            .color(palette.secondary)
    });
    let text_width = (controls.min_rect().left() - space::M - band.left()).max(0.0);
    let mut text_height = line_height(ui, &label, text_width);
    if let Some(caption) = &caption {
        text_height += LINE_GAP + line_height(ui, caption, text_width);
    }
    let height = size::LIST_ROW_HEIGHT.max(text_height + 2.0 * space::XS);
    let rect = Rect::from_min_size(band.min, Vec2::new(band.width(), height));

    let mut text = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_max(
                rect.min,
                Pos2::new(band.left() + text_width, rect.bottom()),
            ))
            .layout(Layout::top_down(Align::Min)),
    );
    text.spacing_mut().item_spacing.y = LINE_GAP;
    text.add_space((height - text_height) / 2.0);
    text.label(label);
    if let Some(caption) = caption {
        text.label(caption);
    }

    ui.allocate_rect(rect, Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, palette.outline),
    );
    result
}

fn line_height(ui: &Ui, text: &RichText, width: f32) -> f32 {
    egui::WidgetText::from(text.clone())
        .into_galley(ui, Some(TextWrapMode::Wrap), width, egui::TextStyle::Body)
        .size()
        .y
}
