use egui::{Key, Ui};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::bisync::ReviewKind;
use syncfast_core::bisync_log::{Change, ChangeKind, RunSummary};
use syncfast_core::preview::Preview;
use syncfast_core::state::GateKind;
use syncfast_gui::theme::{self, Palette};
use syncfast_gui::ui::Action;
use syncfast_gui::ui::review::{
    ReviewAction, ReviewPhase, ReviewView, review_kind, show, subtitle,
};

const REMOTE: &str = "devremote";

struct Case {
    gate: GateKind,
    phase: ReviewPhase,
    actions: Vec<Action>,
}

/// Installs the theme, and tells whether fonts are usable: `set_fonts` takes
/// effect on the next pass, so the first one must not draw.
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

fn open_sheet(gate: GateKind, phase: ReviewPhase) -> Harness<'static, Case> {
    let case = Case {
        gate,
        phase,
        actions: Vec::new(),
    };
    let mut harness = Harness::new_ui_state(
        |ui, case: &mut Case| {
            if ready(ui) {
                let view = ReviewView {
                    name: "Docs",
                    remote: REMOTE,
                    gate: case.gate,
                    phase: &case.phase,
                };
                let actions = show(ui.ctx(), &view);
                case.actions.extend(actions);
            }
        },
        case,
    );
    harness.run_ok();
    harness.run_steps(3);
    harness
}

fn change(kind: ChangeKind, path: &str) -> Change {
    Change {
        kind,
        path: path.to_owned(),
    }
}

fn changes(kind: ChangeKind, prefix: &str, count: usize) -> Vec<Change> {
    (0..count)
        .map(|index| change(kind, &format!("{prefix}{index:03}.txt")))
        .collect()
}

fn preview(changes: Vec<Change>) -> ReviewPhase {
    ReviewPhase::Ready(Preview {
        filters_hash: "hash".to_owned(),
        summary: RunSummary {
            changes,
            ..RunSummary::default()
        },
    })
}

fn ready_sheet(changes: Vec<Change>) -> Harness<'static, Case> {
    open_sheet(GateKind::NeedsReviewFirstRun, preview(changes))
}

fn review(action: ReviewAction) -> Vec<Action> {
    vec![Action::Review(action)]
}

fn is_disabled(harness: &Harness<'_, Case>, label: &str) -> bool {
    harness.get_by_label(label).accesskit_node().is_disabled()
}

#[test]
fn review_kind_asks_a_resync_for_the_needs_review_gates() {
    let gates = [
        GateKind::NeedsReviewFirstRun,
        GateKind::NeedsReviewFiltersChanged,
        GateKind::NeedsReviewCriticalError,
    ];

    for gate in gates {
        assert_eq!(review_kind(gate), ReviewKind::Resync, "{gate:?}");
    }
}

#[test]
fn review_kind_asks_a_force_for_the_mass_change_gates() {
    for gate in [GateKind::TooManyDeletes, GateKind::AllFilesChanged] {
        assert_eq!(review_kind(gate), ReviewKind::Force, "{gate:?}");
    }
}

#[test]
fn subtitle_names_why_each_gate_asks_for_a_review() {
    let expected = [
        (GateKind::NeedsReviewFirstRun, "First sync"),
        (GateKind::NeedsReviewFiltersChanged, "Filters changed"),
        (
            GateKind::NeedsReviewCriticalError,
            "Sync stopped after an error",
        ),
        (GateKind::TooManyDeletes, "Many files would be deleted"),
        (GateKind::AllFilesChanged, "Every file changed"),
    ];

    for (gate, text) in expected {
        assert_eq!(subtitle(gate), text, "{gate:?}");
    }
}

#[test]
fn sheet_shows_the_subtitle_of_each_gate() {
    let gates = [
        GateKind::NeedsReviewFirstRun,
        GateKind::NeedsReviewFiltersChanged,
        GateKind::NeedsReviewCriticalError,
        GateKind::TooManyDeletes,
        GateKind::AllFilesChanged,
    ];

    for gate in gates {
        let harness = open_sheet(gate, preview(Vec::new()));

        assert!(harness.query_by_label(subtitle(gate)).is_some(), "{gate:?}");
    }
}

#[test]
fn sheet_title_names_the_sync() {
    let harness = ready_sheet(Vec::new());

    assert!(harness.query_by_label("Review Changes for Docs").is_some());
}

#[test]
fn empty_preview_says_nothing_changes_and_allows_sync() {
    let harness = ready_sheet(Vec::new());

    assert!(harness.query_by_label("Nothing to change.").is_some());
    assert!(!is_disabled(&harness, "Sync"));
}

#[test]
fn empty_preview_has_no_dry_run_footnote() {
    let harness = ready_sheet(Vec::new());

    assert!(
        harness
            .query_by_label_contains("Preview from a dry run")
            .is_none()
    );
}

