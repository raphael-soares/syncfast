#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;

use egui::accesskit::Role;

use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::backup::keystore::KeyBackend;
use syncfast_core::backup::state::BackupState;
use syncfast_core::backup::status::{BackupProgressFile, BackupStatus, RunningStep};
use syncfast_core::bisync::Fault;
use syncfast_core::config::Config;
use syncfast_core::state::{Gate, GateKind};
use syncfast_gui::backup_runs::{BackupRunEntry, RunKind, RunResult};
use syncfast_gui::gui_state::SettingsPane;
use syncfast_gui::snapshot::BackupSnapshot;
use syncfast_gui::ui::backup_detail::{BackupDetailData, BackupTab, SnapshotsView};
use syncfast_gui::ui::settings::KeyStorageView;
use syncfast_gui::ui::{self, Action, ViewModel};

use common::{harness, view_model};

fn config() -> syncfast_core::backup::config::BackupConfig {
    let text = "version = 3\n\n[[backup]]\nid = \"docs\"\nname = \"Documents\"\npaths = [\"/work/docs\"]\ndest = \"gdrive:Backups/docs\"\n";
    Config::parse(text, Path::new("/home/ada"))
        .unwrap()
        .backups()[0]
        .clone()
}

fn backup(status: BackupStatus) -> BackupSnapshot {
    BackupSnapshot {
        config: config(),
        state: BackupState::default(),
        running: (status == BackupStatus::BackingUp).then_some(RunningStep::Backup),
        status,
        progress: None,
    }
}

fn gate(kind: GateKind, fault: Option<Fault>, detail: &str) -> BackupState {
    BackupState {
        gate: Some(Gate {
            kind,
            since: common::now(),
            detail: detail.into(),
            fault,
            auto_checked: None,
        }),
        ..BackupState::default()
    }
}

fn view(backup: BackupSnapshot) -> ViewModel {
    ViewModel {
        backups: vec![backup],
        selected_backup: Some("docs".into()),
        backup: BackupDetailData {
            tab: BackupTab::Snapshots,
            snapshots: SnapshotsView::Loaded(Vec::new().into()),
            ..BackupDetailData::default()
        },
        ..view_model(Vec::new(), None)
    }
}

fn screen(vm: ViewModel) -> egui_kittest::Harness<'static, common::Frame> {
    let mut harness = harness(vm, ui::root);
    harness.run_steps(4);
    harness
}

fn disabled(harness: &egui_kittest::Harness<'_, common::Frame>, label: &str) -> bool {
    harness
        .get_all_by_label(label)
        .next()
        .unwrap()
        .accesskit_node()
        .is_disabled()
}

#[test]
fn sidebar_lists_backups_and_selects_one_on_click() {
    let mut vm = view(backup(BackupStatus::UpToDate));
    vm.selected_backup = None;
    vm.pairs = vec![common::pair("drive", "Google Drive")];
    vm.selected = Some("drive".into());
    let mut harness = screen(vm);

    harness.get_by_label_contains("Documents").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::SelectBackup("docs".into())]
    );
}

#[test]
fn sidebar_says_there_are_no_backups_yet() {
    let mut vm = view_model(vec![common::pair("drive", "Google Drive")], Some("drive"));
    vm.backups.clear();
    let harness = screen(vm);

    assert!(harness.query_by_label("No backups yet").is_some());
}

#[test]
fn back_up_now_starts_a_round() {
    let mut harness = screen(view(backup(BackupStatus::UpToDate)));

    assert!(!disabled(&harness, "Back Up Now"));
    harness
        .get_all_by_label("Back Up Now")
        .next()
        .unwrap()
        .click();
    harness.run_steps(4);

    assert_eq!(harness.state().actions, [Action::BackUpNow("docs".into())]);
}

#[test]
fn back_up_now_is_disabled_while_a_round_runs() {
    let harness = screen(view(backup(BackupStatus::BackingUp)));

    assert!(disabled(&harness, "Back Up Now"));
}

