//! What the backend does for the backup sheets: the checks of the wizard,
//! creating and adding a backup, and the key operations. The key leaves this
//! module only inside [`BackupReply::KeyShown`], for the window that asked.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::sync::Arc;

use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::DefaultKeyStore;
use syncfast_core::backup::lifecycle::{
    self, AddExistingError, DestinationTarget, ExistingBackup, NewBackup,
};
use syncfast_core::restic::ResticError;
use zeroize::Zeroizing;

use super::{Context, Event, fail, guarded, load_tools};
use crate::ui::backup_wizard::{CheckPart, CheckRequest};

#[derive(Debug, Clone, PartialEq)]
pub enum BackupCommand {
    /// Answers a [`CheckRequest`] with the same token.
    Check(CheckRequest),
    Create {
        draft: Box<NewBackup>,
        key: Key,
    },
    AddExisting {
        draft: ExistingBackup,
        key: Key,
    },
    ShowKey(String),
    MarkKeySaved(String),
    EnterKey {
        id: String,
        key: Key,
    },
    Remove {
        id: String,
        delete_key: bool,
    },
    /// Asks where to write the key, then writes it there.
    SaveKeyFile {
        id: String,
        key: Key,
    },
}

/// Why `Add Backup` failed, by where the person has to go to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddFailure {
    NoRepository(String),
    WrongKey(String),
    Other(String),
}

#[derive(Debug, Clone)]
pub enum BackupReply {
    Checked {
        token: String,
        problem: Option<String>,
    },
    Created(Result<String, String>),
    Added(Result<String, AddFailure>),
    KeyShown {
        id: String,
        result: Result<Key, String>,
    },
    KeyEntered {
        id: String,
        result: Result<(), String>,
    },
    /// `None` when the person closed the file dialog.
    KeyFileSaved(Result<Option<PathBuf>, String>),
}

pub(super) fn spawn(context: &Arc<Context>, command: BackupCommand) {
    let context = Arc::clone(context);
    guarded(context.emitter.clone(), "working on a backup", async move {
        handle(&context, command).await;
        context.refresh.notify_one();
    });
}

fn reply(context: &Context, reply: BackupReply) {
    context.emitter.send(Event::Backup(reply));
}

fn warn(context: &Context, warnings: &[String]) {
    if !warnings.is_empty() {
        fail(context, warnings.join(" "));
    }
}

async fn handle(context: &Context, command: BackupCommand) {
    match command {
        BackupCommand::Check(request) => check(context, request).await,
        BackupCommand::Create { draft, key } => create(context, *draft, key).await,
        BackupCommand::AddExisting { draft, key } => add_existing(context, draft, key).await,
        BackupCommand::ShowKey(id) => {
            let keys = DefaultKeyStore::from_env(&context.paths);
            let result = lifecycle::show_key(&keys, &id)
                .await
                .map_err(|err| err.to_string());
            reply(context, BackupReply::KeyShown { id, result });
        }
        BackupCommand::MarkKeySaved(id) => {
            if let Err(text) = lifecycle::mark_key_saved(&context.paths, &id).await {
                fail(context, text);
            }
        }
        BackupCommand::EnterKey { id, key } => enter_key(context, id, key).await,
        BackupCommand::Remove { id, delete_key } => remove(context, &id, delete_key).await,
        BackupCommand::SaveKeyFile { id, key } => save_key_file(context, &id, key).await,
    }
}

async fn check(context: &Context, request: CheckRequest) {
    let CheckRequest {
        token,
        part,
        folders,
        dest,
    } = request;
    let problem = match load_tools(context).await {
        Ok(tools) => {
            let env = tools.env();
            match part {
                CheckPart::Folders => {
                    let draft = NewBackup::with_defaults(
                        "Backup".to_owned(),
                        "backup".to_owned(),
                        folders,
                        String::new(),
                    );
                    lifecycle::validate_draft(&env, &draft)
                        .await
                        .map_or_else(Some, |problems| problems.folders)
                }
                CheckPart::Destination => {
                    lifecycle::check_destination(&env, &dest, &folders, DestinationTarget::New)
                        .await
                        .err()
                }
                CheckPart::ExistingDestination => {
                    lifecycle::check_destination(&env, &dest, &[], DestinationTarget::Existing)
                        .await
                        .err()
                }
            }
        }
        Err(text) => Some(text),
    };
    reply(context, BackupReply::Checked { token, problem });
}