#[test]
fn non_empty_preview_shows_the_dry_run_footnote() {
    let harness = ready_sheet(changes(ChangeKind::Upload, "up/", 1));

    assert!(
        harness
            .query_by_label(
                "Preview from a dry run. A few listed deletions may not happen in the real sync."
            )
            .is_some()
    );
    assert!(harness.query_by_label("Nothing to change.").is_none());
}

#[test]
fn uploads_section_is_labelled_with_remote_and_count_and_lists_its_paths() {
    let harness = ready_sheet(changes(ChangeKind::Upload, "up/", 2));

    assert!(
        harness
            .query_by_label("Would upload to devremote (2)")
            .is_some()
    );
    assert!(harness.query_by_label("up/000.txt").is_some());
    assert!(harness.query_by_label("up/001.txt").is_some());
}

#[test]
fn plain_preview_confirms_with_sync_on_click() {
    let mut harness = ready_sheet(changes(ChangeKind::Upload, "up/", 1));

    harness.get_by_label("Sync").click();
    harness.run_steps(3);

    assert_eq!(harness.state().actions, review(ReviewAction::Confirm));
}

#[test]
fn plain_preview_confirms_with_sync_on_enter() {
    let mut harness = ready_sheet(changes(ChangeKind::Upload, "up/", 1));

    harness.key_press(Key::Enter);
    harness.run_steps(3);

    assert_eq!(harness.state().actions, review(ReviewAction::Confirm));
}

#[test]
fn preview_with_deletions_offers_delete_and_sync_instead_of_sync() {
    let harness = ready_sheet(changes(ChangeKind::DeleteRemote, "gone/", 1));

    assert!(harness.query_by_label("Delete and Sync").is_some());
    assert!(harness.query_by_label("Sync").is_none());
}

#[test]
fn local_deletions_also_offer_delete_and_sync() {
    let harness = ready_sheet(changes(ChangeKind::DeleteLocal, "gone/", 1));

    assert!(harness.query_by_label("Delete and Sync").is_some());
}

#[test]
fn delete_and_sync_is_not_confirmed_by_enter() {
    let mut harness = ready_sheet(changes(ChangeKind::DeleteRemote, "gone/", 1));

    harness.key_press(Key::Enter);
    harness.run_steps(3);

    assert_eq!(harness.state().actions, review(ReviewAction::Cancel));
}

#[test]
fn delete_and_sync_confirms_on_click() {
    let mut harness = ready_sheet(changes(ChangeKind::DeleteRemote, "gone/", 1));

    harness.get_by_label("Delete and Sync").click();
    harness.run_steps(3);

    assert_eq!(harness.state().actions, review(ReviewAction::Confirm));
}

#[test]
fn delete_section_is_labelled_and_open_even_when_not_first() {
    let mut all = changes(ChangeKind::Upload, "up/", 1);
    all.extend(changes(ChangeKind::DeleteRemote, "gone/", 3));
    let harness = ready_sheet(all);

    assert!(
        harness
            .query_by_label("Would delete on devremote (3)")
            .is_some()
    );
    assert!(harness.query_by_label("gone/000.txt").is_some());
    assert!(harness.query_by_label("gone/002.txt").is_some());
}

#[test]
fn local_delete_section_is_open_by_default() {
    let mut all = changes(ChangeKind::Upload, "up/", 1);
    all.extend(changes(ChangeKind::DeleteLocal, "gone/", 1));
    let harness = ready_sheet(all);

    assert!(
        harness
            .query_by_label("Would delete on this computer (1)")
            .is_some()
    );
    assert!(harness.query_by_label("gone/000.txt").is_some());
}

#[test]
fn only_the_first_non_empty_section_is_open_by_default() {
    let mut all = changes(ChangeKind::Download, "down/", 1);
    all.extend(changes(ChangeKind::OverwriteRemote, "over/", 1));
    let harness = ready_sheet(all);

    assert!(harness.query_by_label("down/000.txt").is_some());
    assert!(harness.query_by_label("over/000.txt").is_none());
}

#[test]
fn overwrite_local_section_says_the_old_version_is_kept_for_30_days() {
    let harness = ready_sheet(changes(ChangeKind::OverwriteLocal, "doc/", 1));

    assert!(
        harness
            .query_by_label("Would replace on this computer (old version kept for 30 days) (1)")
            .is_some()
    );
}

#[test]
fn every_kind_gets_its_title_in_order_and_empty_kinds_are_hidden() {
    let all = vec![
        change(ChangeKind::Other, "NOTICE: something odd"),
        change(ChangeKind::DeleteRemote, "d1"),
        change(ChangeKind::Upload, "u1"),
        change(ChangeKind::OverwriteRemote, "r1"),
    ];
    let harness = ready_sheet(all);

    for title in [
        "Would upload to devremote (1)",
        "Would replace on devremote (1)",
        "Would delete on devremote (1)",
        "Other changes (1)",
    ] {
        assert!(harness.query_by_label(title).is_some(), "{title}");
    }
    for hidden in [
        "Would download to this computer",
        "Would replace on this computer",
        "Would delete on this computer",
    ] {
        assert!(
            harness.query_by_label_contains(hidden).is_none(),
            "{hidden}"
        );
    }
}

