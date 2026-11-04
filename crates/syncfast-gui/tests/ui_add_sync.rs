#![allow(clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Key, Ui};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::state::DisplayStatus;
use syncfast_gui::theme::{self, Palette};
use syncfast_gui::ui::add_sync::{
    AddSyncAction, AddSyncState, AddSyncView, DirListing, LocalFacts, LocalStatus, NewSync, Step,
    show,
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

/// The state of a user who got to `step` with every earlier answer valid.
fn state_at(step: Step) -> AddSyncState {
    AddSyncState {
        step,
        account: Some("devremote".into()),
        remote_path: Some("Backup".into()),
        local: "/home/ada/Documents/notes".into(),
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
        .with_size(egui::vec2(800.0, 800.0))
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
    harness
}

fn harness_at(step: Step) -> Harness<'static, Sheet> {
    harness_with(state_at(step), |_| {})
}

fn is_disabled(harness: &Harness<'_, Sheet>, label: &str) -> bool {
    harness.get_by_label(label).accesskit_node().is_disabled()
}

fn click(harness: &mut Harness<'_, Sheet>, label: &str) {
    harness.get_by_label(label).click();
    harness.run_steps(4);
}

fn type_into(harness: &mut Harness<'_, Sheet>, label: &str, text: &str) {
    type_into_role(harness, Role::TextInput, label, text);
}

/// A focused field blinks its caret, so the frames are counted, not awaited.
fn type_into_role(harness: &mut Harness<'_, Sheet>, role: Role, label: &str, text: &str) {
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

#[test]
fn step_one_lists_every_account() {
    let harness = harness_at(Step::Account);

    for name in ["devremote", "gdrive", "trabalho"] {
        assert!(shown(&harness, name), "{name}");
    }
}

#[test]
fn shared_client_warning_is_shown_only_for_the_drive_without_its_own_client_id() {
    let harness = harness_at(Step::Account);

    let warnings = harness
        .query_all_by_label("Uses rclone's shared client ID")
        .count();

    assert_eq!(warnings, 1);
}

#[test]
fn step_one_without_accounts_says_so() {
    let harness = harness_with(AddSyncState::default(), |sheet| sheet.remotes.clear());

    assert!(shown(&harness, "No accounts yet."));
    assert!(shown(&harness, "Add Google Drive Account\u{2026}"));
}

#[test]
fn continue_is_disabled_until_an_account_is_picked() {
    let harness = harness_with(AddSyncState::default(), |_| {});

    assert!(is_disabled(&harness, "Continue"));
}

#[test]
fn picking_an_account_enables_continue() {
    let mut harness = harness_with(AddSyncState::default(), |_| {});

    click(&mut harness, "gdrive");

    assert_eq!(harness.state().state.account.as_deref(), Some("gdrive"));
    assert!(!is_disabled(&harness, "Continue"));
}

#[test]
fn picking_another_account_forgets_the_folder_choice() {
    let mut state = state_at(Step::Account);
    state.expanded = HashSet::from(["Backup".to_owned()]);
    state.new_folder = Some("draft".into());
    let mut harness = harness_with(state, |_| {});

    click(&mut harness, "gdrive");

    let state = &harness.state().state;
    assert_eq!(
        (&state.remote_path, state.expanded.len(), &state.new_folder),
        (&None, 0, &None)
    );
}

#[test]
fn picking_the_same_account_again_keeps_the_folder_choice() {
    let mut harness = harness_at(Step::Account);

    click(&mut harness, "devremote");

    assert_eq!(harness.state().state.remote_path.as_deref(), Some("Backup"));
}

#[test]
fn add_account_button_asks_the_app_to_add_an_account() {
    let mut harness = harness_at(Step::Account);

    click(&mut harness, "Add Google Drive Account\u{2026}");

    assert_eq!(harness.state().actions, [Action::AddAccount]);
}

#[test]
fn step_one_has_no_back_button_and_continue_goes_to_the_folder_step() {
    let mut harness = harness_at(Step::Account);
    assert!(!shown(&harness, "Back"));

    click(&mut harness, "Continue");

    assert_eq!(harness.state().state.step, Step::RemoteFolder);
}

#[test]
fn step_indicator_names_the_chosen_account_and_marks_the_steps() {
    let harness = harness_at(Step::RemoteFolder);

    assert!(shown(
        &harness,
        "Account \u{203a} Folder in devremote \u{203a} Folder on This Computer \u{203a} Filters \u{203a} Name"
    ));
}

#[test]
fn step_indicator_says_folder_in_account_before_one_is_chosen() {
    let harness = harness_with(AddSyncState::default(), |_| {});

    assert!(shown(
        &harness,
        "Account \u{203a} Folder in Account \u{203a} Folder on This Computer \u{203a} Filters \u{203a} Name"
    ));
}

#[test]
fn step_two_shows_the_whole_account_row_and_asks_for_the_root_listing_once() {
    let mut harness = harness_at(Step::RemoteFolder);
    harness.run();

    assert!(shown(&harness, "Entire devremote"));
    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::LoadDirs("devremote:".into())]
    );
}

