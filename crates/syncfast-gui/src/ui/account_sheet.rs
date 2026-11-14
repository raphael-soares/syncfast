//! The account sheet: connects Google Drive with a client the user creates
//! in Google Cloud (a guide walks through it and the downloaded file fills
//! the fields), or signs in again to an account that exists.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use egui::{RichText, Ui};

use crate::theme::{self, space};

use super::widgets::{
    PrimaryKind, SheetAction, SheetButtons, inline_error, progress_bar, secondary_button,
    secret_field, sheet, text_field,
};
use super::{Action, RemoteRow};

/// The steps of creating a Google client, each with the Google Cloud page
/// it happens on.
pub const GUIDE: [(&str, Option<&str>); 6] = [
    (
        "Create a Google Cloud project.",
        Some("https://console.cloud.google.com/projectcreate"),
    ),
    (
        "Turn on the Google Drive API.",
        Some("https://console.cloud.google.com/apis/library/drive.googleapis.com"),
    ),
    (
        "Set up the sign-in screen: choose External, then under Branding paste the two addresses below.",
        Some("https://console.cloud.google.com/auth/branding"),
    ),
    (
        "Publish it: under Audience, press Publish App.",
        Some("https://console.cloud.google.com/auth/audience"),
    ),
    (
        "Create a client of type Desktop app and download its JSON.",
        Some("https://console.cloud.google.com/auth/clients/create"),
    ),
    (
        "Choose the downloaded file below, or drop it on this window.",
        None,
    ),
];

pub const HOMEPAGE_URL: &str = "https://github.com/raphael-soares/syncfast";
pub const PRIVACY_URL: &str =
    "https://github.com/raphael-soares/syncfast/blob/main/docs/guide/privacy.md";

/// The guide step after which the two Branding addresses are shown.
const BRANDING_STEP: usize = 2;

/// A client secret. Never printed by `Debug`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountMode {
    Add,
    /// Sign in again to the remote `name`, with new credentials.
    Update {
        name: String,
    },
    /// Give the remote `name` the user's own client ID and secret, which
    /// must both be filled in.
    ChangeClientId {
        name: String,
    },
    /// Sign in again under a new name, with the credentials of `from`. The
    /// backend then moves the syncs of `from` to the new account and removes
    /// `from`.
    Rename {
        from: String,
    },
}

impl AccountMode {
    /// The existing remote this mode works on; `None` when it adds one.
    #[must_use]
    pub fn remote(&self) -> Option<&str> {
        match self {
            Self::Add => None,
            Self::Update { name } | Self::ChangeClientId { name } | Self::Rename { from: name } => {
                Some(name)
            }
        }
    }
}

/// What `Connect` asks the backend to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub mode: AccountMode,
    /// The remote's name: the new one, or the one being updated.
    pub name: String,
    pub client_id: String,
    pub client_secret: Secret,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountAction {
    Connect(ConnectRequest),
    /// Closes the sheet; the backend kills the sign-in if it is running.
    Cancel,
    /// `Choose Client File…`: the backend opens the file dialog.
    PickJson,
    /// A file was dropped on the window while the sheet was open.
    JsonDropped(PathBuf),
}

/// What the user typed. The app keeps it while the sheet is open.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountState {
    pub mode: AccountMode,
    /// Typed only when renaming; a new account is named by
    /// [`next_account_name`].
    pub name: String,
    pub client_id: String,
    pub client_secret: String,
}

impl fmt::Debug for AccountState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountState")
            .field("mode", &self.mode)
            .field("name", &self.name)
            .field("client_id", &self.client_id)
            .field("client_secret", &Secret::new(self.client_secret.clone()))
            .finish()
    }
}

impl AccountState {
    #[must_use]
    pub fn new(mode: AccountMode) -> Self {
        let name = match &mode {
            AccountMode::Add | AccountMode::Rename { .. } => String::new(),
            AccountMode::Update { name } | AccountMode::ChangeClientId { name } => name.clone(),
        };
        Self {
            mode,
            name,
            client_id: String::new(),
            client_secret: String::new(),
        }
    }
}

