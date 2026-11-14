//! Recently deleted: what a sync deleted or replaced that can still come back.

use serde::Serialize;

use super::{OpError, Ops};
use crate::recently_deleted::{self, DeletedError, DeletedList, DeletedSide};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Restored {
    pub sync_id: String,
    /// Where the file was written, relative to the sync's folder.
    pub path: String,
    /// A round was started to carry it to the other side.
    pub sync_started: bool,
    pub warning: Option<String>,
}

impl Ops {
    /// What the sync deleted or replaced that can still be restored.
    ///
    /// # Errors
    /// No such sync, or its versions folder cannot be read.
    pub async fn recently_deleted(&self, id: &str) -> Result<DeletedList, OpError> {
        let (pair, behavior) = self.find_pair_and_behavior(id).await?;
        recently_deleted::list(&self.paths, &self.rclone, &pair, &behavior)
            .await
            .map_err(|err| failed(id, err))
    }

    /// Restores one item (`key` as `recently_deleted` lists it) into the
    /// sync's folder, never over a file, and starts a round.
    ///
    /// # Errors
    /// No such sync or item, or the copy failed.
    pub async fn restore_deleted(
        &self,
        id: &str,
        key: &str,
        side: DeletedSide,
    ) -> Result<Restored, OpError> {
        let pair = self.find_pair(id).await?;
        let path = recently_deleted::restore(&self.paths, &self.rclone, &pair, key, side)
            .await
            .map_err(|err| failed(id, err))?;
        let started = self.trigger.start(id).await;
        Ok(Restored {
            sync_id: id.to_owned(),
            path,
            sync_started: started.is_ok(),
            warning: started.err(),
        })
    }
}

fn failed(id: &str, err: DeletedError) -> OpError {
    match err {
        DeletedError::NotFound(_) => OpError::Invalid(err.to_string()),
        other => OpError::Failed(format!("{id}: {other}")),
    }
}
