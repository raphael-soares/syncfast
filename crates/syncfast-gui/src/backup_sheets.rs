//! The sheets of the backup flows: what each one holds, what the user's
//! actions and the backend's replies do to it, and the commands that follow.
//! No I/O here; [`crate::sheets`] owns the sheet that is open.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use syncfast_core::backup::lifecycle::NewBackup;

use crate::backend::backup_sheets::{AddFailure, BackupCommand, BackupReply};
use crate::backend::{Command, Event};
use crate::ui::add_sync::{AddSyncAction, DirListing};
use crate::ui::backup_keys::{self, EnterKeyView, KeyFileOutcome, KeyView, RemoveView};
use crate::ui::backup_wizard::{
    self, BackupFacts, BackupSheetAction, Flow, KeyStoreFacts, PickTarget, Step, TypedKey,
    WizardState, WizardView,
};
use crate::ui::filters::{FilterRules, FilterTest, FilterTestState};
use crate::ui::{Action, RemoteRow};
use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::KeyBackend;

/// What the owner of the sheets has to do after the sheet reacted.
#[derive(Debug, Default)]
pub struct Outcome {
    pub commands: Vec<Command>,
    pub close: bool,
}

impl Outcome {
    fn commands(commands: Vec<Command>) -> Self {
        Self {
            commands,
            close: false,
        }
    }

    fn close(commands: Vec<Command>) -> Self {
        Self {
            commands,
            close: true,
        }
    }
}

#[derive(Debug)]
pub struct WizardSession {
    state: WizardState,
    dirs: HashMap<String, DirListing>,
    mkdir_error: Option<String>,
    key_store: KeyStoreFacts,
    filter_test: Option<FilterTest>,
    pick: Option<PickTarget>,
    key_file: Option<KeyFileOutcome>,
}

#[derive(Debug)]
pub struct KeySession {
    id: String,
    unsaved: bool,
    key: Option<Key>,
    error: Option<String>,
    file: Option<KeyFileOutcome>,
}

#[derive(Debug)]
pub struct EnterKeySession {
    id: String,
    typed: TypedKey,
    show: bool,
    busy: bool,
    error: Option<String>,
}

#[derive(Debug)]
pub struct RemoveSession {
    id: String,
    name: String,
    dest: String,
}

#[derive(Debug)]
pub enum BackupSheet {
    Wizard(Box<WizardSession>),
    ConfirmShowKey { id: String },
    Key(Box<KeySession>),
    EnterKey(Box<EnterKeySession>),
    Remove(Box<RemoveSession>),
}

/// What the sheets know that is not theirs.
#[derive(Debug, Clone, Copy)]
pub struct Shared<'a> {
    pub remotes: &'a [RemoteRow],
    pub home: &'a Path,
    pub backups: &'a [BackupFacts],
}

fn backup(command: BackupCommand) -> Command {
    Command::Backup(command)
}

impl BackupSheet {
    #[must_use]
    pub fn wizard(flow: Flow) -> (Self, Vec<Command>) {
        let session = WizardSession {
            state: WizardState::new(flow),
            dirs: HashMap::new(),
            mkdir_error: None,
            key_store: KeyStoreFacts::Probing,
            filter_test: None,
            pick: None,
            key_file: None,
        };
        let commands = vec![
            Command::LoadRemotes,
            Command::CheckKeyStorage { test: false },
        ];
        (Self::Wizard(Box::new(session)), commands)
    }

    /// `Show Key…`: asks first, unless the key was never saved and the
    /// sheet is the way to save it.
    #[must_use]
    pub fn show_key(facts: &BackupFacts) -> (Self, Vec<Command>) {
        if facts.key_unsaved {
            let session = KeySession {
                id: facts.id.clone(),
                unsaved: true,
                key: None,
                error: None,
                file: None,
            };
            (
                Self::Key(Box::new(session)),
                vec![backup(BackupCommand::ShowKey(facts.id.clone()))],
            )
        } else {
            (
                Self::ConfirmShowKey {
                    id: facts.id.clone(),
                },
                Vec::new(),
            )
        }
    }

