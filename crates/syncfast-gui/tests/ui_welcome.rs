#![allow(clippy::unwrap_used)]

use egui::Ui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use syncfast_gui::theme::{self, Palette};
use syncfast_gui::ui::Action;
use syncfast_gui::ui::welcome::StepState::{Current, Done, Pending};
use syncfast_gui::ui::welcome::{self, WelcomeView};

struct Screen {
    view: WelcomeView,
    actions: Vec<Action>,
}

/// egui applies new fonts at the start of the next pass, so the first frame
/// only installs the theme.
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

fn harness(view: WelcomeView) -> Harness<'static, Screen> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(960.0, 640.0))
        .build_ui_state(
            |ui, screen: &mut Screen| {
                if ready(ui) {
                    let actions = welcome::show(ui, &screen.view);
                    screen.actions.extend(actions);
                }
            },
            Screen {
                view,
                actions: Vec::new(),
            },
        );
    harness.run();
    harness
}

fn fresh(has_remote: bool) -> WelcomeView {
    WelcomeView { has_remote }
}

#[test]
fn welcome_greets_and_explains_what_the_app_does() {
    let harness = harness(fresh(false));

    harness.get_by_label("Welcome to syncfast");
    harness.get_by_label(
        "Keep folders on this computer in sync with Google Drive. Your files stay local first.",
    );
}

#[test]
fn without_an_account_the_first_step_is_current_and_the_others_wait() {
    let harness = harness(fresh(false));

    harness.get_by_label("Connect your Google account, current step");
    harness.get_by_label("Choose a folder, not started");
    harness.get_by_label("Review the first sync, not started");
}

#[test]
fn with_an_account_the_first_step_is_done_and_the_folder_step_is_current() {
    let harness = harness(fresh(true));

    harness.get_by_label("Connect your Google account, done");
    harness.get_by_label("Choose a folder, current step");
    harness.get_by_label("Review the first sync, not started");
}

#[test]
fn steps_follow_the_account_and_the_later_ones_never_count_as_done() {
    assert_eq!(fresh(false).steps(), [Current, Pending, Pending]);
    assert_eq!(fresh(true).steps(), [Done, Current, Pending]);
}

#[test]
fn get_started_without_an_account_asks_to_add_one() {
    let mut harness = harness(fresh(false));

    harness.get_by_label("Get Started").click();
    harness.run();

    assert_eq!(harness.state().actions, [Action::AddAccount]);
}

#[test]
fn get_started_with_an_account_goes_straight_to_add_sync() {
    let mut harness = harness(fresh(true));

    harness.get_by_label("Get Started").click();
    harness.run();

    assert_eq!(harness.state().actions, [Action::AddSync]);
}

#[test]
fn the_settings_button_opens_settings_from_the_welcome() {
    let mut harness = harness(fresh(false));

    harness.get_by_label("Settings…").click();
    harness.run();

    assert_eq!(harness.state().actions, [Action::OpenSettings]);
}

#[test]
fn the_guide_can_be_skipped_whether_or_not_there_is_an_account() {
    for has_remote in [false, true] {
        let mut harness = harness(fresh(has_remote));

        harness.get_by_label(welcome::SKIP).click();
        harness.run();

        assert_eq!(
            harness.state().actions,
            [Action::SkipWelcome],
            "has_remote: {has_remote}"
        );
    }
}

#[test]
fn quit_is_reachable_from_the_welcome() {
    let mut harness = harness(fresh(false));

    harness.get_by_label("syncfast Menu").click();
    harness.run_steps(4);
    harness.get_by_label("Quit syncfast").click();
    harness.run_steps(4);

    assert_eq!(harness.state().actions, [Action::Quit]);
}

#[test]
fn the_welcome_menu_keeps_its_sync_rows_but_they_do_nothing_without_syncs() {
    let mut harness = harness(fresh(false));

    harness.get_by_label("syncfast Menu").click();
    harness.run_steps(4);
    harness.get_by_label("Sync All Now").click();
    harness.get_by_label("Resume All").click();
    harness.run_steps(4);

    assert!(harness.state().actions.is_empty());
}
