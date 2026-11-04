#![allow(clippy::unwrap_used)]

use egui::{Key, Ui};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_gui::theme::{self, Palette};
use syncfast_gui::ui::account_sheet::{
    AccountAction, AccountMode, AccountState, AccountView, ConnectRequest, Secret, name_problem,
    show,
};
use syncfast_gui::ui::{Action, RemoteRow};

struct Screen {
    state: AccountState,
    existing: Vec<RemoteRow>,
    connecting: bool,
    error: Option<String>,
    actions: Vec<Action>,
}

fn ready(ui: &Ui) -> bool {
    theme::install_with(
        ui.ctx(),
        &Palette::dark(),
        fastframe_text::TextRendering::platform_default(),
    );
    ui.ctx().fonts(|fonts| {
        fonts
            .families()
            .contains(&egui::FontFamily::Name("inter-semibold".into()))
    })
}

fn remote(name: &str) -> RemoteRow {
    RemoteRow {
        name: name.into(),
        kind: "drive".into(),
        own_client_id: true,
    }
}

fn harness(mode: AccountMode, tweak: impl FnOnce(&mut Screen)) -> Harness<'static, Screen> {
    let mut screen = Screen {
        state: AccountState::new(mode),
        existing: vec![remote("gdrive")],
        connecting: false,
        error: None,
        actions: Vec::new(),
    };
    tweak(&mut screen);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(960.0, 640.0))
        .build_ui_state(
            |ui, screen: &mut Screen| {
                if ready(ui) {
                    let view = AccountView {
                        existing: &screen.existing,
                        connecting: screen.connecting,
                        error: screen.error.as_deref(),
                    };
                    let actions = show(ui.ctx(), &view, &mut screen.state);
                    screen.actions.extend(actions);
                }
            },
            screen,
        );
    harness.run_ok();
    harness
}

fn filled(screen: &mut Screen) {
    screen.state.name = "trabalho".into();
    screen.state.client_id = "123.apps.googleusercontent.com".into();
    screen.state.client_secret = "hunter2".into();
}

#[test]
fn sheet_explains_the_oauth_client_and_links_to_the_rclone_guide() {
    let harness = harness(AccountMode::Add, |_| {});

    harness.get_by_label("Add Google Drive Account");
    harness.get_by_label(
        "Create an OAuth client of type \"Desktop app\" in Google Cloud, enable the Google Drive API, and set the consent screen to \"In production\" so access does not expire every 7 days.",
    );
    harness.get_by_label("Making your own client ID");
    harness.get_by_label("Account name");
    harness.get_by_label("Client ID");
    harness.get_by_label("Client secret");
}

