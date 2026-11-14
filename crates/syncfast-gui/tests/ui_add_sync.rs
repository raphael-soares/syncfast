#![allow(clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Key, Ui};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::behavior::SyncMode;
use syncfast_core::state::DisplayStatus;
use syncfast_gui::backend::Event;
use syncfast_gui::sheets::{Sheets, Surroundings};
use syncfast_gui::theme::{self, Palette};
use syncfast_gui::ui::account_sheet::AccountAction;
use syncfast_gui::ui::add_sync::{
    AddSyncAction, AddSyncState, AddSyncView, DirListing, LocalFacts, LocalStatus, NewSync, show,
};
use syncfast_gui::ui::filters::{FilterRules, FilterTest, FilterTestState};
use syncfast_gui::ui::{Action, PairRow, RemoteRow};

struct Sheet {
    state: AddSyncState,
    remotes: Vec<RemoteRow>,
    existing: Vec<PairRow>,
    dirs: HashMap<String, DirListing>,
    local: Option<LocalFacts>,
    mkdir_error: Option<String>,
    local_error: Option<String>,
    create_error: Option<String>,
    creating: bool,
    filter_test: Option<FilterTest>,
    actions: Vec<Action>,
}

fn remote(name: &str, kind: &str, own_client_id: bool) -> RemoteRow {
    RemoteRow {
        name: name.into(),
        kind: kind.into(),
        own_client_id,
        email: None,
    }
}

fn pair(id: &str, name: &str, local: &str, remote: &str) -> PairRow {
    PairRow {
        id: id.into(),
        name: name.into(),
        local: local.into(),
        remote: remote.into(),
        enabled: true,
        status: DisplayStatus::UpToDate,
        gate: None,
        last_success: None,
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

fn dir(names: &[&str]) -> DirListing {
    DirListing::Loaded(names.iter().map(|name| (*name).to_owned()).collect())
}

fn facts(path: &str, status: LocalStatus) -> LocalFacts {
    LocalFacts {
        path: path.into(),
        status,
    }
}

const NOTES: &str = "/home/ada/Documents/notes";

/// An account and a local folder that the app has not checked yet.
fn filled() -> AddSyncState {
    AddSyncState {
        account: Some("devremote".into()),
        local: NOTES.into(),
        ..AddSyncState::default()
    }
}

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

/// Plays the app: a `LoadDirs` is answered with `Loading` at once, so the
/// request is not repeated.
fn harness_with(state: AddSyncState, edit: impl FnOnce(&mut Sheet)) -> Harness<'static, Sheet> {
    let mut sheet = Sheet {
        state,
        remotes: vec![
            remote("devremote", "local", false),
            remote("gdrive", "drive", false),
            remote("trabalho", "drive", true),
        ],
        existing: vec![
            pair("drive", "Google Drive", "/home/ada/Drive", "gdrive:"),
            pair("notes", "Notes", "/home/ada/Notes", "other:Notes"),
            pair("photos", "Photos", "/srv/data/Photos", "other:Photos"),
        ],
        dirs: HashMap::new(),
        local: None,
        mkdir_error: None,
        local_error: None,
        create_error: None,
        creating: false,
        filter_test: None,
        actions: Vec::new(),
    };
    edit(&mut sheet);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(800.0, 900.0))
        .build_ui_state(
            |ui, sheet: &mut Sheet| {
                if !ready(ui) {
                    return;
                }
                let view = AddSyncView {
                    remotes: &sheet.remotes,
                    existing: &sheet.existing,
                    dirs: &sheet.dirs,
                    local: sheet.local.as_ref(),
                    mkdir_error: sheet.mkdir_error.as_deref(),
                    create_error: sheet.create_error.as_deref(),
                    creating: sheet.creating,
                    home: std::path::Path::new("/home/ada"),
                    data_dir: std::path::Path::new("/home/ada/.local/share/syncfast"),
                    local_error: sheet.local_error.as_deref(),
                    filter_test: sheet.filter_test.as_ref(),
                };
                let actions = show(ui.ctx(), &view, &mut sheet.state);
                for action in &actions {
                    if let Action::AddSyncSheet(AddSyncAction::LoadDirs(key)) = action {
                        sheet.dirs.insert(key.clone(), DirListing::Loading);
                    }
                }
                sheet.actions.extend(actions);
            },
            sheet,
        );
    harness.run_ok();
    settle(&mut harness, 1);
    harness
}

