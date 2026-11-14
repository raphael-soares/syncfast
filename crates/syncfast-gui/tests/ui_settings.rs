#![allow(clippy::unwrap_used)]

mod common;

use common::{from_config, harness, harness_in, pair, view_model};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::sync::Arc;
use syncfast_core::behavior::ReviewLevel;
use syncfast_core::bisync_log::{Progress, RunSummary};
use syncfast_core::config::SettingScope;
use syncfast_core::preview::ChangeCounts;
use syncfast_core::runs::RunEntry;
use syncfast_core::state::{AutoReviewed, DisplayStatus, GateKind};
use syncfast_gui::gui_state::SettingsPane;
use syncfast_gui::theme::{Palette, ThemeChoice};
use syncfast_gui::ui::settings::AppearanceView;
use syncfast_gui::ui::{self, AccountStatus, Action, RemoteRow, ViewModel};

const SHARED: &str = "Uses rclone's shared client ID, which stops working in 2026.";

fn remote(name: &str, kind: &str, own_client_id: bool) -> RemoteRow {
    RemoteRow {
        name: name.into(),
        kind: kind.into(),
        own_client_id,
    }
}

fn accounts(remotes: Vec<RemoteRow>) -> ViewModel {
    ViewModel {
        remotes,
        settings_pane: SettingsPane::Accounts,
        ..view_model(Vec::new(), None)
    }
}

fn screen() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| ui::settings::dialog(ui.ctx(), vm, false)
}

fn screen_under_an_alert() -> impl FnMut(&mut egui::Ui, &ViewModel) -> Vec<Action> + 'static {
    |ui, vm| {
        let mut actions = ui::settings::dialog(ui.ctx(), vm, true);
        actions.extend(ui::remove_account::show(ui.ctx(), vm));
        actions
    }
}

fn click_at(harness: &egui_kittest::Harness<'_, common::Frame>, pos: egui::Pos2) {
    harness.hover_at(pos);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
}

#[test]
fn only_a_drive_remote_without_its_own_client_id_gets_the_shared_client_warning() {
    let mut harness = harness(
        accounts(vec![
            remote("gdrive", "drive", false),
            remote("trabalho", "drive", true),
            remote("backup", "local", false),
        ]),
        screen(),
    );

    harness.run();

    assert_eq!(harness.query_all_by_label(SHARED).count(), 1);
    harness.get_by_label("gdrive · Google Drive");
    harness.get_by_label("trabalho · Google Drive");
    harness.get_by_label("backup · local");
    harness.get_by_label("Use My Client ID…");
}

#[test]
fn use_my_client_id_asks_to_change_the_client_id_of_that_remote() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", false)]), screen());
    harness.run();

    harness.get_by_label("Use My Client ID…").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::ChangeClientId("gdrive".to_owned())]
    );
}

#[test]
fn add_google_drive_account_asks_for_a_new_account() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    harness.get_by_label("Add Google Drive Account…").click();
    harness.run();

    assert_eq!(harness.state().actions, [Action::AddAccount]);
}

#[test]
fn a_remote_whose_sync_lost_its_sign_in_offers_reconnect() {
    let mut lost = pair("drive", "Drive");
    lost.gate = Some(GateKind::AuthError);
    let mut vm = accounts(vec![
        remote("gdrive", "drive", true),
        remote("trabalho", "drive", true),
    ]);
    vm.pairs = vec![lost];
    let mut harness = harness(vm, screen());

    harness.run();

    assert_eq!(harness.query_all_by_label("Reconnect…").count(), 1);
    harness.get_by_label("Signed out. Reconnect to resume syncing.");
    harness.get_by_label("Reconnect…").click();
    harness.run();
    assert_eq!(
        harness.state().actions,
        [Action::ReconnectAccount("gdrive".to_owned())]
    );
}

