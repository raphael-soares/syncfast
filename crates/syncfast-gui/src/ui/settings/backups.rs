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

/// What the last look at the key storage found, and what the last test of it
/// said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum KeyStorageView {
    #[default]
    Checking,
    Found(KeyBackend),
    Missing(String),
    Tested(KeyBackend),
    Failed(String),
}

pub const TEST_PASSED: &str = "Available";
pub const TEST_FAILED: &str = "Not available";

impl KeyStorageView {
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Checking => "Checking…",
            Self::Found(backend) | Self::Tested(backend) => match backend {
                KeyBackend::Portal => "Secret portal",
                KeyBackend::HostKeyring => "Host keyring (Secret Service)",
                KeyBackend::Plain => "Files in the data folder (development only)",
                KeyBackend::Memory => "Memory (tests only)",
            },
            Self::Missing(_) => NO_STORAGE,
            Self::Failed(_) => TEST_FAILED,
        }
    }

    fn backend(&self) -> Option<KeyBackend> {
        match self {
            Self::Found(backend) | Self::Tested(backend) => Some(*backend),
            _ => None,
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
    match &vm.key_storage {
        KeyStorageView::Tested(_) => {
            ui.label(
                RichText::new(TEST_PASSED)
                    .font(theme::callout())
                    .color(palette.secondary),
            );
        }
        KeyStorageView::Missing(text) | KeyStorageView::Failed(text) => {
            ui.label(
                RichText::new(text)
                    .font(theme::callout())
                    .color(palette.warning),
            );
        }
        _ => {}
    }
    if vm.key_storage.backend() == Some(KeyBackend::HostKeyring) {
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