/// `state` with its local folder already checked by the app as existing.
fn harness_ready(state: AddSyncState, edit: impl FnOnce(&mut Sheet)) -> Harness<'static, Sheet> {
    let local = facts(&state.local, LocalStatus::Exists);
    harness_with(state, |sheet| {
        sheet.local = Some(local);
        edit(sheet);
    })
}

const SETTLE_CEILING: usize = 30;
/// `scroll_to_me` moves nothing for its first two frames.
const SCROLL_LATENCY: usize = 3;

fn layout(harness: &Harness<'_, Sheet>) -> Vec<Option<egui::accesskit::Rect>> {
    harness
        .root()
        .children_recursive()
        .map(|node| node.accesskit_node().bounding_box())
        .collect()
}

/// Steps, at least `at_least` frames, until no widget moves for two frames
/// in a row: the sheet is an `Area` that recentres over a few frames when
/// its content changes size.
fn settle(harness: &mut Harness<'_, Sheet>, at_least: usize) {
    let mut before = layout(harness);
    let mut still = 0;
    let settled = (1..=SETTLE_CEILING).any(|frames| {
        harness.step();
        let after = layout(harness);
        still = if after == before { still + 1 } else { 0 };
        before = after;
        still >= 2 && frames >= at_least
    });
    assert!(
        settled,
        "the layout still moves after {SETTLE_CEILING} frames"
    );
}

fn is_disabled(harness: &Harness<'_, Sheet>, label: &str) -> bool {
    harness.get_by_label(label).accesskit_node().is_disabled()
}

fn click(harness: &mut Harness<'_, Sheet>, label: &str) {
    harness.get_by_label(label).scroll_to_me();
    settle(harness, SCROLL_LATENCY);
    harness.get_by_label(label).click();
    harness.run_steps(4);
}

fn type_into(harness: &mut Harness<'_, Sheet>, label: &str, text: &str) {
    type_into_role(harness, Role::TextInput, label, text);
}

/// A focused field blinks its caret, so the frames are counted, not awaited.
fn type_into_role(harness: &mut Harness<'_, Sheet>, role: Role, label: &str, text: &str) {
    let field = harness.get_by_role_and_label(role, label);
    field.scroll_to_me();
    settle(harness, SCROLL_LATENCY);
    let field = harness.get_by_role_and_label(role, label);
    field.click();
    field.type_text(text);
    harness.run_steps(4);
}

fn shown(harness: &Harness<'_, Sheet>, label: &str) -> bool {
    harness.query_by_label(label).is_some()
}

fn sheet_actions(harness: &Harness<'_, Sheet>) -> Vec<AddSyncAction> {
    harness
        .state()
        .actions
        .iter()
        .filter_map(|action| match action {
            Action::AddSyncSheet(action) => Some(action.clone()),
            _ => None,
        })
        .collect()
}

fn created(harness: &Harness<'_, Sheet>) -> Option<NewSync> {
    sheet_actions(harness)
        .into_iter()
        .find_map(|action| match action {
            AddSyncAction::Create(new) => Some(new),
            _ => None,
        })
}

fn load_requests(harness: &Harness<'_, Sheet>) -> Vec<AddSyncAction> {
    sheet_actions(harness)
        .into_iter()
        .filter(|action| matches!(action, AddSyncAction::LoadDirs(_)))
        .collect()
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not read in tests".into())
    }
}

#[test]
fn the_only_account_is_preselected() {
    let harness = harness_with(AddSyncState::default(), |sheet| {
        sheet.remotes = vec![RemoteRow {
            email: Some("ada@example.com".into()),
            ..remote("devremote", "drive", true)
        }];
    });

    assert_eq!(harness.state().state.account.as_deref(), Some("devremote"));
    assert!(shown(&harness, "In ada@example.com"));
}