#[test]
fn step_two_does_not_ask_for_a_root_listing_that_exists() {
    let harness = harness_with(state_at(Step::RemoteFolder), |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Backup"]));
    });

    assert!(sheet_actions(&harness).is_empty());
}

#[test]
fn step_two_lists_folders_sorted_by_name() {
    let harness = harness_with(state_at(Step::RemoteFolder), |sheet| {
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
    let mut harness = harness_with(state_at(Step::RemoteFolder), |sheet| {
        sheet
            .dirs
            .insert("devremote:".into(), dir(&["Faculdade", "Fotos"]));
    });

    harness.get_by_label("Expand Faculdade").click();
    harness.run_steps(3);

    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::LoadDirs("devremote:Faculdade".into())]
    );
}

#[test]
fn expanding_a_listed_folder_shows_its_children_without_asking() {
    let mut harness = harness_with(state_at(Step::RemoteFolder), |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Pessoal"]));
        sheet
            .dirs
            .insert("devremote:Pessoal".into(), dir(&["Financeiro"]));
    });

    harness.get_by_label("Expand Pessoal").click();
    harness.run();

    assert!(shown(&harness, "Financeiro"));
    assert!(sheet_actions(&harness).is_empty());
}

#[test]
fn selecting_a_nested_folder_stores_its_relative_path() {
    let mut state = state_at(Step::RemoteFolder);
    state.remote_path = None;
    state.expanded = HashSet::from(["Pessoal".to_owned()]);
    let mut harness = harness_with(state, |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Pessoal"]));
        sheet
            .dirs
            .insert("devremote:Pessoal".into(), dir(&["Financeiro"]));
    });

    click(&mut harness, "Financeiro");

    assert_eq!(
        harness.state().state.remote_path.as_deref(),
        Some("Pessoal/Financeiro")
    );
}

#[test]
fn step_two_without_a_choice_has_continue_disabled_and_no_message() {
    let mut state = state_at(Step::RemoteFolder);
    state.remote_path = None;
    let harness = harness_with(state, |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Backup"]));
    });

    assert!(is_disabled(&harness, "Continue"));
    assert!(!shown(&harness, "remote folder overlaps \"Notes\""));
}

#[test]
fn choosing_the_whole_account_enables_continue() {
    let mut state = state_at(Step::RemoteFolder);
    state.remote_path = None;
    let mut harness = harness_with(state, |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Backup"]));
    });

    click(&mut harness, "Entire devremote");

    assert_eq!(harness.state().state.remote_path.as_deref(), Some(""));
    assert!(!is_disabled(&harness, "Continue"));
}

