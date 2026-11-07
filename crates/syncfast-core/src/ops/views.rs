//! The shapes the operations return: what a sync looks like to a reader
//! that is not the GUI. Nothing here holds a secret.

use std::path::PathBuf;

use jiff::Timestamp;
use serde::Serialize;

use super::{OpError, Ops};
use crate::behavior::{Behavior, SyncMode};
use crate::bisync::ReviewKind;
use crate::config::Pair;
use crate::filters;
use crate::notify::{attention_body, too_many_deletes_text};
use crate::settings::lowered_safety;
use crate::state::{Gate, GateKind, LastOutcome, State, display_status, is_running};
use crate::watch::{WATCH_LIMIT_NOTICE, limited_pairs};

/// A sync and where it stands. `status` is the same name `syncfast status`
/// prints and the GUI shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairView {
    pub id: String,
    pub name: String,
    pub local: PathBuf,
    pub remote: String,
    pub enabled: bool,
    pub presets: Vec<String>,
    /// The custom filter rules, one `+ ` or `- ` line each.
    pub filters: Vec<String>,
    pub status: &'static str,
    /// What the GUI says for `status`.
    pub status_text: String,
    pub running: bool,
    pub gate: Option<GateView>,
    /// Which review the gate waits for, when it waits for one: what
    /// `review show` previews.
    pub review: Option<ReviewKind>,
    pub last_success: Option<Timestamp>,
    pub last_attempt: Option<Timestamp>,
    pub last_outcome: Option<LastOutcome>,
    pub consecutive_failures: u32,
    pub conflicts_pending: u32,
    pub mode: SyncMode,
    pub offline: bool,
    /// Something to fix that does not block rounds: the watcher ran out of
    /// inotify watches, so changes wait for the timer.
    pub notice: Option<&'static str>,
    /// Why the config rejects this sync, when it does.
    pub config_error: Option<String>,
    /// The settings of this sync that are less safe than the built-in
    /// default, by key.
    pub lowered_safety: Vec<&'static str>,
}

/// Why normal rounds of a sync are blocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateView {
    pub kind: GateKind,
    pub since: Timestamp,
    pub detail: String,
    /// What to do about it, in a sentence.
    pub explanation: String,
}

/// The counters of the last round, without the per-file list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LastRunView {
    pub started: Timestamp,
    pub finished: Timestamp,
    pub exit_code: i32,
    pub uploaded: Option<u32>,
    pub downloaded: Option<u32>,
    pub deleted_local: Option<u32>,
    pub deleted_remote: Option<u32>,
    pub conflicts_new: Option<u32>,
    pub log_file: PathBuf,
}

/// [`PairView`] plus what only one sync's page needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairDetail {
    #[serde(flatten)]
    pub sync: PairView,
    /// Hash of the filters as they are now; a review is only valid for the
    /// hash it was shown with.
    pub filters_hash: String,
    pub versions_dir: PathBuf,
    pub last_run: Option<LastRunView>,
}

/// The review a gate waits for, if any.
pub(crate) fn review_kind(gate: GateKind) -> Option<ReviewKind> {
    if gate.is_needs_review() {
        Some(ReviewKind::Resync)
    } else if gate.is_force_review() {
        Some(ReviewKind::Force)
    } else {
        None
    }
}

fn explanation(gate: &Gate, behavior: &Behavior) -> String {
    match gate.kind {
        GateKind::NeedsReviewFirstRun => {
            "First sync: review what will change before syncing starts.".to_owned()
        }
        GateKind::NeedsReviewFiltersChanged => {
            "Filters changed: review what will sync.".to_owned()
        }
        GateKind::NeedsReviewModeChanged => {
            "Sync mode changed: review what will change before syncing resumes.".to_owned()
        }
        GateKind::NeedsReviewCriticalError => {
            "Sync stopped after an error: review to resume.".to_owned()
        }
        GateKind::TooManyDeletes if behavior.mode.is_mirror() => format!(
            "{} Review before continuing.",
            too_many_deletes_text(behavior, &gate.detail)
        ),
        GateKind::TooManyDeletes => format!(
            "More than {}% of the files would be deleted: review before continuing.",
            behavior.max_delete
        ),
        GateKind::AllFilesChanged => {
            "Every file changed at once: review before continuing.".to_owned()
        }
        GateKind::ConfigError => gate.detail.clone(),
        GateKind::AuthError => {
            "The account is disconnected: the user must reconnect it in syncfast (Settings > Accounts)."
                .to_owned()
        }
        GateKind::LocalMissing => {
            "The local folder is missing: syncing resumes when it is back.".to_owned()
        }
        GateKind::NeedsAttention => format!(
            "Needs attention: {}",
            attention_body(gate.fault, &gate.detail)
        ),
    }
}

