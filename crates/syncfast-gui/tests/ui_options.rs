#![allow(clippy::unwrap_used)]

mod common;

use common::{Frame, from_config, harness, harness_in};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::config::SettingScope;
use syncfast_gui::theme::Palette;
use syncfast_gui::ui::{self, Action, DetailTab, ViewModel};

const NOTES: &str = "[[pair]]\nid = \"notes\"\nname = \"Notes\"\nlocal = \"/home/ada/notes\"\nremote = \"gdrive:notes\"\n";

fn config(defaults: &str, pair_settings: &str) -> String {
    let defaults = if defaults.is_empty() {
        String::new()
    } else {
        format!("[defaults]\n{defaults}\n")
    };
    format!("version = 2\n\n{defaults}{NOTES}{pair_settings}\n")
}

fn options(defaults: &str, pair_settings: &str) -> ViewModel {
    ViewModel {
        tab: DetailTab::Options,
        ..from_config(&config(defaults, pair_settings), Some("notes"))
    }
}

fn screen() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::options::sync(ui, vm)
}

fn notes() -> SettingScope {
    SettingScope::Sync("notes".to_owned())
}

fn set(key: &str, value: &str) -> Action {
    Action::SetSetting {
        scope: notes(),
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

/// A focused field blinks its caret, so the frames are counted, not awaited.
fn type_into(harness: &mut Harness<'_, Frame>, label: &str, text: &str) {
    let field = harness.get_by_role_and_label(Role::TextInput, label);
    field.click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(text);
    harness.run_steps(2);
}

fn commit(harness: &mut Harness<'_, Frame>) {
    harness.key_press(egui::Key::Enter);
    harness.run_steps(4);
}

#[test]
fn the_rows_are_grouped_in_sections() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    for title in ["Schedule", "Conflicts", "Versions", "Deletions", "Advanced"] {
        harness.get_by_label(title);
    }
    for row in [
        "Watch for changes",
        "Wait after changes",
        "Longest wait while editing",
        "Check Drive every",
        "When both sides change",
        "The losing version",
        "Keep replaced files",
        "Keep for",
        "Stop when more than this share would be deleted",
        "When a first sync finds different files",
        "Sync empty folders",
    ] {
        assert!(harness.query_all_by_label(row).count() >= 1, "{row}");
    }
}

#[test]
fn a_setting_that_is_not_overridden_says_which_default_it_follows() {
    let mut harness = harness(options("interval = \"15m\"", ""), screen());
    harness.run();

    harness.get_by_label("Default: 15 min");
    harness.get_by_label("Default: 30 s");
    harness.get_by_label("Default: 30 days");
    assert!(harness.query_by_label("Reset to Default").is_none());
}

#[test]
fn an_overridden_setting_says_what_it_overrides_and_can_be_reset() {
    let mut harness = harness(options("", "interval = \"30m\"\n"), screen());
    harness.run();

    harness.get_by_label("Overrides the default (5 min)");
    harness.get_by_label("Reset to Default").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::ResetSetting {
            scope: notes(),
            key: "interval".to_owned(),
        }]
    );
}

#[test]
fn a_valid_duration_is_saved_when_the_field_is_committed() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    type_into(&mut harness, "Check Drive every", "10m");
    assert!(harness.state().actions.is_empty(), "nothing before Enter");
    commit(&mut harness);

    assert_eq!(harness.state().actions, [set("interval", "10m")]);
}

#[test]
fn an_invalid_duration_shows_the_reason_and_saves_nothing() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    type_into(&mut harness, "Wait after changes", "1s");
    commit(&mut harness);

    harness.get_by_label("debounce must be at least 5s");
    assert!(harness.state().actions.is_empty());
}

#[test]
fn a_refused_text_stays_in_the_field_until_it_is_fixed() {
    let mut harness = harness(options("", ""), screen());
    harness.run();
    type_into(&mut harness, "Wait after changes", "1s");
    commit(&mut harness);

    harness.run_steps(3);
    assert_eq!(
        harness
            .get_by_role_and_label(Role::TextInput, "Wait after changes")
            .accesskit_node()
            .value(),
        Some("1s".to_owned())
    );

    type_into(&mut harness, "Wait after changes", "8s");
    assert!(
        harness
            .query_by_label("debounce must be at least 5s")
            .is_none(),
        "typing clears the message"
    );
    commit(&mut harness);
    assert_eq!(harness.state().actions, [set("debounce", "8s")]);
}

