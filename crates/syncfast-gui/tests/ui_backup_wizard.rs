#![allow(clippy::unwrap_used)]

mod common;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use common::{Frame, harness, view_model};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::KeyBackend;
use syncfast_gui::backend::Command;
use syncfast_gui::backend::backup_sheets::{AddFailure, BackupCommand, BackupReply};
use syncfast_gui::backup_sheets::{BackupSheet, Shared};
use syncfast_gui::ui::Action;
use syncfast_gui::ui::backup_wizard::{
    BackupFacts, BackupSheetAction, DestKind, Flow, Step, WizardState,
};

struct Rig {
    harness: Harness<'static, Frame>,
    sheet: Rc<RefCell<BackupSheet>>,
    commands: Rc<RefCell<Vec<Command>>>,
}

impl Rig {
    fn new(sheet: BackupSheet) -> Self {
        let sheet = Rc::new(RefCell::new(sheet));
        let commands = Rc::new(RefCell::new(Vec::new()));
        let backups = vec![BackupFacts {
            id: "docs".to_owned(),
            name: "Docs".to_owned(),
            dest: "gdrive:Backups/docs".to_owned(),
            key_unsaved: false,
        }];
        let (shown, sent) = (Rc::clone(&sheet), Rc::clone(&commands));
        let harness = harness(view_model(Vec::new(), None), move |ui, _| {
            let shared = Shared {
                remotes: &[],
                home: Path::new("/home/user"),
                backups: &backups,
            };
            let actions = shown.borrow_mut().show(ui.ctx(), &shared);
            for action in &actions {
                if let Action::BackupSheet(action) = action {
                    let outcome = shown.borrow_mut().apply(action, &backups);
                    sent.borrow_mut().extend(outcome.commands);
                }
            }
            actions
        });
        Self {
            harness,
            sheet,
            commands,
        }
    }

    fn commands(&self) -> Vec<Command> {
        self.commands.borrow().clone()
    }
}

const SETTLE_CEILING: usize = 30;

fn layout(harness: &Harness<'_, Frame>) -> Vec<Option<egui::accesskit::Rect>> {
    harness
        .root()
        .children_recursive()
        .map(|node| node.accesskit_node().bounding_box())
        .collect()
}

/// The sheet is an `Area` that recentres over a few frames when its content
/// changes size, and `run` stops at the first frame without a repaint.
fn settle(harness: &mut Harness<'_, Frame>) {
    let mut before = layout(harness);
    let mut still = 0;
    let settled = (1..=SETTLE_CEILING).any(|_| {
        harness.step();
        let after = layout(harness);
        still = if after == before { still + 1 } else { 0 };
        before = after;
        still >= 2
    });
    assert!(
        settled,
        "the layout still moves after {SETTLE_CEILING} frames"
    );
}

fn facts(unsaved: bool) -> BackupFacts {
    BackupFacts {
        id: "docs".to_owned(),
        name: "Docs".to_owned(),
        dest: "gdrive:Backups/docs".to_owned(),
        key_unsaved: unsaved,
    }
}

fn disabled(rig: &Rig, label: &str) -> bool {
    rig.harness
        .get_by_label(label)
        .accesskit_node()
        .is_disabled()
}

#[test]
fn continue_waits_for_the_key_storage_answer() {
    let (sheet, commands) = BackupSheet::wizard(Flow::New);
    let mut rig = Rig::new(sheet);
    settle(&mut rig.harness);

    assert!(disabled(&rig, "Continue"));
    assert!(commands.contains(&Command::CheckKeyStorage { test: false }));
}

#[test]
fn no_key_storage_blocks_the_first_step_and_says_why() {
    let (sheet, _) = BackupSheet::wizard(Flow::New);
    let mut rig = Rig::new(sheet);
    rig.sheet
        .borrow_mut()
        .on_key_storage(&Err("No keyring answered.".to_owned()));

    settle(&mut rig.harness);

    rig.harness.get_by_label("No keyring answered.");
    assert!(disabled(&rig, "Continue"));
}

