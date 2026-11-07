//! Shows the notices the `run` command words and sends (`notify <notice>`),
//! so a click can open the window on that sync.

use fastframe_notifications::{App, Client, Event, Notification};
use syncfast_core::notify::Notice;

use crate::instance::{Inbox, Request};

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
