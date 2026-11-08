//! Notifications for events that need the user to act, plus the opt-in
//! success ones. Each event notifies once per transition (the caller keeps
//! the dedupe keys in `State::notified`) and only when the [`Policy`] allows
//! it.

use std::time::Duration;

use fastframe_instance::Slot;
use fastframe_notifications::{App, Client, Event, Notification};

use crate::backup::config::BackupConfig;
use crate::backup::effects::BackupEvent;
use crate::behavior::{Behavior, ConflictLoser};
use crate::bisync::{FAILED_TO_BISYNC, Fault, failure_msgs, fault_of};
use crate::bisync_log::{Level, LogLine, RunSummary};
use crate::config::{Pair, split_remote};
use crate::state::GateKind;

/// Single-instance slot of the GUI: the `run` asks it to show notifications
/// so a click can open the window. It never claims the slot.
pub const SLOT: &str = "syncfast";
const BISYNC_ABORTED: &str = "Bisync aborted";
/// The `failures_after` a config that sets nothing has.
const DEFAULT_FAILURES_AFTER: u8 = 3;
const DELIVERY_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Notice {
    pub pair_id: String,
    pub title: String,
    pub body: String,
}

/// Which notifications a round sends, from `[notifications]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub needs_action: bool,
    pub conflicts: bool,
    /// Failed rounds in a row before the "sync failing" notice; 0 never sends it.
    pub failures_after: u8,
    pub success: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            needs_action: true,
            conflicts: true,
            failures_after: DEFAULT_FAILURES_AFTER,
            success: false,
        }
    }
}

impl Policy {
    #[must_use]
    pub fn allows(&self, alert: &Alert) -> bool {
        match alert {
            Alert::Gate { .. } => self.needs_action,
            Alert::NewConflicts(_) => self.conflicts,
            Alert::Failing(_) => self.failures_after > 0,
            Alert::Synced { .. } | Alert::AutoReviewed { .. } => self.success,
        }
    }

    /// Failed rounds in a row at which a sync counts as failing: the
    /// notification threshold, or the default when notifications for it are
    /// off, so the status label does not depend on a notification setting.
    #[must_use]
    pub fn failing_after(&self) -> u32 {
        match self.failures_after {
            0 => u32::from(DEFAULT_FAILURES_AFTER),
            after => u32::from(after),
        }
    }

    #[must_use]
    pub fn allows_backup(&self, event: &BackupEvent) -> bool {
        match event {
            BackupEvent::Gate { .. } | BackupEvent::KeyUnsaved => self.needs_action,
            BackupEvent::Failing { .. } => self.failures_after > 0,
            BackupEvent::Success { .. } => self.success,
        }
    }
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
    /// A round that followed a review syncfast confirmed by itself and
    /// deleted `deleted` files.
    AutoReviewed {
        deleted: u32,
    },
    /// What a round that changed files moved.
    Synced {
        uploaded: u32,
        downloaded: u32,
        deleted: u32,
    },
}

impl Alert {
    /// `None` for a round that changed nothing.
    #[must_use]
    pub fn synced(summary: &RunSummary) -> Option<Self> {
        let uploaded = summary.uploaded.unwrap_or(0);
        let downloaded = summary.downloaded.unwrap_or(0);
        let deleted = summary.deleted_local.unwrap_or(0) + summary.deleted_remote.unwrap_or(0);
        (uploaded + downloaded + deleted > 0).then_some(Self::Synced {
            uploaded,
            downloaded,
            deleted,
        })
    }
}

/// Why a round stopped for deleting too much. A mirror says what it had
/// already deleted (`detail`): `--max-delete` removes files up to the cap
/// before it stops.
pub(crate) fn too_many_deletes_text(behavior: &Behavior, detail: &str) -> String {
    let percent = behavior.max_delete;
    if !behavior.mode.is_mirror() {
        return format!("More than {percent}% of the files would be deleted.");
    }
    let done = if detail.is_empty() {
        "Nothing was deleted yet."
    } else {
        detail
    };
    format!("The mirror would delete more than {percent}% of the files. {done}")
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
            GateKind::NeedsReviewModeChanged => (
                format!("{name}: review needed"),
                "Sync mode changed. Review what will change.".to_owned(),
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
                    "{} Review before continuing.",
                    too_many_deletes_text(behavior, detail)
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
            format!(
                "Failed {} times in a row. {message}",
                behavior.notifications.failing_after()
            ),
        ),
        Alert::Synced {
            uploaded,
            downloaded,
            deleted,
        } => (
            format!("{name}: synced"),
            synced_body(*uploaded, *downloaded, *deleted),
        ),
        Alert::AutoReviewed { deleted } => (
            format!("{name}: reviewed automatically"),
            format!(
                "Deleted {deleted} {} after an automatic review.",
                if *deleted == 1 { "file" } else { "files" }
            ),
        ),
    };
    Notice {
        pair_id: pair.id.clone(),
        title,
        body,
    }
}

