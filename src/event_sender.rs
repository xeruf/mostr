use std::cell::RefCell;
use std::ops::Sub;

use nostr_sdk::prelude::*;
use tokio::sync::mpsc::Sender;

use crate::kinds::TRACKING_KIND;
use crate::tasks;
use log::{debug, error, info, trace, warn};
use nostr_sdk::Event;

const UNDO_DELAY: u64 = 60;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum MostrMessage {
    Flush,
    NewRelay(Url),
    AddTasks(Url, Vec<Event>),
}

type Events = Vec<Event>;

#[derive(Debug, Clone)]
pub(crate) struct EventSender {
    pub(crate) url: Option<Url>,
    pub(crate) tx: Sender<MostrMessage>,
    pub(crate) keys: Keys,
    pub(crate) queue: RefCell<Events>,
}
impl EventSender {
    pub(crate) fn from(url: Option<Url>, tx: &Sender<MostrMessage>, keys: &Keys) -> Self {
        EventSender {
            url,
            tx: tx.clone(),
            keys: keys.clone(),
            queue: Default::default(),
        }
    }

    // TODO this direly needs testing
    pub(crate) fn submit(&self, event_builder: EventBuilder) -> Result<Event> {
        let event = event_builder.sign_with_keys(&self.keys)?;

        let time = event.created_at;
        {
            // Always flush if any event is newer or more than a minute older than the current event
            let borrow = self.queue.borrow();
            if borrow
                .iter()
                .any(|e| e.created_at < time.sub(UNDO_DELAY) || e.created_at > time)
            {
                drop(borrow);
                debug!("Flushing event queue because it is offset from the current event");
                self.force_flush();
            }
        }

        let mut queue = self.queue.borrow_mut();
        if event.kind == TRACKING_KIND {
            // Remove extraneous movements if tracking event is not at a custom time
            queue.retain(|e| e.kind != TRACKING_KIND);
        }
        queue.push(event.clone());
        Ok(event)
    }
    /// Sends all pending events
    pub(crate) fn force_flush(&self) {
        debug!("Flushing {} events from queue", self.queue.borrow().len());
        let values = self.clear();
        self.url.as_ref().map(|url| {
            self.tx
                .try_send(MostrMessage::AddTasks(url.clone(), values))
                .err()
                .map(|e| {
                    error!(
                        "Nostr communication thread failure, changes will not be persisted: {}",
                        e
                    )
                })
        });
    }
    /// Sends all pending events if there is a non-tracking event
    pub(crate) fn flush(&self) {
        if self
            .queue
            .borrow()
            .iter()
            .any(|event| event.kind != TRACKING_KIND)
        {
            self.force_flush()
        }
    }
    pub(crate) fn clear(&self) -> Events {
        trace!("Cleared queue: {:?}", self.queue.borrow());
        self.queue.replace(Vec::with_capacity(3))
    }
    pub(crate) fn pubkey(&self) -> PublicKey {
        self.keys.public_key()
    }
}
impl Drop for EventSender {
    fn drop(&mut self) {
        self.force_flush();
        debug!("Dropped {:?}", self);
    }
}
