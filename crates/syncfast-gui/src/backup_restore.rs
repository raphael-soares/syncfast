//! The restore sheets: what the user's actions and the backend's replies do
//! to the files of a snapshot, the question, the progress and the end of a
//! restore, and the commands that follow. No I/O here.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use syncfast_core::backup::restore_job::RestoreReport;
use syncfast_core::backup::snapshots::SnapshotView;

use crate::backend::backup_sheets::{BackupCommand, BackupReply};
use crate::backend::Command;
use crate::backup_sheets::Outcome;
use crate::ui::Action;
use crate::ui::backup_restore::{self, Browse, RestoreAction};
use crate::ui::backup_wizard::BackupFacts;

/// How often the sheet asks the job how far it is.
const POLL: Duration = Duration::from_millis(500);

#[derive(Debug)]
enum Phase {
    Browsing,
    /// The folder chooser is open.
    Choosing,
    /// The backend picks the name of the new folder.
    Planning,
    Confirm(PathBuf),
    Running {
        destination: PathBuf,
        report: Option<RestoreReport>,
    },
    Done {
        destination: PathBuf,
        report: RestoreReport,
    },
    Failed(String),
}

#[derive(Debug)]
pub struct RestoreSession {
    backup: String,
    backup_name: String,
    snapshot: SnapshotView,
    /// `None` when the whole snapshot is restored without choosing.
    browse: Option<Browse>,
    /// What is restored; none is all of the snapshot.
    paths: Vec<String>,
    /// The folder the person chose, to try again in it.
    parent: Option<PathBuf>,
    phase: Phase,
    next_poll: Option<Instant>,
}

fn command(command: BackupCommand) -> Command {
    Command::Backup(command)
}

impl RestoreSession {
    /// `Browse Files…`: the files of the snapshot, starting with the folder
    /// the first listing needs.
    #[must_use]
    pub fn browse(facts: &BackupFacts, snapshot: &SnapshotView) -> (Self, Vec<Command>) {
        let mut browse = Browse::starting_in(&snapshot.paths);
        let dir = browse.dir().to_owned();
        browse.open(&dir);
        let session = Self::new(facts, snapshot, Some(browse), Phase::Browsing);
        let read = session.list(&dir);
        (session, vec![read])
    }

    /// `Restore Snapshot…`: all of it, to a folder to choose.
    #[must_use]
    pub fn whole(facts: &BackupFacts, snapshot: &SnapshotView) -> (Self, Vec<Command>) {
        let session = Self::new(facts, snapshot, None, Phase::Choosing);
        (session, vec![Command::PickFolder])
    }

    fn new(
        facts: &BackupFacts,
        snapshot: &SnapshotView,
        browse: Option<Browse>,
        phase: Phase,
    ) -> Self {
        Self {
            backup: facts.id.clone(),
            backup_name: facts.name.clone(),
            snapshot: snapshot.clone(),
            browse,
            paths: Vec::new(),
            parent: None,
            phase,
            next_poll: None,
        }
    }

    fn list(&self, dir: &str) -> Command {
        command(BackupCommand::ListFiles {
            backup: self.backup.clone(),
            snapshot: Box::new(self.snapshot.clone()),
            dir: dir.to_owned(),
        })
    }

    fn plan(&mut self, parent: PathBuf) -> Command {
        self.phase = Phase::Planning;
        let plan = command(BackupCommand::PlanRestore {
            backup_name: self.backup_name.clone(),
            parent: parent.clone(),
        });
        self.parent = Some(parent);
        plan
    }

    pub fn apply(&mut self, action: &RestoreAction) -> Outcome {
        match (action, &self.phase) {
            (RestoreAction::Open(dir), Phase::Browsing) => {
                let Some(browse) = &mut self.browse else {
                    return Outcome::default();
                };
                if browse.open(dir) {
                    return Outcome::commands(vec![self.list(dir)]);
                }
                Outcome::default()
            }
            (RestoreAction::Toggle(path), Phase::Browsing) => {
                if let Some(browse) = &mut self.browse {
                    browse.toggle(path);
                }
                Outcome::default()
            }
            (RestoreAction::RestoreTo, Phase::Browsing) => {
                let Some(browse) = &self.browse else {
                    return Outcome::default();
                };
                if browse.count() == 0 {
                    return Outcome::default();
                }
                self.paths = browse.selection();
                self.phase = Phase::Choosing;
                Outcome::commands(vec![Command::PickFolder])
            }
            (RestoreAction::Confirm, Phase::Confirm(destination)) => {
                let destination = destination.clone();
                let start = command(BackupCommand::StartRestore {
                    backup: self.backup.clone(),
                    snapshot: self.snapshot.id.clone(),
                    paths: self.paths.clone(),
                    destination: destination.clone(),
                });
                self.next_poll = None;
                self.phase = Phase::Running {
                    destination,
                    report: None,
                };
                Outcome::commands(vec![start])
            }
            (RestoreAction::TryAgain, Phase::Failed(_)) => match self.parent.clone() {
                Some(parent) => Outcome::commands(vec![self.plan(parent)]),
                None => {
                    self.phase = Phase::Choosing;
                    Outcome::commands(vec![Command::PickFolder])
                }
            },
            (RestoreAction::OpenFolder, Phase::Done { destination, .. }) => {
                Outcome::commands(vec![Command::OpenLocalFolder(destination.clone())])
            }
            (RestoreAction::Done, Phase::Done { .. }) => Outcome::close(Vec::new()),
            _ => Outcome::default(),
        }
    }