#[test]
fn a_missing_key_offers_enter_key() {
    let mut snapshot = backup(BackupStatus::Error);
    snapshot.state = gate(GateKind::NeedsAttention, Some(Fault::KeyMissing), "no key");
    let mut harness = screen(view(snapshot));

    harness.get_by_label("Enter Key…").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::EnterBackupKey("docs".into())]
    );
}

#[test]
fn a_damaged_repository_offers_a_full_check() {
    let mut snapshot = backup(BackupStatus::Error);
    snapshot.state = gate(GateKind::NeedsAttention, Some(Fault::RepoDamaged), "bad");
    let mut harness = screen(view(snapshot));

    harness.get_by_label("Check Again").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::CheckBackup {
            id: "docs".into(),
            kind: syncfast_core::backup::state::CheckKind::Full
        }]
    );
}

#[test]
fn the_key_card_shows_only_while_the_key_is_unsaved() {
    let mut unsaved = backup(BackupStatus::SaveKey);
    unsaved.state.key_unsaved = true;
    let with = screen(view(unsaved));
    let without = screen(view(backup(BackupStatus::UpToDate)));

    assert!(with.query_by_label("Save the backup key").is_some());
    assert!(without.query_by_label("Save the backup key").is_none());
}

#[test]
fn no_snapshots_shows_an_empty_state_with_back_up_now() {
    let harness = screen(view(backup(BackupStatus::UpToDate)));

    assert!(harness.query_by_label("No snapshots yet").is_some());
}

#[test]
fn turning_off_one_file_system_asks_first() {
    let mut vm = view(backup(BackupStatus::UpToDate));
    vm.backup.tab = BackupTab::Settings;
    let mut harness = common::harness_in(
        vm,
        egui::vec2(900.0, 2400.0),
        syncfast_gui::theme::Palette::dark(),
        ui::root,
    );
    harness.run_steps(4);

    harness
        .get_by_role_and_label(Role::CheckBox, "Stay on one file system")
        .click();
    harness.run();

    assert!(
        harness
            .query_by_label("Include mounted drives and subvolumes?")
            .is_some()
    );
}

fn tall(vm: ViewModel) -> egui_kittest::Harness<'static, common::Frame> {
    let mut harness = common::harness_in(
        vm,
        egui::vec2(900.0, 2400.0),
        syncfast_gui::theme::Palette::dark(),
        ui::root,
    );
    harness.run_steps(4);
    harness
}

fn with_state(status: BackupStatus, state: BackupState) -> ViewModel {
    let mut snapshot = backup(status);
    snapshot.state = state;
    view(snapshot)
}

#[test]
fn the_empty_sidebar_offers_new_backup() {
    let mut vm = view_model(vec![common::pair("drive", "Google Drive")], Some("drive"));
    vm.backups.clear();
    let mut harness = screen(vm);

    harness
        .get_all_by_label("New Backup…")
        .last()
        .unwrap()
        .click();
    harness.run_steps(4);

    assert_eq!(harness.state().actions, [Action::NewBackup]);
}

#[test]
fn selecting_a_sync_while_a_backup_is_open_goes_back_to_the_sync() {
    let mut vm = view(backup(BackupStatus::UpToDate));
    vm.pairs = vec![common::pair("drive", "Google Drive")];
    let mut harness = screen(vm);

    harness.get_by_label_contains("Google Drive").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::SelectPair("drive".into())]
    );
}

fn open_full_check(harness: &mut egui_kittest::Harness<'_, common::Frame>) {
    harness.get_by_label("More").click();
    harness.run_steps(4);
    harness.get_by_label("Full Check…").click();
    harness.run_steps(4);
}

