#![allow(clippy::unwrap_used)]

mod common;

use common::{from_config, harness};
use egui_kittest::kittest::Queryable as _;
use syncfast_core::config::SettingScope;
use syncfast_gui::gui_state::SettingsPane;
use syncfast_gui::ui::settings::AutostartView;
use syncfast_gui::ui::{self, Action, ViewModel};

fn pane(config: &str, pane: SettingsPane) -> ViewModel {
    ViewModel {
        settings_pane: pane,
        ..from_config(&format!("version = 2\n\n{config}"), None)
    }
}

fn notifications() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::options::notifications(ui, vm)
}

fn general() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::options::general(ui, vm)
}

#[test]
fn the_sidebar_lists_accounts_and_defaults_then_the_app_panes() {
    let mut harness = harness(pane("", SettingsPane::Defaults), |ui, vm| {
        ui::settings::dialog(ui.ctx(), vm, false)
    });
    harness.run();

    let top = |label: &str| {
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, label)
            .rect()
            .top()
    };
    let order = [
        top("Accounts"),
        top("Defaults"),
        top("General"),
        top("Notifications"),
        top("Appearance"),
    ];
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
}

#[test]
fn turning_a_notification_on_writes_it_to_the_defaults() {
    let vm = pane("", SettingsPane::Notifications);
    let mut harness = harness(vm, notifications());
    harness.run();

    harness
        .get_by_role_and_label(egui::accesskit::Role::CheckBox, "Success")
        .click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::SetSetting {
            scope: SettingScope::Defaults,
            key: "notifications.success".into(),
            value: "true".into(),
        }]
    );
}

#[test]
fn a_changed_notification_can_be_reset_to_its_default() {
    let vm = pane(
        "[notifications]\nsuccess = true\n",
        SettingsPane::Notifications,
    );
    let mut harness = harness(vm, notifications());
    harness.run();

    harness.get_by_label("Reset to Default").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::ResetSetting {
            scope: SettingScope::Defaults,
            key: "notifications.success".into(),
        }]
    );
}

#[test]
fn a_notification_row_names_its_default_under_its_description() {
    let mut harness = harness(pane("", SettingsPane::Notifications), notifications());
    harness.run();

    harness.get_by_label("Notify when a round changed files.\nDefault: Off");
    harness.get_by_label("Notify when a round keeps two versions of a file.\nDefault: On");
}

#[test]
fn outside_the_flatpak_the_autostart_row_says_where_it_is_available() {
    let mut harness = harness(pane("", SettingsPane::General), general());
    harness.run();

    harness.get_by_label_contains("Available in the Flatpak install.");
}

#[test]
fn inside_the_flatpak_the_autostart_row_has_no_such_note() {
    let mut vm = pane("", SettingsPane::General);
    vm.autostart = AutostartView {
        in_flatpak: true,
        ..AutostartView::default()
    };
    let mut harness = harness(vm, general());
    harness.run();

    assert!(
        harness
            .query_by_label_contains("Available in the Flatpak install.")
            .is_none()
    );
    assert!(harness.query_by_label_contains("Hyprland").is_none());
}

#[test]
fn on_hyprland_the_autostart_row_says_the_desktop_does_not_run_entries() {
    let mut vm = pane("", SettingsPane::General);
    vm.autostart = AutostartView {
        in_flatpak: true,
        hyprland: true,
        refused: false,
    };
    let mut harness = harness(vm, general());
    harness.run();

    harness.get_by_label_contains("Hyprland does not run autostart entries.");
}

#[test]
fn a_refusal_from_the_desktop_is_shown_under_the_row() {
    let mut vm = pane("[app]\nautostart = true\n", SettingsPane::General);
    vm.autostart = AutostartView {
        in_flatpak: true,
        hyprland: false,
        refused: true,
    };
    let mut harness = harness(vm, general());
    harness.run();

    harness.get_by_label("The desktop did not allow autostart.");
}