#[test]
fn a_remote_folder_inside_an_existing_sync_is_refused() {
    let mut state = state_at(Step::RemoteFolder);
    state.account = Some("gdrive".into());
    state.remote_path = Some("Pessoal".into());
    let harness = harness_with(state, |sheet| {
        sheet.dirs.insert("gdrive:".into(), dir(&["Pessoal"]));
    });

    assert!(shown(&harness, "remote folder overlaps \"Google Drive\""));
    assert!(is_disabled(&harness, "Continue"));
}

#[test]
fn a_remote_folder_that_does_not_overlap_is_accepted() {
    let harness = harness_with(state_at(Step::RemoteFolder), |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Backup"]));
    });

    assert!(!is_disabled(&harness, "Continue"));
}

#[test]
fn a_failed_listing_shows_its_message() {
    let harness = harness_with(state_at(Step::RemoteFolder), |sheet| {
        sheet.dirs.insert(
            "devremote:".into(),
            DirListing::Failed("couldn't list directory".into()),
        );
    });

    assert!(shown(&harness, "couldn't list directory"));
}

#[test]
fn try_again_after_a_failed_listing_asks_for_that_folder_again() {
    let mut harness = harness_with(state_at(Step::RemoteFolder), |sheet| {
        sheet.dirs.insert(
            "devremote:".into(),
            DirListing::Failed("couldn't list directory".into()),
        );
    });

    click(&mut harness, "Try Again");

    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::LoadDirs("devremote:".into())]
    );
}

fn typed_new_folder(parent: Option<&str>, name: &str) -> Harness<'static, Sheet> {
    let mut state = state_at(Step::RemoteFolder);
    state.remote_path = parent.map(ToOwned::to_owned);
    state.expanded = parent.map(ToOwned::to_owned).into_iter().collect();
    let mut harness = harness_with(state, |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Docs"]));
        sheet.dirs.insert("devremote:Docs".into(), dir(&[]));
    });
    click(&mut harness, "New Folder\u{2026}");
    type_into(&mut harness, "Folder name", name);
    harness
}

#[test]
fn new_folder_inside_the_selected_folder_asks_for_a_mkdir_with_the_full_path() {
    let mut harness = typed_new_folder(Some("Docs"), "Reports");

    click(&mut harness, "Create");

    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::MakeDir("devremote:Docs/Reports".into())]
    );
}

#[test]
fn new_folder_at_the_root_asks_for_a_mkdir_without_a_parent() {
    let mut harness = typed_new_folder(Some(""), "Reports");

    click(&mut harness, "Create");

    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::MakeDir("devremote:Reports".into())]
    );
}

#[test]
fn new_folder_without_a_selection_is_made_at_the_root() {
    let mut harness = typed_new_folder(None, "Reports");

    click(&mut harness, "Create");

    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::MakeDir("devremote:Reports".into())]
    );
}

#[test]
fn new_folder_name_with_a_slash_cannot_be_created() {
    let harness = typed_new_folder(Some("Docs"), "a/b");

    assert!(is_disabled(&harness, "Create"));
}

#[test]
fn new_folder_with_an_empty_name_cannot_be_created() {
    let mut harness = harness_with(state_at(Step::RemoteFolder), |sheet| {
        sheet.dirs.insert("devremote:".into(), dir(&["Docs"]));
    });

    click(&mut harness, "New Folder\u{2026}");

    assert!(is_disabled(&harness, "Create"));
}

#[test]
fn new_folder_error_from_the_app_is_shown_under_the_field() {
    let mut harness = typed_new_folder(Some("Docs"), "Reports");
    harness.state_mut().mkdir_error = Some("directory already exists".into());
    harness.run_steps(4);

    assert!(shown(&harness, "directory already exists"));
}

