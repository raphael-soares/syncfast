#![allow(clippy::unwrap_used)]

mod common;

use common::{harness, pair, view_model};
use egui::Key;
use egui_kittest::kittest::Queryable as _;
use syncfast_gui::ui::{self, Action, ViewModel};

fn removing(account: &str) -> ViewModel {
    let mut elsewhere = pair("work", "Work");
    "other:work".clone_into(&mut elsewhere.remote);
    ViewModel {
        removing_account: Some(account.to_owned()),
        ..view_model(
            vec![pair("notes", "Notes"), pair("docs", "Docs"), elsewhere],
            None,
        )
    }
}

fn screen() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::remove_account::show(ui.ctx(), vm)
}

#[test]
fn the_alert_lists_the_syncs_that_stop_until_the_account_is_back() {
    let mut harness = harness(removing("gdrive"), screen());

    harness.run();

    harness.get_by_label("Remove \"gdrive\"?");
    harness.get_by_label(
        "These syncs stop until the account is back: Notes, Docs. Files on both sides are not deleted.",
    );
}

#[test]
fn an_account_without_syncs_says_only_that_syncfast_forgets_it() {
    let mut harness = harness(removing("other-unused"), screen());

    harness.run();

    harness.get_by_label("Remove \"other-unused\"?");
    harness.get_by_label("syncfast forgets this account. Files on Google Drive are not deleted.");
}

#[test]
fn nothing_shows_while_no_account_is_being_removed() {
    let vm = view_model(vec![pair("notes", "Notes")], None);
    let mut harness = harness(vm, screen());

    harness.run();

    assert!(harness.query_by_label("Remove").is_none());
}

#[test]
fn remove_confirms_the_account() {
    let mut harness = harness(removing("gdrive"), screen());
    harness.run();

    harness.get_by_label("Remove").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::ConfirmRemoveAccount("gdrive".to_owned())]
    );
}

#[test]
fn cancel_closes_the_alert_and_so_does_enter_and_escape() {
    let mut by_click = harness(removing("gdrive"), screen());
    by_click.run();
    by_click.get_by_label("Cancel").click();
    by_click.run();
    assert_eq!(by_click.state().actions, [Action::CancelRemoveAccount]);

    for key in [Key::Enter, Key::Escape] {
        let mut by_key = harness(removing("gdrive"), screen());
        by_key.run();
        by_key.key_press(key);
        by_key.run();
        assert_eq!(
            by_key.state().actions,
            [Action::CancelRemoveAccount],
            "{key:?}"
        );
    }
}
