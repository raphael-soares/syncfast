#![allow(clippy::unwrap_used)]

use egui::{Key, Ui};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_gui::backend::Event;
use syncfast_gui::sheets::{Sheets, Surroundings};
use syncfast_gui::theme::{self, Palette};
use syncfast_gui::ui::account_sheet::{
    AccountAction, AccountMode, AccountState, AccountView, ConnectRequest, GUIDE, HOMEPAGE_URL,
    PRIVACY_URL, Secret, name_problem, next_account_name, show,
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
        email: None,
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
fn adding_an_account_walks_through_the_google_client_and_asks_for_no_name() {
    let harness = harness(AccountMode::Add, |_| {});

    harness.get_by_label("Connect Google Drive");
    for (index, (text, _)) in GUIDE.iter().enumerate() {
        harness.get_by_label(&format!("{}. {text}", index + 1));
    }
    harness.get_by_label(HOMEPAGE_URL);
    harness.get_by_label(PRIVACY_URL);
    harness.get_by_label("Choose Client File\u{2026}");
    assert!(harness.query_by_label("Account name").is_none());
    assert!(harness.query_by_label("Client ID").is_none());
}

#[test]
fn the_guide_opens_each_google_cloud_page_through_the_backend() {
    let mut harness = harness(AccountMode::Add, |_| {});
    let links = harness.get_all_by_label("Open").count();

    for index in 0..links {
        harness.get_all_by_label("Open").nth(index).unwrap().click();
        harness.run();
    }

    let expected: Vec<Action> = GUIDE
        .iter()
        .filter_map(|(_, url)| url.map(|url| Action::OpenUrl(url.to_owned())))
        .collect();
    assert_eq!(expected.len(), 5);
    assert_eq!(harness.state().actions, expected);
}

fn click(harness: &mut Harness<'_, Screen>, label: &str) {
    harness.get_by_label(label).scroll_to_me();
    harness.run_steps(4);
    harness.run();
    harness.get_by_label(label).click();
    harness.run();
}

#[test]
fn choose_client_file_asks_the_backend_for_the_file() {
    let mut harness = harness(AccountMode::Add, |_| {});

    click(&mut harness, "Choose Client File\u{2026}");

    assert_eq!(
        harness.state().actions,
        [Action::Account(AccountAction::PickJson)]
    );
}

#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not read in tests".into())
    }
}

#[test]
fn a_file_dropped_on_the_window_is_sent_to_the_backend() {
    let mut harness = harness(AccountMode::Add, |_| {});

    harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(
            "/home/ada/Downloads/client.json".into(),
        )));
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::Account(AccountAction::JsonDropped(
            "/home/ada/Downloads/client.json".into()
        ))]
    );
}

#[test]
fn the_client_fields_stay_folded_under_enter_manually() {
    let mut harness = harness(AccountMode::Add, |_| {});

    click(&mut harness, "Enter Manually");

    harness.get_by_label("Client ID");
    harness.get_by_label("Client secret");
}

