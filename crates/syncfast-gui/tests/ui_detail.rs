#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use syncfast_core::behavior::{ReviewLevel, SyncMode};
use syncfast_core::bisync::Fault;
use syncfast_core::bisync_log::{Progress, RunSummary};
use syncfast_core::preview::ChangeCounts;
use syncfast_core::runs::RunEntry;
use syncfast_core::state::{AutoReviewed, DisplayStatus, GateKind};
use syncfast_gui::theme::{self, Palette};
use syncfast_gui::ui::{self, Action, DetailTab, PairRow, ViewModel};

fn now() -> Timestamp {
    "2026-11-03T12:00:00Z".parse().unwrap()
}

fn pair(status: DisplayStatus) -> PairRow {
    PairRow {
        id: "drive".into(),
        name: "Google Drive".into(),
        local: "/home/ada/Drive".into(),
        remote: "gdrive:Notes".into(),
        enabled: status != DisplayStatus::Paused,
        status,
        gate: None,
        last_success: Some("2026-11-03T11:55:00Z".parse().unwrap()),
        progress: None,
        gate_detail: String::new(),
        gate_fault: None,
        consecutive_failures: 0,
        realtime_off: false,
        presets: Vec::new(),
        filters: Vec::new(),
        effective: syncfast_core::settings::Effective::default(),
    }
}

fn gated(status: DisplayStatus, gate: GateKind) -> PairRow {
    PairRow {
        gate: Some(gate),
        ..pair(status)
    }
}

fn view(pair: PairRow) -> ViewModel {
    ViewModel {
        pairs: vec![pair],
        selected: Some("drive".into()),
        sidebar_width: 240.0,
        error: None,
        now: now(),
        tab: DetailTab::Activity,
        runs: None,
        ..ViewModel::default()
    }
}

struct Screen {
    vm: ViewModel,
    actions: Vec<Action>,
    /// egui applies new fonts at the start of the next pass, so the first
    /// frame only installs the theme.
    themed: bool,
}

fn harness(vm: ViewModel) -> Harness<'static, Screen> {
    harness_sized(vm, egui::vec2(960.0, 640.0))
}

fn harness_sized(vm: ViewModel, size: egui::Vec2) -> Harness<'static, Screen> {
    let mut harness = Harness::builder().with_size(size).build_ui_state(
        |ui, screen: &mut Screen| {
            if !screen.themed {
                theme::install_with(
                    ui.ctx(),
                    &Palette::dark(),
                    fastframe_text::TextRendering::platform_default(),
                );
                screen.themed = true;
                ui.ctx().request_repaint();
                return;
            }
            let actions = ui::root(ui, &screen.vm);
            screen.actions.extend(actions);
        },
        Screen {
            vm,
            actions: Vec::new(),
            themed: false,
        },
    );
    harness.run_steps(4);
    harness
}

fn disabled(harness: &Harness<'_, Screen>, label: &str) -> bool {
    harness.get_by_label(label).accesskit_node().is_disabled()
}

#[test]
fn a_long_sync_path_is_cut_short_before_the_actions_in_the_smallest_window() {
    let local =
        "/home/ada/Documents/Projects/Archive/2025/Quarterly Reports/Finance and Accounting";
    let mut vm = view(PairRow {
        local: local.into(),
        ..pair(DisplayStatus::UpToDate)
    });
    vm.sidebar_width = 200.0;
    let harness = harness_sized(vm, egui::vec2(760.0, 520.0));
    let subtitle = format!("{local} ↔ gdrive:Notes");

    let subtitle_right = harness.get_by_label(&subtitle).rect().right();
    let title_right = harness.get_by_label("Google Drive").rect().right();
    let actions_left = ["More", "Pause", "Sync Now"]
        .map(|label| harness.get_by_label(label).rect().left())
        .into_iter()
        .fold(f32::INFINITY, f32::min);

    assert!(
        subtitle_right <= actions_left,
        "{subtitle_right} > {actions_left}"
    );
    assert!(
        title_right <= actions_left,
        "{title_right} > {actions_left}"
    );
}