fn backup_on(account: &str, gate: Option<GateKind>) -> syncfast_gui::snapshot::BackupSnapshot {
    let text = format!(
        "version = 3\n\n[[backup]]\nid = \"docs\"\nname = \"Documents\"\npaths = [\"/work/docs\"]\ndest = \"{account}:Backups/docs\"\n"
    );
    let config = syncfast_core::config::Config::parse(&text, std::path::Path::new("/home/ada"))
        .unwrap()
        .backups()[0]
        .clone();
    let state = syncfast_core::backup::state::BackupState {
        gate: gate.map(|kind| syncfast_core::state::Gate {
            kind,
            since: common::now(),
            detail: String::new(),
            fault: None,
            auto_checked: None,
        }),
        ..Default::default()
    };
    syncfast_gui::snapshot::BackupSnapshot {
        config,
        state,
        running: None,
        status: syncfast_core::backup::status::BackupStatus::Error,
        progress: None,
        missing_folders: Vec::new(),
    }
}

#[test]
fn a_remote_whose_backup_lost_its_sign_in_offers_reconnect() {
    let mut vm = accounts(vec![
        remote("gdrive", "drive", true),
        remote("trabalho", "drive", true),
    ]);
    vm.backups = vec![backup_on("gdrive", Some(GateKind::AuthError))];
    let mut harness = harness(vm, screen());

    harness.run();

    assert_eq!(harness.query_all_by_label("Reconnect…").count(), 1);
    harness.get_by_label("Reconnect…").click();
    harness.run();
    assert_eq!(
        harness.state().actions,
        [Action::ReconnectAccount("gdrive".to_owned())]
    );
}

#[test]
fn a_backup_that_is_fine_or_stopped_for_another_reason_does_not_offer_reconnect() {
    let mut vm = accounts(vec![remote("gdrive", "drive", true)]);
    vm.backups = vec![backup_on("gdrive", Some(GateKind::NeedsAttention))];
    let mut harness = harness(vm, screen());

    harness.run();

    assert!(harness.query_by_label("Reconnect…").is_none());
}

#[test]
fn without_a_lost_sign_in_there_is_no_reconnect() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());

    harness.run();

    assert!(harness.query_by_label("Reconnect…").is_none());
}

#[test]
fn the_sidebar_switches_panes() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    harness.get_by_label("Appearance").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::SetSettingsPane(SettingsPane::Appearance)]
    );
}

fn appearance(choice: ThemeChoice, errors: Vec<String>) -> ViewModel {
    let mut vm = ViewModel {
        settings_pane: SettingsPane::Appearance,
        appearance: AppearanceView {
            themes: vec!["noctalia.json".into()],
            errors,
        },
        ..view_model(Vec::new(), None)
    };
    vm.defaults.app.theme = choice.into();
    vm
}

#[test]
fn the_theme_selector_shows_the_current_choice_and_offers_the_catalog() {
    let mut harness = harness(appearance(ThemeChoice::System, Vec::new()), screen());
    harness.run();

    assert_eq!(
        harness
            .get_by_role(egui::accesskit::Role::ComboBox)
            .accesskit_node()
            .value(),
        Some("System".to_owned())
    );
    harness.get_by_role(egui::accesskit::Role::ComboBox).click();
    harness.run();

    harness.get_by_label("Light");
    harness.get_by_label("Dark");
    harness.get_by_label("noctalia");
}

#[test]
fn picking_a_theme_asks_to_save_it() {
    let mut harness = harness(appearance(ThemeChoice::System, Vec::new()), screen());
    harness.run();
    harness.get_by_role(egui::accesskit::Role::ComboBox).click();
    harness.run();

    harness.get_by_label("noctalia").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::SetSetting {
            scope: SettingScope::Defaults,
            key: "app.theme".into(),
            value: "noctalia.json".into(),
        }]
    );
}

#[test]
fn theme_files_that_could_not_be_read_are_listed() {
    let mut harness = harness(
        appearance(
            ThemeChoice::Dark,
            vec!["broken.json: expected value at line 1".into()],
        ),
        screen(),
    );

    harness.run();

    harness.get_by_label("broken.json: expected value at line 1");
}

#[test]
fn the_close_button_closes_settings() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    harness.get_by_label("Close").click();
    harness.run();

    assert_eq!(harness.state().actions, [Action::CloseSettings]);
}

#[test]
fn escape_closes_settings_unless_a_modal_is_above() {
    let mut open = harness(accounts(Vec::new()), screen());
    open.run();
    open.key_press(egui::Key::Escape);
    open.run();
    assert_eq!(open.state().actions, [Action::CloseSettings]);

    let mut covered = harness(accounts(Vec::new()), |ui, vm| {
        ui::settings::dialog(ui.ctx(), vm, true)
    });
    covered.run();
    covered.key_press(egui::Key::Escape);
    covered.run();
    assert!(covered.state().actions.is_empty());
}