#[test]
fn pressing_enter_in_the_new_folder_field_creates_it_and_does_not_continue() {
    let mut harness = typed_new_folder(Some("Docs"), "Reports");

    harness.key_press(Key::Enter);
    harness.run_steps(4);

    assert_eq!(
        (sheet_actions(&harness), harness.state().state.step),
        (
            vec![AddSyncAction::MakeDir("devremote:Docs/Reports".into())],
            Step::RemoteFolder
        )
    );
}

fn local_harness(local: &str, facts: Option<LocalFacts>) -> Harness<'static, Sheet> {
    let mut state = state_at(Step::LocalFolder);
    state.local = local.into();
    harness_with(state, |sheet| sheet.local = facts)
}

#[test]
fn a_relative_local_path_is_refused() {
    let harness = local_harness("Documents", None);

    assert!(shown(&harness, "local folder must be an absolute path"));
    assert!(is_disabled(&harness, "Continue"));
}

#[test]
fn a_local_path_holding_the_versions_folder_is_refused() {
    let harness = local_harness("/home/ada/.local/share", None);

    assert!(shown(
        &harness,
        "local folder cannot contain syncfast's versions folder"
    ));
    assert!(is_disabled(&harness, "Continue"));
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
    assert!(is_disabled(&harness, "Continue"));
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
    assert!(is_disabled(&harness, "Continue"));
}

#[test]
fn a_missing_local_folder_offers_to_create_it() {
    let text = "/home/ada/New";
    let mut harness = local_harness(text, Some(facts(text, LocalStatus::Missing)));

    assert!(shown(&harness, "This folder does not exist."));
    assert!(is_disabled(&harness, "Continue"));
    click(&mut harness, "Create Folder");

    assert!(sheet_actions(&harness).contains(&AddSyncAction::CreateLocal("/home/ada/New".into())));
}

#[test]
fn create_folder_uses_the_expanded_path() {
    let text = "~/New";
    let mut harness = local_harness(text, Some(facts(text, LocalStatus::Missing)));

    click(&mut harness, "Create Folder");

    assert!(sheet_actions(&harness).contains(&AddSyncAction::CreateLocal("/home/ada/New".into())));
}

#[test]
fn a_local_path_that_is_a_file_is_refused() {
    let text = "/home/ada/notes.txt";
    let harness = local_harness(text, Some(facts(text, LocalStatus::NotAFolder)));

    assert!(shown(&harness, "This path is not a folder."));
    assert!(is_disabled(&harness, "Continue"));
}

#[test]
fn an_existing_local_folder_enables_continue() {
    let text = "/home/ada/Documents";
    let harness = local_harness(text, Some(facts(text, LocalStatus::Exists)));

    assert!(!is_disabled(&harness, "Continue"));
}

#[test]
fn local_folder_without_facts_disables_continue_and_shows_no_error() {
    let harness = local_harness("/home/ada/Documents", None);

    assert!(is_disabled(&harness, "Continue"));
    assert!(!shown(&harness, "This folder does not exist."));
}

#[test]
fn facts_for_an_older_text_do_not_count() {
    let harness = local_harness(
        "/home/ada/Documents",
        Some(facts("/home/ada/Doc", LocalStatus::Exists)),
    );

    assert!(is_disabled(&harness, "Continue"));
}

#[test]
fn an_empty_local_folder_disables_continue_and_shows_no_error() {
    let harness = local_harness("", None);

    assert!(is_disabled(&harness, "Continue"));
    assert!(!shown(&harness, "local folder must be an absolute path"));
}

#[test]
fn choose_button_asks_the_app_for_the_folder_dialog() {
    let mut harness = local_harness("", None);

    click(&mut harness, "Choose\u{2026}");

    assert_eq!(sheet_actions(&harness), [AddSyncAction::PickFolder]);
}

#[test]
fn typing_a_local_path_asks_the_app_to_check_it() {
    let mut harness = local_harness("", None);

    type_into(&mut harness, "Folder", "/home/ada/Docs");

    assert_eq!(
        sheet_actions(&harness).last(),
        Some(&AddSyncAction::CheckLocal("/home/ada/Docs".into()))
    );
}

