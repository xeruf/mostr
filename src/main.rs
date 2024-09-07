use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::env::{args, var};
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::iter::once;
use std::ops::Sub;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use chrono::Local;
use colored::Colorize;
use env_logger::{Builder, Target, WriteStyle};
use itertools::Itertools;
use log::{debug, error, info, trace, warn, LevelFilter};
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
use xdg::BaseDirectories;

use crate::helpers::*;
use crate::kinds::{BASIC_KINDS, PROPERTY_COLUMNS, PROP_KINDS, TRACKING_KIND};
use crate::task::{State, MARKER_DEPENDS};
use crate::tasks::{PropertyCollection, StateFilter, Tasks};

mod helpers;
mod task;
mod tasks;
mod kinds;

const UNDO_DELAY: u64 = 60;
const INACTVITY_DELAY: u64 = 200;
const LOCAL_RELAY_NAME: &str = "TEMP";

/// Turn a Result into an Option, showing a warning on error with optional prefix
macro_rules! or_warn {
    ($result:expr) => {
        match $result {
            Ok(value) => Some(value),
            Err(error) => {
                warn!("{}", error);
                None
            }
        }
    };
    ($result:expr, $msg:expr $(, $($arg:tt)*)?) => {
        match $result {
            Ok(value) => Some(value),
            Err(error) => {
                warn!("{}: {}", format!($msg, $($($arg)*)?), error);
                None
            }
        }
    }
}

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
            if event.kind == TRACKING_KIND {
                queue.retain(|e| {
                    e.kind != TRACKING_KIND
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
            self.tx.try_send(MostrMessage::AddTasks(url.clone(), values)).err().map(|e| {
                error!("Nostr communication thread failure, changes will not be persisted: {}", e)
            })
        });
    }
    /// Sends all pending events if there is a non-tracking event
    fn flush(&self) {
        if self.queue.borrow().iter().any(|event| event.kind != TRACKING_KIND) {
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
async fn main() -> Result<()> {
    let mut rl = DefaultEditor::new()?;
    rl.set_auto_add_history(true);

    let mut args = args().skip(1).peekable();
    let mut builder = if args.peek().is_some_and(|arg| arg == "--debug") {
        args.next();
        let mut builder = Builder::new();
        builder.filter(None, LevelFilter::Debug)
            //.filter(Some("mostr"), LevelFilter::Trace)
            .parse_default_env();
        builder
    } else {
        let mut builder = colog::default_builder();
        builder.filter(Some("nostr-relay-pool"), LevelFilter::Error);
        //.filter(Some("nostr-relay-pool::relay::internal"), LevelFilter::Off)
        builder
    };
    or_warn!(
        rl.create_external_writer().map(
            |wr| builder
                .filter(Some("rustyline"), LevelFilter::Warn)
                .write_style(WriteStyle::Always)
                .target(Target::Pipe(wr)))
    );
    builder.init();

    let config_dir = or_warn!(BaseDirectories::new(), "Could not determine config directory")
        .and_then(|d| or_warn!(d.create_config_directory("mostr"), "Could not create config directory"))
        .unwrap_or(PathBuf::new());
    let keysfile = config_dir.join("key");
    let relayfile = config_dir.join("relays");

    let keys = if let Ok(Ok(key)) = fs::read_to_string(&keysfile).map(|s| Keys::from_str(&s)) {
        key
    } else {
        warn!("Could not read keys from {}", keysfile.to_string_lossy());
        let line = rl.readline("Secret key? (leave blank to generate and save a new keypair) ")?;
        let keys = if line.is_empty() {
            info!("Generating and persisting new key");
            Keys::generate()
        } else {
            Keys::from_str(&line).inspect_err(|_| eprintln!())?
        };
        let mut file = match File::create_new(&keysfile) {
            Ok(file) => file,
            Err(e) => {
                let line = rl.readline(&format!("Overwrite {}? (enter anything to abort) ", keysfile.to_string_lossy()))?;
                if line.is_empty() {
                    File::create(&keysfile)?
                } else {
                    eprintln!();
                    Err(e)?
                }
            }
        };
        file.write_all(keys.secret_key().unwrap().to_string().as_bytes())?;
        keys
    };

    let client = ClientBuilder::new()
        .opts(Options::new().automatic_authentication(true))
        .signer(&keys)
        .build();
    info!("My public key: {}", keys.public_key());

    // TODO use NewRelay message for all relays
    match var("MOSTR_RELAY") {
        Ok(relay) => {
            or_warn!(client.add_relay(relay).await);
        }
        _ => match File::open(&relayfile).map(|f| BufReader::new(f).lines().flatten()) {
            Ok(lines) => {
                for line in lines {
                    or_warn!(client.add_relay(line).await);
                }
            }
            Err(e) => {
                warn!("Could not read relays file: {}", e);
                if let Ok(line) = rl.readline("Relay? ") {
                    let url = if line.contains("://") {
                        line
                    } else {
                        "wss://".to_string() + &line
                    };
                    or_warn!(client.add_relay(url.clone()).await).map(|bool| {
                        if bool {
                            or_warn!(fs::write(&relayfile, url));
                        }
                    });
                };
            }
        },
    }

    let mut notifications = client.notifications();
    client.connect().await;

    let sub1 = client.subscribe(vec![Filter::new().kinds(BASIC_KINDS)], None).await;
    info!("Subscribed to tasks with {:?}", sub1);

    let sub2 = client.subscribe(vec![Filter::new().kinds(PROP_KINDS)], None).await;
    info!("Subscribed to updates with {:?}", sub2);

    let metadata = var("USER").ok().map(
        |user| Metadata::new().name(user));
    let moved_metadata = metadata.clone();

    let (tx, mut rx) = mpsc::channel::<MostrMessage>(64);
    let tasks_for_url = |url: Option<Url>| Tasks::from(url, &tx, &keys, metadata.clone());
    let mut relays: HashMap<Option<Url>, Tasks> =
        client.relays().await.into_keys().map(|url| (Some(url.clone()), tasks_for_url(Some(url)))).collect();

    let sender = tokio::spawn(async move {
        let mut queue: Option<(Url, Vec<Event>)> = None;

        if let Some(meta) = moved_metadata.as_ref() {
            or_warn!(client.set_metadata(meta).await, "Unable to set metadata");
        }

        loop {
            let result_received = timeout(Duration::from_secs(INACTVITY_DELAY), rx.recv()).await;
            match result_received {
                Ok(Some(MostrMessage::NewRelay(url))) => {
                    if client.add_relay(&url).await.unwrap() {
                        match client.connect_relay(&url).await {
                            Ok(()) => info!("Connected to {url}"),
                            Err(e) => warn!("Unable to connect to relay {url}: {e}")
                        }
                    } else {
                        warn!("Relay {url} already added");
                    }
                }
                Ok(Some(MostrMessage::AddTasks(url, mut events))) => {
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
                Ok(Some(MostrMessage::Flush)) | Err(Elapsed { .. }) => if let Some((url, events)) = queue {
                    info!("Sending {} events to {url} due to {}", events.len(),
                        result_received.map_or("inactivity", |_| "flush message"));
                    client.batch_event_to(vec![url], events, RelaySendOptions::new()).await;
                    queue = None;
                }
                Ok(None) => {
                    debug!("Finalizing nostr communication thread because communication channel was closed");
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

    if relays.is_empty() {
        relays.insert(None, tasks_for_url(None));
    }
    let mut selected_relay: Option<Url> = relays.keys()
        .find_or_first(|url| url.as_ref().is_some_and(|u| u.scheme() == "wss"))
        .unwrap().clone();

    {
        let tasks = relays.get_mut(&selected_relay).unwrap();
        for argument in args {
            tasks.make_task(&argument);
        }
    }

    loop {
        trace!("All Root Tasks:\n{}", relays.iter().map(|(url, tasks)|
            format!("{}: [{}]",
                url.as_ref().map(ToString::to_string).unwrap_or(LOCAL_RELAY_NAME.to_string()),
                tasks.children_of(None).map(|id| tasks.get_task_title(id)).join("; "))).join("\n"));
        println!();
        let tasks = relays.get(&selected_relay).unwrap();
        let prompt = format!(
            "{} {}{}) ",
            selected_relay.as_ref().map_or(LOCAL_RELAY_NAME.to_string(), |url| url.to_string()).dimmed(),
            tasks.get_task_path(tasks.get_position()).bold(),
            tasks.get_prompt_suffix().italic(),
        );
        match rl.readline(&prompt) {
            Ok(input) => {
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
                        match relays.get_mut(&Some(relay_url.clone())) {
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
                let tasks = relays.get_mut(&selected_relay).unwrap();
                match op {
                    None => {
                        debug!("Flushing Tasks because of empty command");
                        tasks.flush();
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
                                    |task| println!("{}", task.description_events().map(|e| format!("{} {}", format_timestamp_local(&e.created_at), e.content)).join("\n")),
                                );
                                continue;
                            }
                            Some(arg) => {
                                if arg.len() < CHARACTER_THRESHOLD {
                                    warn!("Note needs at least {CHARACTER_THRESHOLD} characters!");
                                    continue;
                                }
                                tasks.make_note(arg)
                            }
                        }

                    Some('>') => {
                        tasks.update_state(arg_default, State::Done);
                        tasks.move_up();
                    }

                    Some('<') => {
                        tasks.update_state(arg_default, State::Closed);
                        tasks.move_up();
                    }

                    Some('&') => {
                        match arg {
                            None => tasks.undo(),
                            Some(text) => match text.parse::<u8>() {
                                Ok(int) => {
                                    tasks.move_back_by(int as usize);
                                }
                                _ => {
                                    if !tasks.move_back_to(text) {
                                        warn!("Did not find a match in history for \"{text}\"");
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    Some('@') => {
                        let success = match arg {
                            None => {
                                let today = Timestamp::now() - 80_000;
                                info!("Filtering for tasks from the last 22 hours");
                                tasks.set_filter_from(today)
                            }
                            Some(arg) => {
                                if arg == "@" {
                                    info!("Filtering for own tasks");
                                    tasks.set_filter_author(keys.public_key())
                                } else if let Ok(key) = PublicKey::from_str(arg) {
                                    let author = tasks.get_author(&key);
                                    info!("Filtering for tasks by {author}");
                                    tasks.set_filter_author(key)
                                } else {
                                    parse_hour(arg, 1)
                                        .or_else(|| parse_date(arg).map(|utc| utc.with_timezone(&Local)))
                                        .map(|time| {
                                            info!("Filtering for tasks from {}", format_datetime_relative(time));
                                            let threshold = time.to_utc().timestamp();
                                            tasks.set_filter_from(
                                                if let Some(t) = 0u64.checked_add_signed(threshold) {
                                                    Timestamp::from(t)
                                                } else { Timestamp::zero() })
                                        })
                                        .unwrap_or(false)
                                }
                            }
                        };
                        if !success {
                            continue;
                        }
                    }

                    Some('*') => {
                        match arg {
                            None => match tasks.get_position_ref() {
                                None => {
                                    info!("Filtering for bookmarked tasks");
                                    tasks.set_view_bookmarks();
                                }
                                Some(pos) => {
                                    info!("Toggling bookmark");
                                    or_warn!(tasks.toggle_bookmark(*pos));
                                }
                            },
                            Some(arg) => info!("Setting priority not yet implemented"),
                        }
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
                                if !arg.starts_with('|') {
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
                            None => {
                                warn!("First select a task to set its state!");
                                info!("Usage: ![(Open|Procedure|Pending|Done|Closed): ][Statename]");
                            }
                            Some(id) => {
                                'block: {
                                    if let Some((left, right)) = arg_default.split_once(": ") {
                                        if let Ok(state) = left.try_into() {
                                            tasks.set_state_for(id, right, state);
                                            break 'block;
                                        }
                                    }
                                    tasks.set_state_for_with(id, arg_default);
                                }
                                tasks.move_up();
                            }
                        }

                    Some('#') =>
                        tasks.set_tags(arg_default.split_whitespace().map(|s| Hashtag(s.to_string()).into())),

                    Some('+') =>
                        match arg {
                            Some(arg) => tasks.add_tag(arg.to_string()),
                            None => {
                                println!("Hashtags of all known tasks:\n{}", tasks.all_hashtags().join(" ").italic());
                                if tasks.has_tag_filter() {
                                    println!("Use # to remove tag filters and . to remove all filters.")
                                }
                                continue;
                            }
                        }

                    Some('-') =>
                        match arg {
                            Some(arg) => tasks.remove_tag(arg),
                            None => tasks.clear_filters()
                        }

                    Some('(') => {
                        if let Some(arg) = arg {
                            if tasks.track_from(arg) {
                                let (label, times) = tasks.times_tracked();
                                println!("{}\n{}", label.italic(),
                                         times.rev().take(15).collect_vec().iter().rev().join("\n"));
                            }
                            // TODO show history of author / pubkey
                        } else {
                            let (label, mut times) = tasks.times_tracked();
                            println!("{}\n{}", label.italic(), times.join("\n"));
                        }
                        continue;
                    }

                    Some(')') => {
                        match arg {
                            None => tasks.move_to(None),
                            Some(arg) => {
                                if parse_tracking_stamp(arg).and_then(|stamp| tasks.track_at(stamp, None)).is_some() {
                                    let (label, times) = tasks.times_tracked();
                                    println!("{}\n{}", label.italic(),
                                             times.rev().take(15).collect_vec().iter().rev().join("\n"));
                                }
                                // So the error message is not covered up
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
                            } else {
                                tasks.clear_filters();
                            }
                        } else if let Ok(depth) = slice.parse::<usize>() {
                            if pos != tasks.get_position_ref() {
                                tasks.move_to(pos.cloned());
                            }
                            tasks.set_depth(depth);
                        } else {
                            tasks.filter_or_create(pos.cloned().as_ref(), slice).map(|id| tasks.move_to(Some(id)));
                        }
                    }

                    Some('/') => if arg.is_none() {
                        tasks.move_to(None);
                    } else {
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
                        } else {
                            let mut transform: Box<dyn Fn(&str) -> String> = Box::new(|s: &str| s.to_string());
                            if !slice.chars().any(|c| c.is_ascii_uppercase()) {
                                // Smart-case - case-sensitive if any uppercase char is entered
                                transform = Box::new(|s| s.to_ascii_lowercase());
                            }

                            let filtered =
                                tasks.get_filtered(|t| {
                                    transform(&t.event.content).contains(slice) ||
                                        t.tags.iter().flatten().any(
                                            |tag| tag.content().is_some_and(|s| transform(s).contains(slice)))
                                });
                            if filtered.len() == 1 {
                                tasks.move_to(filtered.into_iter().next());
                            } else {
                                tasks.move_to(pos.cloned());
                                tasks.set_view(filtered);
                            }
                        }
                    }

                    _ =>
                        if Regex::new("^wss?://").unwrap().is_match(input.trim()) {
                            tasks.move_to(None);
                            if let Some((url, tasks)) = relays.iter().find(|(key, _)| key.as_ref().is_some_and(|url| url.as_str().starts_with(&input))) {
                                selected_relay.clone_from(url);
                                or_warn!(tasks.print_tasks());
                                continue;
                            }
                            or_warn!(Url::parse(&input), "Failed to parse url {}", input).map(|url| {
                                match tx.try_send(MostrMessage::NewRelay(url.clone())) {
                                    Err(e) => error!("Nostr communication thread failure, cannot add relay \"{url}\": {e}"),
                                    Ok(_) => {
                                        info!("Connecting to {url}");
                                        selected_relay = Some(url.clone());
                                        relays.insert(selected_relay.clone(), tasks_for_url(selected_relay.clone()));
                                    }
                                }
                            });
                            continue;
                        } else if input.contains('\n') {
                            input.split('\n').for_each(|line| {
                                if !line.trim().is_empty() {
                                    tasks.make_task(line);
                                }
                            });
                        } else {
                            tasks.filter_or_create(tasks.get_position().as_ref(), &input);
                        }
                }
                or_warn!(tasks.print_tasks());
            }
            Err(ReadlineError::Eof) => break,
            Err(ReadlineError::Interrupted) => break, // TODO exit if prompt was empty, or clear
            Err(e) => warn!("{}", e),
        }
    }
    println!();

    drop(tx);
    drop(relays);

    info!("Submitting pending updates...");
    or_warn!(sender.await);

    Ok(())
}