#[test]
fn the_host_keyring_comes_with_a_note() {
    let (sheet, _) = BackupSheet::wizard(Flow::New);
    let mut rig = Rig::new(sheet);
    rig.sheet
        .borrow_mut()
        .on_key_storage(&Ok(KeyBackend::HostKeyring));

    settle(&mut rig.harness);

    rig.harness
        .get_by_label("Using the host keyring. See docs/guide/backup.md.");
}

#[test]
fn the_first_step_needs_a_folder() {
    let (sheet, _) = BackupSheet::wizard(Flow::New);
    let mut rig = Rig::new(sheet);
    rig.sheet
        .borrow_mut()
        .on_key_storage(&Ok(KeyBackend::Portal));

    settle(&mut rig.harness);

    assert!(disabled(&rig, "Continue"));
    rig.harness.get_by_label("Cancel");
}

#[test]
fn cancel_closes_the_wizard() {
    let (sheet, _) = BackupSheet::wizard(Flow::New);
    let mut rig = Rig::new(sheet);
    settle(&mut rig.harness);

    rig.harness.get_by_label("Cancel").click();
    settle(&mut rig.harness);

    assert_eq!(
        rig.harness.state().actions,
        [Action::BackupSheet(BackupSheetAction::Cancel)]
    );
}

#[test]
fn a_key_that_was_never_saved_opens_the_key_sheet_at_once() {
    let (sheet, commands) = BackupSheet::show_key(&facts(true));
    let mut rig = Rig::new(sheet);
    settle(&mut rig.harness);

    assert_eq!(
        commands,
        [Command::Backup(BackupCommand::ShowKey("docs".to_owned()))]
    );
    rig.harness.get_by_label("I Saved the Key");
}

#[test]
fn i_saved_the_key_marks_it_saved() {
    let (sheet, _) = BackupSheet::show_key(&facts(true));
    let mut rig = Rig::new(sheet);
    rig.sheet.borrow_mut().on_reply(&BackupReply::KeyShown {
        id: "docs".to_owned(),
        result: Ok(Key::generate().unwrap()),
    });
    settle(&mut rig.harness);

    rig.harness.get_by_label("I Saved the Key").click();
    settle(&mut rig.harness);

    assert_eq!(
        rig.commands(),
        [Command::Backup(BackupCommand::MarkKeySaved(
            "docs".to_owned()
        ))]
    );
}

#[test]
fn a_saved_key_is_shown_only_after_a_confirmation() {
    let (sheet, commands) = BackupSheet::show_key(&facts(false));
    let mut rig = Rig::new(sheet);
    settle(&mut rig.harness);

    assert!(commands.is_empty());
    rig.harness.get_by_label("Show the backup key?");

    rig.harness.get_by_label("Show Key").click();
    settle(&mut rig.harness);

    assert_eq!(
        rig.commands(),
        [Command::Backup(BackupCommand::ShowKey("docs".to_owned()))]
    );
}

#[test]
fn cancelling_the_confirmation_asks_nothing_of_the_backend() {
    let (sheet, _) = BackupSheet::show_key(&facts(false));
    let mut rig = Rig::new(sheet);
    settle(&mut rig.harness);

    rig.harness.get_by_label("Cancel").click();
    settle(&mut rig.harness);

    assert!(rig.commands().is_empty());
    assert_eq!(
        rig.harness.state().actions,
        [Action::BackupSheet(BackupSheetAction::Cancel)]
    );
}

#[test]
fn removing_a_backup_names_it_and_keeps_the_key_by_default() {
    let mut rig = Rig::new(BackupSheet::remove(&facts(false)));
    settle(&mut rig.harness);

    rig.harness.get_by_label("Remove \"Docs\"?");
    rig.harness
        .get_by_label("Also delete the key from this computer");

    rig.harness.get_by_label("Remove").click();
    settle(&mut rig.harness);

    assert_eq!(
        rig.commands(),
        [Command::Backup(BackupCommand::Remove {
            id: "docs".to_owned(),
            delete_key: false
        })]
    );
}

