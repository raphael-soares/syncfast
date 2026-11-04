//! Starting rounds and waiting for them.

use std::time::Duration;

use serde::Serialize;
use tokio::time::Instant;

use super::{OpError, Ops, PairView};
use crate::bisync::{Mode, Outcome};
use crate::run::RunReport;
use crate::run_decision::SkipReason;
use crate::state::{GateKind, State, is_running};

const POLL: Duration = Duration::from_millis(200);
/// How long a started round may take to show a sign of life (its lock or a
/// change of its state) before it counts as one that had nothing to do.
const GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    pub sync: PairView,
    /// The round ended within the wait. `false` means it is still going:
    /// read `status` again later.
    pub finished: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncRounds {
    pub rounds: Vec<SyncReport>,
}

/// How a round ended, for `syncfast run --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoundOutcome {
    /// `busy`, `config_invalid`, `skipped`, `gate_set` or `ran`.
    pub result: &'static str,
    /// Why a round was skipped: `paused`, `offline` or `gated`.
    pub skipped: Option<&'static str>,
    /// The gate a round set.
    pub gate: Option<&'static str>,
    /// `normal`, `resync` or `force` for a round that ran.
    pub mode: Option<&'static str>,
    /// How a round that ran ended.
    pub outcome: Option<&'static str>,
}

impl From<RunReport> for RoundOutcome {
    fn from(report: RunReport) -> Self {
        let mut out = Self {
            result: "",
            skipped: None,
            gate: None,
            mode: None,
            outcome: None,
        };
        match report {
            RunReport::Busy => out.result = "busy",
            RunReport::ConfigInvalid => out.result = "config_invalid",
            RunReport::Skipped(reason) => {
                out.result = "skipped";
                out.skipped = Some(match reason {
                    SkipReason::Paused => "paused",
                    SkipReason::Offline => "offline",
                    SkipReason::Gated => "gated",
                });
            }
            RunReport::GateSet(kind) => {
                out.result = "gate_set";
                out.gate = Some(kind.as_str());
            }
            RunReport::Ran { mode, outcome } => {
                out.result = "ran";
                out.mode = Some(match mode {
                    Mode::Normal => "normal",
                    Mode::Resync => "resync",
                    Mode::Force => "force",
                    Mode::DryRun(_) => "dry_run",
                });
                out.outcome = Some(match outcome {
                    Outcome::Ok => "ok",
                    Outcome::NeedsReview(_) => "needs_review",
                    Outcome::TooManyDeletes => "too_many_deletes",
                    Outcome::AllFilesChanged => "all_files_changed",
                    Outcome::AuthError => "auth_error",
                    Outcome::NeedsAttention { .. } => "needs_attention",
                    Outcome::ConfigError => "config_error",
                    Outcome::Transient => "transient_error",
                });
            }
        }
        out
    }
}

/// What a round changes in the state of a sync. Not the state itself: a sync
/// that has never run has no file, and every read of it makes up a new
/// `since` for its first-run gate.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Signs {
    attempt: Option<jiff::Timestamp>,
    success: Option<jiff::Timestamp>,
    finished: Option<jiff::Timestamp>,
    gate: Option<GateKind>,
    offline: bool,
    failures: u32,
}

impl Signs {
    fn of(state: &State) -> Self {
        Self {
            attempt: state.last_attempt,
            success: state.last_success,
            finished: state.last_run.as_ref().map(|last| last.finished),
            gate: state.gate.as_ref().map(|gate| gate.kind),
            offline: state.offline,
            failures: state.consecutive_failures,
        }
    }

    async fn read(ops: &Ops, id: &str) -> Option<Self> {
        State::load(&ops.paths, id)
            .await
            .ok()
            .as_ref()
            .map(Self::of)
    }
}

impl Ops {
    /// Starts a round of the sync `id`, or of every enabled sync when `id`
    /// is `None`, and waits up to `wait` for them to end. The rounds run
    /// on their own (systemd, or a child process), so they go on when the
    /// wait is over or the caller goes away.
    ///
    /// # Errors
    /// No such sync, or the round cannot be started.
    pub async fn sync_now(&self, id: Option<&str>, wait: Duration) -> Result<SyncRounds, OpError> {
        let ids: Vec<String> = match id {
            Some(id) => {
                self.find_pair(id).await?;
                vec![id.to_owned()]
            }
            None => self
                .load_config()
                .await?
                .pairs()
                .iter()
                .filter(|pair| pair.enabled)
                .map(|pair| pair.id.clone())
                .collect(),
        };
        let deadline = Instant::now() + wait;
        let mut started = Vec::with_capacity(ids.len());
        for id in &ids {
            started.push(self.trigger_round(id).await?);
        }
        let mut rounds = Vec::with_capacity(ids.len());
        for (id, before) in ids.iter().zip(started) {
            let finished = self.await_round(id, before.as_ref(), deadline).await;
            rounds.push(SyncReport {
                sync: self.pair_view(id).await?,
                finished,
            });
        }
        Ok(SyncRounds { rounds })
    }

    /// [`Ops::trigger_round`] and [`Ops::await_round`] for one sync;
    /// whether the round ended in time.
    pub(crate) async fn start_round(&self, id: &str, wait: Duration) -> Result<bool, OpError> {
        let deadline = Instant::now() + wait;
        let before = self.trigger_round(id).await?;
        Ok(self.await_round(id, before.as_ref(), deadline).await)
    }

    /// Starts the round and returns the state it started from.
    async fn trigger_round(&self, id: &str) -> Result<Option<Signs>, OpError> {
        let before = Signs::read(self, id).await;
        self.trigger.start(id).await.map_err(OpError::Failed)?;
        Ok(before)
    }

    /// Waits until the round that started from `before` has ended or
    /// `deadline` passes. A round shows itself by taking its lock or by
    /// changing the state; one that does neither within [`GRACE`] had
    /// nothing to do (paused, held for review).
    async fn await_round(&self, id: &str, before: Option<&Signs>, deadline: Instant) -> bool {
        if Instant::now() >= deadline {
            return false;
        }
        let grace = (Instant::now() + GRACE).min(deadline);
        let mut seen_running = false;
        loop {
            let running = is_running(&self.paths, id);
            seen_running |= running;
            if !running {
                let changed = Signs::read(self, id).await.as_ref() != before;
                if seen_running || changed || Instant::now() >= grace {
                    return true;
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }
}
