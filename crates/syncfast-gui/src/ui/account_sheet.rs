//! The account sheet: adds a Google Drive remote with the user's own OAuth
//! client, or signs in again to one that exists.

use std::fmt;

use egui::RichText;

use crate::theme::{self, space};

use super::widgets::{
    PrimaryKind, SheetAction, SheetButtons, inline_error, progress_bar, secret_field, sheet,
    text_field,
};
use super::{Action, RemoteRow};

/// Where the user learns to make an OAuth client.
pub const CLIENT_ID_HELP_URL: &str = "https://rclone.org/drive/#making-your-own-client-id";

const HELP_TEXT: &str = "Create an OAuth client of type \"Desktop app\" in Google Cloud, enable the Google Drive API, and set the consent screen to \"In production\" so access does not expire every 7 days.";

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
}

/// What the user typed. The app keeps it while the sheet is open.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountState {
    pub mode: AccountMode,
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
    /// The last line rclone printed when the sign-in failed.
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

fn can_connect(state: &AccountState, view: &AccountView<'_>) -> bool {
    if view.connecting {
        return false;
    }
    let has_credentials =
        !state.client_id.trim().is_empty() && !state.client_secret.trim().is_empty();
    match state.mode {
        AccountMode::Add | AccountMode::Rename { .. } => {
            name_problem(&state.name, view.existing).is_none() && has_credentials
        }
        AccountMode::ChangeClientId { .. } => has_credentials,
        AccountMode::Update { .. } => true,
    }
}

fn title(mode: &AccountMode) -> String {
    match mode {
        AccountMode::Add => "Add Google Drive Account".to_owned(),
        AccountMode::Update { name } => format!("Reconnect \"{name}\""),
        AccountMode::ChangeClientId { name } => format!("Client ID for \"{name}\""),
        AccountMode::Rename { from } => format!("Rename \"{from}\""),
    }
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
    };
    let mut actions = Vec::new();
    let chosen = sheet(ctx, "account", &title(&state.mode), &buttons, |ui| {
        if view.connecting {
            ui.label(
                RichText::new("Finish signing in in your browser\u{2026}")
                    .font(theme::body())
                    .color(palette.text),
            );
            progress_bar(ui, "Waiting for the browser", None);
            return;
        }
        ui.label(
            RichText::new(HELP_TEXT)
                .font(theme::callout())
                .color(palette.secondary),
        );
        let help = ui.link(
            RichText::new("Making your own client ID")
                .font(theme::callout())
                .color(palette.accent),
        );
        if help.clicked() {
            actions.push(Action::OpenUrl(CLIENT_ID_HELP_URL.to_owned()));
        }
        ui.add_space(space::XS);
        match &state.mode {
            AccountMode::Add | AccountMode::Rename { .. } => {
                if let AccountMode::Rename { from } = &state.mode {
                    ui.label(
                        RichText::new(format!(
                            "Renaming signs in again with the new name. Syncs on \"{from}\" move to it and ask for a review."
                        ))
                        .font(theme::callout())
                        .color(palette.secondary),
                    );
                }
                text_field(ui, "Account name", &mut state.name, "gdrive");
                if !state.name.is_empty()
                    && let Some(problem) = name_problem(&state.name, view.existing)
                {
                    inline_error(ui, &problem);
                }
            }
            AccountMode::Update { .. } | AccountMode::ChangeClientId { .. } => {}
        }
        text_field(
            ui,
            "Client ID",
            &mut state.client_id,
            "123456-abc.apps.googleusercontent.com",
        );
        secret_field(ui, "Client secret", &mut state.client_secret);
        if let Some(error) = view.error {
            inline_error(ui, error);
        }
    });
    match chosen {
        Some(SheetAction::Primary) => {
            actions.push(Action::Account(AccountAction::Connect(ConnectRequest {
                mode: state.mode.clone(),
                name: state.name.clone(),
                client_id: state.client_id.trim().to_owned(),
                client_secret: Secret::new(state.client_secret.trim().to_owned()),
            })));
        }
        Some(SheetAction::Cancel) => actions.push(Action::Account(AccountAction::Cancel)),
        Some(SheetAction::Back) | None => {}
    }
    actions
}
