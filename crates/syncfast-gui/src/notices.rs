//! Notifications the GUI shows when the `run` command asks for one
//! (`notify <id>`), so a click can open the window on that sync.

use fastframe_notifications::{App, Client, Event, Notification};
use syncfast_core::behavior::Behavior;
use syncfast_core::bisync_log::read_log;
use syncfast_core::config::Config;
use syncfast_core::notify::{Alert, Notice, last_error_message, notice};
use syncfast_core::paths::Paths;
use syncfast_core::state::{LastOutcome, NOTIFIED_FAILURES, State};

use crate::instance::{Inbox, Request};

const NO_ERROR_LINE: &str = "Open syncfast for details.";

/// What a sync's state asks the user for, first match wins: a gate, new
/// conflicts from the last round, repeated failures.
#[must_use]
pub fn alert_for(state: &State, last_error: Option<&str>) -> Option<Alert> {
    if let Some(gate) = &state.gate {
        return Some(Alert::Gate {
            kind: gate.kind,
            detail: gate.detail.clone(),
            fault: gate.fault,
        });
    }
    let new_conflicts = state
        .last_run
        .as_ref()
        .and_then(|run| run.summary.conflicts_new)
        .filter(|&count| count > 0);
    if let Some(count) = new_conflicts {
        return Some(Alert::NewConflicts(count));
    }
    let failing = state.last_outcome == Some(LastOutcome::Error)
        && state.notified.iter().any(|key| key == NOTIFIED_FAILURES);
    failing.then(|| Alert::Failing(last_error.unwrap_or(NO_ERROR_LINE).to_owned()))
}

/// The notice for `pair_id`, or `None` when the sync no longer asks for
/// anything (the user fixed it before the message got here).
pub async fn notice_for(paths: &Paths, pair_id: &str) -> Option<Notice> {
    let config = Config::load(paths).await.ok()?;
    let pair = config.pairs().iter().find(|pair| pair.id == pair_id)?;
    let state = State::load(paths, pair_id).await.ok()?;
    let last_error = match &state.last_run {
        Some(run) => read_log(&run.log_file).await.ok(),
        None => None,
    };
    let alert = alert_for(
        &state,
        last_error
            .as_deref()
            .and_then(last_error_message)
            .map(str::trim),
    )?;
    let behavior = Behavior::for_pair(&config.defaults(), pair);
    Some(notice(pair, &behavior, &alert))
}

/// Shows notices through the desktop's notification service. A click puts
/// [`Request::Opened`] in the inbox.
pub struct Notices {
    client: Option<Client>,
    inbox: Inbox,
    wake: fastframe_shell::Waker,
}

impl std::fmt::Debug for Notices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notices").finish_non_exhaustive()
    }
}

impl Notices {
    #[must_use]
    pub fn new(inbox: Inbox, wake: fastframe_shell::Waker) -> Self {
        Self {
            client: None,
            inbox,
            wake,
        }
    }

    pub fn show(&mut self, notice: &Notice) {
        if self.client.is_none() {
            let entry = fastframe_desktop::desktop_entry("syncfast");
            let app = App::new("syncfast", "syncfast")
                .desktop_entry(&entry)
                .icon(&entry);
            match Client::new(app) {
                Ok(client) => self.client = Some(client),
                Err(err) => {
                    log::warn!("cannot start the notification service: {err}");
                    return;
                }
            }
        }
        let Some(client) = &self.client else {
            return;
        };
        let inbox = self.inbox.clone();
        let wake = self.wake.clone();
        let pair_id = notice.pair_id.clone();
        let shown = client.show_with(
            Notification::new(notice.title.as_str()).body(notice.body.as_str()),
            move |event| {
                if event == Event::Activated {
                    inbox.push(Request::Opened(pair_id.clone()));
                    wake.wake();
                }
            },
        );
        if let Err(err) = shown {
            log::warn!("cannot show the notification: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use syncfast_core::bisync_log::RunSummary;
    use syncfast_core::state::{Gate, GateKind, LastRun};

    use super::*;

    fn at() -> jiff::Timestamp {
        "2026-11-03T14:00:00Z".parse().unwrap()
    }

    fn state_with_conflicts(new: Option<u32>) -> State {
        State {
            last_run: Some(LastRun {
                started: at(),
                finished: at(),
                exit_code: 0,
                summary: RunSummary {
                    conflicts_new: new,
                    ..RunSummary::default()
                },
                log_file: "run.jsonl".into(),
                auto_reviewed: None,
            }),
            ..State::default()
        }
    }

    #[test]
    fn gate_comes_before_conflicts_and_failures() {
        let state = State {
            gate: Some(Gate {
                kind: GateKind::AuthError,
                since: at(),
                detail: String::new(),
                fault: None,
                auto_checked: None,
            }),
            consecutive_failures: 5,
            ..state_with_conflicts(Some(2))
        };

        assert!(matches!(
            alert_for(&state, None),
            Some(Alert::Gate {
                kind: GateKind::AuthError,
                ..
            })
        ));
    }

    #[test]
    fn new_conflicts_from_the_last_round_are_announced() {
        assert_eq!(
            alert_for(&state_with_conflicts(Some(2)), None),
            Some(Alert::NewConflicts(2))
        );
        assert_eq!(alert_for(&state_with_conflicts(Some(0)), None), None);
        assert_eq!(alert_for(&state_with_conflicts(None), None), None);
    }

    #[test]
    fn failures_already_announced_by_the_run_carry_the_last_error_line() {
        let failing = State {
            last_outcome: Some(LastOutcome::Error),
            consecutive_failures: 3,
            notified: vec![NOTIFIED_FAILURES.to_owned()],
            ..State::default()
        };
        let retrying = State {
            last_outcome: Some(LastOutcome::Error),
            consecutive_failures: 2,
            ..State::default()
        };

        assert_eq!(
            alert_for(&failing, Some("quota exceeded")),
            Some(Alert::Failing("quota exceeded".into()))
        );
        assert_eq!(
            alert_for(&failing, None),
            Some(Alert::Failing(NO_ERROR_LINE.into()))
        );
        assert_eq!(alert_for(&retrying, None), None);
    }
}