    #[must_use]
    pub fn enter_key(facts: &BackupFacts) -> Self {
        Self::EnterKey(Box::new(EnterKeySession {
            id: facts.id.clone(),
            typed: TypedKey::default(),
            show: false,
            busy: false,
            error: None,
        }))
    }

    #[must_use]
    pub fn remove(facts: &BackupFacts) -> Self {
        Self::Remove(Box::new(RemoveSession {
            id: facts.id.clone(),
            name: facts.name.clone(),
            dest: facts.dest.clone(),
        }))
    }

    /// The wizard, to park it while an account is added.
    ///
    /// # Errors
    /// The sheet is not a wizard: it comes back.
    pub fn into_wizard(self) -> Result<Box<WizardSession>, Self> {
        match self {
            Self::Wizard(session) => Ok(session),
            other => Err(other),
        }
    }

    #[must_use]
    pub fn from_wizard(session: Box<WizardSession>) -> Self {
        Self::Wizard(session)
    }

    pub fn wizard_state(&mut self) -> Option<&mut WizardState> {
        match self {
            Self::Wizard(session) => Some(&mut session.state),
            _ => None,
        }
    }

    pub fn account_added(&mut self, name: &str) {
        if let Self::Wizard(session) = self {
            session.state.select_account(name);
        }
    }

    pub fn apply(&mut self, action: &BackupSheetAction, backups: &[BackupFacts]) -> Outcome {
        match action {
            BackupSheetAction::Cancel => Outcome::close(Vec::new()),
            BackupSheetAction::ConfirmShowKey => self.confirm_show_key(),
            BackupSheetAction::MarkKeySaved => match self {
                Self::Key(session) => Outcome::close(vec![backup(BackupCommand::MarkKeySaved(
                    session.id.clone(),
                ))]),
                _ => Outcome::default(),
            },
            BackupSheetAction::SaveEnteredKey(key) => match self {
                Self::EnterKey(session) => {
                    session.busy = true;
                    session.error = None;
                    Outcome::commands(vec![backup(BackupCommand::EnterKey {
                        id: session.id.clone(),
                        key: key.clone(),
                    })])
                }
                _ => Outcome::default(),
            },
            BackupSheetAction::ConfirmRemove { delete_key } => match self {
                Self::Remove(session) => Outcome::close(vec![backup(BackupCommand::Remove {
                    id: session.id.clone(),
                    delete_key: *delete_key,
                })]),
                _ => Outcome::default(),
            },
            BackupSheetAction::SaveKeyToFile(key) => self.save_key_file(key, backups),
            wizard_action => match self {
                Self::Wizard(session) => Outcome::commands(session.apply(wizard_action)),
                _ => Outcome::default(),
            },
        }
    }

    fn confirm_show_key(&mut self) -> Outcome {
        let Self::ConfirmShowKey { id } = self else {
            return Outcome::default();
        };
        let id = std::mem::take(id);
        let command = backup(BackupCommand::ShowKey(id.clone()));
        *self = Self::Key(Box::new(KeySession {
            id,
            unsaved: false,
            key: None,
            error: None,
            file: None,
        }));
        Outcome::commands(vec![command])
    }

    fn save_key_file(&mut self, key: &Key, backups: &[BackupFacts]) -> Outcome {
        let id = match self {
            Self::Wizard(session) => session.state.backup_id(backups),
            Self::Key(session) => session.id.clone(),
            _ => return Outcome::default(),
        };
        Outcome::commands(vec![backup(BackupCommand::SaveKeyFile {
            id,
            key: key.clone(),
        })])
    }

