//! The alert that asks before an account is removed. Cancel is the default;
//! no file is deleted anywhere, and the syncs on the account wait for it.

use syncfast_core::config::split_remote;

use super::widgets::{AlertButton, alert};
use super::{Action, ViewModel};

const CANCEL: usize = 0;
const REMOVE: usize = 1;

/// Draws the alert while `vm.removing_account` names an account. Nothing
/// otherwise.
#[must_use]
pub fn show(ctx: &egui::Context, vm: &ViewModel) -> Vec<Action> {
    let Some(name) = vm.removing_account.as_deref() else {
        return Vec::new();
    };
    let affected: Vec<&str> = vm
        .pairs
        .iter()
        .filter(|pair| split_remote(&pair.remote).is_some_and(|(account, _)| account == name))
        .map(|pair| pair.name.as_str())
        .collect();
    let text = if affected.is_empty() {
        "syncfast forgets this account. Files on Google Drive are not deleted.".to_owned()
    } else {
        format!(
            "These syncs stop until the account is back: {}. Files on both sides are not deleted.",
            affected.join(", ")
        )
    };
    let chosen = alert(
        ctx,
        "remove-account",
        &format!("Remove \"{name}\"?"),
        &text,
        &[
            AlertButton::cancel("Cancel"),
            AlertButton::destructive("Remove"),
        ],
        |_| {},
    );
    match chosen {
        Some(CANCEL) => vec![Action::CancelRemoveAccount],
        Some(REMOVE) => vec![Action::ConfirmRemoveAccount(name.to_owned())],
        _ => Vec::new(),
    }
}