#[test]
fn connect_is_disabled_until_name_client_id_and_secret_are_filled() {
    let empty = harness(AccountMode::Add, |_| {});
    let no_secret = harness(AccountMode::Add, |screen| {
        filled(screen);
        screen.state.client_secret.clear();
    });
    let complete = harness(AccountMode::Add, filled);

    assert!(empty.get_by_label("Connect").accesskit_node().is_disabled());
    assert!(
        no_secret
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        !complete
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn a_name_that_breaks_the_rule_shows_why_and_blocks_connect() {
    let harness = harness(AccountMode::Add, |screen| {
        filled(screen);
        screen.state.name = "-bad name".into();
    });

    harness.get_by_label_contains("Use letters, numbers");
    assert!(
        harness
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn a_name_that_an_account_already_has_is_refused_by_name() {
    let harness = harness(AccountMode::Add, |screen| {
        filled(screen);
        screen.state.name = "gdrive".into();
    });

    harness.get_by_label("An account named \"gdrive\" already exists.");
    assert!(
        harness
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn name_rule_accepts_letters_digits_underscore_dot_and_dash_after_the_first_character() {
    let existing = [remote("gdrive")];

    for good in ["a", "_a", "9lives", "my-drive.v2_final"] {
        assert_eq!(name_problem(good, &existing), None, "{good}");
    }
    for bad in ["", "-a", ".a", "a b", "á", "a/b"] {
        assert!(name_problem(bad, &existing).is_some(), "{bad}");
    }
}

#[test]
fn the_client_id_link_asks_the_backend_to_open_it() {
    let mut harness = harness(AccountMode::Add, |_| {});

    harness.get_by_label("Making your own client ID").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::OpenUrl(
            "https://rclone.org/drive/#making-your-own-client-id".into()
        )]
    );
}

#[test]
fn the_sheet_is_centered_in_the_window() {
    let harness = harness(AccountMode::Add, |_| {});

    let title = harness.get_by_label("Add Google Drive Account").rect();
    let button = harness.get_by_label("Connect").rect();
    let top = title.min.y;
    let bottom = button.max.y;

    let above = top;
    let below = 640.0 - bottom;
    assert!((above - below).abs() < 40.0, "above {above}, below {below}");
}

#[test]
fn connect_emits_the_request_with_trimmed_credentials() {
    let mut harness = harness(AccountMode::Add, |screen| {
        filled(screen);
        screen.state.client_id = "  123.apps.googleusercontent.com ".into();
    });

    harness.get_by_label("Connect").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::Account(AccountAction::Connect(ConnectRequest {
            mode: AccountMode::Add,
            name: "trabalho".into(),
            client_id: "123.apps.googleusercontent.com".into(),
            client_secret: Secret::new("hunter2".into()),
        }))]
    );
}

#[test]
fn a_secret_never_shows_in_debug_output() {
    let secret = Secret::new("hunter2".into());

    assert!(!format!("{secret:?}").contains("hunter2"));
}

#[test]
fn reconnecting_names_the_account_and_needs_no_new_credentials() {
    let harness = harness(
        AccountMode::Update {
            name: "gdrive".into(),
        },
        |_| {},
    );

    harness.get_by_label("Reconnect \"gdrive\"");
    assert!(
        !harness
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
    assert!(harness.query_by_label("Account name").is_none());
}

#[test]
fn reconnecting_sends_the_update_mode_and_the_account_name() {
    let mut harness = harness(
        AccountMode::Update {
            name: "gdrive".into(),
        },
        |screen| {
            screen.state.client_id = "123.apps.googleusercontent.com".into();
            screen.state.client_secret = "hunter2".into();
        },
    );

    harness.get_by_label("Connect").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::Account(AccountAction::Connect(ConnectRequest {
            mode: AccountMode::Update {
                name: "gdrive".into()
            },
            name: "gdrive".into(),
            client_id: "123.apps.googleusercontent.com".into(),
            client_secret: Secret::new("hunter2".into()),
        }))]
    );
}

#[test]
fn while_signing_in_the_sheet_waits_for_the_browser_and_blocks_connect() {
    let harness = harness(AccountMode::Add, |screen| {
        filled(screen);
        screen.connecting = true;
    });

    harness.get_by_label("Finish signing in in your browser\u{2026}");
    assert!(
        harness
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
    assert!(harness.query_by_label("Client ID").is_none());
}

#[test]
fn a_failed_sign_in_shows_the_last_line_rclone_printed() {
    let harness = harness(AccountMode::Add, |screen| {
        filled(screen);
        screen.error = Some("Failed to configure token: access_denied".into());
    });

    harness.get_by_label("Failed to configure token: access_denied");
}

#[test]
fn cancel_and_escape_both_close_the_sheet() {
    let mut by_button = harness(AccountMode::Add, |_| {});
    let mut by_key = harness(AccountMode::Add, |_| {});

    by_button.get_by_label("Cancel").click();
    by_button.run();
    by_key.key_press(Key::Escape);
    by_key.run();

    let cancel = [Action::Account(AccountAction::Cancel)];
    assert_eq!(by_button.state().actions, cancel);
    assert_eq!(by_key.state().actions, cancel);
}

#[test]
fn the_typed_state_never_shows_the_secret_in_debug_output() {
    let mut state = AccountState::new(AccountMode::Add);
    state.client_secret = "hunter2".into();

    let shown = format!("{state:?}");

    assert!(!shown.contains("hunter2"), "{shown}");
}

#[test]
fn using_my_client_id_names_the_account_and_needs_both_credentials() {
    let mode = || AccountMode::ChangeClientId {
        name: "gdrive".into(),
    };
    let empty = harness(mode(), |_| {});
    let filled = harness(mode(), |screen| {
        screen.state.client_id = "123.apps.googleusercontent.com".into();
        screen.state.client_secret = "hunter2".into();
    });

    empty.get_by_label("Use My Client ID for \"gdrive\"");
    assert!(empty.get_by_label("Connect").accesskit_node().is_disabled());
    assert!(
        !filled
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
    assert!(empty.query_by_label("Account name").is_none());
}
