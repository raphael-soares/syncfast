#![allow(clippy::unwrap_used)]

mod common;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use common::{Frame, harness, view_model};
use egui::accesskit::Toggled;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use syncfast_core::backup::files::{Listing, NodeKind, NodeView};
use syncfast_core::backup::restore::NOTHING_MATCHES;
use syncfast_core::backup::restore_job::RestoreReport;
use syncfast_core::backup::snapshots::SnapshotView;
use syncfast_gui::backend::Command;
use syncfast_gui::backend::backup_sheets::{BackupCommand, BackupReply};
use syncfast_gui::backup_sheets::{BackupSheet, Shared};
use syncfast_gui::ui::Action;
use syncfast_gui::ui::backup_restore::{BUSY_SYNC, date};
use syncfast_gui::ui::backup_wizard::BackupFacts;

const SNAPSHOT: &str = "abcd1234abcd1234";
const DESTINATION: &str = "/home/user/Out/Docs 2026-11-03 1200";

struct Rig {
    harness: Harness<'static, Frame>,
    sheet: Rc<RefCell<BackupSheet>>,
    commands: Rc<RefCell<Vec<Command>>>,
    closed: Rc<Cell<bool>>,
}

fn facts() -> BackupFacts {
    BackupFacts {
        id: "docs".to_owned(),
        name: "Docs".to_owned(),
        dest: "gdrive:Backups/docs".to_owned(),
        key_unsaved: false,
    }
}

fn snapshot() -> SnapshotView {
    SnapshotView {
        id: SNAPSHOT.to_owned(),
        short_id: "abcd1234".to_owned(),
        time: common::now(),
        host: "laptop".to_owned(),
        paths: vec!["/work/docs".to_owned()],
        files: Some(12),
        bytes: Some(2048),
        added_bytes: None,
    }
}