/// What the backend reports while the sheet is open.
#[derive(Debug, Clone, Copy)]
pub struct AccountView<'a> {
    pub existing: &'a [RemoteRow],
    /// `rclone config` is running and waits for the browser.
    pub connecting: bool,
    /// Why the sign-in or the client file failed.
    pub error: Option<&'a str>,
}

/// What is wrong with a new account's name, if anything.
#[must_use]
pub fn name_problem(name: &str, existing: &[RemoteRow]) -> Option<String> {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
        return Some(
            "Use letters, numbers, \"_\", \".\" and \"-\", and do not start with \".\" or \"-\"."
                .to_owned(),
        );
    }
    existing
        .iter()
        .any(|remote| remote.name == name)
        .then(|| format!("An account named \"{name}\" already exists."))
}

/// The name a new account gets: `drive`, else the first free `drive-N`.
#[must_use]
pub fn next_account_name<'a>(existing: impl IntoIterator<Item = &'a str>) -> String {
    let taken: BTreeSet<&str> = existing.into_iter().collect();
    std::iter::once("drive".to_owned())
        .chain((2..=taken.len() + 1).map(|n| format!("drive-{n}")))
        .find(|name| !taken.contains(name.as_str()))
        .unwrap_or_default()
}

fn can_connect(state: &AccountState, view: &AccountView<'_>) -> bool {
    if view.connecting {
        return false;
    }
    let has_credentials =
        !state.client_id.trim().is_empty() && !state.client_secret.trim().is_empty();
    match state.mode {
        AccountMode::Rename { .. } => {
            name_problem(&state.name, view.existing).is_none() && has_credentials
        }
        AccountMode::Add | AccountMode::ChangeClientId { .. } => has_credentials,
        AccountMode::Update { .. } => true,
    }
}

fn label_of<'a>(name: &'a str, existing: &'a [RemoteRow]) -> &'a str {
    existing
        .iter()
        .find(|remote| remote.name == name)
        .map_or(name, RemoteRow::label)
}

fn title(mode: &AccountMode, existing: &[RemoteRow]) -> String {
    match mode {
        AccountMode::Add => "Connect Google Drive".to_owned(),
        AccountMode::Update { name } => format!("Reconnect \"{}\"", label_of(name, existing)),
        AccountMode::ChangeClientId { name } => {
            format!("Client ID for \"{}\"", label_of(name, existing))
        }
        AccountMode::Rename { from } => format!("Rename \"{from}\""),
    }
}

fn note(ui: &mut Ui, text: &str) {
    let palette = theme::palette(ui.ctx());
    ui.label(
        RichText::new(text)
            .font(theme::callout())
            .color(palette.secondary),
    );
}

/// Draws the sheet and returns the `Action::Account(..)` the user chose.
#[must_use]
pub fn show(ctx: &egui::Context, view: &AccountView<'_>, state: &mut AccountState) -> Vec<Action> {
    let palette = theme::palette(ctx);
    let buttons = SheetButtons {
        primary: "Connect",
        primary_kind: PrimaryKind::Normal,
        primary_enabled: can_connect(state, view),
        back: false,
        cancel: true,
    };
    let mut actions = Vec::new();
    if !view.connecting {
        let dropped = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .first()
                .map(|file| file.path().to_path_buf())
        });
        if let Some(path) = dropped {
            actions.push(Action::Account(AccountAction::JsonDropped(path)));
        }
    }
    let title = title(&state.mode, view.existing);
    let chosen = sheet(ctx, "account", &title, &buttons, |ui| {
        if view.connecting {
            ui.label(
                RichText::new("Finish signing in in your browser\u{2026}")
                    .font(theme::body())
                    .color(palette.text),
            );
            progress_bar(ui, "Waiting for the browser", None);
            return;
        }
        body(ui, view, state, &mut actions);
    });
    match chosen {
        Some(SheetAction::Primary) => {
            let name = match &state.mode {
                AccountMode::Add => {
                    next_account_name(view.existing.iter().map(|remote| remote.name.as_str()))
                }
                _ => state.name.clone(),
            };
            actions.push(Action::Account(AccountAction::Connect(ConnectRequest {
                mode: state.mode.clone(),
                name,
                client_id: state.client_id.trim().to_owned(),
                client_secret: Secret::new(state.client_secret.trim().to_owned()),
            })));
        }
        Some(SheetAction::Cancel) => actions.push(Action::Account(AccountAction::Cancel)),
        Some(SheetAction::Back) | None => {}
    }
    actions
}