    /// `LoadDirs` and `MakeDir` of the destination tree.
    pub fn dirs_action(&mut self, action: &AddSyncAction) -> Vec<Command> {
        let Self::Wizard(session) = self else {
            return Vec::new();
        };
        match action {
            AddSyncAction::LoadDirs(path) => {
                session.dirs.insert(path.clone(), DirListing::Loading);
                vec![Command::ListDirs(path.clone())]
            }
            AddSyncAction::MakeDir(path) => {
                session.mkdir_error = None;
                vec![Command::MakeRemoteDir(path.clone())]
            }
            _ => Vec::new(),
        }
    }

    pub fn on_dirs(&mut self, path: &str, result: &Result<Vec<String>, String>) {
        if let Self::Wizard(session) = self {
            let listing = match result {
                Ok(names) => DirListing::Loaded(names.clone()),
                Err(text) => DirListing::Failed(text.clone()),
            };
            session.dirs.insert(path.to_owned(), listing);
        }
    }

    pub fn on_dir_made(&mut self, path: &str, result: &Result<(), String>) -> Vec<Command> {
        let Self::Wizard(session) = self else {
            return Vec::new();
        };
        if let Err(text) = result {
            session.mkdir_error = Some(text.clone());
            return Vec::new();
        }
        let Some((account, rel)) = path.split_once(':') else {
            return Vec::new();
        };
        let (parent, child) = rel.rsplit_once('/').unwrap_or(("", rel));
        session.state.remote_folder_created(parent, child);
        let key = format!("{account}:{parent}");
        session.dirs.insert(key.clone(), DirListing::Loading);
        vec![Command::ListDirs(key)]
    }

    pub fn on_picked(&mut self, picked: Option<&PathBuf>) {
        let Self::Wizard(session) = self else {
            return;
        };
        let target = session.pick.take();
        let (Some(path), Some(target)) = (picked, target) else {
            return;
        };
        match target {
            PickTarget::Folder => session.state.add_folder(path.clone()),
            PickTarget::Destination => session.state.picked_local(path),
        }
    }

    pub fn on_key_storage(&mut self, result: &Result<KeyBackend, String>) {
        if let Self::Wizard(session) = self {
            session.key_store = match result {
                Ok(backend) => KeyStoreFacts::Ready(*backend),
                Err(text) => KeyStoreFacts::Unavailable(text.clone()),
            };
        }
    }

    pub fn on_filter_test(&mut self, event: &Event) {
        let (
            Self::Wizard(session),
            Event::BackupFilterTest {
                id,
                presets,
                rules,
                outcome,
            },
        ) = (self, event)
        else {
            return;
        };
        let rules = FilterRules {
            presets: presets.clone(),
            custom: rules.clone(),
        };
        if session
            .filter_test
            .as_ref()
            .is_some_and(|test| test.id == *id && test.rules == rules)
        {
            let state = match outcome {
                Ok(count) => FilterTestState::Counted(*count),
                Err(text) => FilterTestState::Failed(text.clone()),
            };
            session.filter_test = Some(FilterTest {
                id: id.clone(),
                rules,
                state,
            });
        }
    }

    pub fn on_reply(&mut self, reply: &BackupReply) -> Outcome {
        match (&mut *self, reply) {
            (Self::Wizard(session), BackupReply::Checked { token, problem }) => {
                session.state.record(token.clone(), problem.clone());
            }
            (Self::Wizard(session), BackupReply::Created(result)) => match result {
                Ok(_) => return Outcome::close(Vec::new()),
                Err(text) => session.state.fail(Step::Key, text.clone()),
            },
            (Self::Wizard(session), BackupReply::Added(result)) => match result {
                Ok(_) => return Outcome::close(Vec::new()),
                Err(AddFailure::NoRepository(text)) => {
                    session.state.fail_at(Step::Destination, text.clone());
                }
                Err(AddFailure::WrongKey(text)) => session.state.fail_at(Step::Key, text.clone()),
                Err(AddFailure::Other(text)) => session.state.fail(Step::Name, text.clone()),
            },
            (Self::Key(session), BackupReply::KeyShown { id, result }) if *id == session.id => {
                match result {
                    Ok(key) => session.key = Some(key.clone()),
                    Err(text) => session.error = Some(text.clone()),
                }
            }
            (Self::EnterKey(session), BackupReply::KeyEntered { id, result })
                if *id == session.id =>
            {
                match result {
                    Ok(()) => return Outcome::close(Vec::new()),
                    Err(text) => {
                        session.busy = false;
                        session.error = Some(text.clone());
                    }
                }
            }
            (_, BackupReply::KeyFileSaved(result)) => self.key_file_saved(result),
            _ => {}
        }
        Outcome::default()
    }

