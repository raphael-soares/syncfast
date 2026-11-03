//! The rounds a sync has run and what each one logged.

use jiff::Timestamp;
use serde::Serialize;

use super::{OpError, Ops};
use crate::runs::{self, RunEntry};
use crate::state::is_running;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoundView {
    /// The round's log name; pass it to `log`.
    pub round: String,
    pub started: Option<Timestamp>,
    /// `None` while the round runs.
    pub seconds: Option<u64>,
    pub status: &'static str,
    pub status_text: String,
    pub uploaded: Option<u32>,
    pub downloaded: Option<u32>,
    pub deleted_local: Option<u32>,
    pub deleted_remote: Option<u32>,
    pub conflicts_new: Option<u32>,
    /// The newest error line of a round that failed.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoundLog {
    pub sync_id: String,
    pub round: String,
    /// The last messages of the round's log, oldest first.
    pub messages: Vec<String>,
}

impl From<&RunEntry> for RoundView {
    fn from(entry: &RunEntry) -> Self {
        Self {
            round: entry.id.clone(),
            started: entry.started.as_ref().map(jiff::Zoned::timestamp),
            seconds: entry.seconds,
            status: entry.status.name(),
            status_text: entry.status.text(),
            uploaded: entry.summary.uploaded,
            downloaded: entry.summary.downloaded,
            deleted_local: entry.summary.deleted_local,
            deleted_remote: entry.summary.deleted_remote,
            conflicts_new: entry.summary.conflicts_new,
            detail: entry.detail.clone(),
        }
    }
}

impl Ops {
    async fn rounds(&self, id: &str) -> Result<Vec<RunEntry>, OpError> {
        self.find_pair(id).await?;
        let running = is_running(&self.paths, id);
        Ok(runs::load(&self.paths, id, running).await.to_vec())
    }

    /// The most recent rounds of a sync, newest first.
    ///
    /// # Errors
    /// No such sync.
    pub async fn activity(&self, id: &str) -> Result<Vec<RoundView>, OpError> {
        Ok(self.rounds(id).await?.iter().map(RoundView::from).collect())
    }

    /// What a round logged: the last messages of its log.
    ///
    /// # Errors
    /// No such sync, or no such round among its recent ones.
    pub async fn round_log(&self, id: &str, round: &str) -> Result<RoundLog, OpError> {
        let found = self
            .rounds(id)
            .await?
            .into_iter()
            .find(|entry| entry.id == round);
        let entry = found.ok_or_else(|| {
            OpError::Invalid(format!(
                "\"{id}\" has no recent round named \"{round}\"; list its activity first"
            ))
        })?;
        Ok(RoundLog {
            sync_id: id.to_owned(),
            round: entry.id,
            messages: entry.messages,
        })
    }
}