#[test]
fn with_several_accounts_none_is_chosen_and_create_sync_waits_for_one() {
    let harness = harness_ready(
        AddSyncState {
            local: NOTES.into(),
            ..AddSyncState::default()
        },
        |_| {},
    );

    assert_eq!(harness.state().state.account, None);
    let combo = harness.get_by_role_and_label(Role::ComboBox, "Google Drive account");
    assert_eq!(combo.value().as_deref(), Some("Choose an account"));
    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn the_chosen_account_is_named_in_the_account_list() {
    let mut state = filled();
    state.account = Some("gdrive".into());
    let harness = harness_with(state, |_| {});

    let combo = harness.get_by_role_and_label(Role::ComboBox, "Google Drive account");
    assert_eq!(combo.value().as_deref(), Some("gdrive"));
}

#[test]
fn the_shared_client_warning_shows_only_for_a_chosen_account_without_its_own_client() {
    let mut shared = filled();
    shared.account = Some("gdrive".into());
    let warned = harness_with(shared, |_| {});
    let mut own_client = filled();
    own_client.account = Some("trabalho".into());
    let own = harness_with(own_client, |_| {});

    assert!(shown(&warned, "Uses rclone's shared client ID"));
    assert!(!shown(&own, "Uses rclone's shared client ID"));
}

#[test]
fn without_accounts_the_sheet_offers_to_connect_one() {
    let mut harness = harness_with(AddSyncState::default(), |sheet| sheet.remotes.clear());
    assert!(shown(&harness, "No Google Drive account is connected yet."));

    click(&mut harness, "Connect Google Drive\u{2026}");

    assert_eq!(harness.state().actions, [Action::AddAccount]);
}

#[test]
fn the_root_listing_of_the_account_is_asked_for_once() {
    let mut harness = harness_with(filled(), |_| {});
    harness.run();

    assert_eq!(
        load_requests(&harness),
        [AddSyncAction::LoadDirs("devremote:".into())]
    );
}

#[test]
fn a_root_listing_that_exists_is_not_asked_for() {
    let harness = harness_with(filled(), |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Backup"]));
    });

    assert!(load_requests(&harness).is_empty());
}

#[test]
fn the_drive_folder_follows_the_local_folder_until_edited() {
    let mut state = filled();
    state.local = "/home/ada/Documents/Projects".into();
    let mut harness = harness_with(state, |_| {});
    assert_eq!(harness.state().state.remote_folder, "Projects");

    type_into(&mut harness, "Folder on Google Drive", " Mine");
    harness.state_mut().state.local = "/home/ada/Other".into();
    harness.run_steps(4);

    assert_eq!(harness.state().state.remote_folder, "Projects Mine");
}

#[test]
fn a_drive_folder_with_a_leading_slash_is_refused() {
    let mut state = filled();
    state.remote_folder = "/Docs".into();
    state.remote_edited = true;
    let harness = harness_ready(state, |_| {});

    assert!(shown(
        &harness,
        "Write the Drive folder without a leading \"/\"."
    ));
    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn a_trailing_slash_is_dropped_from_the_drive_folder() {
    let mut state = filled();
    state.remote_folder = "Docs/".into();
    state.remote_edited = true;
    let mut harness = harness_ready(state, |_| {});

    click(&mut harness, "Create Sync");

    assert_eq!(created(&harness).unwrap().remote, "devremote:Docs");
}

#[test]
fn an_empty_drive_folder_syncs_the_entire_drive() {
    let mut state = filled();
    state.remote_edited = true;
    let mut harness = harness_ready(state, |_| {});

    click(&mut harness, "Create Sync");

    assert_eq!(created(&harness).unwrap().remote, "devremote:");
}

#[test]
fn a_drive_folder_inside_an_existing_sync_is_refused() {
    let mut state = filled();
    state.account = Some("gdrive".into());
    let harness = harness_ready(state, |_| {});

    assert!(shown(&harness, "remote folder overlaps \"Google Drive\""));
    assert!(is_disabled(&harness, "Create Sync"));
}

fn browsing(edit: impl FnOnce(&mut AddSyncState)) -> AddSyncState {
    let mut state = filled();
    state.browsing = true;
    edit(&mut state);
    state
}

#[test]
fn browse_opens_the_tree_of_drive_folders() {
    let mut harness = harness_with(filled(), |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Backup"]));
    });
    assert!(!shown(&harness, "Backup"));

    click(&mut harness, "Browse\u{2026}");

    assert!(shown(&harness, "Backup"));
}

