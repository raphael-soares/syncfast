//! One running syncfast window process per user, and what other launches and
//! the `run` command ask of it.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use fastframe_instance::{Claim, Slot};
use fastframe_shell::Waker;
use syncfast_core::config::valid_id;
use syncfast_core::notify::{Notice, SLOT};

const REPLY: &str = "ok";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Show,
    ReloadThemes,
    /// The `run` command worded a notice about a sync; the window shows it.
    Notify(Notice),
    /// The user clicked the notification of this sync.
    Opened(String),
}

impl Request {
    /// The request a later launch or the `run` command sent, if it is one
    /// this version knows. `Opened` never travels: only a click makes it.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "show" => Some(Self::Show),
            "reload-themes" => Some(Self::ReloadThemes),
            _ => {
                let notice: Notice = serde_json::from_str(text.strip_prefix("notify ")?).ok()?;
                valid_id(&notice.pair_id).then_some(Self::Notify(notice))
            }
        }
    }
}

/// Requests waiting for the app, filled from other threads.
#[derive(Debug, Clone, Default)]
pub struct Inbox {
    queue: Arc<Mutex<VecDeque<Request>>>,
}

impl Inbox {
    pub fn push(&self, request: Request) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(request);
    }

    pub fn drain(&self) -> Vec<Request> {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect()
    }
}

/// Becomes the running copy, or hands "show" to the one already running.
/// While the claim lives, later requests land in `inbox` and wake the app.
#[must_use]
pub fn claim(inbox: &Inbox, waker: &Waker) -> Claim {
    claim_on(&Slot::new(SLOT), inbox, waker)
}

fn claim_on(slot: &Slot, inbox: &Inbox, waker: &Waker) -> Claim {
    let inbox = inbox.clone();
    let waker = waker.clone();
    slot.claim("show", move |text| {
        let request = Request::parse(text)?;
        inbox.push(request);
        waker.wake();
        Some(REPLY.to_owned())
    })
}

/// `syncfast-gui reload-themes`: asks the running copy to reread its themes.
///
/// # Errors
/// `NotFound` or `ConnectionRefused` when no copy is running.
pub fn ask_to_reload_themes() -> io::Result<()> {
    Slot::new(SLOT).send("reload-themes").map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(pair_id: &str) -> Notice {
        Notice {
            pair_id: pair_id.to_owned(),
            title: "Drive: new conflicts".to_owned(),
            body: "2 files kept both versions.\nOpen syncfast.".to_owned(),
        }
    }

    fn request(pair_id: &str) -> String {
        format!(
            "notify {}",
            serde_json::to_string(&notice(pair_id)).unwrap()
        )
    }

    #[test]
    fn known_verbs_become_requests() {
        assert_eq!(Request::parse("show"), Some(Request::Show));
        assert_eq!(Request::parse("reload-themes"), Some(Request::ReloadThemes));
        assert_eq!(
            Request::parse(&request("drive")),
            Some(Request::Notify(notice("drive")))
        );
    }

    #[test]
    fn a_synced_notice_reaches_the_app_as_the_core_worded_it() {
        let synced = Notice {
            pair_id: "drive".to_owned(),
            title: "Drive: synced".to_owned(),
            body: "2 uploaded, 1 deleted".to_owned(),
        };
        let text = format!("notify {}", serde_json::to_string(&synced).unwrap());

        assert_eq!(Request::parse(&text), Some(Request::Notify(synced)));
    }

    #[test]
    fn unknown_verbs_and_invalid_sync_ids_are_declined() {
        assert_eq!(Request::parse("quit"), None);
        assert_eq!(Request::parse("notify"), None);
        assert_eq!(Request::parse("notify drive"), None);
        assert_eq!(Request::parse("notify {\"pair_id\":\"x\"}"), None);
        assert_eq!(Request::parse(&request("../etc")), None);
        assert_eq!(Request::parse(&request("Drive")), None);
    }

    #[test]
    fn inbox_hands_requests_over_once_and_in_order() {
        let inbox = Inbox::default();
        inbox.push(Request::Show);
        inbox.push(Request::Opened("drive".into()));

        assert_eq!(
            inbox.drain(),
            [Request::Show, Request::Opened("drive".into())]
        );
        assert!(inbox.drain().is_empty());
    }

    #[test]
    fn running_copy_queues_what_a_later_launch_sends_and_declines_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let slot = Slot::at(dir.path(), "syncfast-test");
        let inbox = Inbox::default();
        let first = claim_on(&slot, &inbox, &Waker::default());
        assert!(matches!(first, Claim::First(_)));

        let reply = slot.send(&request("drive")).unwrap();
        let declined = slot.send("quit");

        assert_eq!(reply, REPLY);
        assert_eq!(
            declined.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(inbox.drain(), [Request::Notify(notice("drive"))]);
    }
}
