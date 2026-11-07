use egui::{
    Align, Context, CornerRadius, Id, Layout, Margin, Order, Rect, RichText, Sense, Stroke, Ui,
    UiBuilder, Vec2,
};

use crate::icons::Icon;
use crate::theme::{self, radius, size, space};

use super::icon_button;
use super::modal::{escape_pressed, take_focus_once};

/// A modal dialog over the whole window: a dimmed backdrop, a centered frame
/// with a header, a sidebar and a scrolling content area. Returns `true` when
/// the user asked to close it this frame (Close, a click outside the frame,
/// Esc). The last two do nothing while `modal_above` is set (a sheet or an
/// alert covers the dialog) or a popup is open.
pub fn dialog(
    ctx: &Context,
    id: &str,
    title: &str,
    modal_above: bool,
    sidebar: impl FnOnce(&mut Ui),
    body: impl FnOnce(&mut Ui),
) -> bool {
    let palette = theme::palette(ctx);
    let id = Id::new(("syncfast-dialog", id));
    let screen = ctx.content_rect();
    let frame_size = Vec2::new(
        (screen.width() - size::WINDOW_MARGIN * 2.0).clamp(0.0, size::DIALOG_MAX[0]),
        (screen.height() - size::WINDOW_MARGIN * 2.0).clamp(0.0, size::DIALOG_MAX[1]),
    );
    let frame_rect = Rect::from_center_size(screen.center(), frame_size);
    let popup_open = egui::Popup::is_any_open(ctx);
    take_focus_once(ctx, id);

    let mut close = false;
    egui::Area::new(id.with("panel"))
        .order(Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (backdrop_rect, backdrop) =
                ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
            ui.painter().rect_filled(backdrop_rect, 0.0, palette.shadow);
            let outside = backdrop
                .interact_pointer_pos()
                .is_some_and(|pos| !frame_rect.contains(pos));
            close |= backdrop.clicked() && outside && !modal_above && !popup_open;

            let mut frame_ui = ui.new_child(UiBuilder::new().max_rect(frame_rect));
            egui::Frame::new()
                .fill(palette.window)
                .stroke(Stroke::new(1.0, palette.outline))
                .corner_radius(radius::WINDOW)
                .shadow(egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 24,
                    spread: 0,
                    color: palette.shadow,
                })
                .show(&mut frame_ui, |ui| {
                    let inner = ui.max_rect();
                    ui.set_min_size(inner.size());
                    close |= contents(ui, inner, title, sidebar, body);
                });
        });

    close || (!modal_above && !popup_open && escape_pressed(ctx))
}

/// Header, sidebar and content inside `inner`. Returns whether Close was
/// clicked.
fn contents(
    ui: &mut Ui,
    inner: Rect,
    title: &str,
    sidebar: impl FnOnce(&mut Ui),
    body: impl FnOnce(&mut Ui),
) -> bool {
    let palette = theme::palette(ui.ctx());
    let line = Stroke::new(1.0, palette.outline);
    let (header, rest) = {
        let mut header = inner;
        header.set_height(size::LIST_ROW_HEIGHT.min(inner.height()));
        let mut rest = inner;
        rest.min.y = header.max.y;
        (header, rest)
    };
    let mut sidebar_rect = rest;
    sidebar_rect.set_width(size::DIALOG_SIDEBAR.min(rest.width()));
    let mut content_rect = rest;
    content_rect.min.x = sidebar_rect.max.x;

    let mut close = false;
    let mut header_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(header.shrink2(Vec2::new(space::L, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    header_ui.label(
        RichText::new(title)
            .font(theme::headline())
            .color(palette.text),
    );
    header_ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        close = icon_button(ui, Icon::X, "Close").clicked();
    });
    ui.painter()
        .hline(inner.x_range(), header.bottom() - 0.5, line);

    ui.painter().rect_filled(
        sidebar_rect,
        CornerRadius {
            sw: radius::WINDOW - 1,
            ..CornerRadius::ZERO
        },
        palette.panel,
    );
    ui.painter()
        .vline(sidebar_rect.right() - 0.5, sidebar_rect.y_range(), line);
    let mut sidebar_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(sidebar_rect.shrink(space::M))
            .layout(Layout::top_down(Align::Min)),
    );
    sidebar_ui.spacing_mut().item_spacing.y = 2.0;
    sidebar(&mut sidebar_ui);

    let mut content_ui = ui.new_child(UiBuilder::new().max_rect(content_rect));
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(&mut content_ui, |ui| {
            egui::Frame::new()
                .inner_margin(Margin::from(space::XL))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = space::M;
                    body(ui);
                });
        });
    close
}
