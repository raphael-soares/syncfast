//! The review behind a held sync: what a resync or a forced round would
//! change, and the confirmation that lets it run. An agent may confirm a
//! review only when it shows no deletion (ADR 0007).

use std::time::Duration;

use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use super::views::{load_state, review_kind};
use super::{OpError, Ops, PairView};
use crate::behavior::SyncMode;
use crate::bisync::ReviewKind;
use crate::bisync_log::Change;
use crate::filters;
use crate::preview::{ChangeCounts, preview};
use crate::run::RunEnv;
use crate::state::{ConfirmMarker, GateKind};

/// Changes listed in the answer; the full list stays in `preview.json`.
const SHOWN_CHANGES: usize = 200;

static CLOCK: fn() -> Zoned = Zoned::now;

/// What the agent was shown, kept so that a confirmation can only refer to
/// something that was seen.
#[derive(Debug, Serialize, Deserialize)]
struct StoredReview {
    kind: ReviewKind,
    filters_hash: String,
    shown: Timestamp,
    changes: Vec<Change>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewView {
    pub sync_id: String,
    pub mode: SyncMode,
    /// `First sync`, `Filters changed`, `Sync mode changed`,
    /// `Sync stopped after an error`, `Many files would be deleted` or
    /// `Every file changed`.
    pub reason: &'static str,
    pub kind: ReviewKind,
    /// Pass this to `review confirm`.
    pub filters_hash: String,
    pub counts: ChangeCounts,
    pub deletes_files: bool,
    /// Whether `review confirm` will accept this review: it does not when
    /// the review deletes files.
    pub agent_can_confirm: bool,
    pub changes: Vec<Change>,
    /// Changes that are not in `changes`, because the list is capped.
    pub changes_omitted: usize,
    /// Folders left out of the sync because they hold a `.git`.
    pub excluded_repos: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfirmedReview {
    pub sync: PairView,
    pub confirmed: ReviewKind,
    /// The round that the confirmation released ended within the wait.
    pub finished: bool,
}

fn reason(gate: GateKind) -> &'static str {
    match gate {
        GateKind::NeedsReviewFirstRun => "First sync",
        GateKind::NeedsReviewFiltersChanged => "Filters changed",
        GateKind::NeedsReviewModeChanged => "Sync mode changed",
        GateKind::NeedsReviewCriticalError => "Sync stopped after an error",
        GateKind::TooManyDeletes => "Many files would be deleted",
        _ => "Every file changed",
    }
}

impl Ops {
    /// Dry-runs the review the sync waits for, keeps what it found in
    /// `pairs/<id>/preview.json` and returns it.
    ///
    /// # Errors
    /// No such sync, the sync waits for no review, a round is running
    /// ([`OpError::Busy`]), or the dry run fails.
    pub async fn show_review(&self, id: &str) -> Result<ReviewView, OpError> {
        let (_, behavior) = self.find_pair_and_behavior(id).await?;
        let state = load_state(self, id).await;
        let gate = state.gate.map(|gate| gate.kind);
        let Some((gate, kind)) = gate.and_then(|gate| review_kind(gate).map(|kind| (gate, kind)))
        else {
            return Err(OpError::Invalid(
                "this sync is not waiting for a review".to_owned(),
            ));
        };
        let env = RunEnv {
            paths: &self.paths,
            rclone: &self.rclone,
            network: self.network.as_ref(),
            notifier: self.notifier.as_ref(),
            clock: &CLOCK,
            forward_signals: false,
        };
        let found = preview(&env, id, kind).await?;
        let stored = StoredReview {
            kind,
            filters_hash: found.filters_hash,
            shown: Timestamp::now(),
            changes: found.summary.changes,
        };
        self.save_review(id, &stored).await?;
        let counts = ChangeCounts::of(&stored.changes);
        let deletes_files = counts.deletes();
        let omitted = stored.changes.len().saturating_sub(SHOWN_CHANGES);
        let note = if deletes_files {
            "This review deletes files. Confirm it in syncfast.".to_owned()
        } else {
            format!(
                "Nothing is deleted. Confirm with: review confirm {id} --filters-hash {}",
                stored.filters_hash
            )
        };
        let mut changes = stored.changes;
        changes.truncate(SHOWN_CHANGES);
        Ok(ReviewView {
            sync_id: id.to_owned(),
            mode: behavior.mode,
            reason: reason(gate),
            kind,
            filters_hash: stored.filters_hash,
            counts,
            deletes_files,
            agent_can_confirm: !deletes_files,
            changes,
            changes_omitted: omitted,
            excluded_repos: found.excluded_repos,
            note,
        })
    }

