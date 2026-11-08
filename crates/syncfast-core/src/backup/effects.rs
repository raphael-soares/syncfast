//! What a backup round writes into `state.json`, and what it announces
//! (spec phase 3, steps 6 and 10). Pure: the caller saves the state first
//! and sends the events after, so whoever is told can already read why.

use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::bisync::Fault;
use crate::state::{Gate, GateKind, LastOutcome, NOTIFIED_FAILURES};

use super::check::CheckOutcome;
use super::classify::BackupOutcome;
use super::config::Every;
use super::run_decision::SkipReason;
use super::state::{BackupState, BackupSummary, CheckKind, LastCheck, LastRun, LastSnapshot};
use super::{
    AFTER_FAILURE_CHECK, CHECK_INTERVAL, PRUNE_FAILURES_BEFORE_GATE, STALE_FACTOR, STALE_MIN,
};

/// Key in [`BackupState::notified`] for the "save the key" notification.
pub const NOTIFIED_KEY_UNSAVED: &str = "key_unsaved";

/// What a backup says when restic still reports a lock after `unlock`: the
/// lock is alive, so it belongs to a process that is still working.
pub const LOCKED_DETAIL: &str = "The backup is in use by another process. It will try again.";
const RETENTION_PREFIX: &str = "Old snapshots could not be cleaned up: ";
const GATE_PREFIX: &str = "gate:";

/// Something a person is told about, one per transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupEvent {
    Gate {
        kind: GateKind,
        detail: String,
        fault: Option<Fault>,
    },
    Failing {
        failures: u32,
        message: String,
    },
    Success {
        changed_files: u64,
        bytes_added: u64,
    },
    KeyUnsaved,
}

/// What a finished run knows besides its outcome.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub started: Timestamp,
    pub finished: Timestamp,
    /// Where `<date>` in a message is read.
    pub zone: TimeZone,
    pub every: Every,
    /// `[notifications] failures_after`; 0 turns the notification off.
    pub failures_after: u8,
    pub exit_code: Option<i32>,
    pub summary: Option<BackupSummary>,
    pub log_file: PathBuf,
}

/// What a skipped round knows.
#[derive(Debug, Clone)]
pub struct SkipContext<'a> {
    pub now: Timestamp,
    pub zone: TimeZone,
    pub every: Every,
    /// The destination as the person wrote it, for messages.
    pub dest: &'a str,
}

/// How long a backup may go without a snapshot, or a destination without
/// being there, before it asks for attention.
#[must_use]
pub fn stale_limit(every: Every) -> Duration {
    let period = match every {
        Every::Hourly => Duration::from_hours(1),
        Every::Daily => Duration::from_hours(24),
        Every::Weekly => Duration::from_hours(7 * 24),
    };
    (period * STALE_FACTOR).max(STALE_MIN)
}

/// The key a gate is announced under in [`BackupState::notified`]: a
/// `needs_attention` gate with a fault announces once per fault.
#[must_use]
pub fn gate_key(kind: GateKind, fault: Option<Fault>) -> String {
    match (kind, fault) {
        (GateKind::NeedsAttention, Some(fault)) => {
            format!("{}:{}", kind.notified_key(), fault_name(fault))
        }
        _ => kind.notified_key(),
    }
}

