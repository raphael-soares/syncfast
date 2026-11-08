use egui::{RichText, Ui};
use syncfast_core::backup::status::BackupStatus;
use syncfast_core::state::{DisplayStatus, Tone};

use crate::icons::{Icon, backup_status_icon, status_icon};
use crate::theme::{self, size, space};

use super::palette_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadgeSize {
    /// Icon only; the status text is the tooltip.
    Compact,
    Regular,
    /// Larger icon and `title3` text, for the status card.
    Large,
}

/// The status of a sync or of a backup: what a badge or a sidebar row draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnyStatus {
    Sync(DisplayStatus),
    Backup(BackupStatus),
}

impl AnyStatus {
    #[must_use]
    pub fn tone(self) -> Tone {
        match self {
            Self::Sync(status) => status.tone(),
            Self::Backup(status) => status.tone(),
        }
    }

    #[must_use]
    pub fn text(self) -> String {
        match self {
            Self::Sync(status) => status.text(),
            Self::Backup(status) => status.label().to_owned(),
        }
    }

    #[must_use]
    pub fn icon(self) -> Icon {
        match self {
            Self::Sync(status) => status_icon(status),
            Self::Backup(status) => backup_status_icon(status),
        }
    }
}

impl From<DisplayStatus> for AnyStatus {
    fn from(status: DisplayStatus) -> Self {
        Self::Sync(status)
    }
}

impl From<BackupStatus> for AnyStatus {
    fn from(status: BackupStatus) -> Self {
        Self::Backup(status)
    }
}

/// Status icon and text in the status's tone. Colour is never the only signal.
pub fn status_badge(ui: &mut Ui, status: impl Into<AnyStatus>, badge: BadgeSize) -> egui::Response {
    let status = status.into();
    let palette = palette_of(ui);
    let color = theme::tone(&palette, status.tone());
    let text = status.text();
    let icon = status.icon();
    match badge {
        BadgeSize::Compact => {
            let response = ui.add(icon.image(color, size::ICON));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Image, ui.is_enabled(), &text)
            });
            response.on_hover_text(&text)
        }
        BadgeSize::Regular | BadgeSize::Large => {
            let (icon_size, font) = if badge == BadgeSize::Large {
                (20.0, theme::title3())
            } else {
                (size::ICON, theme::headline())
            };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S - 2.0;
                ui.add(icon.image(color, icon_size));
                ui.label(RichText::new(&text).font(font).color(color));
            })
            .response
        }
    }
}
