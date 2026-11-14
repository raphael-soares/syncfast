#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::{harness, pair, view_model};
use egui_kittest::kittest::Queryable as _;
use syncfast_core::recently_deleted::{DeletedItem, DeletedKind, DeletedList, DeletedSide};
use syncfast_gui::ui::{self, Action, ViewModel};

fn kept(side: DeletedSide, path: &str, key: &str) -> DeletedItem {
    DeletedItem {
        side,
        kind: DeletedKind::Deleted,
        path: path.to_owned(),
        when: (side != DeletedSide::Drive).then(|| "2026-11-03T10:00:00Z".parse().unwrap()),
        kept_until: None,
        size: 10,
        key: key.to_owned(),
    }
}

fn with(deleted: Option<Result<DeletedList, String>>) -> ViewModel {
    ViewModel {
        deleted: deleted.map(|result| result.map(Arc::new)),
        ..view_model(vec![pair("notes", "Notes")], Some("notes"))
    }
}

fn listed(items: Vec<DeletedItem>, warning: Option<&str>) -> ViewModel {
    with(Some(Ok(DeletedList {
        items,
        warning: warning.map(str::to_owned),
    })))
}

fn screen() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::recently_deleted::show(ui, vm)
}

#[test]
fn each_row_names_the_file_and_where_it_can_come_back_from() {
    let mut harness = harness(
        listed(
            vec![
                kept(
                    DeletedSide::Local,
                    "docs/a.txt",
                    "20261103-100000/docs/a.txt",
                ),
                kept(DeletedSide::Drive, "b.txt", "id-b"),
            ],
            None,
        ),
        screen(),
    );

    harness.run();

    harness.get_by_label("docs/a.txt");
    harness.get_by_label("Deleted on this computer 2 h ago");
    harness.get_by_label("b.txt");
    harness.get_by_label("In the Drive trash");
}

#[test]
fn restore_asks_for_the_file_of_that_row() {
    let mut harness = harness(
        listed(
            vec![
                kept(DeletedSide::Local, "a.txt", "20261103-100000/a.txt"),
                kept(DeletedSide::Drive, "b.txt", "id-b"),
            ],
            None,
        ),
        screen(),
    );
    harness.run();

    harness.get_all_by_label("Restore").nth(1).unwrap().click();
    harness.run();

    let restores: Vec<_> = harness
        .state()
        .actions
        .iter()
        .filter(|action| matches!(action, Action::RestoreDeleted { .. }))
        .collect();
    assert_eq!(
        restores,
        [&Action::RestoreDeleted {
            id: "notes".to_owned(),
            key: "id-b".to_owned(),
            side: DeletedSide::Drive,
        }]
    );
}

#[test]
fn an_empty_list_says_nothing_was_deleted_and_offers_no_restore() {
    let mut harness = harness(listed(Vec::new(), None), screen());

    harness.run();

    harness.get_by_label("Nothing deleted recently");
    assert!(harness.query_by_label("Restore").is_none());
}

#[test]
fn a_sync_that_keeps_no_versions_says_where_to_change_it() {
    let mut vm = listed(Vec::new(), None);
    vm.pairs[0].effective.behavior.versions = false;
    let mut harness = harness(vm, screen());

    harness.run();

    harness.get_by_label("Files deleted on this computer are not kept. Change this in Options.");
}

#[test]
fn the_list_says_it_is_checking_until_it_is_read_and_asks_for_it_once() {
    let mut harness = harness(with(None), screen());

    harness.run();
    harness.run();

    harness.get_by_label("Checking…");
    let refreshes = harness
        .state()
        .actions
        .iter()
        .filter(|action| **action == Action::RefreshDeleted("notes".into()))
        .count();
    assert_eq!(refreshes, 1);
}

#[test]
fn a_trash_that_could_not_be_read_is_said_above_the_local_files() {
    let warning = "cannot read the Drive trash: offline";
    let mut harness = harness(
        listed(
            vec![kept(DeletedSide::Local, "a.txt", "20261103-100000/a.txt")],
            Some(warning),
        ),
        screen(),
    );

    harness.run();

    harness.get_by_label(warning);
    harness.get_by_label("a.txt");
}

#[test]
fn a_list_that_could_not_be_read_says_why() {
    let mut harness = harness(with(Some(Err("rclone is missing".to_owned()))), screen());

    harness.run();

    harness.get_by_label("rclone is missing");
}
