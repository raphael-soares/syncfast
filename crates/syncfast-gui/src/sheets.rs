//! The sheet that is open, if any, and what the backend told it. One at a
//! time: opening another replaces the first. This module decides; it does no
//! I/O. The app sends the [`Command`]s it returns.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use syncfast_core::config::split_remote;
use syncfast_core::ops::FirstRound;
use syncfast_core::state::GateKind;

use crate::backend::{Command, CreatedSync, Event};
use crate::backup_sheets::{BackupSheet, Shared, WizardSession};
use crate::sheet_io::PreviewFailure;
use crate::ui::account_sheet::{self, AccountAction, AccountMode, AccountState, AccountView};
use crate::ui::add_sync::{
    self, AddSyncAction, AddSyncState, AddSyncView, DirListing, LocalFacts, NewSync,
};
use crate::ui::backup_wizard::{BackupFacts, BackupSheetAction, Flow};
use crate::ui::filters::FilterTest;
use crate::ui::review::{self, ReviewAction, ReviewPhase, ReviewView};
use crate::ui::{Action, PairRow, RemoteRow};

/// A review that finds a round running asks again this often.
const REVIEW_RETRY: Duration = Duration::from_secs(5);

#[derive(Debug)]
enum Current {
    Closed,
    AddSync(Box<AddSyncSession>),
    Review(Box<ReviewSession>),
    Account(Box<AccountSession>),
    Backup(Box<BackupSheet>),
}

#[derive(Debug)]
struct AddSyncSession {
    state: AddSyncState,
    dirs: HashMap<String, DirListing>,
    local: Option<LocalFacts>,
    mkdir_error: Option<String>,
    local_error: Option<String>,
    create_error: Option<String>,
    /// What was asked of the backend, to open the review of the new sync.
    creating: Option<NewSync>,
}

#[derive(Debug)]
struct ReviewSession {
    pair_id: String,
    name: String,
    remote: String,
    gate: GateKind,
    phase: ReviewPhase,
    /// When to ask again while a round is running.
    retry_at: Option<Instant>,
}

#[derive(Debug)]
struct AccountSession {
    state: AccountState,
    connecting: bool,
    error: Option<String>,
    /// After the account exists, the Add Sync sheet opens.
    then_add_sync: bool,
    /// Closing without an account brings the Add Sync sheet back.
    back_to_add_sync: bool,
    /// Closing without an account brings the New Backup sheet back.
    back_to_backup: Option<Box<WizardSession>>,
}

/// What the sheets need to know about the app.
#[derive(Debug, Clone, Copy)]
pub struct Surroundings<'a> {
    pub pairs: &'a [PairRow],
    /// The first screen is showing: a new account leads on to Add Sync.
    pub welcome: bool,
}

#[derive(Debug)]
pub struct Sheets {
    remotes: Vec<RemoteRow>,
    home: PathBuf,
    data_dir: PathBuf,
    current: Current,
    backups: Vec<BackupFacts>,
}

