#![allow(clippy::unwrap_used)]

mod common;

use common::{harness, pair, view_model};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::filters::ExclusionSummary;
use syncfast_gui::ui::filters::{FilterRules, FilterTest, FilterTestState};
use syncfast_gui::ui::{self, Action, PairRow, ViewModel};

fn view(pair: PairRow) -> ViewModel {
    view_model(vec![pair], Some("notes"))
}

fn notes(filters: &[&str]) -> PairRow {
    PairRow {
        filters: filters.iter().map(|line| (*line).to_owned()).collect(),
        ..pair("notes", "Notes")
    }
}

fn screen() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::filters::show(ui, vm)
}

fn is_disabled(harness: &egui_kittest::Harness<'_, common::Frame>, label: &str) -> bool {
    harness.get_by_label(label).accesskit_node().is_disabled()
}

#[test]
fn both_presets_show_their_state_and_their_rules_behind_a_disclosure() {
    let mut vm = view(notes(&[]));
    vm.pairs[0].presets = vec!["dev_dirs".into()];
    let mut harness = harness(vm, screen());

    harness.run();

    harness.get_by_label("System junk files");
    harness.get_by_label("Developer folders");
    assert_eq!(harness.query_all_by_label("Show rules").count(), 2);
    harness.get_by_label("Custom rules");
}

#[test]
fn a_custom_line_without_a_sign_gets_an_error_and_blocks_save_and_test() {
    let mut harness = harness(view(notes(&["- *.bak", "tmp"])), screen());

    harness.run();

    harness.get_by_label("Filter line 2 must start with \"+ \" or \"- \"");
    assert!(is_disabled(&harness, "Save"));
    assert!(is_disabled(&harness, "Test Filters"));
}

#[test]
fn typing_a_bad_line_shows_its_error_at_once() {
    let mut harness = harness(view(notes(&[])), screen());
    let field = harness.get_by_label("Custom rules");
    field.click();
    field.type_text("*.tmp");

    harness.run();

    harness.get_by_label("Filter line 1 must start with \"+ \" or \"- \"");
}

#[test]
fn save_stays_disabled_until_something_changes() {
    let mut harness = harness(view(notes(&["- *.bak"])), screen());

    harness.run();

    assert!(is_disabled(&harness, "Save"));
    harness.get_by_label("Save").click();
    harness.run();
    assert!(harness.state().actions.is_empty());
}

#[test]
fn turning_a_preset_off_enables_save_and_saves_the_rest() {
    let mut harness = harness(view(notes(&["- *.bak"])), screen());
    harness.run();

    harness.get_by_label("System junk files").click();
    harness.run();
    assert!(!is_disabled(&harness, "Save"));
    harness.get_by_label("Save").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::SaveFilters {
            id: "notes".to_owned(),
            presets: vec!["dev_dirs".to_owned()],
            rules: vec!["- *.bak".to_owned()],
        }]
    );
}

#[test]
fn test_filters_runs_against_the_local_folder_with_the_rules_on_screen() {
    let mut harness = harness(view(notes(&["- *.bak"])), screen());
    harness.run();

    harness.get_by_label("Test Filters").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::TestFilters {
            id: "notes".to_owned(),
            local: "/home/ada/notes".into(),
            presets: vec!["system_junk".to_owned(), "dev_dirs".to_owned()],
            rules: vec!["- *.bak".to_owned()],
        }]
    );
}

fn tested(state: FilterTestState) -> FilterTest {
    FilterTest {
        id: "notes".into(),
        rules: FilterRules {
            presets: vec!["system_junk".into(), "dev_dirs".into()],
            custom: vec!["- *.bak".into()],
        },
        state,
    }
}

#[test]
fn a_test_result_says_how_many_files_are_excluded_and_where() {
    let mut vm = view(notes(&["- *.bak"]));
    vm.filter_test = Some(tested(FilterTestState::Done(ExclusionSummary {
        total: 10,
        excluded: 3,
        folders: vec![("target".into(), 2), ("docs".into(), 1)],
    })));
    let mut harness = harness(vm, screen());

    harness.run();

    harness.get_by_label("3 of 10 files excluded");
    harness.get_by_label("target · 2 files");
    harness.get_by_label("docs · 1 files");
}

#[test]
fn a_result_for_other_rules_is_not_shown() {
    let mut vm = view(notes(&[]));
    vm.filter_test = Some(tested(FilterTestState::Running));
    let mut harness = harness(vm, screen());

    harness.run();

    assert!(harness.query_by_label("Testing…").is_none());
}

#[test]
fn a_failed_test_shows_the_reason_below_the_buttons() {
    let mut vm = view(notes(&["- *.bak"]));
    vm.filter_test = Some(tested(FilterTestState::Failed(
        "directory not found".to_owned(),
    )));
    let mut harness = harness(vm, screen());

    harness.run();

    harness.get_by_label("directory not found");
}

#[test]
fn after_a_save_the_screen_warns_that_the_next_sync_asks_for_a_review() {
    let mut vm = view(notes(&["- *.bak"]));
    vm.filters_saved = true;
    let mut harness = harness(vm, screen());

    harness.run();

    harness.get_by_label("Saved. The next sync will ask you to review the changes.");
}