#[test]
fn full_check_asks_before_reading_everything_and_cancel_is_the_default() {
    let mut harness = screen(view(backup(BackupStatus::UpToDate)));

    open_full_check(&mut harness);

    harness.get_by_label("Read all backup data?");
    harness.get_by_label("This downloads everything from gdrive:Backups/docs and can take hours.");
    harness.get_by_label("Check");
    harness.key_press(egui::Key::Enter);
    harness.run_steps(4);
    assert!(harness.query_by_label("Read all backup data?").is_none());
    assert!(harness.state().actions.is_empty());
}

#[test]
fn confirming_the_full_check_starts_it() {
    let mut harness = screen(view(backup(BackupStatus::UpToDate)));
    open_full_check(&mut harness);

    harness.get_by_role_and_label(Role::Button, "Check").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::CheckBackup {
            id: "docs".into(),
            kind: syncfast_core::backup::state::CheckKind::Full,
        }]
    );
}

fn keep_config(extra: &str) -> syncfast_core::backup::config::BackupConfig {
    let text = format!(
        "version = 3\n\n[[backup]]\nid = \"docs\"\nname = \"Documents\"\npaths = [\"/work/docs\"]\ndest = \"gdrive:Backups/docs\"\n{extra}"
    );
    Config::parse(&text, Path::new("/home/ada"))
        .unwrap()
        .backups()[0]
        .clone()
}

fn set_keep(harness: &mut egui_kittest::Harness<'_, common::Frame>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run_steps(4);
}

fn settings_of(extra: &str) -> egui_kittest::Harness<'static, common::Frame> {
    let mut snapshot = backup(BackupStatus::UpToDate);
    snapshot.config = keep_config(extra);
    let mut vm = view(snapshot);
    vm.backup.tab = BackupTab::Settings;
    tall(vm)
}

fn save(harness: &mut egui_kittest::Harness<'_, common::Frame>) {
    harness.get_by_role_and_label(Role::Button, "Save").click();
    harness.run_steps(4);
}

#[test]
fn lowering_a_keep_number_asks_before_deleting_old_snapshots() {
    let mut harness = settings_of("");

    set_keep(&mut harness, "Daily", "3");
    save(&mut harness);

    harness.get_by_label("Delete older snapshots?");
    harness.get_by_label(
        "The next backup permanently deletes the snapshots outside the new policy from gdrive:Backups/docs.",
    );
    assert!(harness.state().actions.is_empty());
}

#[test]
fn turning_yearly_off_asks_before_deleting_old_snapshots() {
    let mut harness =
        settings_of("\n[backup.keep]\ndaily = 7\nweekly = 4\nmonthly = 6\nyearly = 2\n");

    set_keep(&mut harness, "Yearly", "0");
    save(&mut harness);
    harness.get_by_label("Delete older snapshots?");
}

#[test]
fn confirming_the_keep_alert_saves_and_cancel_does_not() {
    let mut harness = settings_of("");
    set_keep(&mut harness, "Daily", "3");
    save(&mut harness);

    harness
        .get_by_role_and_label(Role::Button, "Cancel")
        .click();
    harness.run_steps(4);
    assert!(harness.state().actions.is_empty());
    save(&mut harness);
    harness
        .get_by_role_and_label(Role::Button, "Save and Delete Old Snapshots")
        .click();
    harness.run_steps(4);

    assert!(matches!(
        harness.state().actions[..],
        [Action::SaveBackupSettings { .. }]
    ));
}

#[test]
fn raising_a_keep_number_saves_without_an_alert() {
    let mut harness = settings_of("");

    set_keep(&mut harness, "Daily", "30");
    save(&mut harness);

    assert!(harness.query_by_label("Delete older snapshots?").is_none());
    assert!(matches!(
        harness.state().actions[..],
        [Action::SaveBackupSettings { .. }]
    ));
}

