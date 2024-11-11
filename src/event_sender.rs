use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::env::{args, var};
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::ops::Sub;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use chrono::Local;
use colored::Colorize;
use directories::ProjectDirs;
use env_logger::{Builder, Target, WriteStyle};
use itertools::Itertools;
use nostr_sdk::prelude::*;
use nostr_sdk::TagStandard::Hashtag;
use regex::Regex;
use rustyline::config::Configurer;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;
use tokio::time::error::Elapsed;
use tokio::time::timeout;

use crate::helpers::*;
use crate::kinds::{Prio, BASIC_KINDS, PROPERTY_COLUMNS, PROP_KINDS, TRACKING_KIND};
use crate::task::{State, Task, TaskState};
use crate::tasks;
use crate::tasks::{PropertyCollection, StateFilter, TasksRelay};
use log::{debug, error, info, trace, warn, LevelFilter};
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
        let min = Timestamp::now().sub(UNDO_DELAY);
        {
            // Always flush if oldest event older than a minute or newer than now
            let borrow = self.queue.borrow();
            if borrow
                .iter()
                .any(|e| e.created_at < min || e.created_at > Timestamp::now())
            {
                drop(borrow);
                debug!("Flushing event queue because it is older than a minute");
                self.force_flush();
            }
        }
        let mut queue = self.queue.borrow_mut();
        Ok(event_builder.to_event(&self.keys).inspect(|event| {
            if event.kind == TRACKING_KIND
                && event.created_at > min
                && event.created_at < tasks::now()
            {
                // Do not send redundant movements
                queue.retain(|e| e.kind != TRACKING_KIND);
            }
            queue.push(event.clone());
        })?)
    }
    /// Sends all pending events
    fn force_flush(&self) {
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
