//! What a backup round writes into `state.json`, and what it announces
//! (spec phase 3, steps 6 and 10). Pure: the caller saves the state first
//! and sends the events after, so whoever is told can already read why.

use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::bisync::Fault;
use crate::state::{Gate, GateKind, LastOutcome, NOTIFIED_FAILURES};

use super::classify::BackupOutcome;
use super::config::Every;
use super::run_decision::SkipReason;
use super::state::{BackupState, BackupSummary, LastRun, LastSnapshot};
use super::{CHECK_INTERVAL, PRUNE_FAILURES_BEFORE_GATE, STALE_FACTOR, STALE_MIN};

/// Key in [`BackupState::notified`] for the "save the key" notification.
pub const NOTIFIED_KEY_UNSAVED: &str = "key_unsaved";

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

/// Step 6, item 6: the key cannot be had. It counts as a failure.
pub fn apply_key_failure(
    state: &mut BackupState,
    detail: String,
    now: Timestamp,
) -> Vec<BackupEvent> {
    record_failure(state, now);
    apply_gate(
        state,
        now,
        GateKind::NeedsAttention,
        detail,
        Some(Fault::KeyMissing),
    )
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
        BackupOutcome::Transient { detail } | BackupOutcome::Locked { detail } => {
            apply_transient(state, detail, ctx)
        }
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
    fn locked_counts_as_a_failure_and_says_the_message_restic_gave() {
        let mut state = BackupState::default();
        let context = RunContext {
            failures_after: 1,
            ..ctx()
        };
        let locked = BackupOutcome::Locked {
            detail: "unable to create lock in backend: repository is already locked".to_owned(),
        };

        let events = apply_outcome(&mut state, &locked, &context);

        assert_eq!(state.consecutive_failures, 1);
        assert_eq!(
            events,
            [BackupEvent::Failing {
                failures: 1,
                message: "unable to create lock in backend: repository is already locked"
                    .to_owned()
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
}
