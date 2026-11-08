use egui::{Align, Align2, Id, Layout, Order, RichText, Ui};

use crate::theme::{self, radius, size, space};

use super::modal::{enter_pressed, escape_pressed, scrim, take_focus_once};
use super::{destructive_button, primary_button, secondary_button};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimaryKind {
    Normal,
    /// Drawn as a destructive button. It is never the default: Enter then
    /// cancels.
    Destructive,
}

#[derive(Clone, Copy, Debug)]
pub struct SheetButtons<'a> {
    pub primary: &'a str,
    pub primary_kind: PrimaryKind,
    pub primary_enabled: bool,
    /// Adds a `Back` button and moves `Cancel` to the left edge.
    pub back: bool,
    pub cancel: bool,
}

impl<'a> SheetButtons<'a> {
    #[must_use]
    pub fn new(primary: &'a str) -> Self {
        Self {
            primary,
            primary_kind: PrimaryKind::Normal,
            primary_enabled: true,
            back: false,
            cancel: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetAction {
    Cancel,
    Back,
    Primary,
}

/// A modal sheet: one at a time. Enter triggers the primary button (or
/// `Cancel` when the primary is destructive), Esc triggers `Cancel`. `body`
/// scrolls when taller than 80% of the window. Returns the button the user
/// chose this frame.
pub fn sheet(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    buttons: &SheetButtons<'_>,
    body: impl FnOnce(&mut Ui),
) -> Option<SheetAction> {
    sheet_with_header(ctx, id, title, buttons, |_| {}, body)
}

/// Like [`sheet`], with `header` drawn above the title (the steps of an
/// assistant, for example).
pub fn sheet_with_header(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    buttons: &SheetButtons<'_>,
    header: impl FnOnce(&mut Ui),
    body: impl FnOnce(&mut Ui),
) -> Option<SheetAction> {
    let palette = theme::palette(ctx);
    let id = Id::new(("syncfast-sheet", id));
    let screen = ctx.content_rect();
    take_focus_once(ctx, id);
    scrim(ctx, id);

    let width = size::SHEET_WIDTH.min(screen.width() - space::XL * 2.0);
    let body_height = (screen.height() * size::SHEET_MAX_HEIGHT - 140.0).max(80.0);
    let mut action = None;
    egui::Area::new(id.with("panel"))
        .order(Order::Foreground)
        .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(palette.overlay)
                .stroke(egui::Stroke::new(1.0, palette.outline))
                .corner_radius(radius::WINDOW)
                .shadow(egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 24,
                    spread: 0,
                    color: palette.shadow,
                })
                .inner_margin(egui::Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(width - 40.0);
                    ui.spacing_mut().item_spacing.y = space::M;
                    header(ui);
                    ui.label(
                        RichText::new(title)
                            .font(theme::title2())
                            .color(palette.text),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(body_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            body(ui);
                        });
                    action = footer(ui, buttons);
                });
        });

    if action.is_none() {
        if escape_pressed(ctx) {
            action = Some(SheetAction::Cancel);
        } else if enter_pressed(ctx) {
            action = match buttons.primary_kind {
                PrimaryKind::Normal => buttons.primary_enabled.then_some(SheetAction::Primary),
                PrimaryKind::Destructive => Some(SheetAction::Cancel),
            };
        }
    }
    action
}

fn footer(ui: &mut Ui, buttons: &SheetButtons<'_>) -> Option<SheetAction> {
    let mut action = None;
    ui.add_space(space::XS);
    ui.horizontal(|ui| {
        if buttons.back && buttons.cancel && secondary_button(ui, "Cancel").clicked() {
            action = Some(SheetAction::Cancel);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            ui.add_enabled_ui(buttons.primary_enabled, |ui| {
                let primary = match buttons.primary_kind {
                    PrimaryKind::Normal => primary_button(ui, buttons.primary),
                    PrimaryKind::Destructive => destructive_button(ui, buttons.primary),
                };
                if primary.clicked() {
                    action = Some(SheetAction::Primary);
                }
            });
            if buttons.back {
                if secondary_button(ui, "Back").clicked() {
                    action = Some(SheetAction::Back);
                }
            } else if buttons.cancel && secondary_button(ui, "Cancel").clicked() {
                action = Some(SheetAction::Cancel);
            }
        });
    });
    action
}