impl Sheets {
    #[must_use]
    pub fn new(home: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            remotes: Vec::new(),
            home,
            data_dir,
            current: Current::Closed,
            backups: Vec::new(),
        }
    }

    #[must_use]
    pub fn remotes(&self) -> &[RemoteRow] {
        &self.remotes
    }

    /// The backups that exist, for the sheets that name one.
    pub fn set_backups(&mut self, backups: Vec<BackupFacts>) {
        self.backups = backups;
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        !matches!(self.current, Current::Closed)
    }

    /// Opens the sheet an action asks for and applies what the user did in
    /// the open one. Other actions are not for the sheets.
    pub fn apply(&mut self, action: &Action, around: &Surroundings<'_>) -> Vec<Command> {
        match action {
            Action::AddSync => self.open_add_sync(AddSyncState::default()),
            Action::OpenReview(id) => self.open_review(id, around),
            Action::AddAccount => self.open_account(AccountMode::Add, around),
            Action::ReconnectAccount(name) => {
                self.edit_account(AccountMode::Update { name: name.clone() }, around)
            }
            Action::ChangeClientId(name) => {
                self.edit_account(AccountMode::ChangeClientId { name: name.clone() }, around)
            }
            Action::RenameAccount(name) => {
                self.edit_account(AccountMode::Rename { from: name.clone() }, around)
            }
            Action::NewBackup => self.open_backup(BackupSheet::wizard(Flow::New)),
            Action::AddExistingBackup => self.open_backup(BackupSheet::wizard(Flow::Existing)),
            Action::ShowBackupKey(id) => self.open_backup_of(id, BackupSheet::show_key),
            Action::EnterBackupKey(id) => {
                self.open_backup_of(id, |facts| (BackupSheet::enter_key(facts), Vec::new()))
            }
            Action::RemoveBackup(id) => {
                self.open_backup_of(id, |facts| (BackupSheet::remove(facts), Vec::new()))
            }
            Action::BrowseSnapshot { backup, snapshot } => {
                self.open_backup_of(backup, |facts| BackupSheet::browse(facts, snapshot))
            }
            Action::RestoreSnapshot { backup, snapshot } => {
                self.open_backup_of(backup, |facts| BackupSheet::restore_whole(facts, snapshot))
            }
            Action::BackupSheet(action) => self.backup_action(action),
            Action::AddSyncSheet(action) => match &mut self.current {
                Current::Backup(sheet) => sheet.dirs_action(action),
                _ => self.add_sync_action(action),
            },
            Action::Review(action) => self.review_action(*action),
            Action::Account(action) => self.account_action(action),
            _ => Vec::new(),
        }
    }

    /// What the sheet that is about to be replaced must stop.
    fn leave(&self) -> Vec<Command> {
        match &self.current {
            Current::Review(_) => vec![Command::CancelPreview],
            Current::Account(session) if session.connecting => vec![Command::CancelConnect],
            _ => Vec::new(),
        }
    }

    fn open_add_sync(&mut self, state: AddSyncState) -> Vec<Command> {
        let mut commands = self.leave();
        self.current = Current::AddSync(Box::new(AddSyncSession {
            state,
            dirs: HashMap::new(),
            local: None,
            mkdir_error: None,
            local_error: None,
            create_error: None,
            creating: None,
        }));
        commands.push(Command::LoadRemotes);
        commands
    }

    fn open_review_of(
        &mut self,
        pair_id: String,
        name: String,
        remote: &str,
        gate: GateKind,
    ) -> Vec<Command> {
        let account = split_remote(remote).map_or(remote, |(account, _)| account);
        let mut commands = self.leave();
        commands.push(Command::Preview {
            pair_id: pair_id.clone(),
            kind: review::review_kind(gate),
        });
        self.current = Current::Review(Box::new(ReviewSession {
            pair_id,
            name,
            remote: account.to_owned(),
            gate,
            phase: ReviewPhase::Checking,
            retry_at: None,
        }));
        commands
    }

    fn open_review(&mut self, id: &str, around: &Surroundings<'_>) -> Vec<Command> {
        let Some(pair) = around.pairs.iter().find(|pair| pair.id == id) else {
            return Vec::new();
        };
        let Some(gate) = pair
            .gate
            .filter(|gate| gate.is_needs_review() || gate.is_force_review())
        else {
            return Vec::new();
        };
        self.open_review_of(pair.id.clone(), pair.name.clone(), &pair.remote, gate)
    }

    /// The sheet of an existing remote, with the credentials it already has.
    fn edit_account(&mut self, mode: AccountMode, around: &Surroundings<'_>) -> Vec<Command> {
        let name = mode.remote().map(str::to_owned);
        let mut commands = self.open_account(mode, around);
        commands.extend(name.map(Command::LoadCredentials));
        commands
    }

    fn open_account(&mut self, mode: AccountMode, around: &Surroundings<'_>) -> Vec<Command> {
        let parked = if matches!(mode, AccountMode::Add) {
            self.park_wizard()
        } else {
            None
        };
        let from_wizard = matches!(self.current, Current::AddSync(_));
        let renaming = matches!(mode, AccountMode::Rename { .. });
        let mut commands = self.leave();
        commands.push(Command::LoadRemotes);
        self.current = Current::Account(Box::new(AccountSession {
            state: AccountState::new(mode),
            connecting: false,
            error: None,
            then_add_sync: !renaming && parked.is_none() && (from_wizard || around.welcome),
            back_to_add_sync: from_wizard,
            back_to_backup: parked,
        }));
        commands
    }

    fn review_action(&mut self, action: ReviewAction) -> Vec<Command> {
        let Current::Review(session) = &mut self.current else {
            return Vec::new();
        };
        match action {
            ReviewAction::Cancel => {
                self.current = Current::Closed;
                vec![Command::CancelPreview]
            }
            ReviewAction::Retry => {
                session.phase = ReviewPhase::Checking;
                vec![Command::Preview {
                    pair_id: session.pair_id.clone(),
                    kind: review::review_kind(session.gate),
                }]
            }
            ReviewAction::Confirm => {
                let ReviewPhase::Ready(found) = &session.phase else {
                    return Vec::new();
                };
                let command = Command::ConfirmReview {
                    pair_id: session.pair_id.clone(),
                    kind: review::review_kind(session.gate),
                    mode: found.mode,
                    filters_hash: found.filters_hash.clone(),
                };
                self.current = Current::Closed;
                vec![command]
            }
        }
    }

    fn add_sync_action(&mut self, action: &AddSyncAction) -> Vec<Command> {
        let Current::AddSync(session) = &mut self.current else {
            return Vec::new();
        };
        match action {
            AddSyncAction::Cancel => {
                self.current = Current::Closed;
                Vec::new()
            }
            AddSyncAction::LoadDirs(path) => {
                session.dirs.insert(path.clone(), DirListing::Loading);
                vec![Command::ListDirs(path.clone())]
            }
            AddSyncAction::MakeDir(path) => {
                session.mkdir_error = None;
                vec![Command::MakeRemoteDir(path.clone())]
            }
            AddSyncAction::CheckLocal(typed) => {
                session.local_error = None;
                vec![Command::CheckLocal(typed.clone())]
            }
            AddSyncAction::CreateLocal(path) => {
                session.local_error = None;
                vec![Command::CreateLocalDir(path.clone())]
            }
            AddSyncAction::PickFolder => vec![Command::PickFolder],
            AddSyncAction::Create(new) => {
                session.create_error = None;
                session.creating = Some(new.clone());
                vec![Command::CreateSync(new.clone())]
            }
        }
    }

    fn account_action(&mut self, action: &AccountAction) -> Vec<Command> {
        let Current::Account(session) = &mut self.current else {
            return Vec::new();
        };
        match action {
            AccountAction::Connect(request) => {
                session.connecting = true;
                session.error = None;
                vec![Command::ConnectAccount(request.clone())]
            }
            AccountAction::Cancel => {
                let was_connecting = session.connecting;
                let back = session.back_to_add_sync;
                let parked = session.back_to_backup.take();
                self.current = Current::Closed;
                let mut commands = Vec::new();
                if was_connecting {
                    commands.push(Command::CancelConnect);
                }
                if let Some(wizard) = parked {
                    self.current = Current::Backup(Box::new(BackupSheet::from_wizard(wizard)));
                } else if back {
                    commands.extend(self.open_add_sync(AddSyncState::default()));
                }
                commands
            }
        }
    }

    /// The sheet of a backup flow replaces the one that is open.
    fn open_backup(&mut self, (sheet, mut commands): (BackupSheet, Vec<Command>)) -> Vec<Command> {
        let mut stops = self.leave();
        self.current = Current::Backup(Box::new(sheet));
        stops.append(&mut commands);
        stops
    }

    fn open_backup_of(
        &mut self,
        id: &str,
        open: impl FnOnce(&BackupFacts) -> (BackupSheet, Vec<Command>),
    ) -> Vec<Command> {
        let Some(facts) = self.backups.iter().find(|facts| facts.id == id) else {
            return Vec::new();
        };
        let opened = open(facts);
        self.open_backup(opened)
    }

    fn backup_action(&mut self, action: &BackupSheetAction) -> Vec<Command> {
        let Current::Backup(sheet) = &mut self.current else {
            return Vec::new();
        };
        let outcome = sheet.apply(action, &self.backups);
        if outcome.close {
            self.current = Current::Closed;
        }
        outcome.commands
    }

    /// The New Backup sheet, kept aside while an account is added.
    fn park_wizard(&mut self) -> Option<Box<WizardSession>> {
        match std::mem::replace(&mut self.current, Current::Closed) {
            Current::Backup(sheet) => match (*sheet).into_wizard() {
                Ok(wizard) => Some(wizard),
                Err(sheet) => {
                    self.current = Current::Backup(Box::new(sheet));
                    None
                }
            },
            other => {
                self.current = other;
                None
            }
        }
    }

    /// What the backend said that concerns the sheet of a backup flow.
    fn backup_event(&mut self, event: &Event) -> Option<Vec<Command>> {
        let Current::Backup(sheet) = &mut self.current else {
            return None;
        };
        match event {
            Event::Dirs { path, result } => sheet.on_dirs(path, result),
            Event::RemoteDirMade { path, result } => return Some(sheet.on_dir_made(path, result)),
            Event::FolderPicked(picked) => {
                let outcome = sheet.on_picked(picked.as_ref());
                if outcome.close {
                    self.current = Current::Closed;
                }
                return Some(outcome.commands);
            }
            Event::KeyStorage { outcome, .. } => sheet.on_key_storage(outcome),
            Event::BackupFilterTest { .. } => sheet.on_filter_test(event),
            Event::Backup(reply) => {
                let outcome = sheet.on_reply(reply);
                if outcome.close {
                    self.current = Current::Closed;
                }
                return Some(outcome.commands);
            }
            _ => return None,
        }
        Some(Vec::new())
    }

    /// Takes in what the backend reported, if it concerns the sheets.
    pub fn handle_event(&mut self, event: &Event) -> Vec<Command> {
        if let Some(commands) = self.backup_event(event) {
            return commands;
        }
        match event {
            Event::Remotes(remotes) => {
                self.remotes.clone_from(remotes);
                Vec::new()
            }
            Event::Dirs { path, result } => {
                if let Current::AddSync(session) = &mut self.current {
                    let listing = match result {
                        Ok(names) => DirListing::Loaded(names.clone()),
                        Err(text) => DirListing::Failed(text.clone()),
                    };
                    session.dirs.insert(path.clone(), listing);
                }
                Vec::new()
            }
            Event::RemoteDirMade { path, result } => self.remote_dir_made(path, result),
            Event::LocalChecked(facts) => {
                if let Current::AddSync(session) = &mut self.current {
                    session.local = Some(facts.clone());
                }
                Vec::new()
            }
            Event::LocalCreated(result) => self.local_created(result),
            Event::FolderPicked(picked) => {
                self.folder_picked(picked.as_ref());
                Vec::new()
            }
            Event::SyncCreated(result) => self.sync_created(result),
            Event::Preview { pair_id, result } => {
                self.preview_arrived(pair_id, result);
                Vec::new()
            }
            Event::Credentials {
                name,
                client_id,
                client_secret,
            } => {
                if let Current::Account(session) = &mut self.current
                    && session.state.mode.remote() == Some(name.as_str())
                    && session.state.client_id.is_empty()
                    && session.state.client_secret.is_empty()
                {
                    session.state.client_id.clone_from(client_id);
                    client_secret
                        .expose()
                        .clone_into(&mut session.state.client_secret);
                }
                Vec::new()
            }
            Event::Connected(result) => self.connected(result),
            _ => Vec::new(),
        }
    }

    fn remote_dir_made(&mut self, path: &str, result: &Result<(), String>) -> Vec<Command> {
        let Current::AddSync(session) = &mut self.current else {
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

    fn local_created(&mut self, result: &Result<(), String>) -> Vec<Command> {
        let Current::AddSync(session) = &mut self.current else {
            return Vec::new();
        };
        if let Err(text) = result {
            session.local_error = Some(text.clone());
        }
        vec![Command::CheckLocal(session.state.local.clone())]
    }

    /// The sheet asks for the check itself when the path in its field
    /// changes, so a picked folder only has to land in the field.
    fn folder_picked(&mut self, picked: Option<&PathBuf>) {
        if let (Current::AddSync(session), Some(path)) = (&mut self.current, picked) {
            session.state.picked_local(path);
            session.local_error = None;
        }
    }

    fn sync_created(&mut self, result: &Result<CreatedSync, String>) -> Vec<Command> {
        let Current::AddSync(session) = &mut self.current else {
            return Vec::new();
        };
        match result {
            Err(text) => {
                session.create_error = Some(text.clone());
                session.creating = None;
                Vec::new()
            }
            Ok(created) => {
                let Some(new) = session.creating.take() else {
                    return Vec::new();
                };
                if created.first_round != FirstRound::Held {
                    self.current = Current::Closed;
                    return Vec::new();
                }
                self.open_review_of(
                    created.id.clone(),
                    new.name,
                    &new.remote,
                    GateKind::NeedsReviewFirstRun,
                )
            }
        }
    }

    fn preview_arrived(
        &mut self,
        pair_id: &str,
        result: &Result<syncfast_core::preview::Preview, PreviewFailure>,
    ) {
        let Current::Review(session) = &mut self.current else {
            return;
        };
        if session.pair_id != pair_id {
            return;
        }
        match result {
            Ok(found) => {
                session.phase = ReviewPhase::Ready(found.clone());
                session.retry_at = None;
            }
            Err(PreviewFailure::Busy) => {
                session.phase = ReviewPhase::Busy;
                session.retry_at = Some(Instant::now() + REVIEW_RETRY);
            }
            Err(PreviewFailure::Failed(text)) => {
                session.phase = ReviewPhase::Failed(text.clone());
                session.retry_at = None;
            }
        }
    }

    fn connected(&mut self, result: &Result<String, String>) -> Vec<Command> {
        let Current::Account(session) = &mut self.current else {
            return Vec::new();
        };
        match result {
            Err(text) => {
                session.connecting = false;
                session.error = Some(text.clone());
                Vec::new()
            }
            Ok(name) => {
                let next = session.then_add_sync;
                let parked = session.back_to_backup.take();
                self.current = Current::Closed;
                if let Some(wizard) = parked {
                    let mut sheet = BackupSheet::from_wizard(wizard);
                    sheet.account_added(name);
                    self.current = Current::Backup(Box::new(sheet));
                    return vec![Command::LoadRemotes];
                }
                if next {
                    let state = AddSyncState {
                        account: Some(name.clone()),
                        ..AddSyncState::default()
                    };
                    self.open_add_sync(state)
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Asks the backend again when a review waited for a running round and
    /// the wait is over. Returns when to look again, if it is waiting.
    pub fn tick(&mut self, now: Instant) -> (Vec<Command>, Option<Duration>) {
        let session = match &mut self.current {
            Current::Review(session) => session,
            Current::Backup(sheet) => return sheet.tick(now),
            _ => return (Vec::new(), None),
        };
        let Some(due) = session.retry_at else {
            return (Vec::new(), None);
        };
        if now < due {
            return (Vec::new(), Some(due - now));
        }
        session.retry_at = None;
        let command = Command::Preview {
            pair_id: session.pair_id.clone(),
            kind: review::review_kind(session.gate),
        };
        (vec![command], None)
    }

    /// Draws the open sheet and returns what the user did.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pairs: &[PairRow],
        filter_test: Option<&FilterTest>,
    ) -> Vec<Action> {
        match &mut self.current {
            Current::Closed => Vec::new(),
            Current::AddSync(session) => {
                let view = AddSyncView {
                    remotes: &self.remotes,
                    existing: pairs,
                    dirs: &session.dirs,
                    local: session.local.as_ref(),
                    mkdir_error: session.mkdir_error.as_deref(),
                    create_error: session.create_error.as_deref(),
                    creating: session.creating.is_some(),
                    home: &self.home,
                    data_dir: &self.data_dir,
                    local_error: session.local_error.as_deref(),
                    filter_test,
                };
                add_sync::show(ctx, &view, &mut session.state)
            }
            Current::Review(session) => {
                let view = ReviewView {
                    name: &session.name,
                    remote: &session.remote,
                    kept_days: pairs
                        .iter()
                        .find(|pair| pair.id == session.pair_id)
                        .and_then(|pair| {
                            let behavior = &pair.effective.behavior;
                            behavior.versions.then_some(behavior.versions_keep_days)
                        }),
                    gate: session.gate,
                    phase: &session.phase,
                };
                review::show(ctx, &view)
            }
            Current::Account(session) => {
                let view = AccountView {
                    existing: &self.remotes,
                    connecting: session.connecting,
                    error: session.error.as_deref(),
                };
                account_sheet::show(ctx, &view, &mut session.state)
            }
            Current::Backup(sheet) => {
                let shared = Shared {
                    remotes: &self.remotes,
                    home: &self.home,
                    backups: &self.backups,
                };
                sheet.show(ctx, &shared)
            }
        }
    }
}
