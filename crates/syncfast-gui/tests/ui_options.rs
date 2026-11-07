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

    for title in [
        "Schedule",
        "Bandwidth",
        "Conflicts",
        "Versions",
        "Deletions",
        "Advanced",
    ] {
        harness.get_by_label(title);
    }
    for row in [
        "Watch for changes",
        "Wait after changes",
        "Longest wait while editing",
        "Check Drive every",
        "Limit uploads to",
        "Limit downloads to",
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
    let mut harness = harness_in(
        options("", ""),
        egui::vec2(900.0, 1400.0),
        Palette::dark(),
        screen(),
    );
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
        let all = "watch = false\ndebounce = \"1h\"\nmax_wait = \"2h\"\ninterval = \"1h\"\nlimit_upload = \"1536K\"\nlimit_download = \"1G\"\nconflict_resolve = \"none\"\nconflict_loser = \"pathname\"\nversions = false\nversions_keep_days = 365\nmax_delete = 100\nresync_mode = \"remote\"\nempty_dirs = false\n";
        let mut harness = harness_in(options("", all), size, palette, |ui, vm| {
            ui::detail::show(ui, vm)
        });
        harness.run();

        let fits = |rect: egui::Rect| rect.left() >= 0.0 && rect.right() <= size.x;
        for label in [
            "Watch for changes",
            "Check Drive every",
            "Limit uploads to",
            "Limit downloads to",
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

fn defaults_screen() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::options::defaults(ui, vm)
}

fn with_a_sync_that_waits(max_wait: &str, defaults: &str) -> ViewModel {
    from_config(
        &config(defaults, &format!("max_wait = \"{max_wait}\"\n")),
        None,
    )
}

#[test]
fn a_default_that_breaks_another_sync_is_refused_in_line() {
    let mut harness = harness(with_a_sync_that_waits("1m", ""), defaults_screen());
    harness.run();

    type_into(&mut harness, "Wait after changes", "2m");
    commit(&mut harness);

    harness.get_by_label("max_wait must be at least debounce");
    assert!(harness.state().actions.is_empty());
}

#[test]
fn a_default_unrelated_to_what_another_sync_sets_is_saved() {
    let mut harness = harness(with_a_sync_that_waits("1m", ""), defaults_screen());
    harness.run();

    type_into(&mut harness, "Check Drive every", "10m");
    commit(&mut harness);

    assert_eq!(
        harness.state().actions,
        [Action::SetSetting {
            scope: SettingScope::Defaults,
            key: "interval".to_owned(),
            value: "10m".to_owned(),
        }]
    );
}

#[test]
fn resetting_a_setting_that_would_break_the_sync_is_refused_in_line() {
    let vm = options(
        "conflict_resolve = \"none\"",
        "conflict_resolve = \"newer\"\nconflict_loser = \"delete\"\n",
    );
    let mut harness = harness(vm, screen());
    harness.run();

    harness
        .get_all_by_label("Reset to Default")
        .next()
        .unwrap()
        .click();
    harness.run();

    harness.get_by_label("conflict_loser \"delete\" needs a conflict_resolve that picks a winner");
    assert!(harness.state().actions.is_empty());
}

#[test]
fn resetting_a_default_that_would_break_another_sync_is_refused_in_line() {
    let vm = with_a_sync_that_waits("20s", "debounce = \"10s\"");
    let mut harness = harness(vm, defaults_screen());
    harness.run();

    harness.get_by_label("Reset to Default").click();
    harness.run();

    harness.get_by_label("max_wait must be at least debounce");
    assert!(harness.state().actions.is_empty());
}

#[test]
fn a_refusal_goes_away_when_the_settings_it_was_judged_against_change() {
    let mut harness = harness(options("", ""), screen());
    harness.run();
    type_into(&mut harness, "Longest wait while editing", "10s");
    commit(&mut harness);
    harness.get_by_label("max_wait must be at least debounce");

    harness.state_mut().vm = options("", "debounce = \"5s\"\n");
    harness.run();

    assert!(
        harness
            .query_by_label("max_wait must be at least debounce")
            .is_none()
    );
}

#[test]
fn a_number_field_sits_in_its_row_like_the_switch_above_it() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    let centre = |role, label| harness.get_by_role_and_label(role, label).rect().center().y;
    let switch = centre(Role::CheckBox, "Watch for changes");
    let wait = centre(Role::TextInput, "Wait after changes");
    let longest = centre(Role::TextInput, "Longest wait while editing");

    assert!(
        ((wait - switch) - (longest - wait)).abs() < 1.0,
        "{switch} {wait} {longest}"
    );
}