    async fn save_review(&self, id: &str, review: &StoredReview) -> Result<(), OpError> {
        let path = self.paths.preview_file(id);
        let mut json = serde_json::to_vec_pretty(review)
            .map_err(|err| OpError::Failed(format!("cannot encode the review: {err}")))?;
        json.push(b'\n');
        let target = path.clone();
        tokio::task::spawn_blocking(move || fastframe_file::write(&target, json))
            .await
            .unwrap_or_else(|join| Err(std::io::Error::other(join)))
            .map_err(|err| OpError::Failed(format!("cannot write {}: {err}", path.display())))
    }

    async fn stored_review(&self, id: &str) -> Result<StoredReview, OpError> {
        let not_shown = || {
            OpError::ReviewNotShown(
                "no review was shown for this sync; show the review first".to_owned(),
            )
        };
        let bytes = tokio::fs::read(self.paths.preview_file(id))
            .await
            .map_err(|_| not_shown())?;
        serde_json::from_slice(&bytes).map_err(|_| not_shown())
    }

    /// Confirms the review that [`Ops::show_review`] showed, when it
    /// deletes nothing, and starts the round it releases.
    ///
    /// Refused with [`OpError::DeletesNeedUser`] when the review deletes
    /// files, and with [`OpError::ReviewNotShown`] when `filters_hash` is
    /// not the one that was shown or the filters changed since. Nothing
    /// changes when it is refused.
    ///
    /// # Errors
    /// The above, no such sync, the sync waits for no review, or the round
    /// cannot be started.
    pub async fn confirm_review(
        &self,
        id: &str,
        filters_hash: &str,
        wait: Duration,
    ) -> Result<ConfirmedReview, OpError> {
        let pair = self.find_pair(id).await?;
        let state = load_state(self, id).await;
        let Some(kind) = state.gate.and_then(|gate| review_kind(gate.kind)) else {
            return Err(OpError::Invalid(
                "this sync is not waiting for a review".to_owned(),
            ));
        };
        let shown = self.stored_review(id).await?;
        if ChangeCounts::of(&shown.changes).deletes() {
            return Err(OpError::DeletesNeedUser);
        }
        if shown.filters_hash != filters_hash {
            return Err(OpError::ReviewNotShown(
                "the filters hash is not the one of the review that was shown; show the review again"
                    .to_owned(),
            ));
        }
        if shown.filters_hash != filters::hash(&filters::render_live(&pair).await.text) {
            return Err(OpError::ReviewNotShown(
                "the filters changed after the review was shown; show it again".to_owned(),
            ));
        }
        if shown.kind != kind {
            return Err(OpError::ReviewNotShown(
                "the sync waits for a different review than the one that was shown; show it again"
                    .to_owned(),
            ));
        }
        ConfirmMarker {
            kind,
            filters_hash: shown.filters_hash,
        }
        .save(&self.paths, id)
        .await?;
        if let Err(err) = tokio::fs::remove_file(self.paths.preview_file(id)).await {
            log::warn!("cannot delete the shown review of \"{id}\": {err}");
        }
        let finished = self.start_round(id, wait).await?;
        Ok(ConfirmedReview {
            sync: self.pair_view(id).await?,
            confirmed: kind,
            finished,
        })
    }
}
