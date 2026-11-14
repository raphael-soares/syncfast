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

/// The rows stack taller than a window, and a popup only opens on screen.
fn tall(vm: ViewModel) -> Harness<'static, Frame> {
    harness_in(vm, egui::vec2(900.0, 1100.0), Palette::dark(), screen())
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
        "Full check every",
        "Limit uploads to",
        "Limit downloads to",
        "When both sides change",
        "The losing version",
        "Keep replaced files",
        "Keep for",
        "Stop when more than this share would be deleted",
        "When a first sync finds different files",
        "Sync empty folders",
        "Review",
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

    harness.get_by_label("Overrides the default (1 h)");
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

    type_into(&mut harness, "Full check every", "10m");
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
        .get_by_role_and_label(Role::TextInput, "Full check every")
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
    let mut harness = tall(options("", ""));
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
            "Full check every",
            "Limit uploads to",
            "Limit downloads to",
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
    let mut harness = tall(options("", "conflict_resolve = \"none\"\n"));
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

    type_into(&mut harness, "Full check every", "10m");
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
    assert!(harness.query_by_label("Unlimited").is_none());
}

#[test]
fn a_limit_reads_in_binary_units_and_says_what_it_overrides() {
    let vm = options(
        "limit_download = \"500K\"",
        "limit_upload = \"2M\"\nlimit_download = \"1G\"\n",
    );
    let mut harness = harness(vm, screen());
    harness.run();

    for (label, value) in [("Limit uploads to", "2M"), ("Limit downloads to", "1G")] {
        let field = harness.get_by_role_and_label(Role::TextInput, label);
        assert_eq!(field.accesskit_node().value(), Some(value.to_owned()));
    }
    harness.get_by_label("Overrides the default (Unlimited)");
    harness.get_by_label("Overrides the default (500 KiB/s)");
}

#[test]
fn an_inherited_limit_is_shown_as_the_default() {
    let mut harness = harness(
        options("limit_upload = \"500K\"\nlimit_download = \"1G\"", ""),
        screen(),
    );
    harness.run();

    harness.get_by_label("Default: 500 KiB/s");
    harness.get_by_label("Default: 1 GiB/s");
}

#[test]
fn every_number_field_of_the_options_ends_at_the_same_edge() {
    let vm = options(
        "",
        "limit_upload = \"2M\"\nversions_keep_days = 30\nmax_delete = 20\n",
    );
    let mut harness = harness_in(vm, egui::vec2(900.0, 1400.0), Palette::dark(), screen());
    harness.run();

    let right = |harness: &Harness<'static, Frame>, label: &str| {
        harness
            .get_by_role_and_label(Role::TextInput, label)
            .rect()
            .right()
    };
    let reference = right(&harness, "Full check every");
    for label in [
        "Wait after changes",
        "Limit uploads to",
        "Limit downloads to",
        "Keep for",
        "Stop when more than this share would be deleted",
    ] {
        let edge = right(&harness, label);
        assert!(
            (edge - reference).abs() < 0.5,
            "{label} ends at {edge}, not {reference}"
        );
    }
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

    harness.get_by_label(
        "Syncs seconds after a change here, within a minute of a change on Drive, after resume, and every 1 h.",
    );
}

#[test]
fn the_summary_says_when_a_sync_runs_with_watching_off() {
    let mut harness = harness(options("", "watch = false\ninterval = \"10m\"\n"), screen());
    harness.run();

    harness.get_by_label("Syncs every 10 min, after resume, and when the network comes back.");
}

#[test]
fn an_upload_only_sync_does_not_promise_to_follow_drive() {
    let mut harness = harness(options("", "mode = \"mirror_up\"\n"), screen());
    harness.run();

    harness.get_by_label("Syncs seconds after a change here, after resume, and every 1 h.");
}

#[test]
fn the_summary_follows_the_timing_the_sync_resolves() {
    let mut harness = harness(
        options("interval = \"15m\"", "interval = \"2h\"\n"),
        screen(),
    );
    harness.run();

    harness.get_by_label(
        "Syncs seconds after a change here, within a minute of a change on Drive, after resume, and every 2 h.",
    );
}

