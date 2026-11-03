//! Notifications for events that need the user to act. Success never
//! notifies, and each event notifies once per transition (the caller keeps
//! the dedupe keys in `State::notified`).

use std::time::Duration;

use fastframe_instance::Slot;
use fastframe_notifications::{App, Client, Event, Notification};

use crate::bisync_log::{Level, LogLine};
use crate::config::{Pair, split_remote};
use crate::state::GateKind;

/// Single-instance slot of the GUI: the `run` asks it to show notifications
/// so a click can open the window. It never claims the slot.
pub const SLOT: &str = "syncfast";
const DELIVERY_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub pair_id: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alert {
    /// `detail` is the gate's detail: the message of a `config_error`.
    Gate {
        kind: GateKind,
        detail: String,
    },
    NewConflicts(u32),
    /// The newest error line of the log.
    Failing(String),
}

#[must_use]
pub fn notice(pair: &Pair, alert: &Alert) -> Notice {
    let name = &pair.name;
    let (title, body) = match alert {
        Alert::Gate { kind, detail } => match kind {
            GateKind::NeedsReviewFirstRun => (
                format!("{name}: review needed"),
                "Review the first sync before it starts.".to_owned(),
            ),
            GateKind::NeedsReviewFiltersChanged => (
                format!("{name}: review needed"),
                "Filters changed. Review what will sync.".to_owned(),
            ),
            GateKind::NeedsReviewCriticalError => (
                format!("{name}: review needed"),
                "Sync stopped after an error. Review to resume.".to_owned(),
            ),
            GateKind::TooManyDeletes => (
                format!("{name}: many deletions"),
                "More than half of the files would be deleted. Review before continuing."
                    .to_owned(),
            ),
            GateKind::ConfigError => (format!("{name}: configuration error"), detail.clone()),
            GateKind::AuthError => (
                format!("{name}: account disconnected"),
                format!(
                    "Reconnect \"{}\" in Settings > Accounts.",
                    split_remote(&pair.remote).map_or(pair.remote.as_str(), |(account, _)| account)
                ),
            ),
            GateKind::LocalMissing => (
                format!("{name}: folder not found"),
                format!(
                    "{} is missing. Sync resumes when it is back.",
                    pair.local.display()
                ),
            ),
        },
        Alert::NewConflicts(count) => (
            format!("{name}: {count} new conflicts"),
            "Both versions were kept. Open syncfast to resolve.".to_owned(),
        ),
        Alert::Failing(message) => (
            format!("{name}: sync failing"),
            format!("Failed 3 times in a row. {message}"),
        ),
    };
    Notice {
        pair_id: pair.id.clone(),
        title,
        body,
    }
}

/// The message of the newest `error` or `critical` line, if there is one.
#[must_use]
pub fn last_error_message(lines: &[LogLine]) -> Option<&str> {
    lines
        .iter()
        .rev()
        .find(|line| matches!(line.level, Level::Error | Level::Critical))
        .map(|line| line.msg.as_str())
}

/// Where notices go. Delivery is best effort: it never fails the round.
pub trait Notifier: Send + Sync {
    fn notify(&self, notice: &Notice);
}

/// Shows a notice with the desktop's notification service, without actions.
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeNotifier;

impl Notifier for NativeNotifier {
    fn notify(&self, notice: &Notice) {
        let app = App::new("syncfast", "syncfast")
            .desktop_entry("syncfast")
            .icon("syncfast");
        let client = match Client::new(app) {
            Ok(client) => client,
            Err(err) => {
                log::warn!("cannot start the notification service: {err}");
                return;
            }
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let shown = client.show_with(
            Notification::new(notice.title.as_str()).body(notice.body.as_str()),
            move |event| {
                let _ = sender.send(event);
            },
        );
        if let Err(err) = shown {
            log::warn!("cannot show the notification: {err}");
            return;
        }
        // The service delivers on its own thread; leaving now could drop it.
        match receiver.recv_timeout(DELIVERY_WAIT) {
            Ok(Event::Failed(err)) => log::warn!("notification was not delivered: {err}"),
            Ok(_) => {}
            Err(_) => log::warn!("notification delivery did not finish in time"),
        }
    }
}

/// Asks the running GUI to show the notice (`notify <id>`, so a click opens
/// the window) and falls back to `fallback` when nobody answers.
pub struct DesktopNotifier {
    slot: Slot,
    fallback: Box<dyn Notifier>,
}

impl std::fmt::Debug for DesktopNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopNotifier")
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}

impl DesktopNotifier {
    #[must_use]
    pub fn new() -> Self {
        Self::with(Slot::new(SLOT), Box::new(NativeNotifier))
    }

    #[must_use]
    pub fn with(slot: Slot, fallback: Box<dyn Notifier>) -> Self {
        Self { slot, fallback }
    }
}

