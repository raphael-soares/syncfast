use egui::{Align, Layout, Pos2, Rect, RichText, Sense, Stroke, TextWrapMode, Ui, UiBuilder, Vec2};

use crate::theme::{self, size, space};

use super::palette_of;

const LINE_GAP: f32 = 2.0;

/// One line of an options list: the label and an optional caption on the
/// left, the control on the right, a divider below. Returns what `control`
/// returns.
pub fn setting_row<R>(
    ui: &mut Ui,
    label: &str,
    caption: Option<&str>,
    control: impl FnOnce(&mut Ui) -> R,
) -> R {
    let palette = palette_of(ui);
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), size::LIST_ROW_HEIGHT),
        Sense::hover(),
    );

    let mut controls = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
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
    let text_area = Rect::from_min_max(
        rect.min,
        Pos2::new(
            (controls.min_rect().left() - space::M).max(rect.left()),
            rect.bottom(),
        ),
    );
    let mut text_height = line_height(ui, &label, text_area.width());
    if let Some(caption) = &caption {
        text_height += LINE_GAP + line_height(ui, caption, text_area.width());
    }
    let mut text = ui.new_child(
        UiBuilder::new()
            .max_rect(text_area)
            .layout(Layout::top_down(Align::Min)),
    );
    text.spacing_mut().item_spacing.y = LINE_GAP;
    text.add_space(((rect.height() - text_height) / 2.0).max(0.0));
    text.label(label);
    if let Some(caption) = caption {
        text.label(caption);
    }

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