#[test]
fn the_defaults_have_the_bandwidth_section_but_no_summary() {
    let mut harness = harness(from_config(&config("", ""), None), defaults_screen());
    harness.run();

    harness.get_by_label("Bandwidth");
    assert!(harness.query_all_by_label("Limit uploads to").count() >= 1);
    assert!(harness.query_by_label_contains("Syncs every").is_none());
}

const DELETED_AND_KEPT: &str = "The losing version is deleted. On this computer it stays in the versions folder for 30 days; on Drive it goes to the trash.";
const DELETED_AND_LOST: &str = "The losing version is deleted and not kept.";
const NOT_KEPT: &str = "Files replaced or deleted on this computer are not kept.";

fn shows(vm: ViewModel, label: &str) -> bool {
    let mut harness = harness(vm, screen());
    harness.run();
    harness.query_by_label(label).is_some()
}

#[test]
fn a_deleted_loser_warns_that_it_stays_in_the_versions_folder_and_goes_to_the_drive_trash() {
    assert!(shows(
        options("", "conflict_loser = \"delete\"\n"),
        DELETED_AND_KEPT
    ));
    assert!(!shows(
        options("", "conflict_loser = \"delete\"\n"),
        NOT_KEPT
    ));
}

#[test]
fn the_deleted_loser_warning_names_the_days_the_sync_keeps_versions() {
    let vm = options("", "conflict_loser = \"delete\"\nversions_keep_days = 7\n");

    assert!(shows(
        vm,
        "The losing version is deleted. On this computer it stays in the versions folder for 7 days; on Drive it goes to the trash."
    ));
}

#[test]
fn a_deleted_loser_without_versions_warns_that_nothing_is_kept() {
    let vm = options("", "conflict_loser = \"delete\"\nversions = false\n");

    assert!(shows(vm.clone(), DELETED_AND_LOST));
    assert!(shows(vm.clone(), NOT_KEPT));
    assert!(!shows(vm, DELETED_AND_KEPT));
}

#[test]
fn turning_versions_off_warns_in_its_section() {
    assert!(shows(options("", "versions = false\n"), NOT_KEPT));
}

#[test]
fn the_built_in_settings_show_no_warning() {
    for label in [DELETED_AND_KEPT, DELETED_AND_LOST, NOT_KEPT] {
        assert!(!shows(options("", ""), label), "{label}");
    }
}

#[test]
fn the_defaults_panel_warns_about_a_default_that_deletes_the_loser() {
    let vm = options("conflict_loser = \"delete\"\n", "");
    let mut harness = harness(vm, defaults_screen());

    harness.run();

    harness.get_by_label(DELETED_AND_KEPT);
}

#[test]
fn the_warnings_wrap_inside_the_minimum_window_in_both_themes() {
    for palette in [Palette::dark(), Palette::light()] {
        let size = egui::vec2(760.0, 520.0);
        let vm = options("", "conflict_loser = \"delete\"\nversions = false\n");
        let mut harness = harness_in(vm, size, palette, ui::detail::show);
        harness.run();

        for label in [DELETED_AND_LOST, NOT_KEPT] {
            let rect = harness.get_all_by_label(label).next().unwrap().rect();
            assert!(
                rect.left() >= 0.0 && rect.right() <= size.x,
                "{label}: {rect:?}"
            );
        }
    }
}

#[test]
fn a_long_warning_wraps_before_the_right_edge_of_the_options() {
    for palette in [Palette::dark(), Palette::light()] {
        let size = egui::vec2(760.0, 520.0);
        let vm = options("", "conflict_loser = \"delete\"\nreview = \"automatic\"\n");
        let mut harness = harness_in(vm, size, palette, ui::detail::show);
        harness.run();

        let content_right = harness
            .get_all_by_role(Role::ComboBox)
            .map(|combo| combo.rect().right())
            .fold(f32::MIN, f32::max);
        for label in [NEVER_ASK, DELETED_AND_KEPT] {
            let rect = harness.get_all_by_label(label).next().unwrap().rect();
            assert!(
                rect.right() <= content_right,
                "{label} runs past the options: {rect:?} against {content_right}"
            );
        }
    }
}