fn synced_body(uploaded: u32, downloaded: u32, deleted: u32) -> String {
    [
        (uploaded, "uploaded"),
        (downloaded, "downloaded"),
        (deleted, "deleted"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, what)| format!("{count} {what}"))
    .collect::<Vec<_>>()
    .join(", ")
}

/// The notification of one backup transition (spec phase 3, step 10). Its
/// `pair_id` is `backup:<id>`, which is what a click on it opens.
#[must_use]
pub fn backup_notice(backup: &BackupConfig, event: &BackupEvent) -> Notice {
    let name = &backup.name;
    let attention = format!("{name}: backup needs attention");
    let (title, body) = match event {
        BackupEvent::Gate {
            kind: GateKind::ConfigError,
            detail,
            ..
        } => (format!("{name}: configuration error"), detail.clone()),
        BackupEvent::Gate {
            kind: GateKind::AuthError,
            ..
        } => (
            format!("{name}: account disconnected"),
            format!(
                "Reconnect \"{}\" in Settings > Accounts.",
                split_remote(&backup.dest).map_or(backup.dest.as_str(), |(account, _)| account)
            ),
        ),
        BackupEvent::Gate {
            fault: Some(Fault::RepoDamaged),
            ..
        } => (
            format!("{name}: backup problem"),
            "The check found damaged data. Backups are paused until you review it.".to_owned(),
        ),
        BackupEvent::Gate { fault, detail, .. } => {
            (attention, backup_attention_body(backup, *fault, detail))
        }
        BackupEvent::Failing { failures, message } => (
            format!("{name}: backup failing"),
            format!("Failed {failures} times in a row. {message}"),
        ),
        BackupEvent::Success {
            changed_files,
            bytes_added,
        } => (
            format!("{name}: backup finished"),
            format!(
                "{changed_files} new or changed files, {} added.",
                size_text(*bytes_added)
            ),
        ),
        BackupEvent::KeyUnsaved => (
            format!("{name}: save the backup key"),
            "An agent created this backup. Open syncfast to save the key.".to_owned(),
        ),
    };
    Notice {
        pair_id: format!("backup:{}", backup.id),
        title,
        body,
    }
}