#[test]
fn connect_is_disabled_until_client_id_and_secret_are_filled() {
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
fn a_new_account_is_named_drive_then_the_first_free_drive_number() {
    assert_eq!(next_account_name(["gdrive"]), "drive");
    assert_eq!(next_account_name(["drive"]), "drive-2");
    assert_eq!(
        next_account_name(["drive", "drive-2", "drive-4"]),
        "drive-3"
    );
}

#[test]
fn the_second_account_is_connected_as_drive_2() {
    let mut harness = harness(AccountMode::Add, |screen| {
        filled(screen);
        screen.existing = vec![remote("drive")];
    });

    harness.get_by_label("Connect").click();
    harness.run();

    assert!(matches!(
        harness.state().actions.as_slice(),
        [Action::Account(AccountAction::Connect(ConnectRequest { name, .. }))] if name == "drive-2"
    ));
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
fn the_sheet_is_centered_in_the_window() {
    let harness = harness(
        AccountMode::Update {
            name: "gdrive".into(),
        },
        |_| {},
    );

    let title = harness.get_by_label("Reconnect \"gdrive\"").rect();
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
            name: "drive".into(),
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
fn reconnecting_names_the_account_folds_the_guide_and_needs_no_new_credentials() {
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
    assert!(harness.query_by_label(HOMEPAGE_URL).is_none());
}

#[test]
fn reconnecting_titles_the_account_by_its_email_when_known() {
    let harness = harness(
        AccountMode::Update {
            name: "gdrive".into(),
        },
        |screen| {
            screen.existing = vec![RemoteRow {
                email: Some("ada@example.com".into()),
                ..remote("gdrive")
            }];
        },
    );

    harness.get_by_label("Reconnect \"ada@example.com\"");
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

    empty.get_by_label("Client ID for \"gdrive\"");
    assert!(empty.get_by_label("Connect").accesskit_node().is_disabled());
    assert!(
        !filled
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
    assert!(empty.query_by_label("Account name").is_none());
}

fn rename() -> AccountMode {
    AccountMode::Rename {
        from: "gdrive".into(),
    }
}

#[test]
fn renaming_names_the_old_account_and_explains_the_new_sign_in() {
    let harness = harness(rename(), |_| {});

    harness.get_by_label("Rename \"gdrive\"");
    harness.get_by_label(
        "Renaming signs in again with the new name. Syncs on \"gdrive\" move to it and ask for a review.",
    );
    harness.get_by_label("Account name");
    harness.get_by_label("Connect");
}

#[test]
fn renaming_starts_with_an_empty_name_and_keeps_connect_disabled_until_it_is_new() {
    let empty = harness(rename(), |_| {});
    let taken = harness(rename(), |screen| {
        filled(screen);
        screen.state.name = "gdrive".into();
    });
    let no_secret = harness(rename(), |screen| {
        filled(screen);
        screen.state.client_secret.clear();
    });
    let ready = harness(rename(), filled);

    assert_eq!(AccountState::new(rename()).name, "");
    assert!(empty.get_by_label("Connect").accesskit_node().is_disabled());
    taken.get_by_label("An account named \"gdrive\" already exists.");
    assert!(taken.get_by_label("Connect").accesskit_node().is_disabled());
    assert!(
        no_secret
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
    assert!(!ready.get_by_label("Connect").accesskit_node().is_disabled());
}

#[test]
fn renaming_sends_the_rename_mode_with_the_new_name() {
    let mut harness = harness(rename(), filled);

    harness.get_by_label("Connect").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::Account(AccountAction::Connect(ConnectRequest {
            mode: rename(),
            name: "trabalho".into(),
            client_id: "123.apps.googleusercontent.com".into(),
            client_secret: Secret::new("hunter2".into()),
        }))]
    );
}

fn sheets_with(event: &Event) -> Harness<'static, Sheets> {
    let mut sheets = Sheets::new("/home/ada".into(), "/home/ada/.local/share/syncfast".into());
    sheets.apply(
        &Action::AddAccount,
        &Surroundings {
            pairs: &[],
            welcome: false,
        },
    );
    sheets.handle_event(event);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(960.0, 640.0))
        .build_ui_state(
            |ui, sheets: &mut Sheets| {
                if ready(ui) {
                    let _ = sheets.show(ui.ctx(), &[], None);
                }
            },
            sheets,
        );
    harness.run_ok();
    harness
}

#[test]
fn a_client_file_read_by_the_backend_fills_the_client_and_allows_connect() {
    let harness = sheets_with(&Event::ClientJson(Ok((
        "123.apps.googleusercontent.com".into(),
        Secret::new("hunter2".into()),
    ))));

    harness.get_by_label("Client ready: 123.apps.googleusercontent.com");
    assert!(
        !harness
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn a_refused_client_file_says_why_and_keeps_connect_disabled() {
    let harness = sheets_with(&Event::ClientJson(Err(
        "This is not a Google client file.".into()
    )));

    harness.get_by_label("This is not a Google client file.");
    assert!(
        harness
            .get_by_label("Connect")
            .accesskit_node()
            .is_disabled()
    );
}