#[test]
fn the_defaults_panel_warns_about_a_default_that_turns_versions_off() {
    let vm = options("versions = false\n", "");
    let mut harness = harness(vm, defaults_screen());

    harness.run();

    harness.get_by_label(NOT_KEPT);
}

const ASK_EVERY_TIME: &str =
    "Every first sync, filter change, mode change and mass deletion waits for you.";
const ASK_ONLY_FOR_DELETIONS: &str = "A review is confirmed automatically when everything it deletes can be restored from Recently Deleted. The first sync of a folder and deletions that cannot be undone wait for you.";
const NEVER_ASK: &str = "Deletions are confirmed automatically, including mass deletions that cannot be undone. Only an empty source folder or a critical error stops a sync.";

#[test]
fn review_is_a_choice_that_names_the_level_it_is_on() {
    for (value, name) in [
        ("manual", "Ask every time"),
        (
            "deletions",
            "Ask for the first sync and for deletions that cannot be undone",
        ),
        ("automatic", "Never ask"),
    ] {
        let mut harness = harness(options("", &format!("review = \"{value}\"\n")), screen());
        harness.run();

        assert_eq!(
            harness
                .get_by_role_and_label(Role::ComboBox, "Review")
                .accesskit_node()
                .value(),
            Some(name.to_owned())
        );
    }
}

#[test]
fn picking_a_review_level_saves_its_name() {
    let tall = egui::vec2(800.0, 1800.0);
    let mut harness = harness_in(options("", ""), tall, Palette::dark(), screen());
    harness.run();

    harness
        .get_by_role_and_label(Role::ComboBox, "Review")
        .click();
    harness.run();
    harness.get_by_label("Ask every time").click();
    harness.run();

    assert_eq!(harness.state().actions, [set("review", "manual")]);
}

#[test]
fn each_review_level_explains_itself_under_the_row() {
    assert!(shows(options("", "review = \"manual\"\n"), ASK_EVERY_TIME));
    assert!(!shows(
        options("", "review = \"manual\"\n"),
        ASK_ONLY_FOR_DELETIONS
    ));
    assert!(shows(options("", ""), ASK_ONLY_FOR_DELETIONS));
    assert!(!shows(options("", ""), ASK_EVERY_TIME));
    assert!(!shows(
        options("", "review = \"automatic\"\n"),
        ASK_EVERY_TIME
    ));
    assert!(!shows(
        options("", "review = \"automatic\"\n"),
        ASK_ONLY_FOR_DELETIONS
    ));
}

#[test]
fn never_ask_warns_and_the_other_levels_do_not() {
    assert!(shows(options("", "review = \"automatic\"\n"), NEVER_ASK));
    assert!(!shows(options("", ""), NEVER_ASK));
    assert!(!shows(options("", "review = \"manual\"\n"), NEVER_ASK));
}

#[test]
fn the_defaults_panel_warns_about_a_default_of_never_ask() {
    let vm = options("review = \"automatic\"\n", "");
    let mut harness = harness(vm, defaults_screen());

    harness.run();

    harness.get_by_label(NEVER_ASK);
}

#[test]
fn a_sync_on_never_ask_says_its_safety_is_lowered() {
    let mut harness = harness(options("", "review = \"automatic\"\n"), |ui, vm| {
        ui::detail::show(ui, vm)
    });
    harness.run();

    harness.get_by_label("Safety lowered");
}

#[test]
fn the_download_caption_promises_versions_only_while_versions_are_on() {
    let mut kept = harness(options("", "mode = \"download\"\n"), screen());
    let mut not_kept = harness(
        options("", "mode = \"download\"\nversions = false\n"),
        screen(),
    );
    kept.run();
    not_kept.run();

    kept.get_by_label_contains("Replaced files are kept as versions.");
    not_kept.get_by_label_contains("Nothing is ever deleted.");
    assert_eq!(
        not_kept
            .query_all_by_label_contains("Replaced files are kept")
            .count(),
        0
    );
}