impl Rig {
    fn new(sheet: BackupSheet, first: Vec<Command>) -> Self {
        let sheet = Rc::new(RefCell::new(sheet));
        let commands = Rc::new(RefCell::new(first));
        let closed = Rc::new(Cell::new(false));
        let backups = vec![facts()];
        let (shown, sent, gone) = (Rc::clone(&sheet), Rc::clone(&commands), Rc::clone(&closed));
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
                    gone.set(gone.get() || outcome.close);
                }
            }
            actions
        });
        Self {
            harness,
            sheet,
            commands,
            closed,
        }
    }

    fn browsing() -> Self {
        let (sheet, commands) = BackupSheet::browse(&facts(), &snapshot());
        Self::new(sheet, commands)
    }

    fn whole() -> Self {
        let (sheet, commands) = BackupSheet::restore_whole(&facts(), &snapshot());
        Self::new(sheet, commands)
    }

    fn commands(&self) -> Vec<Command> {
        self.commands.borrow().clone()
    }

    fn reply(&mut self, reply: &BackupReply) {
        let outcome = self.sheet.borrow_mut().on_reply(reply);
        self.commands.borrow_mut().extend(outcome.commands);
        self.closed.set(self.closed.get() || outcome.close);
        settle(&mut self.harness);
    }

    fn picked(&mut self, folder: Option<&str>) {
        let folder = folder.map(PathBuf::from);
        let outcome = self.sheet.borrow_mut().on_picked(folder.as_ref());
        self.commands.borrow_mut().extend(outcome.commands);
        self.closed.set(self.closed.get() || outcome.close);
        settle(&mut self.harness);
    }

    fn click(&mut self, label: &str) {
        self.harness.get_by_label(label).click();
        settle(&mut self.harness);
    }

    fn marked(&self, label: &str) -> Option<Toggled> {
        self.harness.get_by_label(label).accesskit_node().toggled()
    }

    fn disabled(&self, label: &str) -> bool {
        self.harness
            .get_by_label(label)
            .accesskit_node()
            .is_disabled()
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

fn node(dir: &str, name: &str, kind: NodeKind) -> NodeView {
    NodeView {
        name: name.to_owned(),
        kind,
        path: format!("{dir}/{name}"),
        size: (kind == NodeKind::File).then_some(1536),
        mtime: Some(common::now()),
    }
}

fn listed(dir: &str, nodes: Vec<NodeView>) -> BackupReply {
    BackupReply::FilesListed {
        snapshot: SNAPSHOT.to_owned(),
        dir: dir.to_owned(),
        result: Ok(Listing {
            nodes,
            truncated: false,
        }),
    }
}

fn root_listing() -> BackupReply {
    listed(
        "/work/docs",
        vec![
            node("/work/docs", "src", NodeKind::Dir),
            node("/work/docs", "readme.txt", NodeKind::File),
        ],
    )
}

fn report(id: Option<&str>, status: &'static str, done: u64, total: u64) -> RestoreReport {
    RestoreReport {
        id: id.map(ToOwned::to_owned),
        backup: "docs".to_owned(),
        snapshot: SNAPSHOT.to_owned(),
        destination: DESTINATION.to_owned(),
        files_restored: done,
        bytes_restored: done * 100,
        total_files: total,
        total_bytes: total * 100,
        status,
        finished: status != "running",
        error: None,
    }
}

fn backup_commands(rig: &Rig) -> Vec<BackupCommand> {
    rig.commands()
        .into_iter()
        .filter_map(|command| match command {
            Command::Backup(command) => Some(command),
            _ => None,
        })
        .collect()
}

fn at_confirmation() -> Rig {
    let mut rig = Rig::whole();
    rig.picked(Some("/home/user/Out"));
    rig.reply(&BackupReply::RestorePlanned(Ok(PathBuf::from(DESTINATION))));
    rig
}

fn running() -> Rig {
    let mut rig = at_confirmation();
    rig.click("Restore");
    rig.reply(&BackupReply::RestoreStarted(Ok(report(
        Some("r1"),
        "running",
        3,
        10,
    ))));
    rig
}

#[test]
fn browsing_reads_the_folder_of_the_snapshot_first() {
    let rig = Rig::browsing();

    assert_eq!(
        backup_commands(&rig),
        [BackupCommand::ListFiles {
            backup: "docs".to_owned(),
            snapshot: Box::new(snapshot()),
            dir: "/work/docs".to_owned(),
        }]
    );
}

#[test]
fn the_sheet_says_it_is_reading_until_the_folder_arrives() {
    let mut rig = Rig::browsing();
    settle(&mut rig.harness);

    rig.harness
        .get_by_label(&format!("Files in {} snapshot", date(common::now())));
    assert!(
        rig.harness
            .query_all_by_label("Reading the snapshot\u{2026}")
            .count()
            > 0
    );
    assert!(rig.harness.query_by_label("readme.txt").is_none());

    rig.reply(&root_listing());

    rig.harness.get_by_label("readme.txt");
    rig.harness.get_by_label("src");
    assert!(
        rig.harness
            .query_by_label("Reading the snapshot\u{2026}")
            .is_none()
    );
}

#[test]
fn selecting_a_folder_marks_what_is_inside_it() {
    let mut rig = Rig::browsing();
    rig.reply(&root_listing());

    rig.click("Select src");
    rig.click("src");
    rig.reply(&listed(
        "/work/docs/src",
        vec![node("/work/docs/src", "main.rs", NodeKind::File)],
    ));

    assert_eq!(rig.marked("Select main.rs"), Some(Toggled::True));
    assert!(rig.disabled("Select main.rs"));
    rig.harness.get_by_label("1 item selected");
}

#[test]
fn a_file_next_to_a_marked_folder_stays_unmarked() {
    let mut rig = Rig::browsing();
    rig.reply(&root_listing());

    rig.click("Select src");

    assert_eq!(rig.marked("Select src"), Some(Toggled::True));
    assert_eq!(rig.marked("Select readme.txt"), Some(Toggled::False));
}

#[test]
fn restore_to_is_disabled_until_something_is_selected() {
    let mut rig = Rig::browsing();
    rig.reply(&root_listing());

    assert!(rig.disabled("Restore To\u{2026}"));
    rig.harness.get_by_label("0 items selected");

    rig.click("Select readme.txt");

    assert!(!rig.disabled("Restore To\u{2026}"));
    rig.harness.get_by_label("1 item selected");
}

#[test]
fn restore_to_asks_for_a_folder_and_then_names_the_new_one() {
    let mut rig = Rig::browsing();
    rig.reply(&root_listing());
    rig.click("Select readme.txt");

    rig.click("Restore To\u{2026}");
    assert!(rig.commands().contains(&Command::PickFolder));
    rig.picked(Some("/home/user/Out"));

    assert!(backup_commands(&rig).contains(&BackupCommand::PlanRestore {
        backup_name: "Docs".to_owned(),
        parent: PathBuf::from("/home/user/Out"),
    }));
}

#[test]
fn a_folder_chooser_that_is_cancelled_goes_back_to_the_files() {
    let mut rig = Rig::browsing();
    rig.reply(&root_listing());
    rig.click("Select readme.txt");
    rig.click("Restore To\u{2026}");

    rig.picked(None);

    assert!(!rig.closed.get());
    assert_eq!(rig.marked("Select readme.txt"), Some(Toggled::True));
}

#[test]
fn restoring_a_whole_snapshot_skips_the_files_and_closes_when_no_folder_is_chosen() {
    let mut rig = Rig::whole();
    assert_eq!(rig.commands(), [Command::PickFolder]);

    rig.picked(None);

    assert!(rig.closed.get());
}

#[test]
fn the_confirmation_names_the_new_folder_and_says_nothing_is_overwritten() {
    let rig = at_confirmation();

    rig.harness.get_by_label("Restore to Docs 2026-11-03 1200?");
    rig.harness
        .get_by_label("Files are restored into a new folder. Nothing is overwritten.");
    rig.harness.get_by_label("Cancel");
}

#[test]
fn confirming_starts_the_restore_of_the_chosen_items_in_the_new_folder() {
    let mut rig = Rig::browsing();
    rig.reply(&root_listing());
    rig.click("Select readme.txt");
    rig.click("Restore To\u{2026}");
    rig.picked(Some("/home/user/Out"));
    rig.reply(&BackupReply::RestorePlanned(Ok(PathBuf::from(DESTINATION))));

    rig.click("Restore");

    assert!(
        backup_commands(&rig).contains(&BackupCommand::StartRestore {
            backup: "docs".to_owned(),
            snapshot: SNAPSHOT.to_owned(),
            paths: vec!["/work/docs/readme.txt".to_owned()],
            destination: PathBuf::from(DESTINATION),
        })
    );
}

#[test]
fn cancelling_the_confirmation_closes_without_starting() {
    let mut rig = at_confirmation();

    rig.click("Cancel");

    assert!(rig.closed.get());
    assert!(
        !backup_commands(&rig)
            .iter()
            .any(|command| matches!(command, BackupCommand::StartRestore { .. }))
    );
}

#[test]
fn progress_counts_the_files_and_cancel_interrupts_the_job() {
    let mut rig = running();

    rig.harness
        .get_by_label("Restoring to Docs 2026-11-03 1200");
    rig.harness.get_by_label("3 of 10 files \u{b7} 300 B");

    rig.click("Cancel");

    assert!(backup_commands(&rig).contains(&BackupCommand::CancelRestore("r1".to_owned())));
    assert!(!rig.closed.get());
}

#[test]
fn escape_does_not_interrupt_a_restore_that_is_running() {
    let mut rig = running();

    rig.harness.key_press(egui::Key::Escape);
    rig.harness.run_steps(4);

    assert!(!backup_commands(&rig).contains(&BackupCommand::CancelRestore("r1".to_owned())));
    assert!(!rig.closed.get());
    rig.harness
        .get_by_label("Restoring to Docs 2026-11-03 1200");
}

#[test]
fn a_restore_that_has_not_reported_yet_says_it_is_starting() {
    let mut rig = at_confirmation();

    rig.click("Restore");

    rig.harness.get_by_label("Starting\u{2026}");
}

#[test]
fn a_finished_restore_says_how_many_files_and_where() {
    let mut rig = running();

    rig.reply(&BackupReply::RestoreProgress(Ok(report(
        Some("r1"),
        "done",
        10,
        10,
    ))));

    rig.harness
        .get_by_label(&format!("Restored 10 files to {DESTINATION}."));
    rig.click("Open Folder");
    assert!(
        rig.commands()
            .contains(&Command::OpenLocalFolder(PathBuf::from(DESTINATION)))
    );
    assert!(!rig.closed.get());
    rig.click("Done");
    assert!(rig.closed.get());
}

#[test]
fn the_progress_of_another_job_is_ignored() {
    let mut rig = running();

    rig.reply(&BackupReply::RestoreProgress(Ok(report(
        Some("other"),
        "done",
        10,
        10,
    ))));

    rig.harness.get_by_label("3 of 10 files \u{b7} 300 B");
}

#[test]
fn a_sync_that_is_running_is_said_so_and_try_again_plans_the_same_folder_again() {
    let mut rig = at_confirmation();
    rig.click("Restore");

    rig.reply(&BackupReply::RestoreStarted(Err(BUSY_SYNC.to_owned())));

    rig.harness
        .get_by_label("This sync is running. Try again when it finishes.");
    rig.click("Try Again");
    let plans = backup_commands(&rig)
        .into_iter()
        .filter(|command| matches!(command, BackupCommand::PlanRestore { .. }))
        .count();
    assert_eq!(plans, 2);
}

#[test]
fn a_path_with_no_match_shows_the_error_the_job_left() {
    let mut rig = running();
    let mut failed = report(Some("r1"), "failed", 0, 0);
    failed.error = Some(NOTHING_MATCHES.to_owned());

    rig.reply(&BackupReply::RestoreProgress(Ok(failed)));

    rig.harness
        .get_by_label("Nothing in the snapshot matches that path.");
    rig.harness.get_by_label("Try Again");
}

#[test]
fn a_listing_that_failed_says_why_and_try_again_reads_it_again() {
    let mut rig = Rig::browsing();
    rig.reply(&BackupReply::FilesListed {
        snapshot: SNAPSHOT.to_owned(),
        dir: "/work/docs".to_owned(),
        result: Err("The backup repository is locked.".to_owned()),
    });

    rig.harness.get_by_label("The backup repository is locked.");
    rig.click("Try Again");

    let reads = backup_commands(&rig)
        .into_iter()
        .filter(|command| matches!(command, BackupCommand::ListFiles { .. }))
        .count();
    assert_eq!(reads, 2);
}

#[test]
fn the_listing_of_another_snapshot_is_ignored() {
    let mut rig = Rig::browsing();

    rig.reply(&BackupReply::FilesListed {
        snapshot: "ffff".to_owned(),
        dir: "/work/docs".to_owned(),
        result: Ok(Listing {
            nodes: vec![node("/work/docs", "other.txt", NodeKind::File)],
            truncated: false,
        }),
    });

    assert!(rig.harness.query_by_label("other.txt").is_none());
}

#[test]
fn a_running_restore_asks_for_its_status_twice_a_second() {
    let rig = running();
    let start = Instant::now();

    let (first, wait) = rig.sheet.borrow_mut().tick(start);
    let (early, soon) = rig
        .sheet
        .borrow_mut()
        .tick(start + Duration::from_millis(100));
    let (due, again) = rig
        .sheet
        .borrow_mut()
        .tick(start + Duration::from_millis(600));

    assert!(first.is_empty());
    assert_eq!(wait, Some(Duration::from_millis(500)));
    assert!(early.is_empty());
    assert_eq!(soon, Some(Duration::from_millis(400)));
    assert_eq!(
        due,
        [Command::Backup(BackupCommand::RestoreStatus(
            "r1".to_owned()
        ))]
    );
    assert_eq!(again, Some(Duration::from_millis(500)));
}

#[test]
fn a_sheet_that_does_not_restore_has_nothing_to_ask() {
    let rig = Rig::browsing();

    let (commands, wait) = rig.sheet.borrow_mut().tick(Instant::now());

    assert!(commands.is_empty());
    assert_eq!(wait, None);
}

#[test]
fn cancel_closes_the_file_list() {
    let mut rig = Rig::browsing();
    rig.reply(&root_listing());

    rig.click("Cancel");

    assert!(rig.closed.get());
}
