//! Notifications for events that need the user to act. Success never
//! notifies, and each event notifies once per transition (the caller keeps
//! the dedupe keys in `State::notified`).

use std::time::Duration;

use fastframe_instance::Slot;
use fastframe_notifications::{App, Client, Event, Notification};

use crate::behavior::{Behavior, ConflictLoser};
use crate::bisync::{FAILED_TO_BISYNC, Fault, failure_msgs, fault_of};
use crate::bisync_log::{Level, LogLine};
use crate::config::{Pair, split_remote};
use crate::run::NOTIFY_FAILURES_AFTER;
use crate::state::GateKind;

/// Single-instance slot of the GUI: the `run` asks it to show notifications
/// so a click can open the window. It never claims the slot.
pub const SLOT: &str = "syncfast";
const BISYNC_ABORTED: &str = "Bisync aborted";
const DELIVERY_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub pair_id: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alert {
    /// `detail` and `fault` are the gate's: the message of a `config_error`,
    /// or the failure a `needs_attention` names and the fault it is.
    Gate {
        kind: GateKind,
        detail: String,
        fault: Option<Fault>,
    },
    NewConflicts(u32),
    /// The newest error line of the log.
    Failing(String),
}

#[must_use]
pub fn notice(pair: &Pair, behavior: &Behavior, alert: &Alert) -> Notice {
    let name = &pair.name;
    let (title, body) = match alert {
        Alert::Gate {
            kind,
            detail,
            fault,
        } => match kind {
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
                format!(
                    "More than {}% of the files would be deleted. Review before continuing.",
                    behavior.max_delete
                ),
            ),
            GateKind::AllFilesChanged => (
                format!("{name}: review needed"),
                "Every file changed at once. Review before continuing.".to_owned(),
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
            GateKind::NeedsAttention => (
                format!("{name}: needs attention"),
                attention_body(*fault, detail).to_owned(),
            ),
        },
        Alert::NewConflicts(count) if behavior.conflict_loser == ConflictLoser::Delete => (
            format!("{name}: {count} conflicts resolved"),
            if behavior.versions {
                "The losing version was deleted. Local copies stay in the versions folder."
            } else {
                "The losing version was deleted and not kept."
            }
            .to_owned(),
        ),
        Alert::NewConflicts(count) => (
            format!("{name}: {count} new conflicts"),
            "Both versions were kept. Open syncfast to resolve.".to_owned(),
        ),
        Alert::Failing(message) => (
            format!("{name}: sync failing"),
            format!("Failed {NOTIFY_FAILURES_AFTER} times in a row. {message}"),
        ),
    };
    Notice {
        pair_id: pair.id.clone(),
        title,
        body,
    }
}

/// What a `needs_attention` gate tells the user. Gates saved before they
/// stored their `fault` read it back from `detail`, the failure message
/// that named it; neither means a sync that kept failing for too long.
#[must_use]
pub fn attention_body(fault: Option<Fault>, detail: &str) -> &'static str {
    match fault.or_else(|| fault_of(detail)) {
        Some(Fault::Account) => "Google Drive refused the account. Open syncfast for the fix.",
        Some(Fault::Quota) => "Google Drive is full.",
        Some(Fault::Permission) => "No permission to a file or folder on Google Drive.",
        Some(Fault::RemoteMissing) => "The folder on Google Drive is missing.",
        Some(Fault::LocalEmpty) => "The folder is empty. If it is on a drive, mount it.",
        Some(Fault::Auth) | None => "Syncing keeps failing. Open syncfast for details.",
    }
}