const SPEED_MESSAGE: &str =
    "must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M";

#[test]
fn bandwidth_is_unlimited_until_a_limit_is_set() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    assert_eq!(harness.query_all_by_label("Default: Unlimited").count(), 2);
    assert_eq!(harness.query_all_by_label("Unlimited").count(), 2);
}

#[test]
fn a_limit_reads_in_binary_units_and_says_what_it_overrides() {
    let vm = options(
        "limit_download = \"500K\"",
        "limit_upload = \"2M\"\nlimit_download = \"1G\"\n",
    );
    let mut harness = harness(vm, screen());
    harness.run();

    harness.get_by_label("2 MiB/s");
    harness.get_by_label("1 GiB/s");
    harness.get_by_label("Overrides the default (Unlimited)");
    harness.get_by_label("Overrides the default (500 KiB/s)");
}

#[test]
fn an_inherited_limit_is_shown_as_the_default() {
    let mut harness = harness(options("limit_upload = \"500K\"", ""), screen());
    harness.run();

    harness.get_by_label("Default: 500 KiB/s");
    harness.get_by_label("500 KiB/s");
}

#[test]
fn a_speed_is_saved_in_its_canonical_form_when_committed() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    type_into(&mut harness, "Limit uploads to", "1024k");
    commit(&mut harness);

    assert_eq!(harness.state().actions, [set("limit_upload", "1M")]);
}

#[test]
fn an_empty_speed_saves_off() {
    let mut harness = harness(options("", "limit_download = \"2M\"\n"), screen());
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Limit downloads to")
        .click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.key_press(egui::Key::Backspace);
    harness.run_steps(2);

    commit(&mut harness);

    assert_eq!(harness.state().actions, [set("limit_download", "off")]);
}

#[test]
fn off_typed_in_a_speed_field_saves_off() {
    let mut harness = harness(options("", "limit_upload = \"2M\"\n"), screen());
    harness.run();

    type_into(&mut harness, "Limit uploads to", "off");
    commit(&mut harness);

    assert_eq!(harness.state().actions, [set("limit_upload", "off")]);
}

#[test]
fn a_zero_speed_shows_the_reason_and_saves_nothing() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    type_into(&mut harness, "Limit uploads to", "0");
    commit(&mut harness);

    harness.get_by_label(&format!("limit_upload {SPEED_MESSAGE}"));
    assert!(harness.state().actions.is_empty());
}

#[test]
fn a_speed_without_a_unit_is_refused_for_downloads_too() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    type_into(&mut harness, "Limit downloads to", "10");
    commit(&mut harness);

    harness.get_by_label(&format!("limit_download {SPEED_MESSAGE}"));
    assert!(harness.state().actions.is_empty());
}

#[test]
fn the_summary_says_when_a_sync_runs_with_watching_on() {
    let mut harness = harness(options("", ""), screen());
    harness.run();

    harness.get_by_label("Syncs every 5 min and 30 s after you stop editing.");
}

#[test]
fn the_summary_says_changes_wait_for_the_next_round_with_watching_off() {
    let mut harness = harness(options("", "watch = false\ninterval = \"10m\"\n"), screen());
    harness.run();

    harness.get_by_label("Syncs every 10 min. Changes on this computer wait for the next round.");
}

#[test]
fn the_summary_follows_the_timing_the_sync_resolves() {
    let mut harness = harness(
        options("debounce = \"1m\"", "interval = \"1h\"\n"),
        screen(),
    );
    harness.run();

    harness.get_by_label("Syncs every 1 h and 1 min after you stop editing.");
}

#[test]
fn the_defaults_have_the_bandwidth_section_but_no_summary() {
    let mut harness = harness(from_config(&config("", ""), None), defaults_screen());
    harness.run();

    harness.get_by_label("Bandwidth");
    assert!(harness.query_all_by_label("Limit uploads to").count() >= 1);
    assert!(harness.query_by_label_contains("Syncs every").is_none());
}