#[test]
fn the_mode_row_offers_every_mode_and_says_what_the_current_one_does() {
    let mut harness = harness(options("", "mode = \"upload\"\n"), screen());
    harness.run();

    harness.get_by_role_and_label(Role::ComboBox, "Mode");
    assert_eq!(
        harness
            .get_by_role_and_label(Role::ComboBox, "Mode")
            .value()
            .as_deref(),
        Some("Upload only")
    );
    harness.get_by_label_contains("Nothing is ever deleted");
    harness.get_by_label("Changing the mode holds this sync for a review.");

    harness
        .get_by_role_and_label(Role::ComboBox, "Mode")
        .click();
    harness.run();
    for name in [
        "Two-way",
        "Download only",
        "Mirror to Drive",
        "Mirror to this computer",
    ] {
        harness.get_by_label(name);
    }
    harness.get_by_label("Mirror to Drive").click();
    harness.run();

    assert_eq!(harness.state().actions, [set("mode", "mirror_up")]);
}

#[test]
fn a_mirror_mode_warns_about_what_it_deletes_and_whether_it_is_kept() {
    let warns = |pair_settings: &str, needle: &str| {
        let mut harness = harness(options("", pair_settings), screen());
        harness.run();
        harness.query_all_by_label_contains(needle).count() > 0
    };

    assert!(warns(
        "mode = \"mirror_down\"\n",
        "kept as versions for 30 days"
    ));
    assert!(warns(
        "mode = \"mirror_down\"\n",
        "An empty folder on Drive is never mirrored"
    ));
    assert!(warns(
        "mode = \"mirror_up\"\n",
        "An empty folder is never mirrored"
    ));
    assert!(warns(
        "mode = \"mirror_up\"\nversions = false\n",
        "deleted there and not kept"
    ));
    assert!(!warns(
        "mode = \"mirror_up\"\nversions = false\n",
        "kept as versions"
    ));
    assert!(!warns("mode = \"upload\"\n", "never mirrored"));
    assert!(!warns("", "never mirrored"));
}

#[test]
fn fields_of_a_direction_the_mode_does_not_sync_are_hidden() {
    let shown = |pair_settings: &str, label: &str| {
        let mut harness = harness(options("", pair_settings), screen());
        harness.run();
        harness.query_all_by_label(label).count() > 0
    };

    assert!(shown("", "Watch for changes"));
    assert!(shown("", "When both sides change"));
    assert!(shown("", "Limit uploads to"));

    assert!(!shown("mode = \"download\"\n", "Watch for changes"));
    assert!(!shown("mode = \"download\"\n", "Wait after changes"));
    assert!(!shown("mode = \"download\"\n", "Limit uploads to"));
    assert!(shown("mode = \"download\"\n", "Limit downloads to"));
    assert!(!shown("mode = \"mirror_down\"\n", "Watch for changes"));
    assert!(!shown("mode = \"mirror_down\"\n", "When both sides change"));
    assert!(shown("mode = \"mirror_up\"\n", "Watch for changes"));
    assert!(shown("mode = \"mirror_up\"\n", "Limit uploads to"));
    assert!(!shown("mode = \"mirror_up\"\n", "Limit downloads to"));
    assert!(!shown("mode = \"upload\"\n", "The losing version"));
}

#[test]
fn only_a_two_way_sync_has_a_conflicts_tab() {
    let tabs = |pair_settings: &str| {
        let mut vm = options("", pair_settings);
        vm.tab = DetailTab::Activity;
        let mut harness = harness(vm, ui::detail::show);
        harness.run();
        harness.query_all_by_label("Conflicts").count() > 0
    };

    assert!(tabs(""));
    for mode in ["upload", "download", "mirror_up", "mirror_down"] {
        assert!(!tabs(&format!("mode = \"{mode}\"\n")), "{mode}");
    }
}

#[test]
fn a_one_way_sync_shows_its_mode_as_a_chip_and_a_two_way_one_does_not() {
    let chip = |pair_settings: &str, label: &str| {
        let mut vm = options("", pair_settings);
        vm.tab = DetailTab::Activity;
        let mut harness = harness(vm, ui::detail::show);
        harness.run();
        harness.query_all_by_label(label).count() > 0
    };

    assert!(chip("mode = \"mirror_up\"\n", "Mirror to Drive"));
    assert!(chip("mode = \"download\"\n", "Download only"));
    assert!(!chip("", "Two-way"));
}