/// The message that says why a run failed, best first: the first failure
/// line that names a [`Fault`]; else the newest `error` or `critical` line
/// that is not rclone's generic `Bisync aborted` summary; else the newest
/// error line of any kind; else the newest `Failed to bisync` line.
#[must_use]
pub fn last_error_message(lines: &[LogLine]) -> Option<&str> {
    let is_error = |line: &&LogLine| matches!(line.level, Level::Error | Level::Critical);
    failure_msgs(lines)
        .find(|msg| fault_of(msg).is_some())
        .or_else(|| {
            lines
                .iter()
                .rev()
                .filter(is_error)
                .find(|line| !line.msg.starts_with(BISYNC_ABORTED))
                .map(|line| line.msg.as_str())
        })
        .or_else(|| {
            lines
                .iter()
                .rev()
                .find(|line| is_error(line))
                .map(|line| line.msg.as_str())
        })
        .or_else(|| {
            lines
                .iter()
                .rev()
                .find(|line| line.msg.starts_with(FAILED_TO_BISYNC))
                .map(|line| line.msg.as_str())
        })
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
        let entry = fastframe_desktop::desktop_entry("syncfast");
        let app = App::new("syncfast", "syncfast")
            .desktop_entry(&entry)
            .icon(&entry);
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
    use crate::bisync_log::fixtures::fixture;
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
            overrides: crate::settings::Overrides::default(),
        }
    }

    fn gate(kind: GateKind, detail: &str) -> Alert {
        Alert::Gate {
            kind,
            detail: detail.to_owned(),
            fault: None,
        }
    }

    fn text(alert: &Alert) -> (String, String) {
        let notice = notice(&pair(), &Behavior::default(), alert);
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
                "More than 50% of the files would be deleted. Review before continuing.".to_owned()
            )
        );
    }

    #[test]
    fn too_many_deletes_names_the_syncs_own_limit() {
        let behavior = Behavior {
            max_delete: 20,
            ..Behavior::default()
        };

        let notice = notice(&pair(), &behavior, &gate(GateKind::TooManyDeletes, ""));

        assert_eq!(
            notice.body,
            "More than 20% of the files would be deleted. Review before continuing."
        );
    }

    #[test]
    fn all_files_changed_asks_for_review() {
        assert_eq!(
            text(&gate(GateKind::AllFilesChanged, "")),
            (
                "Documents: review needed".to_owned(),
                "Every file changed at once. Review before continuing.".to_owned()
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
    fn needs_attention_names_what_the_fault_needs_from_the_user() {
        let cases = [
            (
                "Error 403: Google Drive API has not been used in project 1 before or it is disabled",
                "Google Drive refused the account. Open syncfast for the fix.",
            ),
            ("storageQuotaExceeded", "Google Drive is full."),
            (
                "insufficientFilePermissions",
                "No permission to a file or folder on Google Drive.",
            ),
            (
                "directory not found",
                "The folder on Google Drive is missing.",
            ),
            (
                "Empty current Path1 listing. Cannot sync to an empty directory: x",
                "The folder is empty. If it is on a drive, mount it.",
            ),
            (
                "Syncing has failed for over 6 hours: connection reset",
                "Syncing keeps failing. Open syncfast for details.",
            ),
        ];

        for (detail, body) in cases {
            assert_eq!(
                text(&gate(GateKind::NeedsAttention, detail)),
                ("Documents: needs attention".to_owned(), body.to_owned()),
                "{detail}"
            );
        }
    }

    #[test]
    fn needs_attention_reads_the_stored_fault_before_the_detail() {
        let alert = Alert::Gate {
            kind: GateKind::NeedsAttention,
            detail: "the message no longer names it".to_owned(),
            fault: Some(Fault::Quota),
        };

        assert_eq!(text(&alert).1, "Google Drive is full.");
    }

    #[test]
    fn a_conflict_that_deletes_the_loser_says_so_instead_of_claiming_both_were_kept() {
        let behavior = Behavior {
            conflict_loser: ConflictLoser::Delete,
            ..Behavior::default()
        };

        let notice = notice(&pair(), &behavior, &Alert::NewConflicts(2));

        assert_eq!(notice.title, "Documents: 2 conflicts resolved");
        assert_eq!(
            notice.body,
            "The losing version was deleted. Local copies stay in the versions folder."
        );
    }

    #[test]
    fn a_deleted_loser_of_a_sync_without_versions_says_it_was_not_kept() {
        let behavior = Behavior {
            conflict_loser: ConflictLoser::Delete,
            versions: false,
            ..Behavior::default()
        };

        let notice = notice(&pair(), &behavior, &Alert::NewConflicts(1));

        assert_eq!(notice.title, "Documents: 1 conflicts resolved");
        assert_eq!(notice.body, "The losing version was deleted and not kept.");
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

    #[test]
    fn without_error_lines_the_failed_to_bisync_notice_is_the_message() {
        for name in ["critical", "critical_retryable", "all_files_changed"] {
            let log = fixture(name);
            let closing = log
                .iter()
                .rev()
                .find(|line| line.msg.starts_with("Failed to bisync"))
                .unwrap()
                .msg
                .clone();
            let quiet: Vec<LogLine> = log
                .into_iter()
                .filter(|line| !matches!(line.level, Level::Error | Level::Critical))
                .collect();

            assert_eq!(last_error_message(&quiet), Some(closing.as_str()), "{name}");
        }
    }

    #[test]
    fn the_cause_wins_over_rclones_generic_bisync_aborted_summary() {
        for (name, cause) in [
            ("critical", "Bisync critical error: open "),
            (
                "critical_retryable",
                "Bisync critical error: cannot find prior Path1 or Path2 listings",
            ),
        ] {
            let log = fixture(name);

            let message = last_error_message(&log).unwrap();

            assert!(message.starts_with(cause), "{name}: {message}");
        }
    }

    #[test]
    fn an_aborted_summary_is_still_better_than_nothing() {
        let lines = parse_log("{\"level\":\"error\",\"msg\":\"Bisync aborted. Try again.\"}\n");

        assert_eq!(
            last_error_message(&lines),
            Some("Bisync aborted. Try again.")
        );
    }

    #[test]
    fn a_line_that_names_a_fault_is_the_message_even_before_later_errors() {
        let lines = parse_log(concat!(
            "{\"level\":\"error\",\"msg\":\"Empty current Path1 listing. Cannot sync to an empty directory: /x.lst-new\"}\n",
            "{\"level\":\"error\",\"msg\":\"Bisync critical error: empty current Path1 listing: /x.lst-new\"}\n",
            "{\"level\":\"error\",\"msg\":\"Bisync aborted. Error is retryable without --resync due to --resilient mode.\"}\n",
            "{\"level\":\"notice\",\"msg\":\"Failed to bisync: bisync aborted\"}\n",
        ));

        assert_eq!(
            last_error_message(&lines),
            Some("Empty current Path1 listing. Cannot sync to an empty directory: /x.lst-new")
        );
    }

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Notice>>>);

    impl Notifier for Recorder {
        fn notify(&self, notice: &Notice) {
            self.0.lock().unwrap().push(notice.clone());
        }
    }

    fn some_notice() -> Notice {
        notice(&pair(), &Behavior::default(), &Alert::NewConflicts(1))
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