fn fault_name(fault: Fault) -> String {
    serde_json::to_value(fault)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn announce(state: &mut BackupState, key: String) -> bool {
    if state.notified.contains(&key) {
        return false;
    }
    state.notified.push(key);
    true
}

fn older_than(since: Timestamp, now: Timestamp, limit: Duration) -> bool {
    SignedDuration::try_from(limit).is_ok_and(|limit| now.duration_since(since) > limit)
}

fn date_text(at: Timestamp, zone: &TimeZone) -> String {
    zone.to_datetime(at).date().to_string()
}

fn record_failure(state: &mut BackupState, now: Timestamp) {
    state.last_outcome = Some(LastOutcome::Error);
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    state.failing_since.get_or_insert(now);
}

/// Sets the gate and says whether it is a transition worth announcing. The
/// gate keeps its `since` while the same kind and fault stay on it.
pub fn apply_gate(
    state: &mut BackupState,
    now: Timestamp,
    kind: GateKind,
    detail: String,
    fault: Option<Fault>,
) -> Vec<BackupEvent> {
    let since = state
        .gate
        .as_ref()
        .filter(|gate| gate.kind == kind && gate.fault == fault)
        .map_or(now, |gate| gate.since);
    state.gate = Some(Gate {
        kind,
        since,
        detail: detail.clone(),
        fault,
        auto_checked: None,
    });
    if announce(state, gate_key(kind, fault)) {
        vec![BackupEvent::Gate {
            kind,
            detail,
            fault,
        }]
    } else {
        Vec::new()
    }
}

/// Removes the gate and every notification it earned, so a gate that comes
/// back is announced again.
pub fn clear_gate(state: &mut BackupState) {
    state.gate = None;
    state.notified.retain(|key| !key.starts_with(GATE_PREFIX));
}

/// Step 6, item 6: a run starts. Whatever kept the last attempt from
/// reaching the destination no longer holds.
pub fn begin_attempt(state: &mut BackupState, now: Timestamp) {
    state.last_attempt = Some(now);
    state.offline = false;
    state.destination_unavailable = false;
    state.unavailable_since = None;
}

/// Step 6, item 5: a round that did not run.
pub fn apply_skip(
    state: &mut BackupState,
    reason: SkipReason,
    ctx: &SkipContext<'_>,
) -> Vec<BackupEvent> {
    match reason {
        SkipReason::Paused | SkipReason::Gated => Vec::new(),
        SkipReason::Offline => {
            state.offline = true;
            Vec::new()
        }
        SkipReason::DestinationUnavailable => {
            state.destination_unavailable = true;
            let since = *state.unavailable_since.get_or_insert(ctx.now);
            if older_than(since, ctx.now, stale_limit(ctx.every)) {
                apply_gate(
                    state,
                    ctx.now,
                    GateKind::NeedsAttention,
                    format!(
                        "{} has not been available since {}",
                        ctx.dest,
                        date_text(since, &ctx.zone)
                    ),
                    Some(Fault::DestinationMissing),
                )
            } else {
                Vec::new()
            }
        }
    }
}

/// Step 6, item 6: the key cannot be had. It counts as a failure. A damaged
/// repository keeps its gate, which only a full check clears.
pub fn apply_key_failure(
    state: &mut BackupState,
    detail: String,
    now: Timestamp,
) -> Vec<BackupEvent> {
    record_failure(state, now);
    if is_damaged_gate(state) {
        return Vec::new();
    }
    apply_gate(
        state,
        now,
        GateKind::NeedsAttention,
        detail,
        Some(Fault::KeyMissing),
    )
}

/// The configuration of the backup is invalid. A damaged repository keeps
/// its gate, which only a full check clears; the caller logs the message.
pub fn apply_config_error(
    state: &mut BackupState,
    now: Timestamp,
    message: String,
) -> Vec<BackupEvent> {
    if is_damaged_gate(state) {
        return Vec::new();
    }
    apply_gate(state, now, GateKind::ConfigError, message, None)
}

/// Step 6, item 9.
pub fn apply_outcome(
    state: &mut BackupState,
    outcome: &BackupOutcome,
    ctx: &RunContext,
) -> Vec<BackupEvent> {
    let now = ctx.finished;
    match outcome {
        BackupOutcome::Ok => apply_success(state, 0, ctx),
        BackupOutcome::OkWithWarnings { warnings } => apply_success(state, *warnings, ctx),
        BackupOutcome::Cancelled => {
            state.last_attempt = Some(ctx.started);
            Vec::new()
        }
        BackupOutcome::AuthError => {
            apply_gate(state, now, GateKind::AuthError, String::new(), None)
        }
        BackupOutcome::NeedsAttention { fault, detail } => {
            record_failure(state, now);
            apply_gate(
                state,
                now,
                GateKind::NeedsAttention,
                detail.clone(),
                Some(*fault),
            )
        }
        BackupOutcome::Transient { detail } => apply_transient(state, detail, ctx),
        BackupOutcome::Locked => apply_transient(state, LOCKED_DETAIL, ctx),
    }
}

fn apply_success(state: &mut BackupState, warnings: u64, ctx: &RunContext) -> Vec<BackupEvent> {
    let now = ctx.finished;
    let mut summary = ctx.summary.clone().unwrap_or_default();
    summary.warnings = warnings;
    if !is_retention_gate(state) {
        clear_gate(state);
    }
    state.notified.retain(|key| key != NOTIFIED_FAILURES);
    state.last_outcome = Some(LastOutcome::Ok);
    state.consecutive_failures = 0;
    state.failing_since = None;
    state.last_success = Some(now);
    state.last_snapshot = Some(LastSnapshot {
        id: summary.snapshot_id.clone(),
        time: now,
    });
    state.check_after_failure_done = false;
    if state.next_check.is_none() {
        state.next_check = now.checked_add(CHECK_INTERVAL).ok();
    }
    let changed_files = summary.files_new.saturating_add(summary.files_changed);
    let bytes_added = summary.data_added;
    state.last_run = Some(LastRun {
        started: ctx.started,
        finished: now,
        exit_code: ctx.exit_code.unwrap_or_default(),
        summary,
        log_file: ctx.log_file.clone(),
    });
    if changed_files > 0 {
        vec![BackupEvent::Success {
            changed_files,
            bytes_added,
        }]
    } else {
        Vec::new()
    }
}

fn apply_transient(state: &mut BackupState, detail: &str, ctx: &RunContext) -> Vec<BackupEvent> {
    let now = ctx.finished;
    let mut events = Vec::new();
    record_failure(state, now);
    let threshold = u32::from(ctx.failures_after);
    if threshold > 0
        && state.consecutive_failures >= threshold
        && announce(state, NOTIFIED_FAILURES.to_owned())
    {
        events.push(BackupEvent::Failing {
            failures: state.consecutive_failures,
            message: detail.to_owned(),
        });
    }
    let stale_since = state
        .failing_since
        .filter(|since| older_than(*since, now, stale_limit(ctx.every)));
    let free_to_set = state
        .gate
        .as_ref()
        .is_none_or(|gate| gate.kind == GateKind::NeedsAttention && gate.fault.is_none());
    if let Some(since) = stale_since
        && free_to_set
    {
        events.extend(apply_gate(
            state,
            now,
            GateKind::NeedsAttention,
            format!(
                "Backups have failed since {}: {detail}",
                date_text(since, &ctx.zone)
            ),
            None,
        ));
    }
    events
}

/// The check a successful backup earns when the monthly one is due.
#[must_use]
pub fn monthly_check(state: &BackupState, now: Timestamp) -> Option<CheckKind> {
    state
        .next_check
        .is_some_and(|due| due <= now)
        .then_some(CheckKind::Sample)
}

/// The check that follows a backup that failed for no known cause, once per
/// series of failures. Marks it done.
pub fn take_post_failure_check(
    state: &mut BackupState,
    outcome: &BackupOutcome,
) -> Option<CheckKind> {
    if !matches!(outcome, BackupOutcome::Transient { .. }) || state.check_after_failure_done {
        return None;
    }
    state.check_after_failure_done = true;
    Some(AFTER_FAILURE_CHECK)
}

/// What the check found, as the gate says it.
#[must_use]
pub fn damaged_message(files: u64) -> String {
    let noun = if files == 1 { "file" } else { "files" };
    format!("The check found {files} damaged {noun} in the backup. Backups and cleanup are paused.")
}

pub(crate) fn is_damaged_gate(state: &BackupState) -> bool {
    state.gate.as_ref().is_some_and(|gate| {
        gate.kind == GateKind::NeedsAttention && gate.fault == Some(Fault::RepoDamaged)
    })
}

/// Step 8: the result of one `restic check`. A check that did not finish
/// (`Failed`, `Locked`) counts as a failed round only when `counts_failure`
/// is set; the caller clears it when a backup's own result already counted
/// or has just succeeded.
pub fn apply_check(
    state: &mut BackupState,
    kind: CheckKind,
    outcome: &CheckOutcome,
    ctx: &RunContext,
    counts_failure: bool,
) -> Vec<BackupEvent> {
    let now = ctx.finished;
    match outcome {
        CheckOutcome::Ok => {
            state.last_check = Some(LastCheck {
                at: now,
                kind,
                ok: true,
                detail: String::new(),
            });
            if kind != CheckKind::Quick {
                state.next_check = now.checked_add(CHECK_INTERVAL).ok();
            }
            if kind == CheckKind::Full && is_damaged_gate(state) {
                clear_gate(state);
            }
            Vec::new()
        }
        CheckOutcome::Damaged { detail, .. } => {
            state.last_check = Some(LastCheck {
                at: now,
                kind,
                ok: false,
                detail: detail.clone(),
            });
            apply_gate(
                state,
                now,
                GateKind::NeedsAttention,
                damaged_message(outcome.damaged_files()),
                Some(Fault::RepoDamaged),
            )
        }
        CheckOutcome::Locked if counts_failure => apply_transient(state, LOCKED_DETAIL, ctx),
        CheckOutcome::Failed(detail) if counts_failure => apply_transient(state, detail, ctx),
        CheckOutcome::Locked | CheckOutcome::Failed(_) => Vec::new(),
    }
}

/// Step 6, item 10: the result of `forget --prune` after a backup.
pub fn apply_retention_result(
    state: &mut BackupState,
    result: Result<(), String>,
    now: Timestamp,
) -> Vec<BackupEvent> {
    match result {
        Ok(()) => {
            if is_retention_gate(state) {
                clear_gate(state);
            }
            state.prune_failures = 0;
            state.prune_error = None;
            Vec::new()
        }
        Err(message) => {
            state.prune_failures = state.prune_failures.saturating_add(1);
            let detail = retention_message(&message);
            state.prune_error = Some(message);
            if state.prune_failures >= PRUNE_FAILURES_BEFORE_GATE {
                apply_gate(state, now, GateKind::NeedsAttention, detail, None)
            } else {
                Vec::new()
            }
        }
    }
}

#[must_use]
pub fn retention_message(message: &str) -> String {
    format!("{RETENTION_PREFIX}{message}")
}

/// Only `forget` lifts this gate: a good backup in between leaves it, and its
/// notification, alone. It is the gate that says the stored `prune_error`.
fn is_retention_gate(state: &BackupState) -> bool {
    let Some(message) = state.prune_error.as_deref() else {
        return false;
    };
    state.gate.as_ref().is_some_and(|gate| {
        gate.kind == GateKind::NeedsAttention
            && gate.fault.is_none()
            && gate.detail == retention_message(message)
    })
}

/// An agent created the backup: the person has yet to save the key. Said
/// once, until [`key_saved`].
pub fn key_created_by_agent(state: &mut BackupState) -> Vec<BackupEvent> {
    state.key_unsaved = true;
    if announce(state, NOTIFIED_KEY_UNSAVED.to_owned()) {
        vec![BackupEvent::KeyUnsaved]
    } else {
        Vec::new()
    }
}

pub fn key_saved(state: &mut BackupState) {
    state.key_unsaved = false;
    state.notified.retain(|key| key != NOTIFIED_KEY_UNSAVED);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const HOUR: i64 = 3600;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn now() -> Timestamp {
        at("2026-01-10T12:00:00Z")
    }

    fn before(seconds: i64) -> Timestamp {
        Timestamp::from_second(now().as_second() - seconds).unwrap()
    }

    fn ctx() -> RunContext {
        RunContext {
            started: before(60),
            finished: now(),
            zone: TimeZone::UTC,
            every: Every::Daily,
            failures_after: 3,
            exit_code: Some(0),
            summary: Some(BackupSummary {
                snapshot_id: "abc123".to_owned(),
                files_new: 2,
                files_changed: 1,
                data_added: 4096,
                total_files_processed: 10,
                ..BackupSummary::default()
            }),
            log_file: PathBuf::from("/state/runs/1.jsonl"),
        }
    }

    fn transient(detail: &str) -> BackupOutcome {
        BackupOutcome::Transient {
            detail: detail.to_owned(),
        }
    }

    fn needs(fault: Fault) -> BackupOutcome {
        BackupOutcome::NeedsAttention {
            fault,
            detail: format!("{fault:?} detail"),
        }
    }

    fn attention_gate(since: Timestamp, fault: Option<Fault>) -> Gate {
        Gate {
            kind: GateKind::NeedsAttention,
            since,
            detail: "old".to_owned(),
            fault,
            auto_checked: None,
        }
    }

    #[test]
    fn ok_clears_what_a_failing_backup_had_and_records_the_run() {
        let mut state = BackupState {
            gate: Some(attention_gate(before(HOUR), Some(Fault::Quota))),
            last_outcome: Some(LastOutcome::Error),
            consecutive_failures: 4,
            failing_since: Some(before(5 * HOUR)),
            check_after_failure_done: true,
            notified: vec![
                "failures".to_owned(),
                "gate:needs_attention:quota".to_owned(),
                "gate:auth_error".to_owned(),
                "key_unsaved".to_owned(),
            ],
            ..BackupState::default()
        };

        let events = apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());

        assert_eq!(state.gate, None);
        assert_eq!(state.last_outcome, Some(LastOutcome::Ok));
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.failing_since, None);
        assert_eq!(state.last_success, Some(now()));
        assert_eq!(
            state.last_snapshot,
            Some(LastSnapshot {
                id: "abc123".to_owned(),
                time: now()
            })
        );
        assert!(!state.check_after_failure_done);
        assert_eq!(state.notified, ["key_unsaved"]);
        let run = state.last_run.unwrap();
        assert_eq!(run.started, before(60));
        assert_eq!(run.finished, now());
        assert_eq!(run.exit_code, 0);
        assert_eq!(run.summary.warnings, 0);
        assert_eq!(run.summary.files_new, 2);
        assert_eq!(run.log_file, PathBuf::from("/state/runs/1.jsonl"));
        assert_eq!(
            events,
            [BackupEvent::Success {
                changed_files: 3,
                bytes_added: 4096
            }]
        );
    }

    #[test]
    fn ok_with_warnings_stores_the_warning_count_with_the_run() {
        let mut state = BackupState::default();
        let context = RunContext {
            exit_code: Some(3),
            ..ctx()
        };

        apply_outcome(
            &mut state,
            &BackupOutcome::OkWithWarnings { warnings: 2 },
            &context,
        );

        let run = state.last_run.unwrap();
        assert_eq!(run.exit_code, 3);
        assert_eq!(run.summary.warnings, 2);
        assert_eq!(state.last_outcome, Some(LastOutcome::Ok));
    }

    #[test]
    fn ok_sets_next_check_only_when_there_is_none() {
        let mut fresh = BackupState::default();
        apply_outcome(&mut fresh, &BackupOutcome::Ok, &ctx());
        assert_eq!(
            fresh.next_check,
            Some(now().checked_add(CHECK_INTERVAL).unwrap())
        );

        let due = at("2026-01-01T00:00:00Z");
        let mut scheduled = BackupState {
            next_check: Some(due),
            ..BackupState::default()
        };
        apply_outcome(&mut scheduled, &BackupOutcome::Ok, &ctx());
        assert_eq!(scheduled.next_check, Some(due));
    }

    #[test]
    fn ok_with_nothing_new_or_changed_announces_nothing() {
        let mut state = BackupState::default();
        let context = RunContext {
            summary: Some(BackupSummary {
                snapshot_id: "abc123".to_owned(),
                files_unmodified: 10,
                ..BackupSummary::default()
            }),
            ..ctx()
        };

        assert_eq!(apply_outcome(&mut state, &BackupOutcome::Ok, &context), []);
    }

    #[test]
    fn cancelled_only_touches_last_attempt() {
        let mut state = BackupState {
            consecutive_failures: 2,
            ..BackupState::default()
        };
        let mut expected = state.clone();
        expected.last_attempt = Some(before(60));

        let events = apply_outcome(&mut state, &BackupOutcome::Cancelled, &ctx());

        assert_eq!(state, expected);
        assert_eq!(events, []);
    }

    #[test]
    fn auth_error_sets_the_gate_once_and_is_not_a_failure() {
        let mut state = BackupState::default();

        let first = apply_outcome(&mut state, &BackupOutcome::AuthError, &ctx());
        let second = apply_outcome(&mut state, &BackupOutcome::AuthError, &ctx());

        let gate = state.gate.unwrap();
        assert_eq!(gate.kind, GateKind::AuthError);
        assert_eq!(gate.fault, None);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.last_outcome, None);
        assert_eq!(
            first,
            [BackupEvent::Gate {
                kind: GateKind::AuthError,
                detail: String::new(),
                fault: None
            }]
        );
        assert_eq!(second, []);
        assert_eq!(state.notified, ["gate:auth_error"]);
    }

    #[test]
    fn needs_attention_sets_the_gate_counts_a_failure_and_announces_per_fault() {
        let mut state = BackupState::default();

        let first = apply_outcome(&mut state, &needs(Fault::Quota), &ctx());
        let repeat = apply_outcome(&mut state, &needs(Fault::Quota), &ctx());
        let other = apply_outcome(&mut state, &needs(Fault::Permission), &ctx());

        assert_eq!(first.len(), 1);
        assert_eq!(repeat, []);
        assert_eq!(other.len(), 1);
        assert_eq!(state.consecutive_failures, 3);
        assert_eq!(state.last_outcome, Some(LastOutcome::Error));
        assert_eq!(state.failing_since, Some(now()));
        assert_eq!(
            state.notified,
            [
                "gate:needs_attention:quota",
                "gate:needs_attention:permission"
            ]
        );
        let gate = state.gate.unwrap();
        assert_eq!(gate.fault, Some(Fault::Permission));
        assert_eq!(gate.detail, "Permission detail");
    }

    #[test]
    fn a_gate_keeps_its_since_while_the_fault_stays_and_takes_the_new_detail() {
        let mut state = BackupState::default();
        let early = RunContext {
            finished: before(2 * HOUR),
            ..ctx()
        };
        apply_outcome(&mut state, &needs(Fault::Quota), &early);

        apply_outcome(
            &mut state,
            &BackupOutcome::NeedsAttention {
                fault: Fault::Quota,
                detail: "newer".to_owned(),
            },
            &ctx(),
        );

        let gate = state.gate.unwrap();
        assert_eq!(gate.since, before(2 * HOUR));
        assert_eq!(gate.detail, "newer");
    }

    #[test]
    fn a_gate_of_another_fault_starts_a_new_since() {
        let mut state = BackupState::default();
        apply_outcome(
            &mut state,
            &needs(Fault::Quota),
            &RunContext {
                finished: before(2 * HOUR),
                ..ctx()
            },
        );

        apply_outcome(&mut state, &needs(Fault::Permission), &ctx());

        assert_eq!(state.gate.unwrap().since, now());
    }

    #[test]
    fn transient_counts_failures_and_notifies_when_failures_after_is_reached() {
        let mut state = BackupState::default();
        let mut all = Vec::new();

        for _ in 0..5 {
            all.push(apply_outcome(
                &mut state,
                &transient("connection reset"),
                &ctx(),
            ));
        }

        assert_eq!(all[0], []);
        assert_eq!(all[1], []);
        assert_eq!(
            all[2],
            [BackupEvent::Failing {
                failures: 3,
                message: "connection reset".to_owned()
            }]
        );
        assert_eq!(all[3], []);
        assert_eq!(all[4], []);
        assert_eq!(state.consecutive_failures, 5);
        assert_eq!(state.last_outcome, Some(LastOutcome::Error));
        assert_eq!(state.failing_since, Some(now()));
        assert_eq!(state.notified, ["failures"]);
        assert_eq!(state.gate, None);
    }

    #[test]
    fn transient_keeps_the_first_failing_since_of_the_series() {
        let mut state = BackupState::default();
        apply_outcome(
            &mut state,
            &transient("x"),
            &RunContext {
                finished: before(HOUR),
                ..ctx()
            },
        );

        apply_outcome(&mut state, &transient("x"), &ctx());

        assert_eq!(state.failing_since, Some(before(HOUR)));
    }

    #[test]
    fn failures_after_zero_never_notifies_but_still_counts() {
        let mut state = BackupState::default();
        let silent = RunContext {
            failures_after: 0,
            ..ctx()
        };

        for _ in 0..4 {
            assert_eq!(apply_outcome(&mut state, &transient("x"), &silent), []);
        }

        assert_eq!(state.consecutive_failures, 4);
        assert!(state.notified.is_empty());
    }

    #[test]
    fn a_new_series_notifies_again_after_a_success() {
        let mut state = BackupState::default();
        for _ in 0..3 {
            apply_outcome(&mut state, &transient("x"), &ctx());
        }
        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());

        let mut last = Vec::new();
        for _ in 0..3 {
            last = apply_outcome(&mut state, &transient("y"), &ctx());
        }

        assert_eq!(
            last,
            [BackupEvent::Failing {
                failures: 3,
                message: "y".to_owned()
            }]
        );
    }

    #[test]
    fn locked_counts_as_a_failure_and_says_the_repository_is_in_use() {
        let mut state = BackupState::default();
        let context = RunContext {
            failures_after: 1,
            ..ctx()
        };

        let events = apply_outcome(&mut state, &BackupOutcome::Locked, &context);

        assert_eq!(state.consecutive_failures, 1);
        assert_eq!(
            events,
            [BackupEvent::Failing {
                failures: 1,
                message: "The backup is in use by another process. It will try again.".to_owned()
            }]
        );
    }

    fn stale_gate_after(every: Every, failing_for: i64) -> Option<Gate> {
        let mut state = BackupState {
            failing_since: Some(before(failing_for)),
            consecutive_failures: 1,
            ..BackupState::default()
        };
        let context = RunContext { every, ..ctx() };
        apply_outcome(&mut state, &transient("connection reset"), &context);
        state.gate
    }

    #[test]
    fn the_stale_limit_is_the_larger_of_two_intervals_and_forty_eight_hours() {
        assert_eq!(stale_limit(Every::Hourly), Duration::from_hours(48));
        assert_eq!(stale_limit(Every::Daily), Duration::from_hours(48));
        assert_eq!(stale_limit(Every::Weekly), Duration::from_hours(14 * 24));
    }

    #[test]
    fn exactly_at_the_stale_limit_there_is_no_gate_and_one_second_after_there_is() {
        for (every, hours) in [
            (Every::Hourly, 48),
            (Every::Daily, 48),
            (Every::Weekly, 14 * 24),
        ] {
            let limit = hours * HOUR;

            assert_eq!(stale_gate_after(every, limit - 1), None, "{every:?} before");
            assert_eq!(stale_gate_after(every, limit), None, "{every:?} at");
            assert!(
                stale_gate_after(every, limit + 1).is_some(),
                "{every:?} after"
            );
        }
    }

    #[test]
    fn the_stale_gate_names_the_date_the_failures_began_and_the_last_message() {
        let gate = stale_gate_after(Every::Daily, 49 * HOUR).unwrap();

        assert_eq!(gate.kind, GateKind::NeedsAttention);
        assert_eq!(gate.fault, None);
        assert_eq!(
            gate.detail,
            "Backups have failed since 2026-01-08: connection reset"
        );
    }

    #[test]
    fn the_stale_gate_is_announced_once_and_keeps_its_since() {
        let mut state = BackupState {
            failing_since: Some(before(60 * HOUR)),
            ..BackupState::default()
        };
        let first_round = RunContext {
            failures_after: 0,
            finished: before(HOUR),
            ..ctx()
        };
        let later_round = RunContext {
            failures_after: 0,
            ..ctx()
        };

        let first = apply_outcome(&mut state, &transient("a"), &first_round);
        let second = apply_outcome(&mut state, &transient("b"), &later_round);

        assert_eq!(
            first,
            [BackupEvent::Gate {
                kind: GateKind::NeedsAttention,
                detail: "Backups have failed since 2026-01-08: a".to_owned(),
                fault: None
            }]
        );
        assert_eq!(second, []);
        let gate = state.gate.unwrap();
        assert_eq!(gate.since, before(HOUR));
        assert_eq!(gate.detail, "Backups have failed since 2026-01-08: b");
        assert_eq!(state.notified, ["gate:needs_attention"]);
    }

    #[test]
    fn the_stale_gate_does_not_replace_a_gate_with_a_cause() {
        for gate in [
            Gate {
                kind: GateKind::AuthError,
                fault: None,
                ..attention_gate(before(HOUR), None)
            },
            attention_gate(before(HOUR), Some(Fault::RepoDamaged)),
            attention_gate(before(HOUR), Some(Fault::Quota)),
        ] {
            let mut state = BackupState {
                gate: Some(gate.clone()),
                failing_since: Some(before(60 * HOUR)),
                ..BackupState::default()
            };

            apply_outcome(&mut state, &transient("x"), &ctx());

            assert_eq!(state.gate, Some(gate));
        }
    }

    fn skip_ctx(dest: &str) -> SkipContext<'_> {
        SkipContext {
            now: now(),
            zone: TimeZone::UTC,
            every: Every::Daily,
            dest,
        }
    }

    #[test]
    fn paused_and_gated_skips_write_nothing() {
        for reason in [SkipReason::Paused, SkipReason::Gated] {
            let mut state = BackupState::default();

            let events = apply_skip(&mut state, reason, &skip_ctx("/media/disk"));

            assert_eq!(state, BackupState::default(), "{reason:?}");
            assert_eq!(events, [], "{reason:?}");
        }
    }

    #[test]
    fn an_offline_skip_marks_offline_without_a_gate_or_a_notification() {
        let mut state = BackupState::default();

        let events = apply_skip(&mut state, SkipReason::Offline, &skip_ctx("gdrive:Backups"));

        assert!(state.offline);
        assert_eq!(state.gate, None);
        assert_eq!(events, []);
    }

    #[test]
    fn being_offline_for_a_long_time_never_becomes_a_gate() {
        let mut state = BackupState {
            unavailable_since: Some(before(90 * HOUR)),
            ..BackupState::default()
        };

        apply_skip(&mut state, SkipReason::Offline, &skip_ctx("gdrive:Backups"));

        assert_eq!(state.gate, None);
    }

    #[test]
    fn an_unavailable_destination_remembers_since_when() {
        let mut state = BackupState::default();

        let first = apply_skip(
            &mut state,
            SkipReason::DestinationUnavailable,
            &skip_ctx("/media/disk"),
        );
        let later = SkipContext {
            now: Timestamp::from_second(now().as_second() + HOUR).unwrap(),
            ..skip_ctx("/media/disk")
        };
        apply_skip(&mut state, SkipReason::DestinationUnavailable, &later);

        assert!(state.destination_unavailable);
        assert_eq!(state.unavailable_since, Some(now()));
        assert_eq!(first, []);
        assert_eq!(state.gate, None);
    }

    #[test]
    fn a_destination_missing_past_the_stale_limit_becomes_a_gate_once() {
        let mut state = BackupState {
            unavailable_since: Some(before(49 * HOUR)),
            ..BackupState::default()
        };

        let first = apply_skip(
            &mut state,
            SkipReason::DestinationUnavailable,
            &skip_ctx("/media/disk"),
        );
        let second = apply_skip(
            &mut state,
            SkipReason::DestinationUnavailable,
            &skip_ctx("/media/disk"),
        );

        assert_eq!(
            first,
            [BackupEvent::Gate {
                kind: GateKind::NeedsAttention,
                detail: "/media/disk has not been available since 2026-01-08".to_owned(),
                fault: Some(Fault::DestinationMissing)
            }]
        );
        assert_eq!(second, []);
        assert_eq!(state.gate.unwrap().fault, Some(Fault::DestinationMissing));
        assert_eq!(state.notified, ["gate:needs_attention:destination_missing"]);
    }

    #[test]
    fn a_destination_missing_exactly_the_stale_limit_is_not_yet_a_gate() {
        let mut state = BackupState {
            unavailable_since: Some(before(48 * HOUR)),
            ..BackupState::default()
        };

        apply_skip(
            &mut state,
            SkipReason::DestinationUnavailable,
            &skip_ctx("/media/disk"),
        );

        assert_eq!(state.gate, None);
    }

    #[test]
    fn begin_attempt_clears_what_kept_the_last_one_away() {
        let mut state = BackupState {
            offline: true,
            destination_unavailable: true,
            unavailable_since: Some(before(HOUR)),
            ..BackupState::default()
        };

        begin_attempt(&mut state, now());

        assert_eq!(state.last_attempt, Some(now()));
        assert!(!state.offline);
        assert!(!state.destination_unavailable);
        assert_eq!(state.unavailable_since, None);
    }

    #[test]
    fn a_missing_key_is_a_failure_and_a_gate_with_the_key_missing_fault() {
        let mut state = BackupState::default();

        let first = apply_key_failure(&mut state, "no key in the keyring".to_owned(), now());
        let second = apply_key_failure(&mut state, "no key in the keyring".to_owned(), now());

        assert_eq!(state.consecutive_failures, 2);
        assert_eq!(state.last_outcome, Some(LastOutcome::Error));
        assert_eq!(
            first,
            [BackupEvent::Gate {
                kind: GateKind::NeedsAttention,
                detail: "no key in the keyring".to_owned(),
                fault: Some(Fault::KeyMissing)
            }]
        );
        assert_eq!(second, []);
        assert_eq!(state.notified, ["gate:needs_attention:key_missing"]);
    }

    #[test]
    fn a_missing_key_counts_but_does_not_replace_the_damaged_gate() {
        let mut state = damaged_state();
        let before = state.gate.clone();

        let events = apply_key_failure(&mut state, "no key".to_owned(), now());

        assert_eq!(events, []);
        assert_eq!(state.gate, before);
        assert_eq!(state.consecutive_failures, 1);
        assert_eq!(state.last_outcome, Some(LastOutcome::Error));
        assert_eq!(state.notified, ["gate:needs_attention:repo_damaged"]);
    }

    #[test]
    fn a_config_error_sets_its_gate_when_nothing_holds_the_backup() {
        let mut state = BackupState::default();

        let events = apply_config_error(&mut state, now(), "bad dest".to_owned());

        assert_eq!(state.gate.as_ref().unwrap().kind, GateKind::ConfigError);
        assert_eq!(
            events,
            [BackupEvent::Gate {
                kind: GateKind::ConfigError,
                detail: "bad dest".to_owned(),
                fault: None
            }]
        );
    }

    #[test]
    fn a_config_error_does_not_replace_the_damaged_gate() {
        let mut state = damaged_state();
        let before = state.gate.clone();

        let events = apply_config_error(&mut state, now(), "bad dest".to_owned());

        assert_eq!(events, []);
        assert_eq!(state.gate, before);
        assert_eq!(state.notified, ["gate:needs_attention:repo_damaged"]);
    }

    #[test]
    fn a_successful_cleanup_resets_the_counter_and_forgets_the_message() {
        let mut state = BackupState {
            prune_failures: 2,
            prune_error: Some("repository is locked".to_owned()),
            ..BackupState::default()
        };

        let events = apply_retention_result(&mut state, Ok(()), now());

        assert_eq!(state.prune_failures, 0);
        assert_eq!(state.prune_error, None);
        assert_eq!(events, []);
    }

    #[test]
    fn cleanup_failures_gate_only_at_the_limit_with_the_phrase() {
        let mut state = BackupState::default();
        let failed = || Err("repository is locked".to_owned());

        let first = apply_retention_result(&mut state, failed(), now());
        let second = apply_retention_result(&mut state, failed(), now());
        assert_eq!((first, second), (Vec::new(), Vec::new()));
        assert_eq!(state.gate, None);

        let third = apply_retention_result(&mut state, failed(), now());

        assert_eq!(state.prune_failures, 3);
        assert_eq!(
            third,
            [BackupEvent::Gate {
                kind: GateKind::NeedsAttention,
                detail: "Old snapshots could not be cleaned up: repository is locked".to_owned(),
                fault: None
            }]
        );
        let gate = state.gate.unwrap();
        assert_eq!(gate.fault, None);
        assert_eq!(
            gate.detail,
            "Old snapshots could not be cleaned up: repository is locked"
        );
    }

    #[test]
    fn a_cleanup_failure_does_not_touch_the_backup_result() {
        let mut state = BackupState::default();
        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());
        let before_cleanup = state.clone();

        apply_retention_result(&mut state, Err("x".to_owned()), now());

        assert_eq!(state.last_outcome, before_cleanup.last_outcome);
        assert_eq!(state.last_success, before_cleanup.last_success);
        assert_eq!(state.consecutive_failures, 0);
    }

    #[test]
    fn every_failed_cleanup_keeps_its_message_even_before_the_gate() {
        let mut state = BackupState::default();

        apply_retention_result(&mut state, Err("first".to_owned()), now());
        assert_eq!(state.prune_error.as_deref(), Some("first"));
        assert_eq!(state.gate, None);

        apply_retention_result(&mut state, Err("second".to_owned()), now());
        assert_eq!(state.prune_error.as_deref(), Some("second"));
    }

    #[test]
    fn a_good_round_between_failed_cleanups_does_not_announce_the_gate_again() {
        let mut state = BackupState {
            prune_failures: 2,
            ..BackupState::default()
        };
        let mut announced = Vec::new();

        for _ in 0..3 {
            announced.extend(apply_outcome(&mut state, &BackupOutcome::Ok, &ctx()));
            announced.extend(apply_retention_result(
                &mut state,
                Err("repository is locked".to_owned()),
                now(),
            ));
        }

        let gates: Vec<_> = announced
            .iter()
            .filter(|event| matches!(event, BackupEvent::Gate { .. }))
            .collect();
        assert_eq!(gates.len(), 1);
        assert_eq!(state.prune_failures, 5);
        assert_eq!(state.notified, ["gate:needs_attention"]);
        let gate = state.gate.unwrap();
        assert_eq!(
            gate.detail,
            "Old snapshots could not be cleaned up: repository is locked"
        );
    }

    #[test]
    fn a_cleanup_that_works_again_lifts_its_gate_and_the_next_failure_is_announced() {
        let mut state = BackupState {
            prune_failures: 2,
            ..BackupState::default()
        };
        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());
        apply_retention_result(&mut state, Err("locked".to_owned()), now());
        assert!(state.gate.is_some());

        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());
        apply_retention_result(&mut state, Ok(()), now());

        assert_eq!(state.gate, None);
        assert_eq!(state.prune_error, None);
        assert!(state.notified.is_empty());
        state.prune_failures = 2;
        let again = apply_retention_result(&mut state, Err("locked".to_owned()), now());
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn a_new_cleanup_message_refreshes_the_gate_without_a_new_announcement() {
        let mut state = BackupState::default();
        for _ in 0..3 {
            apply_retention_result(&mut state, Err("first".to_owned()), now());
        }
        let since = state.gate.as_ref().unwrap().since;

        let later = Timestamp::from_second(now().as_second() + HOUR).unwrap();
        let fourth = apply_retention_result(&mut state, Err("second".to_owned()), later);

        assert_eq!(fourth, []);
        assert_eq!(state.prune_failures, 4);
        assert_eq!(state.prune_error.as_deref(), Some("second"));
        let gate = state.gate.clone().unwrap();
        assert_eq!(gate.detail, retention_message("second"));
        assert_eq!(gate.since, since);
        assert_eq!(state.notified, ["gate:needs_attention"]);

        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());
        assert_eq!(state.gate, Some(gate));

        apply_retention_result(&mut state, Ok(()), now());

        assert_eq!(state.gate, None);
        assert_eq!(state.prune_error, None);
        assert!(state.notified.is_empty());
    }

    #[test]
    fn a_good_backup_still_clears_a_gate_that_forget_did_not_set() {
        let mut state = BackupState {
            gate: Some(attention_gate(before(HOUR), None)),
            prune_failures: 3,
            notified: vec!["gate:needs_attention".to_owned()],
            ..BackupState::default()
        };

        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());

        assert_eq!(state.gate, None);
        assert!(state.notified.is_empty());
    }

    #[test]
    fn a_gate_that_only_looks_like_the_cleanup_one_is_cleared_by_a_good_backup() {
        let mut gate = attention_gate(before(HOUR), None);
        gate.detail = retention_message("repository is locked");
        let mut state = BackupState {
            gate: Some(gate),
            prune_failures: 3,
            prune_error: Some("another message".to_owned()),
            notified: vec!["gate:needs_attention".to_owned()],
            ..BackupState::default()
        };

        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());

        assert_eq!(state.gate, None);
        assert!(state.notified.is_empty());
    }

    #[test]
    fn a_good_backup_leaves_the_gate_that_says_the_stored_cleanup_error() {
        let mut gate = attention_gate(before(HOUR), None);
        gate.detail = retention_message("repository is locked");
        let mut state = BackupState {
            gate: Some(gate.clone()),
            prune_failures: 3,
            prune_error: Some("repository is locked".to_owned()),
            notified: vec!["gate:needs_attention".to_owned()],
            ..BackupState::default()
        };

        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());

        assert_eq!(state.gate, Some(gate));
        assert_eq!(state.notified, ["gate:needs_attention"]);
    }

    #[test]
    fn the_key_is_unsaved_is_said_once_until_it_is_saved() {
        let mut state = BackupState::default();

        let first = key_created_by_agent(&mut state);
        let again = key_created_by_agent(&mut state);

        assert_eq!(first, [BackupEvent::KeyUnsaved]);
        assert_eq!(again, []);
        assert!(state.key_unsaved);
        assert_eq!(state.notified, ["key_unsaved"]);

        key_saved(&mut state);

        assert!(!state.key_unsaved);
        assert!(state.notified.is_empty());
    }

    #[test]
    fn a_success_does_not_forget_that_the_key_is_unsaved() {
        let mut state = BackupState::default();
        key_created_by_agent(&mut state);

        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());

        assert!(state.key_unsaved);
        assert_eq!(state.notified, ["key_unsaved"]);
    }

    #[test]
    fn clear_gate_removes_the_gate_and_its_notifications_only() {
        let mut state = BackupState {
            gate: Some(attention_gate(now(), Some(Fault::Quota))),
            notified: vec![
                "gate:needs_attention:quota".to_owned(),
                "failures".to_owned(),
            ],
            ..BackupState::default()
        };

        clear_gate(&mut state);

        assert_eq!(state.gate, None);
        assert_eq!(state.notified, ["failures"]);
    }

    #[test]
    fn gate_keys_name_the_fault_only_for_needs_attention() {
        assert_eq!(
            gate_key(GateKind::NeedsAttention, Some(Fault::RepoDamaged)),
            "gate:needs_attention:repo_damaged"
        );
        assert_eq!(
            gate_key(GateKind::NeedsAttention, None),
            "gate:needs_attention"
        );
        assert_eq!(gate_key(GateKind::AuthError, None), "gate:auth_error");
        assert_eq!(gate_key(GateKind::ConfigError, None), "gate:config_error");
    }

    fn damaged(packs: &[&str]) -> CheckOutcome {
        CheckOutcome::Damaged {
            errors: 1,
            broken_packs: packs.iter().map(|pack| (*pack).to_owned()).collect(),
            detail: "pack aa contains 1 error".to_owned(),
        }
    }

    fn damaged_state() -> BackupState {
        let mut state = BackupState::default();
        apply_check(&mut state, CheckKind::Full, &damaged(&["aa"]), &ctx(), true);
        state
    }

    #[test]
    fn a_sample_check_that_is_due_is_asked_for_and_a_good_one_moves_the_next_to_thirty_days() {
        let mut state = BackupState {
            next_check: Some(before(1)),
            ..BackupState::default()
        };

        assert_eq!(monthly_check(&state, now()), Some(CheckKind::Sample));
        apply_check(
            &mut state,
            CheckKind::Sample,
            &CheckOutcome::Ok,
            &ctx(),
            false,
        );

        assert_eq!(
            state.next_check,
            Some(now().checked_add(CHECK_INTERVAL).unwrap())
        );
        assert_eq!(monthly_check(&state, now()), None);
        assert_eq!(
            monthly_check(&state, before(-30 * 24 * HOUR)),
            Some(CheckKind::Sample)
        );
    }

    #[test]
    fn a_sample_check_is_due_exactly_when_the_time_comes_and_never_without_a_date() {
        let due = BackupState {
            next_check: Some(now()),
            ..BackupState::default()
        };
        let later = BackupState {
            next_check: Some(before(-1)),
            ..BackupState::default()
        };

        assert_eq!(monthly_check(&due, now()), Some(CheckKind::Sample));
        assert_eq!(monthly_check(&later, now()), None);
        assert_eq!(monthly_check(&BackupState::default(), now()), None);
    }

    #[test]
    fn a_quick_check_does_not_move_the_next_sample_but_the_full_one_does() {
        let soon = before(-HOUR);
        let mut state = BackupState {
            next_check: Some(soon),
            ..BackupState::default()
        };

        apply_check(
            &mut state,
            CheckKind::Quick,
            &CheckOutcome::Ok,
            &ctx(),
            true,
        );
        assert_eq!(state.next_check, Some(soon));
        apply_check(&mut state, CheckKind::Full, &CheckOutcome::Ok, &ctx(), true);

        assert_eq!(
            state.next_check,
            Some(now().checked_add(CHECK_INTERVAL).unwrap())
        );
    }

    #[test]
    fn a_good_check_is_recorded_with_its_kind_and_time() {
        let mut state = BackupState::default();

        let events = apply_check(
            &mut state,
            CheckKind::Quick,
            &CheckOutcome::Ok,
            &ctx(),
            true,
        );

        assert_eq!(events, []);
        assert_eq!(
            state.last_check,
            Some(LastCheck {
                at: now(),
                kind: CheckKind::Quick,
                ok: true,
                detail: String::new()
            })
        );
    }

    #[test]
    fn three_failed_backups_in_a_row_run_exactly_one_post_failure_check() {
        let mut state = BackupState::default();
        let mut asked = Vec::new();

        for _ in 0..3 {
            let outcome = transient("connection reset");
            apply_outcome(&mut state, &outcome, &ctx());
            asked.extend(take_post_failure_check(&mut state, &outcome));
        }

        assert_eq!(asked, [CheckKind::Quick]);
        assert!(state.check_after_failure_done);
    }

    #[test]
    fn the_post_failure_check_reads_no_data_and_a_good_backup_earns_another() {
        let mut state = BackupState::default();
        let failed = transient("connection reset");
        apply_outcome(&mut state, &failed, &ctx());
        assert_eq!(
            take_post_failure_check(&mut state, &failed),
            Some(AFTER_FAILURE_CHECK)
        );
        assert_eq!(AFTER_FAILURE_CHECK, CheckKind::Quick);

        apply_outcome(&mut state, &BackupOutcome::Ok, &ctx());
        assert!(!state.check_after_failure_done);
        apply_outcome(&mut state, &failed, &ctx());

        assert_eq!(
            take_post_failure_check(&mut state, &failed),
            Some(AFTER_FAILURE_CHECK)
        );
    }

    #[test]
    fn only_a_failure_without_a_known_cause_asks_for_the_post_failure_check() {
        for outcome in [
            BackupOutcome::Ok,
            BackupOutcome::OkWithWarnings { warnings: 1 },
            BackupOutcome::Cancelled,
            BackupOutcome::AuthError,
            BackupOutcome::Locked,
            needs(Fault::Quota),
        ] {
            let mut state = BackupState::default();

            assert_eq!(
                take_post_failure_check(&mut state, &outcome),
                None,
                "{outcome:?}"
            );
            assert!(!state.check_after_failure_done, "{outcome:?}");
        }
    }

    #[test]
    fn damage_gates_the_backup_notifies_once_and_records_a_bad_check() {
        let mut state = BackupState::default();

        let first = apply_check(
            &mut state,
            CheckKind::Sample,
            &damaged(&["aa", "bb"]),
            &ctx(),
            false,
        );
        let second = apply_check(
            &mut state,
            CheckKind::Full,
            &damaged(&["aa", "bb"]),
            &ctx(),
            true,
        );

        let gate = state.gate.as_ref().unwrap();
        assert_eq!(gate.kind, GateKind::NeedsAttention);
        assert_eq!(gate.fault, Some(Fault::RepoDamaged));
        assert_eq!(
            gate.detail,
            "The check found 2 damaged files in the backup. Backups and cleanup are paused."
        );
        assert_eq!(
            first,
            [BackupEvent::Gate {
                kind: GateKind::NeedsAttention,
                detail: gate.detail.clone(),
                fault: Some(Fault::RepoDamaged)
            }]
        );
        assert_eq!(second, []);
        let check = state.last_check.as_ref().unwrap();
        assert!(!check.ok);
        assert_eq!(check.kind, CheckKind::Full);
        assert_eq!(check.detail, "pack aa contains 1 error");
    }

    #[test]
    fn one_damaged_file_is_said_in_the_singular() {
        assert_eq!(
            damaged_message(1),
            "The check found 1 damaged file in the backup. Backups and cleanup are paused."
        );
        assert!(damaged_message(0).contains("0 damaged files"));
    }

    #[test]
    fn damage_is_not_a_failed_round() {
        let mut state = BackupState::default();

        apply_check(&mut state, CheckKind::Full, &damaged(&["aa"]), &ctx(), true);

        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.last_outcome, None);
    }

    #[test]
    fn a_sample_without_errors_does_not_release_the_damaged_gate_but_a_full_check_does() {
        let mut state = damaged_state();

        apply_check(
            &mut state,
            CheckKind::Sample,
            &CheckOutcome::Ok,
            &ctx(),
            true,
        );
        apply_check(
            &mut state,
            CheckKind::Quick,
            &CheckOutcome::Ok,
            &ctx(),
            true,
        );
        assert_eq!(state.gate.as_ref().unwrap().fault, Some(Fault::RepoDamaged));

        apply_check(&mut state, CheckKind::Full, &CheckOutcome::Ok, &ctx(), true);

        assert_eq!(state.gate, None);
        assert!(state.notified.is_empty());
        assert!(state.last_check.unwrap().ok);
    }

    #[test]
    fn a_full_check_leaves_a_gate_that_is_not_damage_alone() {
        let mut state = BackupState {
            gate: Some(attention_gate(before(HOUR), Some(Fault::Quota))),
            ..BackupState::default()
        };

        apply_check(&mut state, CheckKind::Full, &CheckOutcome::Ok, &ctx(), true);

        assert_eq!(state.gate.as_ref().unwrap().fault, Some(Fault::Quota));
    }

    #[test]
    fn damage_found_again_after_it_was_released_is_announced_again() {
        let mut state = damaged_state();
        apply_check(&mut state, CheckKind::Full, &CheckOutcome::Ok, &ctx(), true);

        let again = apply_check(&mut state, CheckKind::Full, &damaged(&["aa"]), &ctx(), true);

        assert_eq!(again.len(), 1);
    }

    #[test]
    fn a_check_that_did_not_finish_counts_as_a_failed_round_and_leaves_the_last_check_alone() {
        let before_check = LastCheck {
            at: before(HOUR),
            kind: CheckKind::Sample,
            ok: true,
            detail: String::new(),
        };
        let mut state = BackupState {
            last_check: Some(before_check.clone()),
            ..BackupState::default()
        };
        let context = RunContext {
            failures_after: 1,
            ..ctx()
        };

        let events = apply_check(
            &mut state,
            CheckKind::Full,
            &CheckOutcome::Failed("connection reset".to_owned()),
            &context,
            true,
        );

        assert_eq!(state.consecutive_failures, 1);
        assert_eq!(state.last_outcome, Some(LastOutcome::Error));
        assert_eq!(state.last_check, Some(before_check));
        assert_eq!(
            events,
            [BackupEvent::Failing {
                failures: 1,
                message: "connection reset".to_owned()
            }]
        );
    }

    #[test]
    fn a_check_stopped_by_a_lock_counts_with_the_in_use_message() {
        let mut state = BackupState::default();
        let context = RunContext {
            failures_after: 1,
            ..ctx()
        };

        let events = apply_check(
            &mut state,
            CheckKind::Quick,
            &CheckOutcome::Locked,
            &context,
            true,
        );

        assert_eq!(state.consecutive_failures, 1);
        assert_eq!(
            events,
            [BackupEvent::Failing {
                failures: 1,
                message: LOCKED_DETAIL.to_owned()
            }]
        );
    }

    #[test]
    fn a_check_that_did_not_finish_after_a_backup_is_not_counted_twice() {
        for outcome in [CheckOutcome::Locked, CheckOutcome::Failed("x".to_owned())] {
            let mut state = BackupState::default();

            let events = apply_check(&mut state, CheckKind::Quick, &outcome, &ctx(), false);

            assert_eq!(state, BackupState::default(), "{outcome:?}");
            assert_eq!(events, []);
        }
    }
}