#[test]
fn a_click_outside_the_dialog_closes_it_unless_a_modal_is_above() {
    let mut open = harness(accounts(Vec::new()), screen());
    open.run();
    click_at(&open, egui::pos2(4.0, 4.0));
    open.run();
    assert_eq!(open.state().actions, [Action::CloseSettings]);

    let mut covered = harness(accounts(Vec::new()), |ui, vm| {
        ui::settings::dialog(ui.ctx(), vm, true)
    });
    covered.run();
    click_at(&covered, egui::pos2(4.0, 4.0));
    covered.run();
    assert!(covered.state().actions.is_empty());
}

#[test]
fn a_click_inside_the_dialog_keeps_it_open() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    click_at(&harness, egui::pos2(450.0, 300.0));
    harness.run();

    assert!(harness.state().actions.is_empty());
}

#[test]
fn an_alert_over_settings_takes_the_input() {
    let vm = ViewModel {
        removing_account: Some("drive".to_owned()),
        ..accounts(vec![remote("drive", "drive", true)])
    };
    let mut harness = harness(vm, screen_under_an_alert());
    harness.run();

    harness.get_by_label("Appearance").click();
    harness.run();
    assert!(harness.state().actions.is_empty());

    harness.get_by_label("Cancel").click();
    harness.run();
    assert_eq!(harness.state().actions, [Action::CancelRemoveAccount]);
}

#[test]
fn the_sidebar_groups_panes_under_syncs_and_app() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    let syncs = harness.get_by_label("Syncs").rect();
    let accounts = harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Accounts")
        .rect();
    let app = harness.get_by_label("App").rect();
    let appearance = harness.get_by_label("Appearance").rect();

    assert!(syncs.top() < accounts.top());
    assert!(accounts.top() < app.top());
    assert!(app.top() < appearance.top());
}

#[test]
fn the_dialog_fits_the_smallest_window_in_both_themes() {
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 520.0));
    for palette in [
        syncfast_gui::theme::Palette::light(),
        syncfast_gui::theme::Palette::dark(),
    ] {
        let mut harness = harness_in(
            accounts(vec![remote("drive", "drive", true)]),
            window.size(),
            palette,
            screen(),
        );
        harness.run();

        for label in ["Close", "Appearance", "Add Google Drive Account…"] {
            let rect = harness.get_by_label(label).rect();
            assert!(
                window.shrink(20.0).contains_rect(rect),
                "{label}: {rect:?} is outside the dialog"
            );
        }
    }
}

#[test]
fn escape_with_the_theme_selector_open_only_closes_the_selector() {
    let mut harness = harness(appearance(ThemeChoice::System, Vec::new()), screen());
    harness.run();
    harness.get_by_role(egui::accesskit::Role::ComboBox).click();
    harness.run();

    harness.key_press(egui::Key::Escape);
    harness.run();

    assert!(harness.state().actions.is_empty());
}

fn with_status(remotes: Vec<RemoteRow>, name: &str, status: AccountStatus) -> ViewModel {
    let mut vm = accounts(remotes);
    vm.account_status.insert(name.to_owned(), status);
    vm
}

#[test]
fn a_problem_status_shows_the_detail_rclone_gave() {
    let vm = with_status(
        vec![remote("gdrive", "drive", true)],
        "gdrive",
        AccountStatus::Problem("Error 403: x".to_owned()),
    );
    let mut harness = harness(vm, screen());

    harness.run();

    harness.get_by_label("Problem: Error 403: x");
    assert!(harness.query_by_label("Reconnect…").is_none());
}

#[test]
fn the_status_line_says_checking_signed_in_and_offline() {
    for (status, text) in [
        (AccountStatus::Checking, "Checking…"),
        (AccountStatus::SignedIn, "Signed in"),
        (AccountStatus::Offline, "Offline. Not checked."),
    ] {
        let vm = with_status(vec![remote("gdrive", "drive", true)], "gdrive", status);
        let mut harness = harness(vm, screen());

        harness.run();

        harness.get_by_label(text);
    }
}

#[test]
fn an_account_without_a_status_yet_shows_no_status_line() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());

    harness.run();

    for text in ["Checking…", "Signed in", "Offline. Not checked."] {
        assert!(harness.query_by_label(text).is_none(), "{text}");
    }
}