#[test]
fn entering_the_step_with_a_typed_path_checks_it_once() {
    let mut harness = local_harness("~/Docs", None);
    harness.run();

    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::CheckLocal("~/Docs".into())]
    );
}

#[test]
fn a_path_picked_in_the_dialog_is_checked() {
    let mut harness = local_harness("", None);
    harness
        .state_mut()
        .state
        .picked_local(std::path::Path::new("/home/ada/Picked"));
    harness.run();

    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::CheckLocal("/home/ada/Picked".into())]
    );
}

#[test]
fn coming_back_to_the_local_step_checks_the_path_again() {
    let mut harness = local_harness("/home/ada/Docs", None);
    click(&mut harness, "Back");
    harness.state_mut().actions.clear();
    click(&mut harness, "Continue");

    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::CheckLocal("/home/ada/Docs".into())]
    );
}

#[test]
fn a_failure_of_create_folder_is_shown_under_the_field() {
    let mut harness = local_harness("/home/ada/New", None);
    harness.state_mut().local_error = Some("permission denied".into());
    harness.run();

    assert!(shown(&harness, "permission denied"));
}

fn filters_harness(rules: &str) -> Harness<'static, Sheet> {
    let mut state = state_at(Step::Filters);
    state.rules = rules.into();
    harness_with(state, |_| {})
}

#[test]
fn a_rule_without_a_sign_is_refused_with_its_line_number() {
    let harness = filters_harness("keep.txt");

    assert!(shown(
        &harness,
        "filter line 1 must start with \"+ \" or \"- \""
    ));
    assert!(is_disabled(&harness, "Continue"));
}

#[test]
fn rule_problems_count_blank_lines_in_their_numbers() {
    let harness = filters_harness("- *.bak\n\nnope");

    assert!(shown(
        &harness,
        "filter line 3 must start with \"+ \" or \"- \""
    ));
}

#[test]
fn every_bad_rule_gets_its_own_message() {
    let harness = filters_harness("a\n- ok\nb");

    assert!(shown(
        &harness,
        "filter line 1 must start with \"+ \" or \"- \""
    ));
    assert!(shown(
        &harness,
        "filter line 3 must start with \"+ \" or \"- \""
    ));
}

#[test]
fn valid_rules_and_blank_lines_enable_continue() {
    let harness = filters_harness("\n- *.bak\n   \n+ keep/**\n");

    assert!(!is_disabled(&harness, "Continue"));
}

#[test]
fn no_custom_rules_enable_continue() {
    let harness = filters_harness("");

    assert!(!is_disabled(&harness, "Continue"));
}

#[test]
fn clicking_a_preset_switch_turns_it_off() {
    let mut harness = harness_at(Step::Filters);

    click(&mut harness, "System junk files");

    let state = &harness.state().state;
    assert_eq!(state.presets, ["dev_dirs", "git_repos"]);
}

#[test]
fn a_new_sync_starts_with_every_preset_and_the_git_switch_turns_one_off() {
    let mut harness = harness_at(Step::Filters);
    assert_eq!(
        harness.state().state.presets,
        ["system_junk", "dev_dirs", "git_repos"]
    );

    click(&mut harness, "Git repositories");

    assert_eq!(harness.state().state.presets, ["system_junk", "dev_dirs"]);
}

#[test]
fn show_rules_lists_the_rules_of_the_preset() {
    let mut harness = harness_at(Step::Filters);
    assert!(!shown(&harness, "- .DS_Store"));

    harness
        .get_all_by_label("Show rules")
        .next()
        .unwrap()
        .click();
    harness.run();

    assert!(shown(&harness, "- .DS_Store"));
}