#[test]
fn removing_with_the_box_checked_deletes_the_key_too() {
    let mut rig = Rig::new(BackupSheet::remove(&facts(false)));
    settle(&mut rig.harness);
    rig.harness
        .get_by_label("Also delete the key from this computer")
        .click();
    settle(&mut rig.harness);

    rig.harness.get_by_label("Remove").click();
    settle(&mut rig.harness);

    assert_eq!(
        rig.commands(),
        [Command::Backup(BackupCommand::Remove {
            id: "docs".to_owned(),
            delete_key: true
        })]
    );
}

#[test]
fn adding_a_backup_with_the_wrong_key_says_so_on_the_key_step() {
    let (sheet, _) = BackupSheet::wizard(Flow::Existing);
    let mut rig = Rig::new(sheet);
    rig.sheet
        .borrow_mut()
        .on_reply(&BackupReply::Added(Err(AddFailure::WrongKey(
            "That key does not open this backup.".to_owned(),
        ))));

    settle(&mut rig.harness);

    rig.harness
        .get_by_label("That key does not open this backup.");
    rig.harness.get_by_label("Backup key");
}

#[test]
fn adding_a_folder_without_a_backup_sends_the_user_back_to_the_destination() {
    let (sheet, _) = BackupSheet::wizard(Flow::Existing);
    let mut rig = Rig::new(sheet);
    rig.sheet
        .borrow_mut()
        .on_reply(&BackupReply::Added(Err(AddFailure::NoRepository(
            "No backup was found in that folder.".to_owned(),
        ))));

    settle(&mut rig.harness);

    rig.harness
        .get_by_label("No backup was found in that folder.");
}

fn key_step() -> (Rig, Key) {
    let key = Key::generate().unwrap();
    let (sheet, _) = BackupSheet::wizard(Flow::New);
    let mut rig = Rig::new(sheet);
    {
        let mut sheet = rig.sheet.borrow_mut();
        let state = sheet.wizard_state().unwrap();
        state.folders = vec!["/home/user/Documents".into()];
        state.kind = DestKind::Computer;
        "/mnt/backup".clone_into(&mut state.local);
        "Family Photos".clone_into(&mut state.name);
        state.key = Some(key.clone());
        state.step = Step::Key;
    }
    settle(&mut rig.harness);
    (rig, key)
}

#[test]
fn the_key_step_offers_only_cancel_and_i_saved_the_key() {
    let (rig, _) = key_step();

    rig.harness.get_by_label("Your Backup Key");
    rig.harness.get_by_label("Cancel");
    rig.harness.get_by_label("I Saved the Key");
    assert!(rig.harness.query_by_label("Back").is_none());
    assert!(rig.harness.query_by_label("Done").is_none());
}

#[test]
fn cancelling_on_the_key_step_creates_nothing() {
    let (mut rig, _) = key_step();

    rig.harness.get_by_label("Cancel").click();
    settle(&mut rig.harness);

    assert_eq!(
        rig.harness.state().actions,
        [Action::BackupSheet(BackupSheetAction::Cancel)]
    );
    assert!(rig.commands().is_empty());
}