fn mirrored(mode: SyncMode, local: &str) -> PairRow {
    let mut row = pair(DisplayStatus::UpToDate);
    row.local = local.into();
    row.effective.behavior.mode = mode;
    row
}

#[test]
fn the_mode_chip_never_overlaps_the_subtitle_or_the_actions_in_the_smallest_window() {
    let local =
        "/home/ada/Documents/Projects/Archive/2025/Quarterly Reports/Finance and Accounting";
    let mut vm = view(mirrored(SyncMode::MirrorDown, local));
    vm.sidebar_width = 200.0;
    let harness = harness_sized(vm, egui::vec2(760.0, 520.0));

    let chip = harness.get_by_label("Mirror to this computer").rect();
    let subtitle = harness
        .get_by_label(&format!("{local} \u{2190} gdrive:Notes"))
        .rect();
    let actions_left = ["More", "Pause", "Sync Now"]
        .map(|label| harness.get_by_label(label).rect().left())
        .into_iter()
        .fold(f32::INFINITY, f32::min);

    let title_left = harness.get_by_label("Google Drive").rect().left();
    let sync_now = harness.get_by_label("Sync Now").rect();

    assert!(!chip.intersects(subtitle), "{chip:?} {subtitle:?}");
    assert!(chip.right() <= actions_left, "{chip:?} {actions_left}");
    assert!(chip.left() >= title_left, "{chip:?} {title_left}");
    assert!(subtitle.width() > 0.0, "{subtitle:?}");
    assert!(chip.height() < sync_now.height(), "{chip:?} {sync_now:?}");
    assert!(
        (chip.center().y - sync_now.center().y).abs() <= 1.0,
        "{chip:?} {sync_now:?}"
    );
}

#[test]
fn a_two_way_sync_has_no_mode_chip_and_a_one_way_list_row_names_its_mode() {
    let two_way = harness_sized(
        view(mirrored(SyncMode::Bisync, "/home/ada/Notes")),
        egui::vec2(960.0, 640.0),
    );
    let upload = harness_sized(
        view(mirrored(SyncMode::Upload, "/home/ada/Notes")),
        egui::vec2(960.0, 640.0),
    );

    assert_eq!(two_way.query_all_by_label("Upload only").count(), 0);
    assert!(upload.query_all_by_label("Upload only").count() >= 1);
    assert!(
        upload
            .query_all_by_label_contains(" \u{b7} Upload only")
            .count()
            >= 1
    );
}

#[test]
fn a_narrow_sidebar_row_puts_the_sync_age_before_the_mode() {
    let mut vm = view(mirrored(SyncMode::MirrorUp, "/home/ada/Notes"));
    vm.sidebar_width = 200.0;
    let harness = harness_sized(vm, egui::vec2(760.0, 520.0));

    let label = harness
        .get_all_by_label_contains(" \u{b7} Mirror to Drive")
        .map(|node| node.accesskit_node().label().unwrap_or_default())
        .find(|label| label.starts_with("Google Drive, "))
        .expect("the sidebar row");
    let synced = label.find("Synced").expect("the sync age");
    let mode = label.find("Mirror to Drive").expect("the mode");

    assert!(synced < mode, "{label}");
}

#[test]
fn a_mirror_stopped_by_the_delete_cap_says_what_it_already_deleted() {
    let stopped = "3 files were already deleted before the stop. Deleted files are kept in the versions folder of this sync.";
    let mut row = mirrored(SyncMode::MirrorDown, "/home/ada/Notes");
    row.status = DisplayStatus::NeedsReview;
    row.gate = Some(GateKind::TooManyDeletes);
    row.gate_detail = stopped.into();
    let harness = harness_sized(view(row), egui::vec2(960.0, 640.0));

    harness.get_by_label(&format!(
        "{stopped} Review what will change before syncing resumes."
    ));
}

fn shown_times(harness: &Harness<'_, Screen>, text: &str) -> usize {
    harness.query_all_by_label(text).count()
}

