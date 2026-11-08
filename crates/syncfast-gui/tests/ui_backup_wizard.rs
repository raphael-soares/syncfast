#![allow(clippy::unwrap_used)]

mod common;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use common::{Frame, harness, view_model};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::KeyBackend;
use syncfast_gui::backend::Command;
use syncfast_gui::backend::backup_sheets::{AddFailure, BackupCommand, BackupReply};
use syncfast_gui::backup_sheets::{BackupSheet, Shared};
use syncfast_gui::ui::Action;
use syncfast_gui::ui::backup_wizard::{BackupFacts, BackupSheetAction, DestKind, Flow, Step};

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
    rig.harness.run();

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

    rig.harness.run();

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

    rig.harness.run();

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

    rig.harness.run();

    assert!(disabled(&rig, "Continue"));
    rig.harness.get_by_label("Cancel");
}

#[test]
fn cancel_closes_the_wizard() {
    let (sheet, _) = BackupSheet::wizard(Flow::New);
    let mut rig = Rig::new(sheet);
    rig.harness.run();

    rig.harness.get_by_label("Cancel").click();
    rig.harness.run();

    assert_eq!(
        rig.harness.state().actions,
        [Action::BackupSheet(BackupSheetAction::Cancel)]
    );
}

#[test]
fn a_key_that_was_never_saved_opens_the_key_sheet_at_once() {
    let (sheet, commands) = BackupSheet::show_key(&facts(true));
    let mut rig = Rig::new(sheet);
    rig.harness.run();

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
    rig.harness.run();

    rig.harness.get_by_label("I Saved the Key").click();
    rig.harness.run();

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
    rig.harness.run();

    assert!(commands.is_empty());
    rig.harness.get_by_label("Show the backup key?");

    rig.harness.get_by_label("Show Key").click();
    rig.harness.run();

    assert_eq!(
        rig.commands(),
        [Command::Backup(BackupCommand::ShowKey("docs".to_owned()))]
    );
}

#[test]
fn cancelling_the_confirmation_asks_nothing_of_the_backend() {
    let (sheet, _) = BackupSheet::show_key(&facts(false));
    let mut rig = Rig::new(sheet);
    rig.harness.run();

    rig.harness.get_by_label("Cancel").click();
    rig.harness.run();

    assert!(rig.commands().is_empty());
    assert_eq!(
        rig.harness.state().actions,
        [Action::BackupSheet(BackupSheetAction::Cancel)]
    );
}

#[test]
fn removing_a_backup_names_it_and_keeps_the_key_by_default() {
    let mut rig = Rig::new(BackupSheet::remove(&facts(false)));
    rig.harness.run();

    rig.harness.get_by_label("Remove \"Docs\"?");
    rig.harness
        .get_by_label("Also delete the key from this computer");

    rig.harness.get_by_label("Remove").click();
    rig.harness.run();

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
    rig.harness.run();
    rig.harness
        .get_by_label("Also delete the key from this computer")
        .click();
    rig.harness.run();

    rig.harness.get_by_label("Remove").click();
    rig.harness.run();

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

    rig.harness.run();

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

    rig.harness.run();

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
    rig.harness.run();
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
    rig.harness.run();

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
    rig.harness.run();

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
    rig.harness.run();
    rig.harness.get_by_label("Show Key").click();
    rig.harness.run();
    rig.sheet.borrow_mut().on_reply(&BackupReply::KeyShown {
        id: "docs".to_owned(),
        result: Ok(Key::generate().unwrap()),
    });

    rig.harness.run();

    rig.harness.get_by_label("Your Backup Key");
    rig.harness.get_by_label("Done");
    rig.harness.get_by_label("Save Key to File\u{2026}");
    assert!(rig.harness.query_by_label("I Saved the Key").is_none());
}

#[test]
fn the_remove_alert_says_the_backup_stays_where_it_is() {
    let mut rig = Rig::new(BackupSheet::remove(&facts(false)));
    rig.harness.run();

    rig.harness.get_by_label(
        "syncfast will stop backing up these folders. The backup stays at gdrive:Backups/docs and can be added again with Add Existing Backup\u{2026}.",
    );
}