#[test]
fn typing_a_bad_rule_disables_continue_at_once() {
    let mut harness = filters_harness("");

    type_into_role(
        &mut harness,
        Role::MultilineTextInput,
        "Custom rules",
        "oops",
    );

    assert!(is_disabled(&harness, "Continue"));
}

fn name_harness(local: &str) -> Harness<'static, Sheet> {
    let mut state = state_at(Step::Name);
    state.local = local.into();
    harness_with(state, |_| {})
}

#[test]
fn the_name_defaults_to_the_last_segment_of_the_local_folder() {
    let harness = name_harness("/home/ada/Documents/Projects/");

    assert_eq!(harness.state().state.name, "Projects");
}

#[test]
fn the_name_defaults_to_sync_when_the_folder_has_no_last_segment() {
    let harness = name_harness("/");

    assert_eq!(harness.state().state.name, "Sync");
}

#[test]
fn the_name_follows_the_folder_until_the_user_types_one() {
    let mut harness = name_harness("/home/ada/Documents/Projects");
    type_into(&mut harness, "Name", " Mine");
    harness.state_mut().state.local = "/home/ada/Other".into();
    harness.run();

    assert_eq!(harness.state().state.name, "Projects Mine");
}

#[test]
fn the_sync_id_gets_a_suffix_when_it_is_taken() {
    let harness = name_harness("/home/ada/Documents/notes");

    assert!(shown(&harness, "Sync id: notes-2"));
}

#[test]
fn the_sync_id_is_the_slug_of_the_name() {
    let harness = name_harness("/home/ada/Documents/My Photos");

    assert!(shown(&harness, "Sync id: my-photos"));
}