#[test]
fn i_saved_the_key_creates_the_backup_named_by_its_slug() {
    let (mut rig, key) = key_step();

    rig.harness.get_by_label("I Saved the Key").click();
    settle(&mut rig.harness);

    let created: Vec<_> = rig
        .harness
        .state()
        .actions
        .iter()
        .filter_map(|action| match action {
            Action::BackupSheet(BackupSheetAction::Create(draft, sent)) => {
                Some((draft.clone(), sent.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(created.len(), 1);
    let (draft, sent) = &created[0];
    assert_eq!(draft.id, "family-photos");
    assert_eq!(draft.name, "Family Photos");
    assert_eq!(draft.dest, "/mnt/backup");
    assert_eq!(
        draft.paths,
        [std::path::PathBuf::from("/home/user/Documents")]
    );
    assert_eq!(*sent, key);
    assert!(matches!(
        rig.commands().as_slice(),
        [Command::Backup(BackupCommand::Create { .. })]
    ));
}

#[test]
fn the_shown_key_sheet_offers_done_and_saving_to_a_file() {
    let (sheet, _) = BackupSheet::show_key(&facts(false));
    let mut rig = Rig::new(sheet);
    settle(&mut rig.harness);
    rig.harness.get_by_label("Show Key").click();
    settle(&mut rig.harness);
    rig.sheet.borrow_mut().on_reply(&BackupReply::KeyShown {
        id: "docs".to_owned(),
        result: Ok(Key::generate().unwrap()),
    });

    settle(&mut rig.harness);

    rig.harness.get_by_label("Your Backup Key");
    rig.harness.get_by_label("Done");
    rig.harness.get_by_label("Save Key to File\u{2026}");
    assert!(rig.harness.query_by_label("Cancel").is_none());
    assert!(rig.harness.query_by_label("I Saved the Key").is_none());
}

#[test]
fn a_key_that_was_never_saved_keeps_cancel_next_to_i_saved_the_key() {
    let (sheet, _) = BackupSheet::show_key(&facts(true));
    let mut rig = Rig::new(sheet);
    rig.sheet.borrow_mut().on_reply(&BackupReply::KeyShown {
        id: "docs".to_owned(),
        result: Ok(Key::generate().unwrap()),
    });

    settle(&mut rig.harness);

    rig.harness.get_by_label("Cancel");
    rig.harness.get_by_label("I Saved the Key");
    assert!(rig.harness.query_by_label("Done").is_none());
}

fn shows_every_group_of(rig: &Rig, key: &Key) {
    let shown = key.display_groups();
    let groups: Vec<&str> = shown.split('-').collect();
    let last = groups.len() - 1;
    for (index, group) in groups.iter().enumerate() {
        let label = if index < last {
            format!("{group}-")
        } else {
            (*group).to_owned()
        };
        assert!(
            rig.harness.get_all_by_label(label.as_str()).count() >= 1,
            "group {index} is not on screen"
        );
    }
}

#[test]
fn the_key_step_draws_the_key_group_by_group() {
    let (rig, key) = key_step();

    shows_every_group_of(&rig, &key);
    rig.harness.get_by_label("Save Key to File\u{2026}");
    rig.harness.get_by_label("The file is not encrypted.");
}

#[test]
fn the_shown_key_is_drawn_group_by_group_too() {
    let key = Key::generate().unwrap();
    let (sheet, _) = BackupSheet::show_key(&facts(true));
    let mut rig = Rig::new(sheet);
    rig.sheet.borrow_mut().on_reply(&BackupReply::KeyShown {
        id: "docs".to_owned(),
        result: Ok(key.clone()),
    });

    settle(&mut rig.harness);

    shows_every_group_of(&rig, &key);
    rig.harness.get_by_label("The file is not encrypted.");
}

#[test]
fn the_remove_alert_says_the_backup_stays_where_it_is() {
    let mut rig = Rig::new(BackupSheet::remove(&facts(false)));
    settle(&mut rig.harness);

    rig.harness.get_by_label(
        "syncfast will stop backing up these folders. The backup stays at gdrive:Backups/docs and can be added again with Add Existing Backup\u{2026}.",
    );
}

const FOLDER: &str = "/home/user/Documents";
const DEST: &str = "/mnt/backup";
const DEST_TOKEN: &str = "/mnt/backup\n/home/user/Documents";

fn wizard_at(flow: Flow, step: Step, setup: impl FnOnce(&mut WizardState)) -> Rig {
    let (sheet, _) = BackupSheet::wizard(flow);
    let mut rig = Rig::new(sheet);
    {
        let mut sheet = rig.sheet.borrow_mut();
        sheet.on_key_storage(&Ok(KeyBackend::Portal));
        let state = sheet.wizard_state().unwrap();
        state.folders = vec![FOLDER.into()];
        state.kind = DestKind::Computer;
        DEST.clone_into(&mut state.local);
        state.step = step;
        setup(state);
    }
    settle(&mut rig.harness);
    rig
}

fn answer(rig: &mut Rig, token: &str, problem: Option<&str>) {
    rig.sheet.borrow_mut().on_reply(&BackupReply::Checked {
        token: token.to_owned(),
        problem: problem.map(str::to_owned),
    });
    settle(&mut rig.harness);
}

fn step_of(rig: &Rig) -> Step {
    rig.sheet.borrow_mut().wizard_state().unwrap().step
}

fn assert_blocked(rig: &Rig, primary: &str, message: &str) {
    assert!(disabled(rig, primary), "{primary} is enabled");
    rig.harness.get_by_label(message);
}

#[test]
fn a_problem_found_in_the_folders_blocks_continue_and_is_shown() {
    let mut rig = wizard_at(Flow::New, Step::Folders, |_| {});

    answer(&mut rig, FOLDER, Some("That folder does not exist."));

    assert_blocked(&rig, "Continue", "That folder does not exist.");
}

#[test]
fn a_relative_destination_blocks_continue_and_is_shown() {
    let rig = wizard_at(Flow::New, Step::Destination, |state| {
        "backup".clone_into(&mut state.local);
    });

    assert_blocked(&rig, "Continue", "Enter the full path of the folder.");
}

#[test]
fn a_problem_found_in_the_destination_blocks_continue_and_is_shown() {
    let mut rig = wizard_at(Flow::New, Step::Destination, |_| {});

    answer(
        &mut rig,
        DEST_TOKEN,
        Some("That folder is inside a folder to back up."),
    );

    assert_blocked(
        &rig,
        "Continue",
        "That folder is inside a folder to back up.",
    );
}

#[test]
fn a_problem_found_in_an_existing_backup_destination_blocks_continue_and_is_shown() {
    let mut rig = wizard_at(Flow::Existing, Step::Destination, |_| {});

    answer(
        &mut rig,
        "existing\n/mnt/backup",
        Some("There is no backup in that folder."),
    );

    assert_blocked(&rig, "Continue", "There is no backup in that folder.");
}

#[test]
fn a_keep_count_that_is_not_a_number_blocks_continue_and_is_shown() {
    let rig = wizard_at(Flow::New, Step::Schedule, |state| {
        "abc".clone_into(&mut state.keep[0]);
    });

    assert_blocked(&rig, "Continue", "keep.daily must be between 0 and 999");
}

#[test]
fn keeping_nothing_blocks_continue_and_is_shown() {
    let rig = wizard_at(Flow::New, Step::Schedule, |state| {
        for keep in &mut state.keep {
            "0".clone_into(keep);
        }
    });

    assert_blocked(&rig, "Continue", "keep must retain at least one snapshot");
}

#[test]
fn a_speed_limit_that_is_not_a_speed_blocks_continue_and_is_shown() {
    let rig = wizard_at(Flow::New, Step::Schedule, |state| {
        "fast".clone_into(&mut state.limit_upload);
    });

    assert_blocked(
        &rig,
        "Continue",
        "limit_upload must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M",
    );
}

#[test]
fn a_filter_rule_that_includes_blocks_continue_and_is_shown() {
    let rig = wizard_at(Flow::New, Step::Filters, |state| {
        "+ keep".clone_into(&mut state.rules);
    });

    assert_blocked(
        &rig,
        "Continue",
        "backup filters only exclude: remove line 1 (\"+ \")",
    );
}

#[test]
fn a_key_that_could_not_be_generated_blocks_the_key_step_and_is_shown() {
    let rig = wizard_at(Flow::New, Step::Key, |state| {
        state.key_error = Some("The system has no random numbers.".to_owned());
    });

    assert_blocked(&rig, "I Saved the Key", "The system has no random numbers.");
}

#[test]
fn a_malformed_typed_key_blocks_continue_and_is_shown() {
    let mut rig = wizard_at(Flow::Existing, Step::Key, |state| {
        state.show_typed_key = true;
    });
    type_into(&mut rig, Role::TextInput, "Backup key", "ABC");

    assert_blocked(
        &rig,
        "Continue",
        "The key must have 52 letters and digits (A to Z, 2 to 7).",
    );
}

#[test]
fn continue_and_back_walk_the_steps_of_a_new_backup() {
    let mut rig = wizard_at(Flow::New, Step::Folders, |_| {});
    assert!(disabled(&rig, "Continue"));
    answer(&mut rig, FOLDER, None);
    assert!(!disabled(&rig, "Continue"));
    assert!(rig.harness.query_by_label("Back").is_none());

    rig.harness.get_by_label("Continue").click();
    settle(&mut rig.harness);
    assert_eq!(step_of(&rig), Step::Destination);
    assert!(disabled(&rig, "Continue"));
    answer(&mut rig, DEST_TOKEN, None);
    assert!(!disabled(&rig, "Continue"));

    rig.harness.get_by_label("Continue").click();
    settle(&mut rig.harness);
    assert_eq!(step_of(&rig), Step::Schedule);

    rig.harness.get_by_label("Back").click();
    settle(&mut rig.harness);
    assert_eq!(step_of(&rig), Step::Destination);
    answer(&mut rig, DEST_TOKEN, None);

    for expected in [Step::Schedule, Step::Filters, Step::Name] {
        rig.harness.get_by_label("Continue").click();
        settle(&mut rig.harness);
        assert_eq!(step_of(&rig), expected);
    }

    rig.harness.get_by_label("Continue").click();
    settle(&mut rig.harness);
    assert_eq!(step_of(&rig), Step::Key);
    rig.harness.get_by_label("I Saved the Key");
    assert!(rig.harness.query_by_label("Back").is_none());
}

#[test]
fn continue_and_back_walk_the_steps_of_an_existing_backup() {
    let mut rig = wizard_at(Flow::Existing, Step::Destination, |_| {});
    answer(&mut rig, "existing\n/mnt/backup", None);

    rig.harness.get_by_label("Continue").click();
    settle(&mut rig.harness);
    assert_eq!(step_of(&rig), Step::Key);

    rig.harness.get_by_label("Back").click();
    settle(&mut rig.harness);
    assert_eq!(step_of(&rig), Step::Destination);
}

#[test]
fn a_long_message_does_not_widen_the_sheet() {
    let long = "Cannot reach the key storage: no Secret portal and no Secret Service answered, so backups cannot keep their key. Install one of them and try again; see docs/guide/backup.md.";
    let mut narrow = wizard_at(Flow::New, Step::Schedule, |_| {});
    let mut wide = wizard_at(Flow::New, Step::Folders, |_| {});
    wide.sheet
        .borrow_mut()
        .on_key_storage(&Err(long.to_owned()));
    settle(&mut wide.harness);
    settle(&mut narrow.harness);

    wide.harness.get_by_label(long);
    let edge = |rig: &Rig| rig.harness.get_by_label("Continue").rect().right();
    assert!((edge(&wide) - edge(&narrow)).abs() < 0.5);
}

fn type_into(rig: &mut Rig, role: Role, label: &str, text: &str) {
    let field = rig.harness.get_by_role_and_label(role, label);
    field.click();
    field.type_text(text);
    rig.harness.run_steps(4);
}

#[test]
fn the_enter_key_sheet_saves_a_valid_key_typed_after_show() {
    let key = Key::generate().unwrap();
    let mut rig = Rig::new(BackupSheet::enter_key(&facts(false)));
    settle(&mut rig.harness);

    rig.harness.get_by_label("Enter Key");
    rig.harness.get_by_label("Cancel");
    assert!(disabled(&rig, "Save Key"));
    rig.harness.get_by_role_and_label(Role::CheckBox, "Show");

    rig.harness
        .get_by_role_and_label(Role::CheckBox, "Show")
        .click();
    settle(&mut rig.harness);
    type_into(
        &mut rig,
        Role::TextInput,
        "Backup key",
        &key.display_groups(),
    );
    assert!(!disabled(&rig, "Save Key"));

    rig.harness.get_by_label("Save Key").click();
    settle(&mut rig.harness);
    assert!(matches!(
        rig.commands().as_slice(),
        [Command::Backup(BackupCommand::EnterKey { id, key: sent })] if id == "docs" && *sent == key
    ));
}

#[test]
fn the_enter_key_sheet_says_what_is_wrong_with_a_malformed_key() {
    let mut rig = Rig::new(BackupSheet::enter_key(&facts(false)));
    settle(&mut rig.harness);
    rig.harness
        .get_by_role_and_label(Role::CheckBox, "Show")
        .click();
    settle(&mut rig.harness);

    type_into(&mut rig, Role::TextInput, "Backup key", "ABC");

    rig.harness
        .get_by_label("The key must have 52 letters and digits (A to Z, 2 to 7).");
    assert!(disabled(&rig, "Save Key"));
}