fn snapshot_view(id: &str, host: &str) -> syncfast_core::backup::snapshots::SnapshotView {
    syncfast_core::backup::snapshots::SnapshotView {
        id: format!("{id}0000"),
        short_id: id.to_owned(),
        time: common::now(),
        host: host.to_owned(),
        paths: vec!["/work/docs".to_owned()],
        files: Some(12),
        bytes: Some(2048),
        added_bytes: Some(512),
    }
}

#[test]
fn snapshots_show_the_host_only_when_it_is_not_this_computer() {
    let own = syncfast_core::restic::backup_host().unwrap_or("this-computer");
    let mut vm = view(backup(BackupStatus::UpToDate));
    vm.backup.snapshots = SnapshotsView::Loaded(
        vec![
            snapshot_view("aaaa", own),
            snapshot_view("bbbb", "laptop-ada"),
        ]
        .into(),
    );
    let harness = screen(vm);

    assert_eq!(harness.query_all_by_label("laptop-ada").count(), 1);
    assert_eq!(harness.query_all_by_label(own).count(), 0);
    assert!(harness.query_by_label("No snapshots yet").is_none());
}

#[test]
fn a_failed_snapshot_list_shows_the_error_and_try_again_asks_for_it_again() {
    let mut vm = view(backup(BackupStatus::UpToDate));
    vm.backup.snapshots = SnapshotsView::Failed("repository is locked".into());
    let mut harness = screen(vm);

    harness.get_by_label("repository is locked");
    harness.get_by_label("Try Again").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::RefreshBackupSnapshots("docs".into())]
    );
}

fn run(id: &str, result: RunResult, messages: &[&str]) -> BackupRunEntry {
    BackupRunEntry {
        id: id.to_owned(),
        started: None,
        seconds: Some(7),
        kind: RunKind::Backup,
        result,
        files_new: Some(3),
        files_changed: Some(2),
        data_added: Some(2048),
        check_errors: None,
        messages: messages.iter().map(|text| (*text).to_owned()).collect(),
    }
}

fn activity(runs: Vec<BackupRunEntry>) -> ViewModel {
    let mut vm = view(backup(BackupStatus::UpToDate));
    vm.backup.tab = BackupTab::Activity;
    vm.backup.runs = Some(runs.into());
    vm
}

#[test]
fn activity_lists_the_rounds_and_opens_the_log_of_one() {
    let mut harness = screen(activity(vec![
        run(
            "20260102-100000.jsonl",
            RunResult::Done,
            &["scanned 5 files"],
        ),
        run("20260101-100000.jsonl", RunResult::DoneWithWarnings(1), &[]),
    ]));

    harness.get_by_label_contains("+5 new");
    harness.get_by_label("1 file could not be read");
    assert!(harness.query_by_label("scanned 5 files").is_none());
    harness.get_all_by_label("Show log").next().unwrap().click();
    harness.run_steps(4);

    harness.get_by_label("scanned 5 files");
    assert!(harness.query_by_label("No activity yet").is_none());
}

#[test]
fn activity_without_rounds_says_so() {
    let harness = screen(activity(Vec::new()));

    harness.get_by_label("No activity yet");
}

fn card(status: BackupStatus, state: BackupState) -> egui_kittest::Harness<'static, common::Frame> {
    screen(with_state(status, state))
}

#[test]
fn the_card_explains_each_state_that_is_not_an_error() {
    let failing = BackupState {
        consecutive_failures: 4,
        ..BackupState::default()
    };
    let cases = [
        (
            BackupStatus::Paused,
            BackupState::default(),
            "This backup is paused. Resume it to back up again.",
        ),
        (
            BackupStatus::Offline,
            BackupState::default(),
            "No network. Backups resume when it is back.",
        ),
        (
            BackupStatus::Failing,
            failing,
            "The last 4 backups failed in a row.",
        ),
        (
            BackupStatus::Retrying,
            BackupState::default(),
            "The last backup failed. It will try again soon.",
        ),
        (
            BackupStatus::Unavailable,
            BackupState::default(),
            "gdrive:Backups/docs is not available. Backups resume when it is back.",
        ),
    ];

    for (status, state, sentence) in cases {
        let harness = card(status, state);
        assert!(harness.query_by_label(sentence).is_some(), "{sentence}");
    }
}