#[test]
fn a_signed_out_status_offers_reconnect_and_says_so_once() {
    let mut lost = pair("drive", "Drive");
    lost.gate = Some(GateKind::AuthError);
    let mut vm = with_status(
        vec![remote("gdrive", "drive", true)],
        "gdrive",
        AccountStatus::SignedOut,
    );
    vm.pairs = vec![lost];
    let mut harness = harness(vm, screen());

    harness.run();

    assert_eq!(
        harness
            .query_all_by_label("Signed out. Reconnect to resume syncing.")
            .count(),
        1
    );
    harness.get_by_label("Reconnect…").click();
    harness.run();
    assert_eq!(
        harness.state().actions,
        [Action::ReconnectAccount("gdrive".to_owned())]
    );
}

#[test]
fn a_signed_out_status_alone_offers_reconnect() {
    let vm = with_status(
        vec![remote("gdrive", "drive", true)],
        "gdrive",
        AccountStatus::SignedOut,
    );
    let mut harness = harness(vm, screen());

    harness.run();

    harness.get_by_label("Signed out. Reconnect to resume syncing.");
    harness.get_by_label("Reconnect…");
}

#[test]
fn a_drive_row_with_its_own_client_id_offers_change_rename_and_remove() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());
    harness.run();

    harness.get_by_label("Change Client ID…");
    harness.get_by_label("Rename…");
    harness.get_by_label("Remove…");
    assert!(harness.query_by_label("Use My Client ID…").is_none());

    harness.get_by_label("Change Client ID…").click();
    harness.run();
    assert_eq!(
        harness.state().actions,
        [Action::ChangeClientId("gdrive".to_owned())]
    );
}

#[test]
fn rename_asks_to_rename_that_account() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());
    harness.run();

    harness.get_by_label("Rename…").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::RenameAccount("gdrive".to_owned())]
    );
}

#[test]
fn remove_asks_to_remove_that_account() {
    let mut harness = harness(accounts(vec![remote("gdrive", "drive", true)]), screen());
    harness.run();

    harness.get_by_label("Remove…").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::RemoveAccount("gdrive".to_owned())]
    );
}

#[test]
fn a_local_remote_can_only_be_removed() {
    let mut harness = harness(accounts(vec![remote("backup", "local", false)]), screen());

    harness.run();

    harness.get_by_label("Remove…");
    for text in ["Rename…", "Change Client ID…", "Use My Client ID…"] {
        assert!(harness.query_by_label(text).is_none(), "{text}");
    }
}

const TWO_SYNCS: &str = "version = 2\n\n[defaults]\ninterval = \"15m\"\n\n[[pair]]\nid = \"a\"\nname = \"A\"\nlocal = \"/home/ada/a\"\nremote = \"gdrive:a\"\ninterval = \"30m\"\n\n[[pair]]\nid = \"b\"\nname = \"B\"\nlocal = \"/home/ada/b\"\nremote = \"gdrive:b\"\ninterval = \"1h\"\n\n[[pair]]\nid = \"c\"\nname = \"C\"\nlocal = \"/home/ada/c\"\nremote = \"gdrive:c\"\n";

fn defaults() -> ViewModel {
    ViewModel {
        settings_pane: SettingsPane::Defaults,
        ..from_config(TWO_SYNCS, None)
    }
}

#[test]
fn a_default_that_was_changed_says_what_it_replaces_and_how_many_syncs_override_it() {
    let mut harness = harness(defaults(), screen());
    harness.run();

    harness.get_by_label("Built-in default: 1 h · Overridden by 2 syncs");
}

#[test]
fn the_defaults_pane_has_no_mode_row_because_the_mode_is_set_per_sync() {
    let mut harness = harness(defaults(), screen());
    harness.run();

    assert!(harness.query_by_label("Sync mode").is_none());
    assert!(harness.query_by_label("Mode").is_none());
    assert!(
        harness
            .query_by_label("Changing the mode holds this sync for a review.")
            .is_none()
    );
}

#[test]
fn resetting_a_default_asks_to_reset_it_in_the_defaults() {
    let mut harness = harness(defaults(), screen());
    harness.run();

    harness.get_by_label("Reset to Default").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::ResetSetting {
            scope: SettingScope::Defaults,
            key: "interval".to_owned(),
        }]
    );
}