#[test]
fn a_number_outside_its_range_shows_the_reason_and_saves_nothing() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    type_into(
        &mut harness,
        "Stop when more than this share would be deleted",
        "0",
    );
    commit(&mut harness);

    assert!(harness.state().actions.is_empty());
    assert!(
        harness
            .query_all_by_label_contains("max_delete")
            .next()
            .is_some()
    );
}

#[test]
fn committing_the_value_that_is_already_set_asks_for_nothing() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    harness
        .get_by_role_and_label(Role::TextInput, "Check Drive every")
        .click();
    harness.run_steps(2);
    commit(&mut harness);

    assert!(harness.state().actions.is_empty());
}

#[test]
fn flipping_a_switch_saves_the_new_value() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    harness
        .get_by_role_and_label(Role::CheckBox, "Watch for changes")
        .click();
    harness.run();

    assert_eq!(harness.state().actions, [set("watch", "false")]);
}

#[test]
fn picking_a_choice_saves_its_name() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    harness
        .get_by_role_and_label(Role::ComboBox, "When both sides change")
        .click();
    harness.run();
    harness.get_by_label("Larger wins").click();
    harness.run();

    assert_eq!(harness.state().actions, [set("conflict_resolve", "larger")]);
}

#[test]
fn picking_the_choice_already_set_asks_for_nothing() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    harness
        .get_by_role_and_label(Role::ComboBox, "The losing version")
        .click();
    harness.run();
    harness.get_by_label("Rename as .conflictN").click();
    harness.run();

    assert!(harness.state().actions.is_empty());
}

#[test]
fn a_choice_names_the_option_it_is_on() {
    let mut harness = harness(options("", "conflict_resolve = \"remote\"\n"), screen());
    harness.run();

    assert_eq!(
        harness
            .get_by_role_and_label(Role::ComboBox, "When both sides change")
            .accesskit_node()
            .value(),
        Some("Drive wins".to_owned())
    );
}

#[test]
fn a_sync_that_lowers_safety_says_so() {
    let mut harness = harness(options("", "versions = false\n"), |ui, vm| {
        ui::detail::show(ui, vm)
    });
    harness.run();

    harness.get_by_label("Safety lowered");
}

#[test]
fn a_sync_with_the_built_in_safety_has_no_badge() {
    let mut harness = harness(options("", ""), ui::detail::show);
    harness.run();

    assert!(harness.query_by_label("Safety lowered").is_none());
}

#[test]
fn the_options_fit_the_minimum_window_in_both_themes() {
    for palette in [Palette::dark(), Palette::light()] {
        let size = egui::vec2(760.0, 520.0);
        let all = "watch = false\ndebounce = \"1h\"\nmax_wait = \"2h\"\ninterval = \"1h\"\nconflict_resolve = \"none\"\nconflict_loser = \"pathname\"\nversions = false\nversions_keep_days = 365\nmax_delete = 100\nresync_mode = \"remote\"\nempty_dirs = false\n";
        let mut harness = harness_in(options("", all), size, palette, |ui, vm| {
            ui::detail::show(ui, vm)
        });
        harness.run();

        let fits = |rect: egui::Rect| rect.left() >= 0.0 && rect.right() <= size.x;
        for label in [
            "Watch for changes",
            "Check Drive every",
            "Stop when more than this share would be deleted",
            "When a first sync finds different files",
        ] {
            let rect = harness.get_all_by_label(label).next().unwrap().rect();
            assert!(fits(rect), "{label} sticks out: {rect:?}");
        }
        for control in harness.get_all_by_label("Reset to Default") {
            assert!(fits(control.rect()));
        }
        for field in harness.get_all_by_role(Role::TextInput) {
            assert!(fits(field.rect()));
        }
    }
}

#[test]
fn a_longest_wait_below_the_wait_after_changes_is_refused_in_line() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    type_into(&mut harness, "Longest wait while editing", "10s");
    commit(&mut harness);

    harness.get_by_label("max_wait must be at least debounce");
    assert!(harness.state().actions.is_empty());
}

#[test]
fn deleting_the_loser_without_a_winner_is_refused_in_line() {
    let mut harness = harness(options("", "conflict_resolve = \"none\"\n"), screen());
    harness.run();

    harness
        .get_by_role_and_label(Role::ComboBox, "The losing version")
        .click();
    harness.run();
    harness.get_by_label("Delete").click();
    harness.run();

    harness.get_by_label("conflict_loser \"delete\" needs a conflict_resolve that picks a winner");
    assert!(harness.state().actions.is_empty());
}