    fn key_file_saved(&mut self, result: &Result<Option<PathBuf>, String>) {
        let outcome = match result {
            Ok(Some(path)) => KeyFileOutcome::Saved(path.clone()),
            Ok(None) => return,
            Err(text) => KeyFileOutcome::Failed(text.clone()),
        };
        match self {
            Self::Wizard(session) => session.key_file = Some(outcome),
            Self::Key(session) => session.file = Some(outcome),
            _ => {}
        }
    }

    /// Draws the sheet and returns what the user did.
    pub fn show(&mut self, ctx: &egui::Context, shared: &Shared<'_>) -> Vec<Action> {
        match self {
            Self::Wizard(session) => session.show(ctx, shared),
            Self::ConfirmShowKey { .. } => backup_keys::show_key_alert(ctx),
            Self::Key(session) => backup_keys::show_key(
                ctx,
                &KeyView {
                    key: session.key.as_ref(),
                    unsaved: session.unsaved,
                    error: session.error.as_deref(),
                    file: session.file.as_ref(),
                },
            ),
            Self::EnterKey(session) => {
                let view = EnterKeyView {
                    error: session.error.as_deref(),
                    busy: session.busy,
                };
                backup_keys::enter_key(ctx, &view, &mut session.typed, &mut session.show)
            }
            Self::Remove(session) => backup_keys::remove(
                ctx,
                &RemoveView {
                    id: &session.id,
                    name: &session.name,
                    dest: &session.dest,
                },
            ),
        }
    }
}

impl WizardSession {
    fn apply(&mut self, action: &BackupSheetAction) -> Vec<Command> {
        match action {
            BackupSheetAction::PickFolder(target) => {
                self.pick = Some(*target);
                vec![Command::PickFolder]
            }
            BackupSheetAction::Check(request) => {
                vec![backup(BackupCommand::Check(request.clone()))]
            }
            BackupSheetAction::TestFilters {
                paths,
                presets,
                rules,
            } => {
                self.filter_test = Some(FilterTest {
                    id: String::new(),
                    rules: FilterRules {
                        presets: presets.clone(),
                        custom: rules.clone(),
                    },
                    state: FilterTestState::Running,
                });
                let mut draft = NewBackup::with_defaults(
                    String::new(),
                    String::new(),
                    paths.clone(),
                    String::new(),
                );
                draft.presets.clone_from(presets);
                draft.filters.clone_from(rules);
                vec![Command::TestBackupFilters {
                    backup: Box::new(draft.to_config()),
                }]
            }
            BackupSheetAction::Create(draft, key) => {
                self.state.start_call();
                vec![backup(BackupCommand::Create {
                    draft: draft.clone(),
                    key: key.clone(),
                })]
            }
            BackupSheetAction::AddExisting(draft, key) => {
                self.state.start_call();
                vec![backup(BackupCommand::AddExisting {
                    draft: draft.clone(),
                    key: key.clone(),
                })]
            }
            _ => Vec::new(),
        }
    }

    fn show(&mut self, ctx: &egui::Context, shared: &Shared<'_>) -> Vec<Action> {
        let view = WizardView {
            remotes: shared.remotes,
            backups: shared.backups,
            dirs: &self.dirs,
            mkdir_error: self.mkdir_error.as_deref(),
            home: shared.home,
            key_store: &self.key_store,
            filter_test: self.filter_test.as_ref(),
            key_file: self.key_file.as_ref(),
        };
        backup_wizard::show(ctx, &view, &mut self.state)
    }
}
