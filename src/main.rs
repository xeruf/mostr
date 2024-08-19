use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::env::{args, var};
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, stdin, stdout, Write};
use std::iter::once;
use std::ops::Sub;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::Sender;
use std::time::Duration;

use colored::{ColoredString, Colorize};
use env_logger::Builder;
use itertools::Itertools;
use log::{debug, error, info, LevelFilter, trace, warn};
use nostr_sdk::prelude::*;
use nostr_sdk::TagStandard::Hashtag;
use regex::Regex;
use xdg::BaseDirectories;

use crate::helpers::*;
use crate::kinds::{KINDS, PROP_KINDS, PROPERTY_COLUMNS, TRACKING_KIND};
use crate::task::{MARKER_DEPENDS, MARKER_PARENT, State};
use crate::tasks::{PropertyCollection, StateFilter, Tasks};

mod helpers;
mod task;
mod tasks;
mod kinds;

const UNDO_DELAY: u64 = 60;
const INACTVITY_DELAY: u64 = 200;

type Events = Vec<Event>;

#[derive(Debug, Clone)]
struct EventSender {
    url: Option<Url>,
    tx: Sender<MostrMessage>,
    keys: Keys,
    queue: RefCell<Events>,
}
impl EventSender {
    fn from(url: Option<Url>, tx: &Sender<MostrMessage>, keys: &Keys) -> Self {
        EventSender {
            url,
            tx: tx.clone(),
            keys: keys.clone(),
            queue: Default::default(),
        }
    }