    /// `Cancel`: closes the sheet, or interrupts the restore that runs.
    pub fn cancel(&mut self) -> Outcome {
        match &self.phase {
            Phase::Running {
                report: Some(report),
                ..
            } => match &report.id {
                Some(id) => Outcome::commands(vec![command(BackupCommand::CancelRestore(
                    id.clone(),
                ))]),
                None => Outcome::default(),
            },
            Phase::Running { report: None, .. } => Outcome::default(),
            _ => Outcome::close(Vec::new()),
        }
    }

    /// The folder chooser answered.
    pub fn on_picked(&mut self, picked: Option<&PathBuf>) -> Outcome {
        if !matches!(self.phase, Phase::Choosing) {
            return Outcome::default();
        }
        match picked {
            Some(parent) => Outcome::commands(vec![self.plan(parent.clone())]),
            None if self.browse.is_some() => {
                self.phase = Phase::Browsing;
                Outcome::default()
            }
            None => Outcome::close(Vec::new()),
        }
    }

    pub fn on_reply(&mut self, reply: &BackupReply) -> Outcome {
        match reply {
            BackupReply::FilesListed {
                snapshot,
                dir,
                result,
            } if *snapshot == self.snapshot.id => {
                if let Some(browse) = &mut self.browse {
                    browse.loaded(dir, result);
                }
            }
            BackupReply::RestorePlanned(result) if matches!(self.phase, Phase::Planning) => {
                match result {
                    Ok(destination) => self.phase = Phase::Confirm(destination.clone()),
                    Err(text) => self.phase = Phase::Failed(text.clone()),
                }
            }
            BackupReply::RestoreStarted(result) => self.report_arrived(result, true),
            BackupReply::RestoreProgress(result) => self.report_arrived(result, false),
            _ => {}
        }
        Outcome::default()
    }

    fn report_arrived(&mut self, result: &Result<RestoreReport, String>, started: bool) {
        let Phase::Running {
            destination,
            report: shown,
        } = &self.phase
        else {
            return;
        };
        let next = match result {
            Err(text) => Phase::Failed(text.clone()),
            Ok(report) => {
                let known = shown.as_ref().and_then(|shown| shown.id.as_ref());
                if !started && known != report.id.as_ref() {
                    return;
                }
                match report.status {
                    "done" => Phase::Done {
                        destination: destination.clone(),
                        report: report.clone(),
                    },
                    "failed" => Phase::Failed(
                        report
                            .error
                            .clone()
                            .unwrap_or_else(|| "The restore failed.".to_owned()),
                    ),
                    _ => Phase::Running {
                        destination: destination.clone(),
                        report: Some(report.clone()),
                    },
                }
            }
        };
        self.phase = next;
    }

    /// Asks the job how far it is while one runs. Returns when to look
    /// again.
    pub fn tick(&mut self, now: Instant) -> (Vec<Command>, Option<Duration>) {
        let Phase::Running {
            report: Some(report),
            ..
        } = &self.phase
        else {
            return (Vec::new(), None);
        };
        let Some(id) = report.id.clone() else {
            return (Vec::new(), None);
        };
        let due = *self.next_poll.get_or_insert(now + POLL);
        if now < due {
            return (Vec::new(), Some(due - now));
        }
        self.next_poll = Some(now + POLL);
        (
            vec![command(BackupCommand::RestoreStatus(id))],
            Some(POLL),
        )
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Vec<Action> {
        match &self.phase {
            Phase::Browsing | Phase::Choosing | Phase::Planning => match &self.browse {
                Some(browse) => {
                    backup_restore::browse(ctx, self.snapshot.time, &self.snapshot.paths, browse)
                }
                None => Vec::new(),
            },
            Phase::Confirm(destination) => backup_restore::confirm(ctx, destination),
            Phase::Running {
                destination,
                report,
            } => backup_restore::progress(ctx, destination, report.as_ref()),
            Phase::Done {
                destination,
                report,
            } => backup_restore::done(ctx, destination, report),
            Phase::Failed(text) => backup_restore::failed(ctx, text),
        }
    }
}
