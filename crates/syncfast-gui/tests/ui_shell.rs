#![allow(clippy::unwrap_used)]

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use jiff::Timestamp;
use syncfast_core::state::{DisplayStatus, GateKind};
use syncfast_gui::theme::{self, Palette};
use syncfast_gui::ui::{self, Action, PairRow, ViewModel};

fn now() -> Timestamp {
    "2026-11-03T12:00:00Z".parse().unwrap()
}

fn row(id: &str, name: &str, status: DisplayStatus) -> PairRow {
    PairRow {
        id: id.into(),
        name: name.into(),
        local: format!("/home/ada/{id}").into(),
        remote: format!("gdrive:{id}"),
        enabled: status != DisplayStatus::Paused,
        status,
        gate: None,
        last_success: Some("2026-11-03T11:55:00Z".parse().unwrap()),
        progress: None,
        gate_detail: String::new(),
        consecutive_failures: 0,
    }
}

fn view(pairs: Vec<PairRow>, selected: Option<&str>) -> ViewModel {
    ViewModel {
        pairs,
        selected: selected.map(ToOwned::to_owned),
        sidebar_width: 240.0,
        error: None,
        now: now(),
        tab: ui::DetailTab::Activity,
        runs: None,
        remotes: Vec::new(),
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
    Harness::builder()
        .with_size(egui::vec2(960.0, 640.0))
        .build_ui_state(
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
                let mut actions = ui::shortcuts(ui.ctx(), &screen.vm);
                actions.extend(ui::root(ui, &screen.vm));
                screen.actions.extend(actions);
            },
            Screen {
                vm,
                actions: Vec::new(),
                themed: false,
            },
        )
}

fn two_syncs() -> Vec<PairRow> {
    vec![
        row("drive", "Google Drive", DisplayStatus::UpToDate),
        row("notes", "Notes", DisplayStatus::Syncing),
    ]
}

#[test]
fn sidebar_lists_every_sync_with_its_status_text() {
    let mut harness = harness(view(
        vec![
            row("drive", "Google Drive", DisplayStatus::UpToDate),
            row("notes", "Notes", DisplayStatus::NeedsReview),
            row("work", "Work", DisplayStatus::Conflicts(3)),
        ],
        Some("drive"),
    ));

    harness.run();

    harness.get_by_label("Syncs");
    harness.get_by_label_contains("Google Drive, Up to date");
    harness.get_by_label_contains("Notes, Review needed");
    harness.get_by_label_contains("Work, 3 conflicts");
}

#[test]
fn sidebar_line_under_the_name_says_when_the_sync_last_succeeded() {
    let mut never = row("notes", "Notes", DisplayStatus::NeedsReview);
    never.last_success = None;
    let mut harness = harness(view(
        vec![row("drive", "Google Drive", DisplayStatus::UpToDate), never],
        Some("drive"),
    ));

    harness.run();

    harness.get_by_label_contains("Google Drive, Up to date, Synced 5 min ago");
    harness.get_by_label_contains("Notes, Review needed, Never synced");
}

#[test]
fn clicking_a_sync_selects_it() {
    let mut harness = harness(view(two_syncs(), Some("drive")));
    harness.run();

    harness.get_by_label_contains("Notes").click();
    harness.run();

    assert!(
        harness
            .state()
            .actions
            .contains(&Action::SelectPair("notes".into()))
    );
}

#[test]
fn plus_button_in_the_sidebar_header_asks_to_add_a_sync() {
    let mut harness = harness(view(two_syncs(), Some("drive")));
    harness.run();

    harness.get_by_label("Add Sync…").click();
    harness.run();

    assert!(harness.state().actions.contains(&Action::AddSync));
}

#[test]
fn window_title_is_the_selected_sync_or_syncs_without_selection() {
    let selected = view(two_syncs(), Some("notes"));
    let none = view(two_syncs(), None);
    let unknown = view(two_syncs(), Some("gone"));

    assert_eq!(selected.title(), "Notes");
    assert_eq!(none.title(), "Syncs");
    assert_eq!(unknown.title(), "Syncs");
}