impl Default for DesktopNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier for DesktopNotifier {
    fn notify(&self, notice: &Notice) {
        match self.slot.send(&format!("notify {}", notice.pair_id)) {
            Ok(_) => {}
            Err(err) => {
                log::debug!("no syncfast window answered ({err}); notifying directly");
                self.fallback.notify(notice);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fastframe_instance::Claim;

    use super::*;
    use crate::bisync_log::parse_log;

    fn pair() -> Pair {
        Pair {
            id: "docs".into(),
            name: "Documents".into(),
            local: "/home/ada/Documents".into(),
            remote: "gdrive:Docs".into(),
            enabled: true,
            presets: vec![],
            filters: vec![],
        }
    }

    fn gate(kind: GateKind, detail: &str) -> Alert {
        Alert::Gate {
            kind,
            detail: detail.to_owned(),
        }
    }

    fn text(alert: &Alert) -> (String, String) {
        let notice = notice(&pair(), alert);
        (notice.title, notice.body)
    }

    #[test]
    fn review_gates_use_the_literal_texts() {
        assert_eq!(
            text(&gate(GateKind::NeedsReviewFirstRun, "")),
            (
                "Documents: review needed".to_owned(),
                "Review the first sync before it starts.".to_owned()
            )
        );
        assert_eq!(
            text(&gate(GateKind::NeedsReviewFiltersChanged, "")).1,
            "Filters changed. Review what will sync."
        );
        assert_eq!(
            text(&gate(GateKind::NeedsReviewCriticalError, "")).1,
            "Sync stopped after an error. Review to resume."
        );
    }

    #[test]
    fn too_many_deletes_names_the_deletions() {
        assert_eq!(
            text(&gate(GateKind::TooManyDeletes, "")),
            (
                "Documents: many deletions".to_owned(),
                "More than half of the files would be deleted. Review before continuing."
                    .to_owned()
            )
        );
    }

    #[test]
    fn config_error_shows_the_gate_message() {
        assert_eq!(
            text(&gate(
                GateKind::ConfigError,
                "local folder must be an absolute path"
            )),
            (
                "Documents: configuration error".to_owned(),
                "local folder must be an absolute path".to_owned()
            )
        );
    }

    #[test]
    fn auth_error_names_the_account_without_the_folder() {
        assert_eq!(
            text(&gate(GateKind::AuthError, "")),
            (
                "Documents: account disconnected".to_owned(),
                "Reconnect \"gdrive\" in Settings > Accounts.".to_owned()
            )
        );
    }

    #[test]
    fn local_missing_names_the_folder() {
        assert_eq!(
            text(&gate(GateKind::LocalMissing, "")),
            (
                "Documents: folder not found".to_owned(),
                "/home/ada/Documents is missing. Sync resumes when it is back.".to_owned()
            )
        );
    }

    #[test]
    fn conflicts_and_failures_use_the_literal_texts() {
        assert_eq!(
            text(&Alert::NewConflicts(2)),
            (
                "Documents: 2 new conflicts".to_owned(),
                "Both versions were kept. Open syncfast to resolve.".to_owned()
            )
        );
        assert_eq!(
            text(&Alert::Failing("Failed to bisync: quota".to_owned())),
            (
                "Documents: sync failing".to_owned(),
                "Failed 3 times in a row. Failed to bisync: quota".to_owned()
            )
        );
    }

    #[test]
    fn last_error_message_picks_the_newest_error_line_and_ignores_other_levels() {
        let lines = parse_log(concat!(
            "{\"level\":\"error\",\"msg\":\"first\"}\n",
            "{\"level\":\"critical\",\"msg\":\"second\"}\n",
            "{\"level\":\"notice\",\"msg\":\"third\"}\n",
            "not json at all\n",
        ));

        assert_eq!(last_error_message(&lines), Some("second"));
        assert_eq!(last_error_message(&lines[2..]), None);
    }

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Notice>>>);

    impl Notifier for Recorder {
        fn notify(&self, notice: &Notice) {
            self.0.lock().unwrap().push(notice.clone());
        }
    }

    fn some_notice() -> Notice {
        notice(&pair(), &Alert::NewConflicts(1))
    }

    #[test]
    fn without_a_running_gui_the_notice_goes_to_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::default();
        let notifier = DesktopNotifier::with(
            Slot::at(dir.path(), "syncfast-test"),
            Box::new(recorder.clone()),
        );

        notifier.notify(&some_notice());

        assert_eq!(*recorder.0.lock().unwrap(), vec![some_notice()]);
    }

    #[test]
    fn a_running_gui_gets_the_request_and_the_fallback_stays_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let slot = Slot::at(dir.path(), "syncfast-test");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let claim = slot.claim("show", move |request| {
            seen.lock().unwrap().push(request.to_owned());
            Some("ok".to_owned())
        });
        assert!(matches!(claim, Claim::First(_)));
        let recorder = Recorder::default();
        let notifier = DesktopNotifier::with(slot, Box::new(recorder.clone()));

        notifier.notify(&some_notice());

        assert_eq!(*requests.lock().unwrap(), vec!["notify docs".to_owned()]);
        assert!(recorder.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_gui_that_declines_the_request_sends_it_to_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let slot = Slot::at(dir.path(), "syncfast-test");
        let claim = slot.claim("show", |_| None);
        assert!(matches!(claim, Claim::First(_)));
        let recorder = Recorder::default();
        let notifier = DesktopNotifier::with(slot, Box::new(recorder.clone()));

        notifier.notify(&some_notice());

        assert_eq!(recorder.0.lock().unwrap().len(), 1);
    }
}
