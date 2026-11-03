//! Settings > Accounts: the rclone remotes, with a warning for a Google Drive
//! remote on the shared client ID and a `Reconnect…` for one whose sync lost
//! its sign-in.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Frame, Layout, Margin, RichText, Ui};
use syncfast_core::config::split_remote;
use syncfast_core::state::GateKind;

use crate::icons::Icon;
use crate::theme::{self, Palette, radius, size, space};
use crate::ui::widgets::{ButtonKind, button_with_icon, primary_button, secondary_button};
use crate::ui::{Action, RemoteRow, ViewModel};

pub const SHARED_CLIENT_ID: &str = "Uses rclone's shared client ID, which stops working in 2026.";
pub const DISCONNECTED: &str = "Signed out. Reconnect to resume syncing.";

const ROW_HEIGHT: f32 = 28.0;
const FLAGGED_HEIGHT: f32 = 44.0;

/// The type as people know it: `drive` is Google Drive.
fn kind_label(kind: &str) -> &str {
    if kind == "drive" {
        "Google Drive"
    } else {
        kind
    }
}

/// Whether a sync on `remote` stopped because the sign-in was lost.
fn needs_reconnect(vm: &ViewModel, remote: &str) -> bool {
    vm.pairs.iter().any(|pair| {
        pair.gate == Some(GateKind::AuthError)
            && split_remote(&pair.remote).is_some_and(|(account, _)| account == remote)
    })
}

pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    for remote in &vm.remotes {
        let shared = remote.shares_client_id();
        let reconnect = needs_reconnect(vm, &remote.name);
        actions.extend(row(ui, &palette, remote, shared, reconnect));
    }
    if vm.remotes.is_empty() {
        ui.label(
            RichText::new("No accounts yet.")
                .font(theme::callout())
                .color(palette.secondary),
        );
    }
    if button_with_icon(
        ui,
        ButtonKind::Secondary,
        Some(Icon::Plus),
        "Add Google Drive Account…",
    )
    .clicked()
    {
        actions.push(Action::AddAccount);
    }
    actions
}

fn title(palette: &Palette, remote: &RemoteRow) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        &remote.name,
        0.0,
        TextFormat::simple(theme::headline(), palette.text),
    );
    job.append(
        &format!(" · {}", kind_label(&remote.kind)),
        0.0,
        TextFormat::simple(theme::body(), palette.secondary),
    );
    job
}

fn row(
    ui: &mut Ui,
    palette: &Palette,
    remote: &RemoteRow,
    shared: bool,
    reconnect: bool,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let flagged = shared || reconnect;
    Frame::new()
        .stroke(egui::Stroke::new(1.0, palette.outline))
        .corner_radius(radius::CARD)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(if flagged { FLAGGED_HEIGHT } else { ROW_HEIGHT });
                ui.spacing_mut().item_spacing.x = space::M;
                let (icon, color) = if flagged {
                    (Icon::CircleAlert, palette.warning)
                } else {
                    (Icon::User, palette.secondary)
                };
                ui.add(icon.image(color, size::ICON));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(title(palette, remote));
                    for (text, shown) in [(DISCONNECTED, reconnect), (SHARED_CLIENT_ID, shared)] {
                        if shown {
                            ui.label(
                                RichText::new(text)
                                    .font(theme::callout())
                                    .color(palette.secondary),
                            );
                        }
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = space::S;
                    if shared && secondary_button(ui, "Use My Client ID…").clicked() {
                        actions.push(Action::ChangeClientId(remote.name.clone()));
                    }
                    if reconnect && primary_button(ui, "Reconnect…").clicked() {
                        actions.push(Action::ReconnectAccount(remote.name.clone()));
                    }
                });
            });
        });
    actions
}