    fn submit(&self, event_builder: EventBuilder) -> Result<Event> {
        {
            // Always flush if oldest event older than a minute or newer than now
            let borrow = self.queue.borrow();
            let min = Timestamp::now().sub(UNDO_DELAY);
            if borrow.iter().any(|e| e.created_at < min || e.created_at > Timestamp::now()) {
                drop(borrow);
                debug!("Flushing event queue because it is older than a minute");
                self.force_flush();
            }
        }
        let mut queue = self.queue.borrow_mut();
        Ok(event_builder.to_event(&self.keys).inspect(|event| {
            if event.kind.as_u16() == TRACKING_KIND {
                queue.retain(|e| {
                    e.kind.as_u16() != TRACKING_KIND
                });
            }
            queue.push(event.clone());
        })?)
    }
    /// Sends all pending events
    fn force_flush(&self) {
        debug!("Flushing {} events from queue", self.queue.borrow().len());
        let values = self.clear();
        self.url.as_ref().map(|url| {
            self.tx.send(MostrMessage::AddTasks(url.clone(), values)).inspect_err(|e| {
                error!("Nostr communication thread failure, changes will not be persisted: {}", e)
            })
        });
    }
    /// Sends all pending events if there is a non-tracking event
    fn flush(&self) {
        if self.queue.borrow().iter().any(|event| event.kind.as_u16() != TRACKING_KIND) {
            self.force_flush()
        }
    }
    fn clear(&self) -> Events {
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum MostrMessage {
    Flush,
    NewRelay(Url),
    AddTasks(Url, Vec<Event>),
}

#[tokio::main]
async fn main() {
    let mut args = args().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "--debug") {
        args.next();
        Builder::new()
            .filter(None, LevelFilter::Debug)
            //.filter(Some("mostr"), LevelFilter::Trace)
            .parse_default_env()
            .init();
    } else {
        colog::default_builder()
            .filter(Some("nostr-relay-pool"), LevelFilter::Error)
            //.filter(Some("nostr-relay-pool::relay::internal"), LevelFilter::Off)
            .init();
    }

    let config_dir = or_print(BaseDirectories::new())
        .and_then(|d| or_print(d.create_config_directory("mostr")))
        .unwrap_or(PathBuf::new());
    let keysfile = config_dir.join("key");
    let relayfile = config_dir.join("relays");

    let keys = match fs::read_to_string(&keysfile).map(|s| Keys::from_str(&s)) {
        Ok(Ok(key)) => key,
        _ => {
            warn!("Could not read keys from {}", keysfile.to_string_lossy());
            let keys = prompt("Secret Key?")
                .and_then(|s| or_print(Keys::from_str(&s)))
                .unwrap_or_else(|| Keys::generate());
            or_print(fs::write(&keysfile, keys.secret_key().unwrap().to_string()));
            keys
        }
    };

    let client = Client::new(&keys);
    info!("My public key: {}", keys.public_key());

    // TODO use NewRelay message for all relays
    match var("MOSTR_RELAY") {
        Ok(relay) => {
            or_print(client.add_relay(relay).await);
        }
        _ => match File::open(&relayfile).map(|f| BufReader::new(f).lines().flatten()) {
            Ok(lines) => {
                for line in lines {
                    or_print(client.add_relay(line).await);
                }
            }
            Err(e) => {
                warn!("Could not read relays file: {}", e);
                if let Some(line) = prompt("Relay?") {
                    let url = if line.contains("://") {
                        line
                    } else {
                        "wss://".to_string() + &line
                    };
                    or_print(client.add_relay(url.clone()).await).map(|bool| {
                        if bool {
                            or_print(fs::write(&relayfile, url));
                        }
                    });
                };
            }
        },
    }

    let sub1 = client.subscribe(vec![
        Filter::new().kinds(KINDS.into_iter().map(|k| Kind::from(k)))
    ], None).await;
    info!("Subscribed to tasks with {:?}", sub1);

    let mut notifications = client.notifications();
    client.connect().await;

    let sub2 = client.subscribe(vec![
        Filter::new().kinds(PROP_KINDS.into_iter().map(|k| Kind::from(k)))
    ], None).await;
    info!("Subscribed to updates with {:?}", sub2);

    let (tx, rx) = mpsc::channel::<MostrMessage>();
    let tasks_for_url = |url: Option<Url>| Tasks::from(url, &tx, &keys);
    let mut relays: HashMap<Url, Tasks> =
        client.relays().await.into_keys().map(|url| (url.clone(), tasks_for_url(Some(url)))).collect();

    let sender = tokio::spawn(async move {
        let mut queue: Option<(Url, Vec<Event>)> = None;

        loop {
            if let Ok(user) = var("USER") {
                let metadata = Metadata::new()
                    .name(user);
                //    .display_name("My Username")
                //    .about("Description")
                //    .picture(Url::parse("https://example.com/avatar.png")?)
                //    .banner(Url::parse("https://example.com/banner.png")?)
                //    .nip05("username@example.com")
                //    .lud16("yuki@getalby.com")
                //    .custom_field("custom_field", "my value");
                or_print(client.set_metadata(&metadata).await);
            }

            let result_received = rx.recv_timeout(Duration::from_secs(INACTVITY_DELAY));
            match result_received {
                Ok(MostrMessage::NewRelay(url)) => {
                    if client.add_relay(&url).await.unwrap() {
                        match client.connect_relay(&url).await {
                            Ok(()) => info!("Connected to {url}"),
                            Err(e) => warn!("Unable to connect to relay {url}: {e}")
                        }
                    } else {
                        warn!("Relay {url} already added");
                    }
                }
                Ok(MostrMessage::AddTasks(url, mut events)) => {
                    trace!("Queueing {:?}", &events);
                    if let Some((queue_url, mut queue_events)) = queue {
                        if queue_url == url {
                            queue_events.append(&mut events);
                            queue = Some((queue_url, queue_events));
                        } else {
                            info!("Sending {} events to {url} due to relay change", queue_events.len());
                            client.batch_event_to(vec![queue_url], queue_events, RelaySendOptions::new()).await;
                            queue = None;
                        }
                    }
                    if queue.is_none() {
                        events.reserve(events.len() + 10);
                        queue = Some((url, events))
                    }
                }
                Ok(MostrMessage::Flush) | Err(RecvTimeoutError::Timeout) => if let Some((url, events)) = queue {
                    info!("Sending {} events to {url} due to {}", events.len(),
                        result_received.map_or_else(|e| format!("{:?}", e), |m| format!("{:?}", m)));
                    client.batch_event_to(vec![url], events, RelaySendOptions::new()).await;
                    queue = None;
                }
                Err(err) => {
                    debug!("Finalizing nostr communication thread because of {:?}: {}", err, err);
                    break;
                }
            }
        }
        if let Some((url, events)) = queue {
            info!("Sending {} events to {url} before exiting", events.len());
            client.batch_event_to(vec![url], events, RelaySendOptions::new()).await;
        }
        info!("Shutting down nostr communication thread");
    });

    let mut local_tasks = Tasks::from(None, &tx, &keys);
    let mut selected_relay: Option<Url> = relays.keys().nth(0).cloned();

    {
        let tasks = selected_relay.as_ref().and_then(|url| relays.get_mut(&url)).unwrap_or_else(|| &mut local_tasks);
        for argument in args {
            tasks.make_task(&argument);
        }
    }

    let mut lines = stdin().lines();
    loop {
        trace!("All Root Tasks:\n{}", relays.iter().map(|(url, tasks)|
            format!("{}: [{}]", url, tasks.children_of(None).map(|id| tasks.get_task_title(id)).join("; "))).join("\n"));
        println!();
        let tasks = selected_relay.as_ref().and_then(|url| relays.get(url)).unwrap_or(&local_tasks);
        print!(
            "{} {}{}) ",
            selected_relay.as_ref().map_or("TEMP".to_string(), |url| url.to_string()).bright_black(),
            tasks.get_task_path(tasks.get_position()).bold(),
            tasks.get_prompt_suffix().italic(),
        );
        stdout().flush().unwrap();
        match lines.next() {
            Some(Ok(input)) => {
                let mut count = 0;
                while let Ok(notification) = notifications.try_recv() {
                    if let RelayPoolNotification::Event {
                        relay_url,
                        event,
                        ..
                    } = notification
                    {
                        debug!(
                            "At {} found {} kind {} content \"{}\" tags {:?}",
                            event.created_at, event.id, event.kind, event.content, event.tags.iter().map(|tag| tag.as_vec()).collect_vec()
                        );
                        match relays.get_mut(&relay_url) {
                            Some(tasks) => tasks.add(*event),
                            None => warn!("Event received from unknown relay {relay_url}: {:?}", *event)
                        }
                        count += 1;
                    }
                }
                if count > 0 {
                    info!("Received {count} Updates");
                }

                let mut iter = input.chars();
                let op = iter.next();
                let arg = if input.len() > 1 {
                    Some(input[1..].trim())
                } else {
                    None
                };
                let arg_default = arg.unwrap_or("");
                let tasks = selected_relay.as_ref().and_then(|url| relays.get_mut(&url)).unwrap_or_else(|| &mut local_tasks);
                match op {
                    None => {
                        debug!("Flushing Tasks because of empty command");
                        tasks.flush()
                    }

                    Some(':') => {
                        let next = iter.next();
                        if let Some(':') = next {
                            let str: String = iter.collect();
                            let result = str.split_whitespace().map(|s| s.to_string()).collect::<VecDeque<_>>();
                            if result.len() == 1 {
                                tasks.add_sorting_property(str.trim().to_string())
                            } else {
                                tasks.set_sorting(result)
                            }
                        } else if let Some(digit) = next.and_then(|s| s.to_digit(10)) {
                            let index = (digit as usize).saturating_sub(1);
                            let remaining = iter.collect::<String>().trim().to_string();
                            if remaining.is_empty() {
                                tasks.get_columns().remove_at(index);
                            } else {
                                tasks.get_columns().add_or_remove_at(remaining, index);
                            }
                        } else if let Some(arg) = arg {
                            tasks.get_columns().add_or_remove(arg.to_string());
                        } else {
                            println!("{}", PROPERTY_COLUMNS);
                            continue;
                        }
                    }

                    Some(',') =>
                        match arg {
                            None => {
                                tasks.get_current_task().map_or_else(
                                    || info!("With a task selected, use ,NOTE to attach NOTE and , to list all its notes"),
                                    |task| println!("{}", task.description_events().map(|e| format!("{} {}", local_datetimestamp(&e.created_at), e.content)).join("\n")),
                                );
                                continue;
                            }
                            Some(arg) => tasks.make_note(arg),
                        }

                    Some('>') => {
                        tasks.update_state(&arg_default, State::Done);
                        tasks.move_up();
                    }

                    Some('<') => {
                        tasks.update_state(&arg_default, State::Closed);
                        tasks.move_up();
                    }

                    Some('&') => {
                        tasks.undo();
                    }

                    Some('@') => {
                        let author = arg.and_then(|a| PublicKey::from_str(a).ok()).unwrap_or_else(|| keys.public_key());
                        info!("Filtering for events by {author}");
                        tasks.set_filter(
                            tasks.filtered_tasks(tasks.get_position_ref())
                                .filter(|t| t.event.pubkey == author)
                                .map(|t| t.event.id)
                                .collect()
                        )
                    }

                    Some('*') => {
                        info!("Setting priority not yet implemented")
                    }

                    Some('|') =>
                        match arg {
                            None => match tasks.get_position() {
                                None => {
                                    tasks.set_state_filter(
                                        StateFilter::State(State::Procedure.to_string()));
                                }
                                Some(id) => {
                                    tasks.set_state_for(id, "", State::Procedure);
                                }
                            },
                            Some(arg) => 'arm: {
                                if arg.chars().next() != Some('|') {
                                    if let Some(pos) = tasks.get_position() {
                                        tasks.move_up();
                                        tasks.make_task_with(
                                            arg,
                                            once(tasks.make_event_tag_from_id(pos, MARKER_DEPENDS)),
                                            true);
                                        break 'arm;
                                    }
                                }
                                let arg: String = arg.chars().skip_while(|c| c == &'|').collect();
                                tasks.make_task_and_enter(&arg, State::Procedure);
                            }
                        }

                    Some('?') => {
                        match arg {
                            None => tasks.set_state_filter(StateFilter::Default),
                            Some("?") => tasks.set_state_filter(StateFilter::All),
                            Some(arg) => tasks.set_state_filter(StateFilter::State(arg.to_string())),
                        }
                    }

                    Some('!') =>
                        match tasks.get_position() {
                            None => warn!("First select a task to set its state!"),
                            Some(id) => {
                                tasks.set_state_for_with(id, arg_default);
                                tasks.move_up();
                            }
                        }

                    Some('#') =>
                        match arg {
                            Some(arg) => tasks.set_tags(arg.split_whitespace().map(|s| Hashtag(s.to_string()).into())),
                            None => {
                                println!("Hashtags of all known tasks:\n{}", tasks.all_hashtags().join(" "));
                                continue;
                            }
                        }

                    Some('+') =>
                        match arg {
                            Some(arg) => tasks.add_tag(arg.to_string()),
                            None => tasks.clear_filter()
                        }

                    Some('-') =>
                        match arg {
                            Some(arg) => tasks.remove_tag(arg),
                            None => tasks.clear_filter()
                        }

                    Some('(') => {
                        if let Some(arg) = arg {
                            if tasks.track_from(arg) {
                                let (label, times) = tasks.times_tracked();
                                println!("{}\n{}", label.italic(), times.rev().take(15).join("\n"));
                            }
                        } else {
                            let (label, mut times) = tasks.times_tracked();
                            println!("{}\n{}", label.italic(), times.join("\n"));
                        }
                        continue;
                    }

                    Some(')') => {
                        tasks.move_to(None);
                        if let Some(arg) = arg {
                            if !tasks.track_from(arg) {
                                continue;
                            }
                        }
                    }

                    Some('.') => {
                        let mut dots = 1;
                        let mut pos = tasks.get_position_ref();
                        for _ in iter.take_while(|c| c == &'.') {
                            dots += 1;
                            pos = tasks.get_parent(pos);
                        }

                        let slice = input[dots..].trim();
                        if slice.is_empty() {
                            tasks.move_to(pos.cloned());
                            if dots > 1 {
                                info!("Moving up {} tasks", dots - 1)
                            }
                        } else if let Ok(depth) = slice.parse::<i8>() {
                            if pos != tasks.get_position_ref() {
                                tasks.move_to(pos.cloned());
                            }
                            tasks.set_depth(depth);
                        } else {
                            tasks.filter_or_create(pos.cloned().as_ref(), slice).map(|id| tasks.move_to(Some(id)));
                        }
                    }

                    Some('/') => {
                        let mut dots = 1;
                        let mut pos = tasks.get_position_ref();
                        for _ in iter.take_while(|c| c == &'/') {
                            dots += 1;
                            pos = tasks.get_parent(pos);
                        }

                        let slice = input[dots..].trim();
                        if slice.is_empty() {
                            tasks.move_to(pos.cloned());
                            if dots > 1 {
                                info!("Moving up {} tasks", dots - 1)
                            }
                        } else if let Ok(depth) = slice.parse::<i8>() {
                            if pos != tasks.get_position_ref() {
                                tasks.move_to(pos.cloned());
                            }
                            tasks.set_depth(depth);
                        } else {
                            let mut transform: Box<dyn Fn(&str) -> String> = Box::new(|s: &str| s.to_string());
                            if slice.chars().find(|c| c.is_ascii_uppercase()).is_none() {
                                // Smart-case - case-sensitive if any uppercase char is entered
                                transform = Box::new(|s| s.to_ascii_lowercase());
                            }

                            let filtered = tasks.filtered_tasks(pos)
                                .filter(|t| {
                                    transform(&t.event.content).contains(slice) || t.tags.iter().flatten().any(|tag|
                                    tag.content().is_some_and(|s| transform(s).contains(slice))
                                    )
                                })
                                .map(|t| t.event.id)
                                .collect_vec();
                            if filtered.len() == 1 {
                                tasks.move_to(filtered.into_iter().nth(0));
                            } else {
                                tasks.move_to(pos.cloned());
                                tasks.set_filter(filtered);
                            }
                        }
                    }

                    _ =>
                        if Regex::new("^wss?://").unwrap().is_match(&input.trim()) {
                            tasks.move_to(None);
                            if let Some((url, tasks)) = relays.iter().find(|(key, _)| key.as_str().starts_with(&input)) {
                                selected_relay = Some(url.clone());
                                or_print(tasks.print_tasks());
                                continue;
                            }
                            match Url::parse(&input) {
                                Err(e) => warn!("Failed to parse url \"{input}\": {}", e),
                                Ok(url) => match tx.send(MostrMessage::NewRelay(url.clone())) {
                                    Err(e) => error!("Nostr communication thread failure, cannot add relay \"{url}\": {e}"),
                                    Ok(_) => {
                                        info!("Connecting to {url}");
                                        selected_relay = Some(url.clone());
                                        relays.insert(url.clone(), tasks_for_url(Some(url)));
                                    }
                                }
                            }
                            continue;
                        } else {
                            tasks.filter_or_create(tasks.get_position().as_ref(), &input);
                        }
                }
                or_print(tasks.print_tasks());
            }
            Some(Err(e)) => warn!("{}", e),
            None => break,
        }
    }
    println!();

    drop(tx);
    drop(local_tasks);
    drop(relays);

    info!("Submitting pending updates...");
    or_print(sender.await);
}