#[test]
fn without_syncs_the_window_offers_to_add_one() {
    let mut harness = harness(view(Vec::new(), None));

    harness.run();
    harness.get_by_label("No syncs yet");
    let buttons: Vec<_> = harness.get_all_by_label("Add Sync…").collect();
    assert_eq!(
        buttons.len(),
        2,
        "the sidebar's plus and the empty state's button"
    );
    buttons.last().unwrap().click();
    harness.run();

    assert!(harness.state().actions.contains(&Action::AddSync));
}

#[test]
fn a_problem_is_shown_in_the_detail_area() {
    let mut vm = view(two_syncs(), Some("drive"));
    vm.error = Some("config.toml: expected a table".into());
    let mut harness = harness(vm);

    harness.run();

    harness.get_by_label_contains("config.toml: expected a table");
}

fn press(harness: &mut Harness<'_, Screen>, key: Key) {
    harness.key_press_modifiers(Modifiers::CTRL, key);
    harness.run_steps(4);
}

#[test]
fn ctrl_shortcuts_emit_the_matching_actions() {
    let mut harness = harness(view(two_syncs(), Some("drive")));
    harness.run();

    for key in [Key::N, Key::R, Key::Comma, Key::W, Key::Q] {
        press(&mut harness, key);
    }

    assert_eq!(
        harness.state().actions,
        [
            Action::AddSync,
            Action::SyncNow("drive".into()),
            Action::OpenSettings,
            Action::HideWindow,
            Action::Quit,
        ]
    );
}

#[test]
fn ctrl_r_does_nothing_while_the_selected_sync_is_running_or_blocked() {
    let mut running = harness(view(two_syncs(), Some("notes")));
    let mut blocked_rows = two_syncs();
    blocked_rows[0].gate = Some(GateKind::NeedsReviewFirstRun);
    let mut blocked = harness(view(blocked_rows, Some("drive")));
    let mut nothing = harness(view(Vec::new(), None));

    for harness in [&mut running, &mut blocked, &mut nothing] {
        harness.run_steps(4);
        press(harness, Key::R);
    }

    assert!(running.state().actions.is_empty());
    assert!(blocked.state().actions.is_empty());
    assert!(nothing.state().actions.is_empty());
}

struct Window {
    _dir: tempfile::TempDir,
    app: syncfast_gui::app::App,
    attached: bool,
}

fn open_window() -> Harness<'static, Window> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let paths = syncfast_core::paths::Paths::new(
        root.join("home"),
        root.join("config"),
        root.join("state"),
        root.join("data"),
        root.join("run"),
    );
    let app = syncfast_gui::app::App::new(
        &paths,
        &fastframe_shell::Waker::default(),
        syncfast_gui::instance::Inbox::default(),
        syncfast_gui::gui_state::GuiState::default(),
        theme::Themes::load(&root.join("config/themes")),
        false,
    )
    .unwrap();
    Harness::builder()
        .with_size(egui::vec2(960.0, 640.0))
        .build_ui_state(
            |ui, window: &mut Window| {
                // The creator of the real window does this before the first frame.
                if !window.attached {
                    window.app.attach(ui.ctx());
                    window.attached = true;
                    ui.ctx().request_repaint();
                    return;
                }
                window.app.window_logic(ui.ctx());
                window.app.frame_ui(ui);
            },
            Window {
                _dir: dir,
                app,
                attached: false,
            },
        )
}

#[test]
fn a_new_system_colour_scheme_repaints_the_open_window_without_any_input() {
    let mut harness = open_window();
    harness.run();
    assert!(
        theme::palette(&harness.ctx).dark,
        "dark until the portal answers"
    );

    harness
        .state_mut()
        .app
        .handle_event(syncfast_gui::backend::Event::SystemDark(false));
    harness.run();

    assert!(!theme::palette(&harness.ctx).dark);
}
