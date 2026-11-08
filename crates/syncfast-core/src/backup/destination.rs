//! Where a backup's repository lives, and creating or opening it.

use std::path::{Path, PathBuf};

use crate::config::split_remote;
use crate::rclone::{Rclone, RcloneError};
use crate::restic::{InitResult, Repository, Restic, ResticError};

use super::key::Key;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DestinationError {
    #[error("backup destination must be an account folder (account:path) or an absolute path")]
    Form,
    #[error("backup destination needs a folder inside the account")]
    NeedsFolder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// A folder of an rclone account, `gdrive:Backups/laptop`.
    Account {
        name: String,
        path: String,
    },
    Local(PathBuf),
}

impl Destination {
    /// An absolute path is local; otherwise `account:folder` with a name
    /// and at least one folder name. A `~` has been expanded by now.
    ///
    /// # Errors
    /// [`DestinationError::Form`], or [`DestinationError::NeedsFolder`] for
    /// an account with no folder (the root of an account is not a place for
    /// a repository).
    pub fn parse(text: &str) -> Result<Self, DestinationError> {
        if text.starts_with('/') {
            return Ok(Self::Local(PathBuf::from(text)));
        }
        let Some((name, path)) = split_remote(text) else {
            return Err(DestinationError::Form);
        };
        if path.split('/').all(str::is_empty) {
            return Err(DestinationError::NeedsFolder);
        }
        Ok(Self::Account {
            name: name.to_owned(),
            path: path.to_owned(),
        })
    }

    /// The `-r` value restic takes.
    #[must_use]
    pub fn repo_arg(&self) -> String {
        match self {
            Self::Account { name, path } => format!("rclone:{name}:{path}"),
            Self::Local(path) => path.to_string_lossy().into_owned(),
        }
    }

    /// `account:folder` or the path, as rclone takes it.
    #[must_use]
    pub fn rclone_path(&self) -> String {
        match self {
            Self::Account { name, path } => format!("{name}:{path}"),
            Self::Local(path) => path.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("The destination folder is not empty.")]
    NotEmpty,
    #[error(transparent)]
    Restic(#[from] ResticError),
    #[error("Cannot look at the destination folder: {0}")]
    Rclone(#[from] RcloneError),
    #[error("Cannot look at the destination folder: {0}")]
    Io(#[source] std::io::Error),
}

/// Whether `destination` holds nothing; a folder that does not exist
/// counts as empty.
///
/// # Errors
/// The folder cannot be looked at.
pub async fn is_empty(rclone: &Rclone, destination: &Destination) -> Result<bool, RepositoryError> {
    match destination {
        Destination::Account { .. } => Ok(!rclone.has_entries(&destination.rclone_path()).await?),
        Destination::Local(path) => local_is_empty(path).await,
    }
}

async fn local_is_empty(path: &Path) -> Result<bool, RepositoryError> {
    match tokio::fs::read_dir(path).await {
        Ok(mut entries) => Ok(entries
            .next_entry()
            .await
            .map_err(RepositoryError::Io)?
            .is_none()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(err) => Err(RepositoryError::Io(err)),
    }
}

/// Makes a new repository at `repository`. The folder must be empty or
/// absent: a repository is never started on top of other files.
///
/// # Errors
/// [`RepositoryError::NotEmpty`], or the folder cannot be looked at or
/// `restic init` fails.
pub async fn create_repository(
    restic: &Restic,
    repository: &Repository,
    key: &Key,
) -> Result<InitResult, RepositoryError> {
    if !is_empty(restic.rclone(), &repository.destination).await? {
        return Err(RepositoryError::NotEmpty);
    }
    Ok(restic.init(repository, key).await?)
}

/// Checks that `repository` is a repository `key` opens.
///
/// # Errors
/// [`ResticError::NoRepository`] (`No backup was found in that folder.`),
/// [`ResticError::WrongKey`] (`That key does not open this backup.`) or
/// any other failure of `restic cat config`.
pub async fn open_repository(
    restic: &Restic,
    repository: &Repository,
    key: &Key,
) -> Result<(), ResticError> {
    restic.verify(repository, key).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_path_is_local() {
        assert_eq!(
            Destination::parse("/mnt/usb/backups").unwrap(),
            Destination::Local("/mnt/usb/backups".into())
        );
    }

    #[test]
    fn an_account_folder_is_an_account_destination() {
        assert_eq!(
            Destination::parse("gdrive:Backups/laptop").unwrap(),
            Destination::Account {
                name: "gdrive".into(),
                path: "Backups/laptop".into()
            }
        );
    }

    #[test]
    fn the_account_root_needs_a_folder() {
        for text in ["gdrive:", "gdrive:/", "gdrive://"] {
            assert_eq!(
                Destination::parse(text),
                Err(DestinationError::NeedsFolder),
                "{text}"
            );
        }
    }

    #[test]
    fn anything_else_is_the_form_error() {
        for text in ["", "relative/path", ":folder", "plain", "~/x"] {
            assert_eq!(
                Destination::parse(text),
                Err(DestinationError::Form),
                "{text}"
            );
        }
    }

    #[test]
    fn the_error_texts_are_the_ones_the_spec_table_gives() {
        assert_eq!(
            DestinationError::Form.to_string(),
            "backup destination must be an account folder (account:path) or an absolute path"
        );
        assert_eq!(
            DestinationError::NeedsFolder.to_string(),
            "backup destination needs a folder inside the account"
        );
    }

    #[test]
    fn repo_arg_goes_through_rclone_for_an_account_and_is_the_path_for_a_local_folder() {
        let account = Destination::parse("gdrive:Backups/laptop").unwrap();
        let local = Destination::parse("/mnt/usb").unwrap();

        assert_eq!(account.repo_arg(), "rclone:gdrive:Backups/laptop");
        assert_eq!(local.repo_arg(), "/mnt/usb");
        assert_eq!(account.rclone_path(), "gdrive:Backups/laptop");
        assert_eq!(local.rclone_path(), "/mnt/usb");
    }

    #[tokio::test]
    async fn a_missing_or_empty_local_folder_is_empty_and_one_with_a_file_is_not() {
        let dir = tempfile::tempdir().unwrap();

        assert!(local_is_empty(&dir.path().join("missing")).await.unwrap());
        assert!(local_is_empty(dir.path()).await.unwrap());
        std::fs::write(dir.path().join("a"), "x").unwrap();
        assert!(!local_is_empty(dir.path()).await.unwrap());
    }

    #[tokio::test]
    async fn a_local_destination_that_is_a_file_is_an_error_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a");
        std::fs::write(&file, "x").unwrap();

        assert!(local_is_empty(&file).await.is_err());
    }
}
