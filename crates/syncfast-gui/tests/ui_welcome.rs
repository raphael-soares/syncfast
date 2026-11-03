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
    WelcomeView {
        has_remote,
        has_sync: false,
        reviewed: false,
    }
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
fn steps_follow_account_sync_and_first_round() {
    let steps = |has_remote, has_sync, reviewed| {
        WelcomeView {
            has_remote,
            has_sync,
            reviewed,
        }
        .steps()
    };

    assert_eq!(steps(false, false, false), [Current, Pending, Pending]);
    assert_eq!(steps(true, false, false), [Done, Current, Pending]);
    assert_eq!(steps(true, true, false), [Done, Done, Current]);
    assert_eq!(steps(true, true, true), [Done, Done, Done]);
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
