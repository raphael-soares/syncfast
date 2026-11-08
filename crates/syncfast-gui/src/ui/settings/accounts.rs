//! Settings > Accounts: the rclone remotes, each with the state of its
//! sign-in, a warning for a Google Drive remote on the shared client ID and
//! the buttons that reconnect, change the client ID, rename or remove it.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Ui};
use syncfast_core::backup::destination::Destination;
use syncfast_core::config::split_remote;
use syncfast_core::state::GateKind;

use crate::icons::Icon;
use crate::theme::{self, Palette, radius, size, space};
use crate::ui::widgets::{
    ButtonKind, button_with_icon, destructive_button, primary_button, secondary_button,
};
use crate::ui::{AccountStatus, Action, RemoteRow, ViewModel};

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

/// Whether a sync or a backup on `remote` stopped because the sign-in was
/// lost.
fn needs_reconnect(vm: &ViewModel, remote: &str) -> bool {
    let sync = vm.pairs.iter().any(|pair| {
        pair.gate == Some(GateKind::AuthError)
            && split_remote(&pair.remote).is_some_and(|(account, _)| account == remote)
    });
    sync || vm.backups.iter().any(|backup| {
        backup
            .state
            .gate
            .as_ref()
            .is_some_and(|gate| gate.kind == GateKind::AuthError)
            && matches!(
                Destination::parse(&backup.config.dest),
                Ok(Destination::Account { name, .. }) if name == remote
            )
    })
}

pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    for remote in &vm.remotes {
        let shared = remote.shares_client_id();
        let reconnect = needs_reconnect(vm, &remote.name);
        let status = vm.account_status.get(&remote.name);
        actions.extend(row(ui, &palette, remote, shared, reconnect, status));
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

/// The line under the title that says how the sign-in stands, if any.
/// `Signed out` shows once, whether the status or a held sync says it.
fn status_line(
    palette: &Palette,
    status: Option<&AccountStatus>,
    disconnected: bool,
) -> Option<(String, Color32)> {
    if disconnected && !matches!(status, Some(AccountStatus::Problem(_))) {
        return Some((DISCONNECTED.to_owned(), palette.warning));
    }
    match status? {
        AccountStatus::Checking => Some(("Checking…".to_owned(), palette.secondary)),
        AccountStatus::SignedIn => Some(("Signed in".to_owned(), palette.secondary)),
        AccountStatus::SignedOut => Some((DISCONNECTED.to_owned(), palette.warning)),
        AccountStatus::Offline => Some(("Offline. Not checked.".to_owned(), palette.secondary)),
        AccountStatus::Problem(detail) => Some((format!("Problem: {detail}"), palette.warning)),
    }
}

fn row(
    ui: &mut Ui,
    palette: &Palette,
    remote: &RemoteRow,
    shared: bool,
    reconnect: bool,
    status: Option<&AccountStatus>,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let signed_out = reconnect || status == Some(&AccountStatus::SignedOut);
    let problem = matches!(status, Some(AccountStatus::Problem(_)));
    let flagged = shared || signed_out || problem;
    let line = status_line(palette, status, signed_out);
    Frame::new()
        .stroke(egui::Stroke::new(1.0, palette.outline))
        .corner_radius(radius::CARD)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(if line.is_some() || shared {
                    FLAGGED_HEIGHT
                } else {
                    ROW_HEIGHT
                });
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
                    if let Some((text, color)) = &line {
                        ui.label(RichText::new(text).font(theme::callout()).color(*color));
                    }
                    if shared {
                        ui.label(
                            RichText::new(SHARED_CLIENT_ID)
                                .font(theme::callout())
                                .color(palette.secondary),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = space::S;
                    if destructive_button(ui, "Remove…").clicked() {
                        actions.push(Action::RemoveAccount(remote.name.clone()));
                    }
                    if remote.is_drive() && secondary_button(ui, "Rename…").clicked() {
                        actions.push(Action::RenameAccount(remote.name.clone()));
                    }
                    if remote.is_drive() {
                        let label = if shared {
                            "Use My Client ID…"
                        } else {
                            "Change Client ID…"
                        };
                        if secondary_button(ui, label).clicked() {
                            actions.push(Action::ChangeClientId(remote.name.clone()));
                        }
                    }
                    if signed_out && primary_button(ui, "Reconnect…").clicked() {
                        actions.push(Action::ReconnectAccount(remote.name.clone()));
                    }
                });
            });
        });
    actions
}