#[test]
fn changing_a_default_is_scoped_to_the_defaults() {
    let mut harness = harness(defaults(), screen());
    harness.run();

    harness
        .get_by_role_and_label(egui::accesskit::Role::CheckBox, "Watch for changes")
        .click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::SetSetting {
            scope: SettingScope::Defaults,
            key: "watch".to_owned(),
            value: "false".to_owned(),
        }]
    );
}

#[test]
fn the_defaults_pane_sits_between_accounts_and_the_app_group() {
    let mut harness = harness(defaults(), screen());
    harness.run();

    let button = |label| {
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, label)
            .rect()
    };
    let (accounts, defaults) = (button("Accounts"), button("Defaults"));

    assert!(accounts.top() < defaults.top());
    assert!(defaults.top() < harness.get_by_label("App").rect().top());
}

#[test]
fn the_defaults_pane_fits_the_minimum_window_in_both_themes() {
    for palette in [Palette::dark(), Palette::light()] {
        let size = egui::vec2(760.0, 520.0);
        let mut harness = harness_in(defaults(), size, palette, screen());
        harness.run();

        let fits = |rect: egui::Rect| rect.left() >= 0.0 && rect.right() <= size.x;
        for field in harness.get_all_by_role(egui::accesskit::Role::TextInput) {
            assert!(fits(field.rect()), "{:?}", field.rect());
        }
        for control in harness.get_all_by_label("Reset to Default") {
            assert!(fits(control.rect()));
        }
        for row in ["Full check every", "Watch for changes"] {
            let rect = harness.get_all_by_label(row).next().unwrap().rect();
            assert!(fits(rect), "{row} sticks out: {rect:?}");
        }
    }
}

fn advanced_view(pairs: Vec<syncfast_gui::ui::PairRow>) -> ViewModel {
    ViewModel {
        settings_pane: SettingsPane::Advanced,
        ..view_model(pairs, Some("a"))
    }
}

fn with_runs(runs: Vec<RunEntry>) -> ViewModel {
    let mut vm = advanced_view(vec![pair("a", "Notes"), pair("b", "Photos")]);
    vm.runs = Some(Arc::from(runs));
    vm
}

fn advanced(vm: ViewModel) -> egui_kittest::Harness<'static, common::Frame> {
    let mut harness = harness(vm, screen());
    harness.run();
    harness
}

#[test]
fn the_advanced_pane_names_the_selected_sync_in_a_combo_and_lists_its_rounds() {
    let harness = advanced(with_runs(vec![run(
        "a",
        "2026-11-03T11:55:00Z",
        DisplayStatus::UpToDate,
    )]));

    assert_eq!(
        harness
            .get_by_role(egui::accesskit::Role::ComboBox)
            .accesskit_node()
            .value(),
        Some("Notes".to_owned())
    );
    assert!(harness.query_all_by_label("Sync").count() > 0);
    harness.get_by_label("Today 11:55");
}

#[test]
fn picking_another_sync_in_the_advanced_pane_asks_to_select_it() {
    let mut harness = advanced(with_runs(Vec::new()));
    harness.get_by_role(egui::accesskit::Role::ComboBox).click();
    harness.run();

    harness.get_by_label("Photos").click();
    harness.run();

    assert_eq!(harness.state().actions, [Action::SelectPair("b".into())]);
}

#[test]
fn the_advanced_pane_without_syncs_says_so() {
    let harness = advanced(advanced_view(Vec::new()));

    harness.get_by_label("No syncs yet");
    assert!(
        harness
            .query_by_role(egui::accesskit::Role::ComboBox)
            .is_none()
    );
}

#[test]
fn the_advanced_pane_is_in_the_sidebar() {
    let mut harness = harness(accounts(Vec::new()), screen());
    harness.run();

    harness.get_by_label("Advanced").click();
    harness.run();

    assert_eq!(
        harness.state().actions,
        [Action::SetSettingsPane(SettingsPane::Advanced)]
    );
}