#[test]
fn only_a_cut_subtitle_shows_in_full_on_hover() {
    let long = "/home/ada/Documents/Projects/Archive/2025/Quarterly Reports/Finance and Accounting";
    let size = egui::vec2(760.0, 520.0);
    let cut_text = format!("{long} ↔ gdrive:Notes");
    let mut cut = harness_sized(
        view(PairRow {
            local: long.into(),
            ..pair(DisplayStatus::UpToDate)
        }),
        size,
    );
    let whole_text = "/home/ada/Drive ↔ gdrive:Notes";
    let mut whole = harness_sized(view(pair(DisplayStatus::UpToDate)), size);

    let (cut_before, whole_before) = (
        shown_times(&cut, &cut_text),
        shown_times(&whole, whole_text),
    );
    cut.get_by_label(&cut_text).hover();
    cut.run_steps(8);
    whole.get_by_label(whole_text).hover();
    whole.run_steps(8);

    assert!(shown_times(&cut, &cut_text) > cut_before);
    assert_eq!(shown_times(&whole, whole_text), whole_before);
}

#[test]
fn only_a_cut_title_shows_in_full_on_hover() {
    let long = "Quarterly reports of the finance and accounting department for every year";
    let size = egui::vec2(760.0, 520.0);
    let mut cut = harness_sized(
        view(PairRow {
            name: long.into(),
            ..pair(DisplayStatus::UpToDate)
        }),
        size,
    );
    let mut whole = harness_sized(view(pair(DisplayStatus::UpToDate)), size);
    let (cut_before, whole_before) = (shown_times(&cut, long), shown_times(&whole, "Google Drive"));

    cut.get_by_label(long).hover();
    cut.run_steps(8);
    whole.get_by_label("Google Drive").hover();
    whole.run_steps(8);

    assert!(shown_times(&cut, long) > cut_before);
    assert_eq!(shown_times(&whole, "Google Drive"), whole_before);
}

fn syncing_progress(total_transfers: u64) -> Progress {
    Progress {
        transfers: 412,
        total_transfers,
        bytes: 1_800_000_000,
        total_bytes: 4_300_000_000,
        speed: 12_400_000.0,
        eta_seconds: Some(170),
        current: Some("Fotos/23-12-2025/IMG_4471.jpg".into()),
    }
}

#[test]
fn sync_now_is_enabled_for_an_idle_sync_and_asks_for_a_round() {
    let mut harness = harness(view(pair(DisplayStatus::UpToDate)));

    assert!(!disabled(&harness, "Sync Now"));
    harness.get_by_label("Sync Now").click();
    harness.run_steps(4);

    assert_eq!(harness.state().actions, [Action::SyncNow("drive".into())]);
}

#[test]
fn a_sync_without_real_time_watching_says_how_to_turn_it_back_on() {
    let off = harness(view(PairRow {
        realtime_off: true,
        ..pair(DisplayStatus::UpToDate)
    }));
    let on = harness(view(pair(DisplayStatus::UpToDate)));
    let notice = "Real-time sync is off: raise fs.inotify.max_user_watches.";

    assert!(off.query_by_label(notice).is_some());
    assert!(on.query_by_label(notice).is_none());
}

#[test]
fn sync_now_is_disabled_while_a_round_runs() {
    let mut harness = harness(view(pair(DisplayStatus::Syncing)));

    assert!(disabled(&harness, "Sync Now"));
    harness.get_by_label("Sync Now").click();
    harness.run_steps(4);

    assert!(harness.state().actions.is_empty());
}

#[test]
fn sync_now_is_disabled_with_any_gate() {
    let gates = [
        (GateKind::NeedsReviewFirstRun, DisplayStatus::NeedsReview),
        (
            GateKind::NeedsReviewFiltersChanged,
            DisplayStatus::NeedsReview,
        ),
        (
            GateKind::NeedsReviewCriticalError,
            DisplayStatus::NeedsReview,
        ),
        (GateKind::TooManyDeletes, DisplayStatus::NeedsReview),
        (GateKind::AllFilesChanged, DisplayStatus::NeedsReview),
        (GateKind::ConfigError, DisplayStatus::Error),
        (GateKind::AuthError, DisplayStatus::Error),
        (GateKind::LocalMissing, DisplayStatus::Error),
    ];
    for (gate, status) in gates {
        let harness = harness(view(gated(status, gate)));

        assert!(disabled(&harness, "Sync Now"), "{gate:?}");
    }
}

