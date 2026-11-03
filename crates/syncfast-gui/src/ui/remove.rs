//! The alert that asks before a sync is removed. Cancel is the default; the
//! files on both sides stay either way.

use egui::Id;
use syncfast_core::config::split_remote;

use super::widgets::{AlertButton, alert};
use super::{Action, ViewModel};
use crate::theme;

const CANCEL: usize = 0;
const REMOVE: usize = 1;

/// Draws the alert while `vm.removing` names a sync. Nothing otherwise.
#[must_use]
pub fn show(ctx: &egui::Context, vm: &ViewModel) -> Vec<Action> {
    let Some(id) = vm.removing.as_deref() else {
        return Vec::new();
    };
    let Some(pair) = vm.pairs.iter().find(|pair| pair.id == id) else {
        return vec![Action::CancelRemove];
    };
    let remote = split_remote(&pair.remote).map_or(pair.remote.as_str(), |(account, _)| account);
    let checkbox = Id::new(("remove-sync-versions", id));
    let mut delete_versions = ctx
        .data(|data| data.get_temp::<bool>(checkbox))
        .unwrap_or(false);
    let chosen = alert(
        ctx,
        "remove-sync",
        &format!("Remove \"{}\"?", pair.name),
        &format!(
            "syncfast will stop syncing this folder. Files on this computer and on {remote} are not deleted."
        ),
        &[
            AlertButton::cancel("Cancel"),
            AlertButton::destructive("Remove"),
        ],
        |ui| {
            let outline = egui::Stroke::new(1.0, theme::palette(ctx).dim);
            ui.visuals_mut().widgets.inactive.bg_stroke = outline;
            ui.checkbox(&mut delete_versions, "Also delete saved versions");
        },
    );
    match chosen {
        Some(CANCEL) => {
            ctx.data_mut(|data| data.remove::<bool>(checkbox));
            vec![Action::CancelRemove]
        }
        Some(REMOVE) => {
            ctx.data_mut(|data| data.remove::<bool>(checkbox));
            vec![Action::ConfirmRemove {
                id: pair.id.clone(),
                delete_versions,
            }]
        }
        _ => {
            ctx.data_mut(|data| data.insert_temp(checkbox, delete_versions));
            Vec::new()
        }
    }
}
