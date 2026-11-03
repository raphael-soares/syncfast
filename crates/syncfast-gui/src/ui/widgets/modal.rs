use egui::{Context, Id, Key, Order, Sense};

use crate::theme;

/// Dims everything below and swallows its input. One modal at a time.
pub(super) fn scrim(ctx: &Context, id: Id) {
    let palette = theme::palette(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(id.with("scrim"))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
            ui.painter().rect_filled(rect, 0.0, palette.shadow);
        });
}

/// Drops keyboard focus from the window below on the first frame a modal is
/// shown, so Enter reaches the modal and not a button behind it.
pub(super) fn take_focus_once(ctx: &Context, id: Id) {
    let frame = ctx.cumulative_frame_nr();
    let key = id.with("last-frame");
    let previous = ctx.data_mut(|data| {
        let previous = data.get_temp::<u64>(key);
        data.insert_temp(key, frame);
        previous
    });
    let reopened = previous.is_none_or(|previous| previous.saturating_add(1) < frame);
    if reopened {
        ctx.memory_mut(|memory| {
            if let Some(focused) = memory.focused() {
                memory.surrender_focus(focused);
            }
        });
    }
}

pub(super) fn escape_pressed(ctx: &Context) -> bool {
    ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape))
}

/// Enter acts on the default button unless a widget holds focus: a focused
/// button already handles Enter itself, and a multi-line field needs it.
pub(super) fn enter_pressed(ctx: &Context) -> bool {
    let free = ctx.memory(|memory| memory.focused().is_none());
    free && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter))
}
