#![allow(clippy::unwrap_used)]

mod common;

use common::{harness, pair, view_model};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::state::GateKind;
use syncfast_gui::gui_state::SettingsPane;
use syncfast_gui::theme::ThemeChoice;
use syncfast_gui::ui::settings::AppearanceView;
use syncfast_gui::ui::{self, AccountStatus, Action, RemoteRow, ViewModel};

const SHARED: &str = "Uses rclone's shared client ID, which stops working in 2026.";

fn remote(name: &str, kind: &str, own_client_id: bool) -> RemoteRow {
    RemoteRow {
        name: name.into(),
        kind: kind.into(),
        own_client_id,
    }
}

fn accounts(remotes: Vec<RemoteRow>) -> ViewModel {
    ViewModel {
        remotes,
        settings_pane: SettingsPane::Accounts,
        ..view_model(Vec::new(), None)
    }
}

fn screen() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::settings::content(ui, vm)
}

#[test]
fn only_a_drive_remote_without_its_own_client_id_gets_the_shared_client_warning() {
    let mut harness = harness(
        accounts(vec![
            remote("gdrive", "drive", false),
            remote("trabalho", "drive", true),
            remote("backup", "local", false),
        ]),
        screen(),
    );

    harness.run();

    assert_eq!(harness.query_all_by_label(SHARED).count(), 1);
    harness.get_by_label("gdrive · Google Drive");
    harness.get_by_label("trabalho · Google Drive");
    harness.get_by_label("backup · local");
    harness.get_by_label("Use My Client ID…");
}

#[test]
fn use_my_client_id_asks_to_change_the_client_id_of_that_remote() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", false)]), screen());
    harness.run();

    harness.get_by_label("Use My Client ID…").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::ChangeClientId("gdrive".to_owned())]
    );
}

#[test]
fn add_google_drive_account_asks_for_a_new_account() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    harness.get_by_label("Add Google Drive Account…").click();
    harness.run();

    assert_eq!(harness.state().actions, [Action::AddAccount]);
}

#[test]
fn a_remote_whose_sync_lost_its_sign_in_offers_reconnect() {
    let mut lost = pair("drive", "Drive");
    lost.gate = Some(GateKind::AuthError);
    let mut vm = accounts(vec![
        remote("gdrive", "drive", true),
        remote("trabalho", "drive", true),
    ]);
    vm.pairs = vec![lost];
    let mut harness = harness(vm, screen());

    harness.run();

    assert_eq!(harness.query_all_by_label("Reconnect…").count(), 1);
    harness.get_by_label("Signed out. Reconnect to resume syncing.");
    harness.get_by_label("Reconnect…").click();
    harness.run();
    assert_eq!(
        harness.state().actions,
        [Action::ReconnectAccount("gdrive".to_owned())]
    );
}

#[test]
fn without_a_lost_sign_in_there_is_no_reconnect() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());

    harness.run();

    assert!(harness.query_by_label("Reconnect…").is_none());
}

#[test]
fn the_sidebar_switches_panes() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    harness.get_by_label("Appearance").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::SetSettingsPane(SettingsPane::Appearance)]
    );
}

fn appearance(choice: ThemeChoice, errors: Vec<String>) -> ViewModel {
    ViewModel {
        settings_pane: SettingsPane::Appearance,
        appearance: AppearanceView {
            choice,
            themes: vec!["noctalia.json".into()],
            errors,
        },
        ..view_model(Vec::new(), None)
    }
}

#[test]
fn the_theme_selector_shows_the_current_choice_and_offers_the_catalog() {
    let mut harness = harness(appearance(ThemeChoice::System, Vec::new()), screen());
    harness.run();

    assert_eq!(
        harness
            .get_by_role(egui::accesskit::Role::ComboBox)
            .accesskit_node()
            .value(),
        Some("System".to_owned())
    );
    harness.get_by_role(egui::accesskit::Role::ComboBox).click();
    harness.run();

    harness.get_by_label("Light");
    harness.get_by_label("Dark");
    harness.get_by_label("noctalia");
}

#[test]
fn picking_a_theme_asks_to_save_it() {
    let mut harness = harness(appearance(ThemeChoice::System, Vec::new()), screen());
    harness.run();
    harness.get_by_role(egui::accesskit::Role::ComboBox).click();
    harness.run();

    harness.get_by_label("noctalia").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::SetTheme(ThemeChoice::Named("noctalia.json".into()))]
    );
}

#[test]
fn theme_files_that_could_not_be_read_are_listed() {
    let mut harness = harness(
        appearance(
            ThemeChoice::Dark,
            vec!["broken.json: expected value at line 1".into()],
        ),
        screen(),
    );

    harness.run();

    harness.get_by_label("broken.json: expected value at line 1");
}