async fn create(context: &Context, draft: NewBackup, key: Key) {
    let result = match load_tools(context).await {
        Ok(tools) => lifecycle::create_backup(&tools.env(), draft, key, false)
            .await
            .map_err(|err| err.to_string()),
        Err(text) => Err(text),
    };
    match result {
        Ok(created) => {
            warn(context, &created.warnings);
            reply(context, BackupReply::Created(Ok(created.id)));
        }
        Err(text) => reply(context, BackupReply::Created(Err(text))),
    }
}

async fn add_existing(context: &Context, draft: ExistingBackup, key: Key) {
    let result = match load_tools(context).await {
        Ok(tools) => lifecycle::add_existing_backup(&tools.env(), &draft, key)
            .await
            .map_err(|err| match err {
                AddExistingError::Open(ResticError::NoRepository) => {
                    AddFailure::NoRepository(ResticError::NoRepository.to_string())
                }
                AddExistingError::Open(ResticError::WrongKey) => {
                    AddFailure::WrongKey(ResticError::WrongKey.to_string())
                }
                other => AddFailure::Other(other.to_string()),
            }),
        Err(text) => Err(AddFailure::Other(text)),
    };
    match result {
        Ok(applied) => {
            warn(context, &applied.warnings);
            reply(context, BackupReply::Added(Ok(draft.id)));
        }
        Err(failure) => reply(context, BackupReply::Added(Err(failure))),
    }
}

async fn enter_key(context: &Context, id: String, key: Key) {
    let result = match load_tools(context).await {
        Ok(tools) => lifecycle::enter_key(&tools.env(), &id, key)
            .await
            .map_err(|err| err.to_string()),
        Err(text) => Err(text),
    };
    let result = result.map(|applied| warn(context, &applied.warnings));
    reply(context, BackupReply::KeyEntered { id, result });
}

async fn remove(context: &Context, id: &str, delete_key: bool) {
    let result = match load_tools(context).await {
        Ok(tools) => lifecycle::remove_backup(&tools.env(), id, delete_key)
            .await
            .map_err(|err| err.to_string()),
        Err(text) => Err(text),
    };
    match result {
        Ok(applied) => warn(context, &applied.warnings),
        Err(text) => fail(context, text),
    }
}

async fn save_key_file(context: &Context, id: &str, key: Key) {
    let picked = rfd::AsyncFileDialog::new()
        .set_title("Save Key to File")
        .set_file_name(format!("{id}-backup-key.txt"))
        .save_file()
        .await;
    let Some(handle) = picked else {
        reply(context, BackupReply::KeyFileSaved(Ok(None)));
        return;
    };
    let path = handle.path().to_path_buf();
    let target = path.clone();
    let written = tokio::task::spawn_blocking(move || write_key_file(&target, &key))
        .await
        .map_err(|err| err.to_string())
        .and_then(|result| result.map_err(|err| format!("Cannot save the key: {err}")));
    let outcome = written.map(|()| Some(path));
    reply(context, BackupReply::KeyFileSaved(outcome));
}

fn write_key_file(path: &std::path::Path, key: &Key) -> std::io::Result<()> {
    let text = Zeroizing::new(key.display_groups());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(text.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_file_is_private_and_holds_the_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("key.txt");
        let key = Key::generate().expect("key");

        write_key_file(&path, &key).expect("written");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = std::fs::read_to_string(&path).expect("text");
        assert_eq!(Key::parse(&text).expect("parses"), key);
    }

    #[test]
    fn key_file_replaces_a_wider_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("key.txt");
        std::fs::write(&path, "old old old old old old old old old old").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
        let key = Key::generate().expect("key");

        write_key_file(&path, &key).expect("written");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = std::fs::read_to_string(&path).expect("text");
        assert_eq!(Key::parse(&text).expect("parses"), key);
    }
}
