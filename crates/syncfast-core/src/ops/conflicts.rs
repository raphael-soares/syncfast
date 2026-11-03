//! Conflicts: both versions of a file were kept as `<name>.conflict<N>`.

use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned};
use serde::Serialize;

use super::{OpError, Ops};
use crate::conflicts::{self, FileFacts, Resolution};
use crate::filters::{self, DirMatcher};

/// What to keep when both versions of a file exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Keep {
    /// Delete the conflicted copy.
    Current,
    /// The conflicted copy becomes the file; the current one goes to the
    /// versions folder.
    Conflicted,
    /// Keep the conflicted copy beside the file under a new name.
    Both,
}

impl From<Keep> for Resolution {
    fn from(keep: Keep) -> Self {
        match keep {
            Keep::Current => Self::KeepCurrent,
            Keep::Conflicted => Self::KeepConflicted,
            Keep::Both => Self::KeepBoth,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileView {
    pub size: u64,
    pub modified: Option<Timestamp>,
}

impl From<FileFacts> for FileView {
    fn from(facts: FileFacts) -> Self {
        Self {
            size: facts.size,
            modified: facts.modified,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictView {
    /// The `<name>.conflict<N>` file, relative to the sync's folder.
    pub path: PathBuf,
    /// The file it is a copy of, relative to the sync's folder.
    pub original: PathBuf,
    /// `None` when the original no longer exists.
    pub current: Option<FileView>,
    pub conflicted: FileView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolved {
    pub sync_id: String,
    pub path: PathBuf,
    pub keep: Keep,
    /// A round was started to sync the result.
    pub sync_started: bool,
    pub warning: Option<String>,
}

impl Ops {
    /// The conflicts in the sync's local folder. A folder that is not there
    /// has none.
    ///
    /// # Errors
    /// No such sync, or the folder cannot be listed.
    pub async fn conflicts(&self, id: &str) -> Result<Vec<ConflictView>, OpError> {
        let pair = self.find_pair(id).await?;
        let matcher = DirMatcher::new(&filters::render(&pair));
        let local = pair.local;
        let scanned = tokio::task::spawn_blocking(move || {
            conflicts::list_conflicts(&local, &matcher).map_err(|err| (local, err))
        })
        .await
        .map_err(|err| OpError::Failed(err.to_string()))?;
        match scanned {
            Ok(found) => Ok(found
                .into_iter()
                .map(|conflict| ConflictView {
                    path: conflict.path,
                    original: conflict.original,
                    current: conflict.current.map(FileView::from),
                    conflicted: conflict.conflicted.into(),
                })
                .collect()),
            Err((_, err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err((local, err)) => Err(OpError::Failed(format!(
                "cannot look for conflicts in {}: {err}",
                local.display()
            ))),
        }
    }

    /// Settles one conflict of the sync (`path` as `conflicts` lists it) and
    /// starts a round to sync the result.
    ///
    /// # Errors
    /// No such sync, `path` is not a conflict file of it, or a file cannot
    /// be moved or deleted.
    pub async fn resolve_conflict(
        &self,
        id: &str,
        path: &Path,
        keep: Keep,
    ) -> Result<Resolved, OpError> {
        let pair = self.find_pair(id).await?;
        let versions = self.paths.versions_dir(id);
        let conflict = path.to_owned();
        tokio::task::spawn_blocking(move || {
            conflicts::resolve(
                &pair.local,
                &conflict,
                keep.into(),
                &versions,
                &Zoned::now(),
            )
            .map_err(|err| OpError::Failed(format!("cannot settle {}: {err}", conflict.display())))
        })
        .await
        .map_err(|err| OpError::Failed(err.to_string()))??;
        let started = self.trigger.start(id).await;
        Ok(Resolved {
            sync_id: id.to_owned(),
            path: path.to_owned(),
            keep,
            sync_started: started.is_ok(),
            warning: started.err(),
        })
    }
}
