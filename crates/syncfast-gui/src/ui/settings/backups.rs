//! The Backups pane of Settings: where the backup keys are kept, the
//! restic that runs the backups, and the guide.

use egui::{RichText, Ui};
use syncfast_core::backup::keystore::KeyBackend;

use crate::theme;
use crate::ui::widgets::{secondary_button, setting_row};
use crate::ui::{Action, ViewModel};

pub const GUIDE_URL: &str =
    "https://github.com/raphael-soares/syncfast/blob/main/docs/guide/backup.md";

pub const NO_STORAGE: &str =
    "No Secret portal and no Secret Service answered. See docs/guide/backup.md.";

pub const HOST_KEYRING_NOTE: &str = "The Flatpak can read every secret in your keyring. Routing the Secret portal is safer; see docs/guide/backup.md.";

/// What the last look at the key storage found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyStorageView {
    #[default]
    Checking,
    Found(KeyBackend),
    Missing,
}

impl KeyStorageView {
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Checking => "Checking…",
            Self::Found(KeyBackend::Portal) => "Secret portal",
            Self::Found(KeyBackend::HostKeyring) => "Host keyring (Secret Service)",
            Self::Found(KeyBackend::Plain) => "Files in the data folder (development only)",
            Self::Found(KeyBackend::Memory) => "Memory (tests only)",
            Self::Missing => NO_STORAGE,
        }
    }
}

#[must_use]
pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme::space::M;
    setting_row(ui, "Key storage", Some(vm.key_storage.text()), |ui| {
        if secondary_button(ui, "Test Key Storage").clicked() {
            actions.push(Action::TestKeyStorage);
        }
    });
    if vm.key_storage == KeyStorageView::Found(KeyBackend::HostKeyring) {
        ui.label(
            RichText::new(HOST_KEYRING_NOTE)
                .font(theme::callout())
                .color(palette.warning),
        );
    }
    let restic = vm
        .restic_version
        .as_deref()
        .map_or_else(|| "Not found".to_owned(), str::to_owned);
    setting_row(ui, "restic", Some(&restic), |_| {});
    if ui
        .link(
            RichText::new("Backing up with syncfast")
                .font(theme::callout())
                .color(palette.accent),
        )
        .clicked()
    {
        actions.push(Action::OpenUrl(GUIDE_URL.to_owned()));
    }
    actions
}