#[test]
fn pause_pauses_an_active_sync() {
    let mut harness = harness(view(pair(DisplayStatus::UpToDate)));

    harness.get_by_label("Pause").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::SetEnabled("drive".into(), false)]
    );
}

#[test]
fn a_paused_sync_offers_resume_and_cannot_sync_now() {
    let mut harness = harness(view(pair(DisplayStatus::Paused)));

    assert!(disabled(&harness, "Sync Now"));
    harness.get_by_label("Resume").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::SetEnabled("drive".into(), true)]
    );
}

#[test]
fn the_more_menu_opens_folders_and_asks_to_remove() {
    let mut harness = harness(view(pair(DisplayStatus::UpToDate)));

    for (label, action) in [
        ("Open Local Folder", Action::OpenLocalFolder("drive".into())),
        (
            "Open Versions Folder",
            Action::OpenVersionsFolder("drive".into()),
        ),
        ("Remove Sync…", Action::RemoveSync("drive".into())),
    ] {
        harness.get_by_label("More").click();
        harness.run_steps(4);
        harness.get_by_label(label).click();
        harness.run_steps(4);

        assert_eq!(harness.state().actions.last(), Some(&action), "{label}");
    }
}

#[test]
fn the_card_of_a_sync_waiting_for_review_says_so_and_offers_review() {
    let mut harness = harness(view(gated(
        DisplayStatus::NeedsReview,
        GateKind::NeedsReviewFiltersChanged,
    )));

    harness.get_by_label("Review needed");
    harness.get_by_label("Review what will change before syncing resumes.");
    harness.get_by_label("Review…").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::OpenReview("drive".into())]
    );
}

#[test]
fn the_card_of_a_disconnected_account_offers_to_reconnect_in_settings() {
    let mut harness = harness(view(gated(DisplayStatus::Error, GateKind::AuthError)));

    harness.get_by_label("Needs attention");
    harness.get_by_label("Reconnect \"gdrive\" in Settings > Accounts.");
    harness.get_by_label("Reconnect Account…").click();
    harness.run_steps(4);

    assert_eq!(harness.state().actions, [Action::OpenAccounts]);
}

#[test]
fn the_card_of_a_sync_that_needs_attention_explains_it_shows_the_message_and_offers_to_try_again() {
    let row = PairRow {
        gate_detail: "googleapi: Error 403: storageQuotaExceeded".into(),
        gate_fault: Some(Fault::Quota),
        ..gated(DisplayStatus::Error, GateKind::NeedsAttention)
    };
    let mut harness = harness(view(row));

    harness.get_by_label("Needs attention");
    harness.get_by_label("Google Drive is full.");
    harness.get_by_label("googleapi: Error 403: storageQuotaExceeded");
    assert!(!disabled(&harness, "Sync Now"));
    harness.get_by_label("Try Again").click();
    harness.run_steps(4);

    assert_eq!(harness.state().actions, [Action::SyncNow("drive".into())]);
}

#[test]
fn the_card_of_an_older_gate_without_a_stored_fault_reads_it_from_the_message() {
    let row = PairRow {
        gate_detail: "googleapi: Error 403: storageQuotaExceeded".into(),
        gate_fault: None,
        ..gated(DisplayStatus::Error, GateKind::NeedsAttention)
    };
    let harness = harness(view(row));

    harness.get_by_label("Google Drive is full.");
}

#[test]
fn the_card_of_a_missing_folder_names_it_and_offers_no_button() {
    let harness = harness(view(gated(DisplayStatus::Error, GateKind::LocalMissing)));

    harness.get_by_label("/home/ada/Drive is missing.");
    assert!(harness.query_by_label("Review…").is_none());
    assert!(harness.query_by_label("Reconnect Account…").is_none());
}

#[test]
fn the_card_of_an_up_to_date_sync_says_when_it_last_synced() {
    let harness = harness(view(pair(DisplayStatus::UpToDate)));

    harness.get_by_label("Up to date");
    harness.get_by_label("Synced 5 min ago");
}