#[test]
fn the_tree_lists_folders_sorted_by_name() {
    let harness = harness_with(browsing(|_| {}), |sheet| {
        sheet
            .dirs
            .insert("devremote:".into(), dir(&["Livros", "fotos", "Faculdade"]));
    });

    let top = |label: &str| harness.get_by_label(label).rect().min.y;

    assert!(top("Faculdade") < top("fotos"));
    assert!(top("fotos") < top("Livros"));
}

#[test]
fn expanding_an_unlisted_folder_asks_for_its_listing() {
    let mut harness = harness_with(browsing(|_| {}), |sheet| {
        sheet
            .dirs
            .insert("devremote:".into(), dir(&["Faculdade", "Fotos"]));
    });

    click(&mut harness, "Expand Faculdade");

    assert_eq!(
        load_requests(&harness),
        [AddSyncAction::LoadDirs("devremote:Faculdade".into())]
    );
}

#[test]
fn expanding_a_listed_folder_shows_its_children_without_asking() {
    let mut harness = harness_with(browsing(|_| {}), |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Pessoal"]));
        sheet
            .dirs
            .insert("devremote:Pessoal".into(), dir(&["Financeiro"]));
    });

    click(&mut harness, "Expand Pessoal");

    assert!(shown(&harness, "Financeiro"));
    assert!(load_requests(&harness).is_empty());
}

#[test]
fn picking_a_folder_in_the_tree_fills_the_drive_folder_and_stops_following() {
    let state = browsing(|state| {
        state.expanded = HashSet::from(["Pessoal".to_owned()]);
    });
    let mut harness = harness_with(state, |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Pessoal"]));
        sheet
            .dirs
            .insert("devremote:Pessoal".into(), dir(&["Financeiro"]));
    });

    click(&mut harness, "Financeiro");
    harness.state_mut().state.local = "/home/ada/Other".into();
    harness.run_steps(4);

    assert_eq!(harness.state().state.remote_folder, "Pessoal/Financeiro");
}

#[test]
fn a_failed_listing_shows_its_message_and_try_again_asks_once_more() {
    let mut harness = harness_with(browsing(|_| {}), |sheet| {
        sheet.dirs.insert(
            "devremote:".into(),
            DirListing::Failed("couldn't list directory".into()),
        );
    });
    assert!(shown(&harness, "couldn't list directory"));

    click(&mut harness, "Try Again");

    assert_eq!(
        load_requests(&harness),
        [AddSyncAction::LoadDirs("devremote:".into())]
    );
}

fn typed_new_folder(parent: &str, name: &str) -> Harness<'static, Sheet> {
    let state = browsing(|state| {
        parent.clone_into(&mut state.remote_folder);
        state.remote_edited = true;
        state.expanded = HashSet::from(["Docs".to_owned()]);
    });
    let mut harness = harness_with(state, |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Docs"]));
        sheet.dirs.insert("devremote:Docs".into(), dir(&[]));
    });
    click(&mut harness, "New Folder\u{2026}");
    type_into(&mut harness, "Folder name", name);
    harness
}

#[test]
fn new_folder_inside_the_chosen_folder_asks_for_a_mkdir_with_the_full_path() {
    let mut harness = typed_new_folder("Docs", "Reports");

    click(&mut harness, "Create");

    assert!(
        sheet_actions(&harness).contains(&AddSyncAction::MakeDir("devremote:Docs/Reports".into()))
    );
}