fn body(ui: &mut Ui, view: &AccountView<'_>, state: &mut AccountState, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    match &state.mode {
        AccountMode::Add => {
            note(
                ui,
                "syncfast reaches your Drive through a Google client of your own. Creating one takes a few minutes, once.",
            );
        }
        AccountMode::ChangeClientId { .. } => {
            note(
                ui,
                "Give this account a Google client of your own, then sign in again.",
            );
        }
        AccountMode::Update { .. } => {
            note(
                ui,
                "Sign in again in your browser. The account keeps its client unless you choose another one.",
            );
        }
        AccountMode::Rename { from } => {
            note(
                ui,
                &format!(
                    "Renaming signs in again with the new name. Syncs on \"{from}\" move to it and ask for a review."
                ),
            );
            text_field(ui, "Account name", &mut state.name, "gdrive");
            if !state.name.is_empty()
                && let Some(problem) = name_problem(&state.name, view.existing)
            {
                inline_error(ui, &problem);
            }
        }
    }
    let guide_open = matches!(
        state.mode,
        AccountMode::Add | AccountMode::ChangeClientId { .. }
    );
    egui::CollapsingHeader::new(
        RichText::new("Create Your Google Client")
            .font(theme::headline())
            .color(palette.text),
    )
    .id_salt("account-guide")
    .default_open(guide_open)
    .show(ui, |ui| guide(ui, actions));
    if secondary_button(ui, "Choose Client File\u{2026}").clicked() {
        actions.push(Action::Account(AccountAction::PickJson));
    }
    if !state.client_id.trim().is_empty() && !state.client_secret.trim().is_empty() {
        note(ui, &format!("Client ready: {}", state.client_id.trim()));
    }
    egui::CollapsingHeader::new(
        RichText::new("Enter Manually")
            .font(theme::callout())
            .color(palette.secondary),
    )
    .id_salt("account-manual")
    .default_open(false)
    .show(ui, |ui| {
        text_field(
            ui,
            "Client ID",
            &mut state.client_id,
            "123456-abc.apps.googleusercontent.com",
        );
        secret_field(ui, "Client secret", &mut state.client_secret);
    });
    if let Some(error) = view.error {
        inline_error(ui, error);
    }
}

fn guide(ui: &mut Ui, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    for (index, (text, url)) in GUIDE.iter().enumerate() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{}. {text}", index + 1))
                    .font(theme::body())
                    .color(palette.text),
            );
            if let Some(url) = url
                && ui
                    .link(
                        RichText::new("Open")
                            .font(theme::body())
                            .color(palette.accent),
                    )
                    .clicked()
            {
                actions.push(Action::OpenUrl((*url).to_owned()));
            }
        });
        if index == BRANDING_STEP {
            ui.add_space(space::XS);
            copy_row(ui, "Home page", HOMEPAGE_URL);
            copy_row(ui, "Privacy policy", PRIVACY_URL);
            ui.add_space(space::XS);
        }
    }
}

fn copy_row(ui: &mut Ui, label: &str, value: &str) {
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        if secondary_button(ui, "Copy").clicked() {
            ui.ctx().copy_text(value.to_owned());
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(
                RichText::new(label)
                    .font(theme::footnote())
                    .color(palette.secondary),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(value)
                        .font(theme::footnote())
                        .color(palette.text),
                )
                .truncate(),
            );
        });
    });
}