#[test]
fn an_empty_name_is_refused_and_create_sync_is_disabled() {
    let mut harness = name_harness("/home/ada/Documents/notes");
    harness.state_mut().state.name = String::new();
    harness.state_mut().state.name_edited = true;
    harness.run();

    assert!(shown(&harness, "sync name must be one line of text"));
    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn create_sync_asks_the_app_to_create_the_sync() {
    let mut state = state_at(Step::Name);
    state.account = Some("gdrive".into());
    state.remote_path = Some("Pessoal/Notas".into());
    state.local = "~/Documents/Study Notes".into();
    state.presets.retain(|name| name != "dev_dirs");
    state.rules = "- *.bak\n\n+ keep/**\n".into();
    let mut harness = harness_with(state, |_| {});

    click(&mut harness, "Create Sync");

    assert_eq!(
        sheet_actions(&harness),
        [AddSyncAction::Create(NewSync {
            id: "study-notes".into(),
            name: "Study Notes".into(),
            local: PathBuf::from("/home/ada/Documents/Study Notes"),
            remote: "gdrive:Pessoal/Notas".into(),
            presets: vec!["system_junk".into(), "git_repos".into()],
            filters: vec!["- *.bak".into(), "+ keep/**".into()],
        })]
    );
}

#[test]
fn create_sync_trims_the_typed_name() {
    let mut harness = name_harness("/home/ada/Documents/Projects");
    harness.state_mut().state.name = "  Work  ".into();
    harness.state_mut().state.name_edited = true;

    click(&mut harness, "Create Sync");

    let created = sheet_actions(&harness)
        .into_iter()
        .find_map(|action| match action {
            AddSyncAction::Create(new) => Some((new.name, new.id)),
            _ => None,
        });
    assert_eq!(created, Some(("Work".to_owned(), "work".to_owned())));
}

#[test]
fn create_error_from_the_app_is_shown_on_the_name_step() {
    let mut harness = name_harness("/home/ada/Documents/Projects");
    harness.state_mut().create_error = Some("could not write the config".into());
    harness.run();

    assert!(shown(&harness, "could not write the config"));
}

#[test]
fn create_sync_is_disabled_while_the_app_is_creating() {
    let mut harness = name_harness("/home/ada/Documents/Projects");
    harness.state_mut().creating = true;
    harness.run();

    assert!(is_disabled(&harness, "Create Sync"));
}

#[test]
fn enter_creates_the_sync_when_nothing_has_focus() {
    let mut harness = name_harness("/home/ada/Documents/Projects");

    harness.key_press(Key::Enter);
    harness.run();

    assert!(matches!(
        sheet_actions(&harness).as_slice(),
        [AddSyncAction::Create(_)]
    ));
}

#[test]
fn only_the_last_step_offers_create_sync() {
    let harness = harness_at(Step::Filters);

    assert!(shown(&harness, "Continue"));
    assert!(!shown(&harness, "Create Sync"));
}

#[test]
fn back_returns_to_the_previous_step_and_keeps_what_was_typed() {
    let mut state = state_at(Step::Filters);
    state.rules = "- *.bak".into();
    state.local = "/home/ada/Docs".into();
    let mut harness = harness_with(state, |_| {});

    click(&mut harness, "Back");

    let state = &harness.state().state;
    assert_eq!(
        (state.step, state.local.as_str(), state.rules.as_str()),
        (Step::LocalFolder, "/home/ada/Docs", "- *.bak")
    );
}

#[test]
fn continue_does_nothing_while_the_step_is_invalid() {
    let mut state = state_at(Step::Filters);
    state.rules = "nope".into();
    let mut harness = harness_with(state, |_| {});

    harness.key_press(Key::Enter);
    harness.run();

    assert_eq!(harness.state().state.step, Step::Filters);
}

#[test]
fn cancel_button_emits_cancel() {
    let mut harness = harness_at(Step::Filters);

    click(&mut harness, "Cancel");

    assert_eq!(sheet_actions(&harness), [AddSyncAction::Cancel]);
}

#[test]
fn escape_emits_cancel() {
    let mut harness = harness_at(Step::Account);

    harness.key_press(Key::Escape);
    harness.run();

    assert_eq!(sheet_actions(&harness), [AddSyncAction::Cancel]);
}

#[test]
fn test_filters_in_the_wizard_runs_on_the_chosen_folder_with_the_rules_on_screen() {
    let mut harness = filters_harness("- *.bak");

    click(&mut harness, "Test Filters");

    assert!(harness.state().actions.contains(&Action::TestFilters {
        id: String::new(),
        local: "/home/ada/Documents/notes".into(),
        presets: vec!["system_junk".into(), "dev_dirs".into(), "git_repos".into()],
        rules: vec!["- *.bak".into()],
    }));
}

#[test]
fn a_filter_test_of_the_wizard_shows_how_many_files_are_excluded() {
    let mut state = state_at(Step::Filters);
    state.rules = "- *.bak".into();
    let harness = harness_with(state, |sheet| {
        sheet.filter_test = Some(FilterTest {
            id: String::new(),
            rules: FilterRules {
                presets: vec!["system_junk".into(), "dev_dirs".into(), "git_repos".into()],
                custom: vec!["- *.bak".into()],
            },
            state: FilterTestState::Done(syncfast_core::filters::ExclusionSummary {
                total: 10,
                excluded: 4,
                folders: vec![("target".into(), 3)],
                git_repos: Vec::new(),
            }),
        });
    });

    assert!(shown(&harness, "4 of 10 files excluded"));
    assert!(shown(&harness, "target \u{b7} 3 files"));
}

#[test]
fn a_filter_test_of_a_sync_is_not_shown_in_the_wizard() {
    let mut state = state_at(Step::Filters);
    state.rules = "- *.bak".into();
    let harness = harness_with(state, |sheet| {
        sheet.filter_test = Some(FilterTest {
            id: "notes".into(),
            rules: FilterRules {
                presets: vec!["system_junk".into(), "dev_dirs".into(), "git_repos".into()],
                custom: vec!["- *.bak".into()],
            },
            state: FilterTestState::Failed("nope".into()),
        });
    });

    assert!(!shown(&harness, "nope"));
}