#[test]
fn new_folder_with_the_entire_drive_chosen_is_made_at_the_root() {
    let mut harness = typed_new_folder("", "Reports");

    click(&mut harness, "Create");

    assert!(sheet_actions(&harness).contains(&AddSyncAction::MakeDir("devremote:Reports".into())));
}

#[test]
fn new_folder_name_with_a_slash_cannot_be_created() {
    let harness = typed_new_folder("Docs", "a/b");

    assert!(is_disabled(&harness, "Create"));
}

#[test]
fn new_folder_error_from_the_app_is_shown_under_the_field() {
    let mut harness = typed_new_folder("Docs", "Reports");
    harness.state_mut().mkdir_error = Some("directory already exists".into());
    harness.run_steps(4);

    assert!(shown(&harness, "directory already exists"));
}

#[test]
fn pressing_enter_in_the_new_folder_field_makes_the_folder_and_not_the_sync() {
    let mut harness = typed_new_folder("Docs", "Reports");
    harness.state_mut().local = Some(facts(NOTES, LocalStatus::Exists));

    harness.key_press(Key::Enter);
    harness.run_steps(4);

    let actions = sheet_actions(&harness);
    assert!(actions.contains(&AddSyncAction::MakeDir("devremote:Docs/Reports".into())));
    assert_eq!(created(&harness), None);
}

fn local_harness(local: &str, facts: Option<LocalFacts>) -> Harness<'static, Sheet> {
    let mut state = filled();
    state.local = local.into();
    harness_with(state, |sheet| sheet.local = facts)
}