fn backup_attention_body(backup: &BackupConfig, fault: Option<Fault>, detail: &str) -> String {
    match fault {
        Some(Fault::Auth | Fault::Account) => {
            "Google Drive refused the account. Open syncfast for the fix.".to_owned()
        }
        Some(Fault::Quota) => "Google Drive is full.".to_owned(),
        Some(Fault::Permission) => "No permission to the backup folder on Google Drive.".to_owned(),
        Some(Fault::RemoteMissing | Fault::RepositoryMissing) => {
            "The backup folder is missing.".to_owned()
        }
        Some(Fault::DestinationMissing) => format!(
            "{} is not available. Plug it in to resume backups.",
            backup.dest
        ),
        Some(Fault::KeyMissing | Fault::WrongKey) => {
            "The key for this backup is missing on this computer. Open syncfast to enter it."
                .to_owned()
        }
        Some(Fault::SourceMissing) => {
            format!("{detail} is missing. Backups resume when it is back.")
        }
        Some(Fault::SourceEmpty) => format!("{detail} is empty. If it is on a drive, mount it."),
        Some(Fault::RepoDamaged | Fault::LocalEmpty | Fault::RemoteEmpty) | None => {
            "Backups keep failing. Open syncfast for details.".to_owned()
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Sends the notice of each event the policy allows. Best effort, like
/// every notification.
pub fn notify_backup(
    notifier: &dyn Notifier,
    policy: &Policy,
    backup: &BackupConfig,
    events: &[BackupEvent],
) {
    for event in events.iter().filter(|event| policy.allows_backup(event)) {
        notifier.notify(&backup_notice(backup, event));
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
        Some(Fault::RemoteEmpty) => "The folder on Google Drive is empty. Nothing was changed.",
        _ => "Syncing keeps failing. Open syncfast for details.",
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

/// Asks the running GUI to show the notice (`notify <json of the notice>`, so
/// a click opens the window) and falls back to `fallback` when nobody answers.
/// The GUI shows exactly what was decided here and decides nothing itself.
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
        let request = match serde_json::to_string(notice) {
            Ok(json) => format!("notify {json}"),
            Err(err) => {
                log::warn!("cannot encode the notification: {err}");
                return;
            }
        };
        match self.slot.send(&request) {
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
    fn a_stopped_mirror_says_what_it_had_already_deleted() {
        let behavior = Behavior {
            mode: crate::behavior::SyncMode::MirrorUp,
            max_delete: 20,
            ..Behavior::default()
        };
        let partial =
            "2 files were already deleted before the stop. Versions are off, so they are not kept.";

        let with = notice(&pair(), &behavior, &gate(GateKind::TooManyDeletes, partial));
        let without = notice(&pair(), &behavior, &gate(GateKind::TooManyDeletes, ""));

        assert_eq!(
            with.body,
            format!(
                "The mirror would delete more than 20% of the files. {partial} Review before continuing."
            )
        );
        assert_eq!(
            without.body,
            "The mirror would delete more than 20% of the files. Nothing was deleted yet. Review before continuing."
        );
    }

    #[test]
    fn an_automatic_review_says_how_many_files_it_deleted() {
        assert_eq!(
            text(&Alert::AutoReviewed { deleted: 12 }),
            (
                "Documents: reviewed automatically".to_owned(),
                "Deleted 12 files after an automatic review.".to_owned()
            )
        );
        assert_eq!(
            text(&Alert::AutoReviewed { deleted: 1 }).1,
            "Deleted 1 file after an automatic review."
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

        let sent = requests.lock().unwrap().clone();
        assert_eq!(
            sent,
            vec![format!(
                "notify {}",
                serde_json::to_string(&some_notice()).unwrap()
            )]
        );
        assert!(!sent[0].contains('\n'));
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

    fn summary(
        uploaded: u32,
        downloaded: u32,
        deleted_local: u32,
        deleted_remote: u32,
    ) -> RunSummary {
        RunSummary {
            uploaded: Some(uploaded),
            downloaded: Some(downloaded),
            deleted_local: Some(deleted_local),
            deleted_remote: Some(deleted_remote),
            ..RunSummary::default()
        }
    }

    #[test]
    fn a_round_that_changed_files_says_what_moved() {
        let alert = Alert::synced(&summary(3, 1, 1, 1)).unwrap();

        assert_eq!(
            text(&alert),
            (
                "Documents: synced".to_owned(),
                "3 uploaded, 1 downloaded, 2 deleted".to_owned()
            )
        );
    }

    #[test]
    fn a_round_that_changed_nothing_has_no_synced_alert() {
        assert_eq!(Alert::synced(&summary(0, 0, 0, 0)), None);
        assert_eq!(Alert::synced(&RunSummary::default()), None);
    }

    #[test]
    fn the_default_policy_keeps_the_notices_that_existed_before_the_settings() {
        let policy = Policy::default();

        assert!(policy.allows(&gate(GateKind::TooManyDeletes, "")));
        assert!(policy.allows(&Alert::NewConflicts(1)));
        assert!(policy.allows(&Alert::Failing("x".into())));
        assert!(!policy.allows(&Alert::synced(&summary(1, 0, 0, 0)).unwrap()));
        assert!(!policy.allows(&Alert::AutoReviewed { deleted: 1 }));
    }

    #[test]
    fn each_policy_flag_silences_only_its_own_alert() {
        let off = Policy {
            needs_action: false,
            conflicts: false,
            failures_after: 0,
            success: true,
        };

        assert!(!off.allows(&gate(GateKind::TooManyDeletes, "")));
        assert!(!off.allows(&Alert::NewConflicts(1)));
        assert!(!off.allows(&Alert::Failing("x".into())));
        assert!(off.allows(&Alert::synced(&summary(1, 0, 0, 0)).unwrap()));
        assert!(off.allows(&Alert::AutoReviewed { deleted: 1 }));
    }

    #[test]
    fn failures_after_zero_keeps_the_failing_label_at_the_default_threshold() {
        let off = Policy {
            failures_after: 0,
            ..Policy::default()
        };
        let five = Policy {
            failures_after: 5,
            ..Policy::default()
        };

        assert_eq!(off.failing_after(), 3);
        assert_eq!(five.failing_after(), 5);
    }

    #[test]
    fn each_sync_fault_has_its_own_attention_text() {
        let faults = [
            Fault::Account,
            Fault::Quota,
            Fault::Permission,
            Fault::RemoteMissing,
            Fault::LocalEmpty,
            Fault::RemoteEmpty,
        ];
        let mut texts: Vec<_> = faults
            .iter()
            .map(|fault| attention_body(Some(*fault), ""))
            .collect();
        texts.sort_unstable();
        texts.dedup();

        assert_eq!(texts.len(), faults.len());
        assert!(texts.iter().all(|text| *text != attention_body(None, "")));
    }

    fn backup() -> BackupConfig {
        BackupConfig {
            id: "docs".into(),
            name: "Documents".into(),
            paths: vec!["/home/ada/Documents".into()],
            dest: "gdrive:Backups/docs".into(),
            every: "daily".into(),
            keep: crate::backup::config::KeepSetting::default(),
            presets: vec![],
            filters: vec![],
            one_file_system: true,
            limit_upload: "off".into(),
            limit_download: "off".into(),
            enabled: true,
        }
    }

    fn backup_gate(kind: GateKind, fault: Option<Fault>, detail: &str) -> BackupEvent {
        BackupEvent::Gate {
            kind,
            detail: detail.into(),
            fault,
        }
    }

    fn attention(fault: Fault, detail: &str) -> Notice {
        backup_notice(
            &backup(),
            &backup_gate(GateKind::NeedsAttention, Some(fault), detail),
        )
    }

    #[test]
    fn a_backup_notice_opens_the_backup_and_names_it() {
        let notice = attention(Fault::Quota, "");

        assert_eq!(notice.pair_id, "backup:docs");
        assert_eq!(notice.title, "Documents: backup needs attention");
    }

    #[test]
    fn backup_faults_that_are_google_drive_problems_say_so() {
        assert_eq!(
            attention(Fault::Account, "").body,
            "Google Drive refused the account. Open syncfast for the fix."
        );
        assert_eq!(
            attention(Fault::Auth, "").body,
            attention(Fault::Account, "").body
        );
        assert_eq!(attention(Fault::Quota, "").body, "Google Drive is full.");
        assert_eq!(
            attention(Fault::Permission, "").body,
            "No permission to the backup folder on Google Drive."
        );
    }

    #[test]
    fn a_missing_backup_folder_has_one_text_for_both_faults() {
        for fault in [Fault::RemoteMissing, Fault::RepositoryMissing] {
            assert_eq!(attention(fault, "").body, "The backup folder is missing.");
        }
    }

    #[test]
    fn an_unavailable_destination_names_where_it_is() {
        assert_eq!(
            attention(Fault::DestinationMissing, "").body,
            "gdrive:Backups/docs is not available. Plug it in to resume backups."
        );
    }

    #[test]
    fn a_missing_or_wrong_key_asks_for_it_on_this_computer() {
        for fault in [Fault::KeyMissing, Fault::WrongKey] {
            assert_eq!(
                attention(fault, "Fatal: wrong password").body,
                "The key for this backup is missing on this computer. Open syncfast to enter it."
            );
        }
    }

    #[test]
    fn a_missing_or_empty_source_names_the_folder() {
        assert_eq!(
            attention(Fault::SourceMissing, "/media/disk/docs").body,
            "/media/disk/docs is missing. Backups resume when it is back."
        );
        assert_eq!(
            attention(Fault::SourceEmpty, "/media/disk/docs").body,
            "/media/disk/docs is empty. If it is on a drive, mount it."
        );
    }

    #[test]
    fn damaged_data_is_a_backup_problem_not_a_needs_attention() {
        let notice = attention(Fault::RepoDamaged, "");

        assert_eq!(notice.title, "Documents: backup problem");
        assert_eq!(
            notice.body,
            "The check found damaged data. Backups are paused until you review it."
        );
    }

    #[test]
    fn a_long_failure_without_a_fault_says_backups_keep_failing() {
        let notice = backup_notice(
            &backup(),
            &backup_gate(GateKind::NeedsAttention, None, "exit code 1"),
        );

        assert_eq!(notice.title, "Documents: backup needs attention");
        assert_eq!(
            notice.body,
            "Backups keep failing. Open syncfast for details."
        );
    }

    #[test]
    fn a_disconnected_account_names_the_remote() {
        let notice = backup_notice(&backup(), &backup_gate(GateKind::AuthError, None, ""));

        assert_eq!(notice.title, "Documents: account disconnected");
        assert_eq!(notice.body, "Reconnect \"gdrive\" in Settings > Accounts.");
    }

    #[test]
    fn a_configuration_error_shows_its_message() {
        let notice = backup_notice(
            &backup(),
            &backup_gate(GateKind::ConfigError, None, "keep.daily must be 0 or more"),
        );

        assert_eq!(notice.title, "Documents: configuration error");
        assert_eq!(notice.body, "keep.daily must be 0 or more");
    }

    #[test]
    fn failures_in_a_row_say_how_many_and_why() {
        let notice = backup_notice(
            &backup(),
            &BackupEvent::Failing {
                failures: 3,
                message: "connection reset".into(),
            },
        );

        assert_eq!(notice.title, "Documents: backup failing");
        assert_eq!(notice.body, "Failed 3 times in a row. connection reset");
    }

    #[test]
    fn a_finished_backup_says_how_much_changed() {
        let notice = backup_notice(
            &backup(),
            &BackupEvent::Success {
                changed_files: 12,
                bytes_added: 1_572_864,
            },
        );

        assert_eq!(notice.title, "Documents: backup finished");
        assert_eq!(notice.body, "12 new or changed files, 1.5 MiB added.");
    }

    #[test]
    fn small_sizes_are_in_bytes() {
        let notice = backup_notice(
            &backup(),
            &BackupEvent::Success {
                changed_files: 1,
                bytes_added: 900,
            },
        );

        assert_eq!(notice.body, "1 new or changed files, 900 B added.");
    }

    #[test]
    fn an_agent_created_backup_asks_to_save_the_key() {
        let notice = backup_notice(&backup(), &BackupEvent::KeyUnsaved);

        assert_eq!(notice.title, "Documents: save the backup key");
        assert_eq!(
            notice.body,
            "An agent created this backup. Open syncfast to save the key."
        );
    }

    fn sent(policy: Policy, events: &[BackupEvent]) -> Vec<Notice> {
        let recorder = Recorder::default();
        notify_backup(&recorder, &policy, &backup(), events);
        recorder.0.lock().unwrap().clone()
    }

    fn every_event() -> Vec<BackupEvent> {
        vec![
            backup_gate(GateKind::NeedsAttention, Some(Fault::Quota), ""),
            BackupEvent::Failing {
                failures: 3,
                message: String::new(),
            },
            BackupEvent::Success {
                changed_files: 1,
                bytes_added: 1,
            },
            BackupEvent::KeyUnsaved,
        ]
    }

    #[test]
    fn the_default_policy_sends_everything_but_success() {
        let titles: Vec<String> = sent(Policy::default(), &every_event())
            .into_iter()
            .map(|notice| notice.title)
            .collect();

        assert_eq!(
            titles,
            [
                "Documents: backup needs attention",
                "Documents: backup failing",
                "Documents: save the backup key",
            ]
        );
    }

    #[test]
    fn success_notifications_are_opt_in() {
        let policy = Policy {
            success: true,
            ..Policy::default()
        };

        assert!(
            sent(policy, &every_event())
                .iter()
                .any(|notice| notice.title == "Documents: backup finished")
        );
    }

    #[test]
    fn needs_action_off_silences_gates_and_the_unsaved_key() {
        let policy = Policy {
            needs_action: false,
            ..Policy::default()
        };

        let titles: Vec<String> = sent(policy, &every_event())
            .into_iter()
            .map(|notice| notice.title)
            .collect();

        assert_eq!(titles, ["Documents: backup failing"]);
    }

    #[test]
    fn failures_after_zero_silences_the_failing_notice() {
        let policy = Policy {
            failures_after: 0,
            ..Policy::default()
        };

        assert!(
            sent(policy, &every_event())
                .iter()
                .all(|notice| notice.title != "Documents: backup failing")
        );
    }
}