#[test]
fn the_card_of_a_round_in_progress_shows_the_bar_the_counts_and_the_file() {
    let mut snapshot = backup(BackupStatus::BackingUp);
    snapshot.progress = Some(BackupProgressFile {
        percent_done: Some(0.3),
        files_done: Some(3),
        total_files: Some(10),
        current: Some("/work/docs/a.txt".into()),
        ..BackupProgressFile::of_step(RunningStep::Backup)
    });
    let harness = screen(view(snapshot));

    harness.get_by_label("Backup progress");
    harness.get_by_label_contains("3 of 10 files");
    harness.get_by_label("Current: /work/docs/a.txt");
}

#[test]
fn a_round_that_reported_nothing_yet_says_it_is_looking_for_changes() {
    let harness = screen(view(backup(BackupStatus::BackingUp)));

    harness.get_by_label("Looking for changes…");
}

#[test]
fn the_last_backup_line_counts_new_or_changed_files_and_the_size_added() {
    let summary = syncfast_core::backup::state::BackupSummary {
        files_new: 3,
        files_changed: 2,
        data_added: 2048,
        ..Default::default()
    };
    let state = BackupState {
        last_success: Some(common::now()),
        last_run: Some(syncfast_core::backup::state::LastRun {
            started: common::now(),
            finished: common::now(),
            exit_code: 0,
            summary,
            log_file: "/state/runs/1.jsonl".into(),
        }),
        ..BackupState::default()
    };
    let harness = card(BackupStatus::UpToDate, state);

    harness.get_by_label_contains("5 new or changed files, 2.0 KB added.");
}

fn settings_pane(storage: KeyStorageView) -> egui_kittest::Harness<'static, common::Frame> {
    let vm = ViewModel {
        settings_pane: SettingsPane::Backups,
        key_storage: storage,
        restic_version: Some("restic 0.18.0".into()),
        ..view_model(Vec::new(), None)
    };
    let mut harness = harness(vm, |ui, vm| ui::settings::dialog(ui.ctx(), vm, false));
    harness.run_steps(4);
    harness
}

#[test]
fn the_backups_pane_names_the_portal_and_has_nothing_to_warn_about() {
    let harness = settings_pane(KeyStorageView::Found(KeyBackend::Portal));

    harness.get_by_label("Key storage");
    harness.get_by_label("Secret portal");
    harness.get_by_label("Test Key Storage");
    harness.get_by_label("restic 0.18.0");
    assert!(
        harness
            .query_by_label_contains("can read every secret")
            .is_none()
    );
}

#[test]
fn the_backups_pane_warns_about_the_host_keyring() {
    let harness = settings_pane(KeyStorageView::Found(KeyBackend::HostKeyring));

    harness.get_by_label("Host keyring (Secret Service)");
    harness.get_by_label(
        "The Flatpak can read every secret in your keyring. Routing the Secret portal is safer; see docs/guide/backup.md.",
    );
}

#[test]
fn the_backups_pane_says_when_no_key_storage_answers() {
    let harness = settings_pane(KeyStorageView::Missing);

    harness
        .get_by_label("No Secret portal and no Secret Service answered. See docs/guide/backup.md.");
}

#[test]
fn test_key_storage_and_the_guide_link_emit_their_actions() {
    let mut harness = settings_pane(KeyStorageView::Found(KeyBackend::Portal));

    harness.get_by_label("Test Key Storage").click();
    harness.run_steps(4);
    harness.get_by_label("Backing up with syncfast").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [
            Action::TestKeyStorage,
            Action::OpenUrl(
                "https://github.com/raphael-soares/syncfast/blob/main/docs/guide/backup.md".into()
            ),
        ]
    );
}