#[test]
fn a_relative_local_path_is_refused() {
    let harness = local_harness("Documents", None);

    assert!(shown(&harness, "local folder must be an absolute path"));
    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn a_local_path_holding_the_versions_folder_is_refused() {
    let harness = local_harness("/home/ada/.local/share", None);

    assert!(shown(
        &harness,
        "local folder cannot contain syncfast's versions folder"
    ));
    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn a_tilde_path_is_expanded_before_it_is_checked() {
    let harness = local_harness("~/.local", None);

    assert!(shown(
        &harness,
        "local folder cannot contain syncfast's versions folder"
    ));
}

#[test]
fn a_local_path_inside_an_existing_sync_is_refused() {
    let harness = local_harness("/home/ada/Drive/sub", None);

    assert!(shown(&harness, "local folder overlaps \"Google Drive\""));
    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn a_local_path_equal_to_an_existing_sync_is_refused_even_with_a_trailing_slash() {
    let harness = local_harness("/home/ada/Notes/", None);

    assert!(shown(&harness, "local folder overlaps \"Notes\""));
}

#[test]
fn a_local_path_holding_an_existing_sync_is_refused() {
    let harness = local_harness("/srv/data", None);

    assert!(shown(&harness, "local folder overlaps \"Photos\""));
}

#[test]
fn a_missing_local_folder_offers_to_create_it_with_the_expanded_path() {
    let text = "~/New";
    let mut harness = local_harness(text, Some(facts(text, LocalStatus::Missing)));
    assert!(shown(&harness, "This folder does not exist."));
    assert!(is_disabled(&harness, "Create Sync"));

    click(&mut harness, "Create Folder");

    assert!(sheet_actions(&harness).contains(&AddSyncAction::CreateLocal("/home/ada/New".into())));
}

#[test]
fn a_local_path_that_is_a_file_is_refused() {
    let text = "/home/ada/notes.txt";
    let harness = local_harness(text, Some(facts(text, LocalStatus::NotAFolder)));

    assert!(shown(&harness, "This path is not a folder."));
    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn create_sync_is_disabled_until_the_local_folder_is_ready() {
    let unchecked = local_harness(NOTES, None);
    let stale = local_harness(NOTES, Some(facts("/home/ada/Doc", LocalStatus::Exists)));
    let empty = local_harness("", None);
    let checked = local_harness(NOTES, Some(facts(NOTES, LocalStatus::Exists)));

    assert!(is_disabled(&unchecked, "Create Sync"));
    assert!(!shown(&unchecked, "This folder does not exist."));
    assert!(is_disabled(&stale, "Create Sync"));
    assert!(is_disabled(&empty, "Create Sync"));
    assert!(!shown(&empty, "local folder must be an absolute path"));
    assert!(!is_disabled(&checked, "Create Sync"));
}

#[test]
fn choose_button_asks_the_app_for_the_folder_dialog() {
    let mut harness = local_harness("", None);

    click(&mut harness, "Choose\u{2026}");

    assert!(sheet_actions(&harness).contains(&AddSyncAction::PickFolder));
}

#[test]
fn typing_a_local_path_asks_the_app_to_check_it() {
    let mut harness = local_harness("", None);

    type_into(&mut harness, "Folder on this computer", "/home/ada/Docs");

    assert_eq!(
        sheet_actions(&harness).last(),
        Some(&AddSyncAction::CheckLocal("/home/ada/Docs".into()))
    );
}

#[test]
fn opening_with_a_typed_path_checks_it_once() {
    let mut harness = local_harness("~/Docs", None);
    harness.run();

    let checks: Vec<_> = sheet_actions(&harness)
        .into_iter()
        .filter(|action| matches!(action, AddSyncAction::CheckLocal(_)))
        .collect();
    assert_eq!(checks, [AddSyncAction::CheckLocal("~/Docs".into())]);
}

#[test]
fn a_folder_dropped_on_the_window_becomes_the_local_folder_and_is_checked() {
    let mut harness = local_harness("", None);

    harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped("/home/ada/Dropped".into())));
    harness.run_steps(4);

    assert_eq!(harness.state().state.local, "/home/ada/Dropped");
    assert!(
        sheet_actions(&harness).contains(&AddSyncAction::CheckLocal("/home/ada/Dropped".into()))
    );
}

#[test]
fn a_failure_of_create_folder_is_shown_under_the_field() {
    let mut harness = local_harness("/home/ada/New", None);
    harness.state_mut().local_error = Some("permission denied".into());
    harness.run();

    assert!(shown(&harness, "permission denied"));
}

#[test]
fn a_new_sync_keeps_both_sides_the_same_by_default() {
    let mut harness = harness_ready(filled(), |_| {});

    click(&mut harness, "Create Sync");

    assert_eq!(created(&harness).unwrap().mode, SyncMode::Bisync);
}

#[test]
fn copy_to_drive_creates_an_upload_sync() {
    let mut harness = harness_ready(filled(), |_| {});

    click(&mut harness, "A copy on Google Drive");
    click(&mut harness, "Create Sync");

    assert_eq!(created(&harness).unwrap().mode, SyncMode::Upload);
}

#[test]
fn options_are_collapsed_by_default() {
    let mut harness = harness_with(filled(), |_| {});
    assert!(!shown(&harness, "Custom rules"));

    click(&mut harness, "Options");

    assert!(shown(&harness, "Custom rules"));
    assert!(shown(&harness, "Name"));
}

fn with_options(state: AddSyncState) -> Harness<'static, Sheet> {
    let mut harness = harness_ready(state, |_| {});
    click(&mut harness, "Options");
    harness
}

fn with_rules(rules: &str) -> AddSyncState {
    AddSyncState {
        rules: rules.into(),
        ..filled()
    }
}

#[test]
fn a_bad_rule_under_closed_options_is_flagged_outside_and_blocks_create() {
    let harness = harness_ready(with_rules("keep.txt"), |_| {});

    assert!(shown(
        &harness,
        "Fix the name or the filters under Options."
    ));
    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn each_bad_rule_gets_its_own_message_numbered_with_blank_lines() {
    let harness = with_options(with_rules("a\n\n- ok\nb"));

    assert!(shown(
        &harness,
        "filter line 1 must start with \"+ \" or \"- \""
    ));
    assert!(shown(
        &harness,
        "filter line 4 must start with \"+ \" or \"- \""
    ));
}

#[test]
fn valid_rules_and_blank_lines_allow_create_sync() {
    let harness = harness_ready(with_rules("\n- *.bak\n   \n+ keep/**\n"), |_| {});

    assert!(!is_disabled(&harness, "Create Sync"));
    assert!(!shown(
        &harness,
        "Fix the name or the filters under Options."
    ));
}

#[test]
fn a_new_sync_starts_with_the_default_presets_and_a_switch_turns_one_off() {
    let mut harness = with_options(filled());
    assert_eq!(
        harness.state().state.presets,
        ["system_junk", "dev_dirs", "git_repos"]
    );

    click(&mut harness, "Git repositories");

    assert_eq!(harness.state().state.presets, ["system_junk", "dev_dirs"]);
}

#[test]
fn typing_a_bad_rule_disables_create_sync_at_once() {
    let mut harness = with_options(filled());

    type_into_role(
        &mut harness,
        Role::MultilineTextInput,
        "Custom rules",
        "oops",
    );

    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn the_name_defaults_to_the_last_segment_of_the_local_folder() {
    let mut state = filled();
    state.local = "/home/ada/Documents/Projects/".into();
    let harness = harness_with(state, |_| {});

    assert_eq!(harness.state().state.name, "Projects");
}

#[test]
fn a_folder_without_a_last_segment_gives_the_name_sync_and_the_entire_drive() {
    let mut state = filled();
    state.local = "/".into();
    let harness = harness_with(state, |_| {});

    assert_eq!(harness.state().state.name, "Sync");
    assert_eq!(harness.state().state.remote_folder, "");
}

#[test]
fn the_name_follows_the_folder_until_the_user_types_one() {
    let mut state = filled();
    state.local = "/home/ada/Documents/Projects".into();
    let mut harness = with_options(state);

    type_into(&mut harness, "Name", " Mine");
    harness.state_mut().state.local = "/home/ada/Other".into();
    harness.run_steps(4);

    assert_eq!(harness.state().state.name, "Projects Mine");
}

#[test]
fn an_empty_name_is_refused_and_create_sync_is_disabled() {
    let mut harness = with_options(filled());
    harness.state_mut().state.name = String::new();
    harness.state_mut().state.name_edited = true;
    harness.run_steps(4);

    assert!(shown(&harness, "sync name must be one line of text"));
    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn create_sync_asks_the_app_to_create_the_sync() {
    let mut state = filled();
    state.account = Some("trabalho".into());
    state.remote_folder = "Pessoal/Notas".into();
    state.remote_edited = true;
    state.local = "~/Documents/Study Notes".into();
    state.presets.retain(|name| name != "dev_dirs");
    state.rules = "- *.bak\n\n+ keep/**\n".into();
    let mut harness = harness_ready(state, |_| {});

    click(&mut harness, "Create Sync");

    assert_eq!(
        sheet_actions(&harness).last(),
        Some(&AddSyncAction::Create(NewSync {
            id: "study-notes".into(),
            name: "Study Notes".into(),
            local: PathBuf::from("/home/ada/Documents/Study Notes"),
            remote: "trabalho:Pessoal/Notas".into(),
            presets: vec!["system_junk".into(), "git_repos".into()],
            filters: vec!["- *.bak".into(), "+ keep/**".into()],
            mode: SyncMode::Bisync,
        }))
    );
}

#[test]
fn the_sync_id_gets_a_suffix_when_it_is_taken() {
    let mut harness = harness_ready(filled(), |_| {});

    click(&mut harness, "Create Sync");

    assert_eq!(created(&harness).unwrap().id, "notes-2");
}

#[test]
fn create_sync_trims_the_typed_name() {
    let mut state = filled();
    state.name = "  Work  ".into();
    state.name_edited = true;
    let mut harness = harness_ready(state, |_| {});

    click(&mut harness, "Create Sync");

    let new = created(&harness).unwrap();
    assert_eq!((new.name.as_str(), new.id.as_str()), ("Work", "work"));
}

#[test]
fn create_error_from_the_app_is_shown() {
    let mut harness = harness_ready(filled(), |_| {});
    harness.state_mut().create_error = Some("could not write the config".into());
    harness.run();

    assert!(shown(&harness, "could not write the config"));
}

#[test]
fn create_sync_is_disabled_while_the_app_is_creating() {
    let harness = harness_ready(filled(), |sheet| sheet.creating = true);

    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn enter_creates_the_sync_when_nothing_has_focus() {
    let mut harness = harness_ready(filled(), |_| {});

    harness.key_press(Key::Enter);
    harness.run();

    assert!(created(&harness).is_some());
}

#[test]
fn enter_does_nothing_while_the_sheet_is_not_ready() {
    let mut harness = harness_with(filled(), |_| {});

    harness.key_press(Key::Enter);
    harness.run();

    assert_eq!(created(&harness), None);
}

#[test]
fn cancel_button_emits_cancel() {
    let mut harness = harness_with(filled(), |_| {});

    click(&mut harness, "Cancel");

    assert_eq!(sheet_actions(&harness).last(), Some(&AddSyncAction::Cancel));
}

#[test]
fn escape_emits_cancel() {
    let mut harness = harness_with(filled(), |_| {});

    harness.key_press(Key::Escape);
    harness.run();

    assert_eq!(sheet_actions(&harness).last(), Some(&AddSyncAction::Cancel));
}

#[test]
fn test_filters_runs_on_the_chosen_folder_with_the_rules_on_screen() {
    let mut harness = with_options(with_rules("- *.bak"));

    click(&mut harness, "Test Filters");

    assert!(harness.state().actions.contains(&Action::TestFilters {
        id: String::new(),
        local: NOTES.into(),
        presets: vec!["system_junk".into(), "dev_dirs".into(), "git_repos".into()],
        rules: vec!["- *.bak".into()],
    }));
}

fn filter_test(id: &str, state: FilterTestState) -> FilterTest {
    FilterTest {
        id: id.into(),
        rules: FilterRules {
            presets: vec!["system_junk".into(), "dev_dirs".into(), "git_repos".into()],
            custom: vec!["- *.bak".into()],
        },
        state,
    }
}

#[test]
fn a_filter_test_of_the_new_sync_shows_how_many_files_are_excluded() {
    let mut harness = harness_ready(with_rules("- *.bak"), |sheet| {
        sheet.filter_test = Some(filter_test(
            "",
            FilterTestState::Done(syncfast_core::filters::ExclusionSummary {
                total: 10,
                excluded: 4,
                folders: vec![("target".into(), 3)],
                git_repos: Vec::new(),
            }),
        ));
    });
    click(&mut harness, "Options");

    assert!(shown(&harness, "4 of 10 files excluded"));
    assert!(shown(&harness, "target \u{b7} 3 files"));
}

#[test]
fn a_filter_test_of_another_sync_is_not_shown() {
    let mut harness = harness_ready(with_rules("- *.bak"), |sheet| {
        sheet.filter_test = Some(filter_test("notes", FilterTestState::Failed("nope".into())));
    });
    click(&mut harness, "Options");

    assert!(!shown(&harness, "nope"));
}

#[test]
fn cancelling_connect_google_drive_brings_the_sheet_back_as_it_was() {
    let around = Surroundings {
        pairs: &[],
        welcome: false,
    };
    let mut sheets = Sheets::new("/home/ada".into(), "/home/ada/.local/share/syncfast".into());
    sheets.apply(&Action::AddSync, &around);
    sheets.handle_event(&Event::FolderPicked(Some("/home/ada/Picked".into())));
    sheets.apply(&Action::AddAccount, &around);
    sheets.apply(&Action::Account(AccountAction::Cancel), &around);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(960.0, 640.0))
        .build_ui_state(
            |ui, sheets: &mut Sheets| {
                if ready(ui) {
                    let _ = sheets.show(ui.ctx(), &[], None);
                }
            },
            sheets,
        );
    harness.run_ok();

    let field = harness.get_by_role_and_label(Role::TextInput, "Folder on this computer");
    assert_eq!(field.value().as_deref(), Some("/home/ada/Picked"));
}