#[test]
fn other_changes_list_the_raw_message() {
    let mut all = changes(ChangeKind::Upload, "u/", 1);
    all.push(change(
        ChangeKind::Other,
        "NOTICE: Skipped copy as --dry-run",
    ));
    let mut harness = ready_sheet(all);

    harness.get_by_label("Other changes (1)").click();
    harness.run_steps(3);

    assert!(
        harness
            .query_by_label("NOTICE: Skipped copy as --dry-run")
            .is_some()
    );
}

#[test]
fn clicking_a_closed_section_opens_it() {
    let mut all = changes(ChangeKind::Upload, "up/", 1);
    all.extend(changes(ChangeKind::Download, "down/", 1));
    let mut harness = ready_sheet(all);
    assert!(harness.query_by_label("down/000.txt").is_none());

    harness
        .get_by_label("Would download to this computer (1)")
        .click();
    harness.run_steps(3);

    assert!(harness.query_by_label("down/000.txt").is_some());
}

#[test]
fn clicking_an_open_section_closes_it_and_the_choice_is_kept() {
    let mut harness = ready_sheet(changes(ChangeKind::Upload, "up/", 1));

    harness
        .get_by_label("Would upload to devremote (1)")
        .click();
    harness.run_steps(3);
    harness.run_steps(3);

    assert!(harness.query_by_label("up/000.txt").is_none());
}

#[test]
fn section_with_150_paths_lists_100_and_counts_the_rest() {
    let harness = ready_sheet(changes(ChangeKind::Upload, "dir/file-", 150));

    assert!(
        harness
            .query_by_label("Would upload to devremote (150)")
            .is_some()
    );
    assert_eq!(
        harness.query_all_by_label_contains("dir/file-").count(),
        100
    );
    assert!(harness.query_by_label("and 50 more").is_some());
}

#[test]
fn section_with_exactly_100_paths_has_no_more_line() {
    let harness = ready_sheet(changes(ChangeKind::Upload, "dir/file-", 100));

    assert_eq!(
        harness.query_all_by_label_contains("dir/file-").count(),
        100
    );
    assert!(harness.query_by_label_contains(" more").is_none());
}

#[test]
fn checking_shows_the_wait_text_and_disables_the_primary() {
    let harness = open_sheet(GateKind::NeedsReviewFirstRun, ReviewPhase::Checking);

    assert!(
        harness
            .query_by_label("Checking what would change…")
            .is_some()
    );
    assert!(is_disabled(&harness, "Sync"));
}

#[test]
fn checking_ignores_enter() {
    let mut harness = open_sheet(GateKind::NeedsReviewFirstRun, ReviewPhase::Checking);

    harness.key_press(Key::Enter);
    harness.run_steps(3);

    assert!(harness.state().actions.is_empty());
}

#[test]
fn busy_says_the_review_opens_when_the_sync_finishes_and_disables_the_primary() {
    let harness = open_sheet(GateKind::NeedsReviewFirstRun, ReviewPhase::Busy);

    assert!(
        harness
            .query_by_label("This sync is running. The review opens when it finishes.")
            .is_some()
    );
    assert!(is_disabled(&harness, "Sync"));
}

#[test]
fn failed_shows_the_message_and_disables_the_primary() {
    let harness = open_sheet(
        GateKind::NeedsReviewFirstRun,
        ReviewPhase::Failed("couldn't list remote: token expired".to_owned()),
    );

    assert!(
        harness
            .query_by_label("couldn't list remote: token expired")
            .is_some()
    );
    assert!(is_disabled(&harness, "Sync"));
}

#[test]
fn try_again_after_a_failure_emits_retry() {
    let mut harness = open_sheet(
        GateKind::NeedsReviewFirstRun,
        ReviewPhase::Failed("boom".to_owned()),
    );

    harness.get_by_label("Try Again").click();
    harness.run_steps(3);

    assert_eq!(harness.state().actions, review(ReviewAction::Retry));
}

#[test]
fn cancel_button_emits_cancel() {
    let mut harness = ready_sheet(changes(ChangeKind::Upload, "up/", 1));

    harness.get_by_label("Cancel").click();
    harness.run_steps(3);

    assert_eq!(harness.state().actions, review(ReviewAction::Cancel));
}

#[test]
fn escape_emits_cancel_in_every_phase() {
    let phases = [
        ReviewPhase::Checking,
        ReviewPhase::Busy,
        ReviewPhase::Failed("boom".to_owned()),
        preview(changes(ChangeKind::Upload, "up/", 1)),
    ];

    for phase in phases {
        let mut harness = open_sheet(GateKind::NeedsReviewFirstRun, phase);

        harness.key_press(Key::Escape);
        harness.run_steps(3);

        assert_eq!(harness.state().actions, review(ReviewAction::Cancel));
    }
}
