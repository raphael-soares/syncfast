use egui::{Align, Align2, Id, Layout, Order, RichText, Ui};

use crate::theme::{self, radius, space};

use super::modal::{enter_pressed, escape_pressed, scrim, take_focus_once};
use super::{ButtonKind, button_with_icon};

const WIDTH: f32 = 340.0;
const MAX_BUTTONS: usize = 3;

#[derive(Clone, Copy, Debug)]
pub struct AlertButton<'a> {
    pub label: &'a str,
    pub kind: ButtonKind,
    /// Triggered by Enter and drawn with the accent fill.
    pub is_default: bool,
    /// Triggered by Esc.
    pub is_cancel: bool,
}

impl<'a> AlertButton<'a> {
    /// `Cancel`: the default, so Enter never confirms a destructive action.
    #[must_use]
    pub fn cancel(label: &'a str) -> Self {
        Self {
            label,
            kind: ButtonKind::Primary,
            is_default: true,
            is_cancel: true,
        }
    }

    #[must_use]
    pub fn destructive(label: &'a str) -> Self {
        Self {
            label,
            kind: ButtonKind::Destructive,
            is_default: false,
            is_cancel: false,
        }
    }

    #[must_use]
    pub fn normal(label: &'a str) -> Self {
        Self {
            label,
            kind: ButtonKind::Secondary,
            is_default: false,
            is_cancel: false,
        }
    }
}

/// A modal alert for a rare, irreversible action: title, explanation, an
/// optional `body` (a checkbox, say) and at most three buttons, laid out
/// left to right. Returns the index of the button chosen this frame.
pub fn alert(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    text: &str,
    buttons: &[AlertButton<'_>],
    body: impl FnOnce(&mut Ui),
) -> Option<usize> {
    let palette = theme::palette(ctx);
    let id = Id::new(("syncfast-alert", id));
    let buttons = &buttons[..buttons.len().min(MAX_BUTTONS)];
    take_focus_once(ctx, id);
    scrim(ctx, id);

    let mut chosen = None;
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
                    ui.set_width(WIDTH - 40.0);
                    ui.spacing_mut().item_spacing.y = space::M;
                    ui.label(
                        RichText::new(title)
                            .font(theme::title3())
                            .color(palette.text),
                    );
                    ui.label(
                        RichText::new(text)
                            .font(theme::callout())
                            .color(palette.secondary),
                    );
                    body(ui);
                    ui.add_space(space::XS);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = space::S;
                        for (index, button) in buttons.iter().enumerate().rev() {
                            let kind = if button.is_default {
                                ButtonKind::Primary
                            } else {
                                button.kind
                            };
                            if button_with_icon(ui, kind, None, button.label).clicked() {
                                chosen = Some(index);
                            }
                        }
                    });
                });
        });

    if chosen.is_none() {
        if escape_pressed(ctx) {
            chosen = buttons.iter().position(|button| button.is_cancel);
        } else if enter_pressed(ctx) {
            chosen = buttons.iter().position(|button| button.is_default);
        }
    }
    chosen
}
