#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::{harness, pair, view_model};
use egui_kittest::kittest::Queryable as _;
use syncfast_core::conflicts::{Conflict, FileFacts, Resolution};
use syncfast_gui::ui::{self, Action, ViewModel};

fn conflict(original: &str, current: u64, conflicted: u64) -> Conflict {
    Conflict {
        path: PathBuf::from(format!("{original}.conflict1")),
        original: PathBuf::from(original),
        current: Some(FileFacts {
            size: current,
            modified: None,
        }),
        conflicted: FileFacts {
            size: conflicted,
            modified: None,
        },
    }
}

fn with_conflicts(conflicts: Vec<Conflict>) -> ViewModel {
    ViewModel {
        conflicts,
        ..view_model(vec![pair("notes", "Notes")], Some("notes"))
    }
}

fn two_conflicts() -> ViewModel {
    with_conflicts(vec![
        conflict("inbox/2026-11-02.md", 4200, 3900),
        conflict("principios/scrum.md", 12_288, 11_264),
    ])
}

fn screen() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::conflicts::show(ui, vm)
}

fn resolutions(actions: &[Action]) -> Vec<(String, PathBuf, Resolution)> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::ResolveConflict {
                id,
                path,
                resolution,
            } => Some((id.clone(), path.clone(), *resolution)),
            _ => None,
        })
        .collect()
}

#[test]
fn each_row_shows_the_file_and_both_sizes() {
    let mut harness = harness(two_conflicts(), screen());

    harness.run();

    harness.get_by_label("inbox/2026-11-02.md");
    harness.get_by_label("principios/scrum.md");
    harness.get_by_label("Current: 4.2 KB · Conflicted copy: 3.9 KB");
}

#[test]
fn keep_current_asks_to_settle_the_conflict_of_that_row() {
    let mut harness = harness(two_conflicts(), screen());
    harness.run();

    harness
        .get_all_by_label("Keep Current")
        .nth(1)
        .unwrap()
        .click();
    harness.run();

    assert_eq!(
        resolutions(&harness.state().actions),
        [(
            "notes".to_owned(),
            PathBuf::from("principios/scrum.md.conflict1"),
            Resolution::KeepCurrent
        )]
    );
}

#[test]
fn keep_conflicted_copy_asks_to_settle_the_conflict_of_that_row() {
    let mut harness = harness(two_conflicts(), screen());
    harness.run();

    harness
        .get_all_by_label("Keep Conflicted Copy")
        .next()
        .unwrap()
        .click();
    harness.run();

    assert_eq!(
        resolutions(&harness.state().actions),
        [(
            "notes".to_owned(),
            PathBuf::from("inbox/2026-11-02.md.conflict1"),
            Resolution::KeepConflicted
        )]
    );
}

#[test]
fn keep_both_asks_to_settle_the_conflict_of_that_row() {
    let mut harness = harness(two_conflicts(), screen());
    harness.run();

    harness
        .get_all_by_label("Keep Both")
        .next()
        .unwrap()
        .click();
    harness.run();

    assert_eq!(
        resolutions(&harness.state().actions),
        [(
            "notes".to_owned(),
            PathBuf::from("inbox/2026-11-02.md.conflict1"),
            Resolution::KeepBoth
        )]
    );
}

#[test]
fn without_conflicts_the_tab_says_so_and_offers_no_action() {
    let mut harness = harness(with_conflicts(Vec::new()), screen());

    harness.run();

    harness.get_by_label("No conflicts");
    assert!(harness.query_by_label("Keep Both").is_none());
}

#[test]
fn opening_the_tab_asks_to_look_for_conflicts_once() {
    let mut harness = harness(two_conflicts(), screen());

    harness.run();
    harness.run();

    let refreshes = harness
        .state()
        .actions
        .iter()
        .filter(|action| **action == Action::RefreshConflicts("notes".into()))
        .count();
    assert_eq!(refreshes, 1);
}
