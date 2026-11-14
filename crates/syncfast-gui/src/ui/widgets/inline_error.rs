use egui::{RichText, Ui};

use crate::icons::Icon;
use crate::theme::{self, size};

use super::palette_of;

const FRIENDLY: [(&[&str], &str); 4] = [
    (
        &["accessnotconfigured", "has not been used in project"],
        "The Google Drive API is off in your Google Cloud project. Turn it on and try again.",
    ),
    (
        &["invalid_client"],
        "Google did not accept this client. Choose the client file again.",
    ),
    (
        &["access_denied"],
        "Google did not grant access. If your Google Cloud app is in testing, add your account as a test user.",
    ),
    (
        &["dial tcp", "no such host"],
        "Could not reach Google. Check the network and try again.",
    ),
];

/// An error shown right below the field it concerns.
pub fn inline_error(ui: &mut Ui, message: &str) -> egui::Response {
    let palette = palette_of(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add(Icon::CircleAlert.image(palette.danger, size::ICON));
        ui.label(
            RichText::new(message)
                .font(theme::callout())
                .color(palette.danger),
        );
    })
    .response
}

/// What a known rclone or Google error means, in plain words.
#[must_use]
pub fn friendly(detail: &str) -> Option<&'static str> {
    let detail = detail.to_lowercase();
    FRIENDLY
        .iter()
        .find(|(needles, _)| needles.iter().any(|needle| detail.contains(needle)))
        .map(|(_, text)| *text)
}

/// An error from rclone: the plain meaning when it is known, with the raw
/// text kept under a collapsed `Details`.
pub fn error_with_details(ui: &mut Ui, detail: &str) {
    let Some(text) = friendly(detail) else {
        inline_error(ui, detail);
        return;
    };
    inline_error(ui, text);
    egui::CollapsingHeader::new("Details")
        .id_salt(("error-details", detail))
        .show(ui, |ui| {
            let palette = palette_of(ui);
            ui.add(
                egui::Label::new(
                    RichText::new(detail)
                        .font(theme::monospace())
                        .color(palette.secondary),
                )
                .selectable(true),
            );
        });
}
