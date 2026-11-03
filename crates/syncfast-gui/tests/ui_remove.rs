#![allow(clippy::unwrap_used)]

mod common;

use common::{harness, pair, view_model};
use egui::Key;
use egui_kittest::kittest::Queryable as _;
use syncfast_gui::ui::{self, Action, ViewModel};

fn removing(id: &str) -> ViewModel {
    ViewModel {
        removing: Some(id.to_owned()),
        ..view_model(vec![pair("notes", "Notes")], Some("notes"))
    }
}

fn screen() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::remove::show(ui.ctx(), vm)
}

#[test]
fn the_alert_names_the_sync_and_says_no_file_is_deleted() {
    let mut harness = harness(removing("notes"), screen());

    harness.run();

    harness.get_by_label("Remove \"Notes\"?");
    harness.get_by_label(
        "syncfast will stop syncing this folder. Files on this computer and on gdrive are not deleted.",
    );
    harness.get_by_label("Also delete saved versions");
}

#[test]
fn nothing_shows_while_no_sync_is_being_removed() {
    let vm = view_model(vec![pair("notes", "Notes")], Some("notes"));
    let mut harness = harness(vm, screen());

    harness.run();

    assert!(harness.query_by_label("Remove").is_none());
}

#[test]
fn remove_keeps_the_saved_versions_unless_the_box_is_checked() {
    let mut harness = harness(removing("notes"), screen());
    harness.run();

    harness.get_by_label("Remove").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::ConfirmRemove {
            id: "notes".to_owned(),
            delete_versions: false
        }]
    );
}

#[test]
fn remove_with_the_box_checked_also_deletes_the_versions() {
    let mut harness = harness(removing("notes"), screen());
    harness.run();
    harness.get_by_label("Also delete saved versions").click();
    harness.run();

    harness.get_by_label("Remove").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::ConfirmRemove {
            id: "notes".to_owned(),
            delete_versions: true
        }]
    );
}

#[test]
fn cancel_closes_the_alert_and_so_does_enter_and_escape() {
    let mut by_click = harness(removing("notes"), screen());
    by_click.run();
    by_click.get_by_label("Cancel").click();
    by_click.run();
    assert_eq!(by_click.state().actions, [Action::CancelRemove]);

    for key in [Key::Enter, Key::Escape] {
        let mut by_key = harness(removing("notes"), screen());
        by_key.run();
        by_key.key_press(key);
        by_key.run();
        assert_eq!(by_key.state().actions, [Action::CancelRemove], "{key:?}");
    }
}

#[test]
fn an_alert_for_a_sync_that_is_gone_closes_itself() {
    let mut harness = harness(removing("other"), screen());

    harness.run();

    assert_eq!(harness.state().actions.first(), Some(&Action::CancelRemove));
}