fn run(id: &str, started: &str, status: DisplayStatus) -> RunEntry {
    RunEntry {
        id: id.into(),
        started: Some(
            started
                .parse::<Timestamp>()
                .unwrap()
                .to_zoned(TimeZone::UTC),
        ),
        seconds: Some(4),
        status,
        summary: RunSummary {
            uploaded: Some(2),
            downloaded: Some(1),
            deleted_local: Some(0),
            deleted_remote: Some(3),
            renamed: None,
            conflicts_new: Some(0),
            changes: Vec::new(),
            errors: Vec::new(),
        },
        detail: None,
        auto_reviewed: None,
        idle: false,
        messages: vec!["Synching Path1".into(), "Bisync successful".into()],
    }
}

#[test]
fn activity_without_rounds_says_there_is_none_yet() {
    let harness = advanced(with_runs(Vec::new()));

    harness.get_by_label("No activity yet");
}

#[test]
fn activity_lists_a_round_with_its_time_duration_result_and_counters() {
    let harness = advanced(with_runs(vec![run(
        "a",
        "2026-11-03T11:55:00Z",
        DisplayStatus::UpToDate,
    )]));

    harness.get_by_label("Today 11:55");
    harness.get_by_label("4 s");
    harness.get_by_label("↑ 2  ↓ 1  ✕ 3");
    assert!(harness.query_by_label("No activity yet").is_none());
}

#[test]
fn activity_says_when_a_round_was_reviewed_automatically_beside_its_counters() {
    let mut reviewed = run("a", "2026-11-03T11:55:00Z", DisplayStatus::UpToDate);
    reviewed.auto_reviewed = Some(AutoReviewed {
        gate: GateKind::TooManyDeletes,
        level: ReviewLevel::Automatic,
        counts: ChangeCounts {
            delete_remote: 3,
            ..ChangeCounts::default()
        },
        recoverable: false,
    });
    let plain = run("b", "2026-11-03T11:50:00Z", DisplayStatus::UpToDate);
    let harness = advanced(with_runs(vec![reviewed, plain]));

    harness.get_by_label("Reviewed automatically  ↑ 2  ↓ 1  ✕ 3");
    harness.get_by_label("↑ 2  ↓ 1  ✕ 3");
}

#[test]
fn activity_shows_an_idle_check_with_its_time_and_says_nothing_changed() {
    let mut idle = run(
        "b.idle.jsonl",
        "2026-11-03T11:55:00Z",
        DisplayStatus::UpToDate,
    );
    idle.idle = true;
    idle.summary = RunSummary::default();
    let harness = advanced(with_runs(vec![idle]));

    harness.get_by_label("Today 11:55");
    harness.get_by_label("No changes");
}

#[test]
fn activity_shows_the_reason_of_a_failed_round_instead_of_counters() {
    let mut failed = run("b", "2026-11-02T13:50:00Z", DisplayStatus::Retrying);
    failed.detail = Some("network error".into());
    let harness = advanced(with_runs(vec![failed]));

    harness.get_by_label("Yesterday 13:50");
    harness.get_by_label("Will retry");
    harness.get_by_label("network error");
}

#[test]
fn activity_shows_the_round_in_progress_as_running() {
    let mut running = pair("a", "Notes");
    running.status = DisplayStatus::Syncing;
    running.progress = Some(Progress {
        transfers: 412,
        total_transfers: 980,
        bytes: 1_800_000_000,
        total_bytes: 4_300_000_000,
        speed: 12_400_000.0,
        eta_seconds: Some(170),
        current: Some("Fotos/IMG_4471.jpg".into()),
    });
    let mut vm = advanced_view(vec![running]);
    let mut current = run("a", "2026-11-03T11:59:00Z", DisplayStatus::Syncing);
    current.seconds = None;
    vm.runs = Some(Arc::from(vec![current]));
    let harness = advanced(vm);

    harness.get_by_label("running");
    harness.get_by_label("412 of 980 files");
}

#[test]
fn clicking_a_round_shows_its_log_and_clicking_again_hides_it() {
    let mut harness = advanced(with_runs(vec![run(
        "a",
        "2026-11-03T11:55:00Z",
        DisplayStatus::UpToDate,
    )]));
    assert!(harness.query_by_label("Bisync successful").is_none());

    harness.get_by_label("Today 11:55").click();
    harness.run_steps(4);
    harness.get_by_label("Synching Path1");
    harness.get_by_label("Bisync successful");

    harness.get_by_label("Today 11:55").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Bisync successful").is_none());
}