#[test]
fn a_running_round_shows_a_determinate_bar_the_numbers_and_the_current_file() {
    let mut running = pair(DisplayStatus::Syncing);
    running.progress = Some(syncing_progress(980));
    let harness = harness(view(running));

    harness.get_by_label("412 of 980 files · 1.8 GB/4.3 GB · 12.4 MB/s · 3 min left");
    harness.get_by_label("Current: Fotos/23-12-2025/IMG_4471.jpg");
    let bar = harness.get_by_role(egui::accesskit::Role::ProgressIndicator);
    let percent = bar.accesskit_node().numeric_value().unwrap();
    assert!((percent - 42.0).abs() < 0.5, "{percent}");
}

#[test]
fn a_running_round_without_a_total_shows_an_indeterminate_bar() {
    let mut running = pair(DisplayStatus::Syncing);
    running.progress = Some(syncing_progress(0));
    let harness = harness(view(running));

    harness.get_by_label("Checking for changes…");
    let bar = harness.get_by_role(egui::accesskit::Role::ProgressIndicator);
    assert_eq!(bar.accesskit_node().numeric_value(), None);
}

#[test]
fn the_tabs_are_activity_conflicts_and_filters_and_picking_one_asks_to_switch() {
    let mut harness = harness(view(pair(DisplayStatus::UpToDate)));

    harness.get_by_label("Activity");
    harness.get_by_label("Filters");
    harness.get_by_label("Conflicts").click();
    harness.run_steps(4);

    assert_eq!(
        harness.state().actions,
        [Action::SelectTab(DetailTab::Conflicts)]
    );
}

#[test]
fn the_selected_tab_decides_what_the_detail_shows() {
    let mut vm = view(pair(DisplayStatus::UpToDate));
    vm.tab = DetailTab::Conflicts;
    let harness = harness(vm);

    harness.get_by_label("No conflicts");
    assert!(harness.query_by_label("No activity yet").is_none());
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
        },
        detail: None,
        auto_reviewed: None,
        idle: false,
        messages: vec!["Synching Path1".into(), "Bisync successful".into()],
    }
}

fn with_runs(runs: Vec<RunEntry>) -> ViewModel {
    let mut vm = view(pair(DisplayStatus::UpToDate));
    vm.runs = Some(Arc::from(runs));
    vm
}

#[test]
fn activity_without_rounds_says_there_is_none_yet() {
    let harness = harness(with_runs(Vec::new()));

    harness.get_by_label("No activity yet");
}

#[test]
fn activity_lists_a_round_with_its_time_duration_result_and_counters() {
    let harness = harness(with_runs(vec![run(
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
    });
    let plain = run("b", "2026-11-03T11:50:00Z", DisplayStatus::UpToDate);
    let harness = harness(with_runs(vec![reviewed, plain]));

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
    let harness = harness(with_runs(vec![idle]));

    harness.get_by_label("Today 11:55");
    harness.get_by_label("No changes");
}

#[test]
fn activity_shows_the_reason_of_a_failed_round_instead_of_counters() {
    let mut failed = run("b", "2026-11-02T13:50:00Z", DisplayStatus::Retrying);
    failed.detail = Some("network error".into());
    let harness = harness(with_runs(vec![failed]));

    harness.get_by_label("Yesterday 13:50");
    harness.get_by_label("Will retry");
    harness.get_by_label("network error");
}

#[test]
fn activity_shows_the_round_in_progress_as_running() {
    let mut running = pair(DisplayStatus::Syncing);
    running.progress = Some(syncing_progress(980));
    let mut vm = view(running);
    let mut current = run("a", "2026-11-03T11:59:00Z", DisplayStatus::Syncing);
    current.seconds = None;
    vm.runs = Some(Arc::from(vec![current]));
    let harness = harness(vm);

    harness.get_by_label("running");
    harness.get_by_label("412 of 980 files");
}

#[test]
fn clicking_a_round_shows_its_log_and_clicking_again_hides_it() {
    let mut harness = harness(with_runs(vec![run(
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