#[test]
fn the_back_button_closes_settings() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    harness.get_by_label("Back").click();
    harness.run();

    assert_eq!(harness.state().actions, [Action::CloseSettings]);
}

#[test]
fn escape_closes_settings() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    harness.key_press(egui::Key::Escape);
    harness.run();

    assert_eq!(harness.state().actions, [Action::CloseSettings]);
}

#[test]
fn escape_with_the_theme_selector_open_only_closes_the_selector() {
    let mut harness = harness(appearance(ThemeChoice::System, Vec::new()), screen());
    harness.run();
    harness.get_by_role(egui::accesskit::Role::ComboBox).click();
    harness.run();

    harness.key_press(egui::Key::Escape);
    harness.run();

    assert!(harness.state().actions.is_empty());
}

fn with_status(remotes: Vec<RemoteRow>, name: &str, status: AccountStatus) -> ViewModel {
    let mut vm = accounts(remotes);
    vm.account_status.insert(name.to_owned(), status);
    vm
}

#[test]
fn a_problem_status_shows_the_detail_rclone_gave() {
    let vm = with_status(
        vec![remote("gdrive", "drive", true)],
        "gdrive",
        AccountStatus::Problem("Error 403: x".to_owned()),
    );
    let mut harness = harness(vm, screen());

    harness.run();

    harness.get_by_label("Problem: Error 403: x");
    assert!(harness.query_by_label("Reconnect…").is_none());
}

#[test]
fn the_status_line_says_checking_signed_in_and_offline() {
    for (status, text) in [
        (AccountStatus::Checking, "Checking…"),
        (AccountStatus::SignedIn, "Signed in"),
        (AccountStatus::Offline, "Offline. Not checked."),
    ] {
        let vm = with_status(vec![remote("gdrive", "drive", true)], "gdrive", status);
        let mut harness = harness(vm, screen());

        harness.run();

        harness.get_by_label(text);
    }
}

#[test]
fn an_account_without_a_status_yet_shows_no_status_line() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());

    harness.run();

    for text in ["Checking…", "Signed in", "Offline. Not checked."] {
        assert!(harness.query_by_label(text).is_none(), "{text}");
    }
}

#[test]
fn a_signed_out_status_offers_reconnect_and_says_so_once() {
    let mut lost = pair("drive", "Drive");
    lost.gate = Some(GateKind::AuthError);
    let mut vm = with_status(
        vec![remote("gdrive", "drive", true)],
        "gdrive",
        AccountStatus::SignedOut,
    );
    vm.pairs = vec![lost];
    let mut harness = harness(vm, screen());

    harness.run();

    assert_eq!(
        harness
            .query_all_by_label("Signed out. Reconnect to resume syncing.")
            .count(),
        1
    );
    harness.get_by_label("Reconnect…").click();
    harness.run();
    assert_eq!(
        harness.state().actions,
        [Action::ReconnectAccount("gdrive".to_owned())]
    );
}

#[test]
fn a_signed_out_status_alone_offers_reconnect() {
    let vm = with_status(
        vec![remote("gdrive", "drive", true)],
        "gdrive",
        AccountStatus::SignedOut,
    );
    let mut harness = harness(vm, screen());

    harness.run();

    harness.get_by_label("Signed out. Reconnect to resume syncing.");
    harness.get_by_label("Reconnect…");
}

#[test]
fn a_drive_row_with_its_own_client_id_offers_change_rename_and_remove() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());
    harness.run();

    harness.get_by_label("Change Client ID…");
    harness.get_by_label("Rename…");
    harness.get_by_label("Remove…");
    assert!(harness.query_by_label("Use My Client ID…").is_none());

    harness.get_by_label("Change Client ID…").click();
    harness.run();
    assert_eq!(
        harness.state().actions,
        [Action::ChangeClientId("gdrive".to_owned())]
    );
}

#[test]
fn rename_asks_to_rename_that_account() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());
    harness.run();

    harness.get_by_label("Rename…").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::RenameAccount("gdrive".to_owned())]
    );
}

#[test]
fn remove_asks_to_remove_that_account() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());
    harness.run();

    harness.get_by_label("Remove…").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::RemoveAccount("gdrive".to_owned())]
    );
}

#[test]
fn a_local_remote_can_only_be_removed() {
    let mut harness = harness(accounts(vec![remote("backup", "local", false)]), screen());

    harness.run();

    harness.get_by_label("Remove…");
    for text in ["Rename…", "Change Client ID…", "Use My Client ID…"] {
        assert!(harness.query_by_label(text).is_none(), "{text}");
    }
}