impl GateView {
    fn of(gate: &Gate, behavior: &Behavior) -> Self {
        Self {
            kind: gate.kind,
            since: gate.since,
            detail: gate.detail.clone(),
            explanation: explanation(gate, behavior),
        }
    }
}

impl PairView {
    pub(crate) fn of(
        pair: &Pair,
        behavior: &Behavior,
        state: &State,
        running: bool,
        config_error: Option<String>,
        realtime_off: bool,
    ) -> Self {
        let status = display_status(pair, state, running, behavior.notifications.failing_after());
        Self {
            id: pair.id.clone(),
            name: pair.name.clone(),
            local: pair.local.clone(),
            remote: pair.remote.clone(),
            enabled: pair.enabled,
            presets: pair.presets.clone(),
            filters: pair.filters.clone(),
            status: status.name(),
            status_text: status.text(),
            running,
            gate: state.gate.as_ref().map(|gate| GateView::of(gate, behavior)),
            review: state.gate.as_ref().and_then(|gate| review_kind(gate.kind)),
            last_success: state.last_success,
            last_attempt: state.last_attempt,
            last_outcome: state.last_outcome,
            consecutive_failures: state.consecutive_failures,
            conflicts_pending: state.conflicts_pending,
            mode: behavior.mode,
            offline: state.offline,
            notice: realtime_off.then_some(WATCH_LIMIT_NOTICE),
            config_error,
            lowered_safety: lowered_safety(behavior),
        }
    }
}

impl LastRunView {
    fn of(state: &State) -> Option<Self> {
        let last = state.last_run.as_ref()?;
        Some(Self {
            started: last.started,
            finished: last.finished,
            exit_code: last.exit_code,
            uploaded: last.summary.uploaded,
            downloaded: last.summary.downloaded,
            deleted_local: last.summary.deleted_local,
            deleted_remote: last.summary.deleted_remote,
            conflicts_new: last.summary.conflicts_new,
            log_file: last.log_file.clone(),
        })
    }
}

/// A state file that cannot be read shows as a configuration error rather
/// than as a healthy sync, as in the GUI.
pub(crate) async fn load_state(ops: &Ops, id: &str) -> State {
    State::load_or_config_error(&ops.paths, id).await
}

impl Ops {
    /// Every sync in the config with its status, valid or not.
    ///
    /// # Errors
    /// The config has a global problem, or rclone cannot list the accounts.
    pub async fn status(&self) -> Result<Vec<PairView>, OpError> {
        let config = self.load_config().await?;
        let accounts = self.rclone.listremotes().await?;
        let validation = config.validate(&self.paths, &accounts);
        let limited = limited_pairs(&self.paths);
        let defaults = config.defaults();
        let mut views = Vec::with_capacity(config.pairs().len());
        for pair in config.pairs() {
            let problem = validation
                .errors
                .iter()
                .find(|error| error.pair_id == pair.id)
                .map(|error| error.message.clone());
            let realtime_off = limited.contains(&pair.id);
            let behavior = Behavior::for_pair(&defaults, pair);
            views.push(self.view_of(pair, &behavior, problem, realtime_off).await);
        }
        Ok(views)
    }

    async fn view_of(
        &self,
        pair: &Pair,
        behavior: &Behavior,
        config_error: Option<String>,
        realtime_off: bool,
    ) -> PairView {
        let state = load_state(self, &pair.id).await;
        let running = is_running(&self.paths, &pair.id);
        PairView::of(pair, behavior, &state, running, config_error, realtime_off)
    }

    /// One sync, as `status` shows it, read again from disk.
    pub(crate) async fn pair_view(&self, id: &str) -> Result<PairView, OpError> {
        let found = self.status().await?.into_iter().find(|view| view.id == id);
        found.ok_or_else(|| OpError::NotFound(id.to_owned()))
    }

    /// One sync with its filters hash and the counters of its last round.
    ///
    /// # Errors
    /// No such sync, or what [`Ops::status`] reports.
    pub async fn show(&self, id: &str) -> Result<PairDetail, OpError> {
        let sync = self.pair_view(id).await?;
        let pair = self.find_pair(id).await?;
        let state = load_state(self, id).await;
        Ok(PairDetail {
            sync,
            filters_hash: filters::hash(&filters::render_live(&pair).await.text),
            versions_dir: self.paths.versions_dir(id),
            last_run: LastRunView::of(&state),
        })
    }
}
