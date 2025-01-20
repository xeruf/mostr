use std::collections::{HashMap, VecDeque};
use std::env::{args, var};
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::iter::once;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use crate::event_sender::MostrMessage;
use crate::hashtag::Hashtag;
use crate::helpers::*;
use crate::kinds::{format_tag_basic, match_event_tag, Prio, BASIC_KINDS, PROPERTY_COLUMNS, PROP_KINDS};
use crate::task::{State, StateChange, Task, MARKER_PROPERTY};
use crate::tasks::{referenced_event, PropertyCollection, StateFilter, TasksRelay};
use chrono::{DateTime, Local, TimeZone, Utc};
use colored::Colorize;
use directories::ProjectDirs;
use env_logger::{Builder, Target, WriteStyle};
use itertools::Itertools;
use keyring::Entry;
use log::{debug, error, info, trace, warn, LevelFilter};
use nostr_sdk::prelude::*;
use nostr_sdk::serde_json::Serializer;
use regex::Regex;
use rustyline::config::Configurer;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use tokio::sync::mpsc;
use tokio::time::error::Elapsed;
use tokio::time::timeout;

mod helpers;
mod task;
mod tasks;
mod kinds;
mod event_sender;
mod hashtag;

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

fn read_keys(readline: &mut DefaultEditor) -> Result<Keys> {
    let keys_entry = Entry::new("mostr", "keys")?;
    if let Ok(pass) = keys_entry.get_secret() {
        return Ok(SecretKey::from_slice(&pass).map(|s| Keys::new(s))
            .inspect_err(|e| eprintln!("Invalid key in keychain: {e}"))?);
    }
    let line = readline.readline("Secret key? (leave blank to generate and save a new keypair) ")?;
    let keys = if line.is_empty() {
        info!("Generating and persisting new key");
        Keys::generate()
    } else {
        Keys::from_str(&line)
            .inspect_err(|e| eprintln!("Invalid key provided: {e}"))?
    };
    or_warn!(keys_entry.set_secret(keys.secret_key().as_secret_bytes()),
        "Could not persist keys");
    Ok(keys)
}


#[tokio::main]
async fn main() -> Result<()> {
    println!("Running Mostr Version {}", env!("CARGO_PKG_VERSION"));

    let mut debug = false;
    let mut args = args().skip(1).peekable();
    let mut builder = if args.peek().is_some_and(|arg| arg == "--debug") {
        debug = true;
        args.next();
        let mut builder = Builder::new();
        builder.filter(None, LevelFilter::Debug)
            .filter(Some("mostr"), LevelFilter::Trace)
            .parse_default_env();
        builder
    } else {
        let mut builder = colog::default_builder();
        builder.filter(Some("nostr-relay-pool"), LevelFilter::Error);
        //.filter(Some("nostr-relay-pool::relay::internal"), LevelFilter::Off)
        builder
    };

    let mut rl = DefaultEditor::new()?;
    rl.set_auto_add_history(true);
    or_warn!(
        rl.create_external_writer().map(
            |wr| builder
                // Without this filter at least at Info, the program hangs
                .filter(Some("rustyline"), LevelFilter::Warn)
                .write_style(WriteStyle::Always)
                .target(Target::Pipe(wr)))
    );
    builder.init();

    let config_dir =
        ProjectDirs::from("", "", "mostr")
            .map(|p| {
                let config = p.config_dir();
                debug!("Config Directory: {:?}", config);
                or_warn!(fs::create_dir_all(config), "Could not create config directory '{:?}'", config);
                config.to_path_buf()
            })
            .unwrap_or_else(|| {
                warn!("Could not determine config directory, using current directory");
                PathBuf::new()
            });

    let key_file = config_dir.join("key");
    if let Ok(Some(keys)) = fs::read_to_string(key_file.as_path()).map(|s| or_warn!(Keys::from_str(&s.trim()))) {
        info!("Migrating private key from plaintext file {}", key_file.to_string_lossy());
        or_warn!(Entry::new("mostr", "keys")
            .and_then(|e| e.set_secret(keys.secret_key().as_secret_bytes()))
            .inspect(|_| { or_warn!(fs::remove_file(key_file)); }));
    }

    let keys = read_keys(&mut rl)?;
    let relays_file = config_dir.join("relays");

    let client = ClientBuilder::new()
        .opts(Options::new()
            .automatic_authentication(true)
            .notification_channel_size(16384)
        )
        .signer(keys.clone())
        .build();
    info!("My public key: {}", keys.public_key());

    // TODO use NewRelay message for all relays
    match var("MOSTR_RELAY") {
        Ok(relay) => {
            or_warn!(client.add_relay(relay).await);
        }
        _ => match File::open(&relays_file).map(|f| BufReader::new(f).lines().flatten()) {
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
                            or_warn!(fs::write(&relays_file, url));
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

    if args.peek().is_some_and(|arg| arg == "--watch-events") {
        loop {
            match notifications.recv().await {
                Ok(notification) => {
                    if let RelayPoolNotification::Event { event, .. } = notification {
                        println!("At {} found {} kind {} content \"{}\"", event.created_at, event.id, event.kind, event.content);
                    }
                }
                Err(e) => {
                    println!("Aborting due to {:?}", e);
                    return Ok(());
                }
            }
        }
    }

    let metadata = Metadata::new()
        .name(whoami::username())
        .display_name(whoami::realname());
    let metadata_clone = metadata.clone();

    let (tx, mut rx) = mpsc::channel::<MostrMessage>(64);
    let tasks_for_url = |url: Option<RelayUrl>| TasksRelay::from(url, &tx, &keys, Some(metadata.clone()));
    let mut relays: HashMap<Option<RelayUrl>, TasksRelay> =
        client.relays().await.into_keys().map(|url| (Some(url.clone()), tasks_for_url(Some(url)))).collect();

    let sender = tokio::spawn(async move {
        or_warn!(client.set_metadata(&metadata_clone).await, "Unable to set metadata");

        'receiver: loop {
            match rx.recv().await {
                Some(MostrMessage::NewRelay(url)) => {
                    if client.add_relay(&url).await.unwrap() {
                        match client.connect_relay(&url).await {
                            Ok(()) => info!("Connected to {url}"),
                            Err(e) => warn!("Unable to connect to relay {url}: {e}")
                        }
                    } else {
                        warn!("Relay {url} already added");
                    }
                }
                Some(MostrMessage::SendTask(url, event)) => {
                    trace!("Sending {:?}", &event);
                    let id = event.id;
                    let url_str = url.as_str_without_trailing_slash().to_string();
                    if let Err(e) = client.send_event_to(vec![url], event.clone()).await {
                        let url_s = url_str.split("//").last().map(ToString::to_string).unwrap_or(url_str);
                        if debug {
                            debug!("Error sending event: {:?}", e);
                            continue 'receiver;
                        }
                        let path = format!("failed-events-{}/", url_s);
                        let dir = fs::create_dir_all(&path).map(|_| path).unwrap_or("".to_string());
                        let filename = dir.to_string() + &id.to_string();
                        match File::create(&filename).and_then(|mut f|
                            f.write_all(or_warn!(serde_json::to_string_pretty(&event), "Failed serializing event for file writing").unwrap_or(String::new()).as_bytes())) {
                            Ok(_) => error!("Failed sending update, saved a copy at {filename}: {:?}", e),
                            Err(fe) => error!("Failed sending update {:?} and saving copy of event {:?}", e, fe),
                        }
                    }
                }
                None => {
                    debug!("Finalizing nostr communication thread because communication channel was closed");
                    break 'receiver;
                }
            }
        }
        info!("Shutting down nostr communication thread");
    });

    if relays.is_empty() {
        relays.insert(None, tasks_for_url(None));
    }
    let mut selected_relay: Option<RelayUrl> = relays.keys().next().unwrap().clone();

    {
        let tasks = relays.get_mut(&selected_relay).unwrap();
        for argument in args {
            tasks.make_task(&argument);
        }
    }

    'repl: loop {
        println!();
        let tasks = relays.get(&selected_relay).unwrap();
        let prompt = format!(
            "{}{} {}{}{}",
            selected_relay.as_ref().map_or(LOCAL_RELAY_NAME.to_string(), |url| url.to_string()).dimmed(),
            tasks.pubkey_str().map_or(String::new(), |s| format!(" @{s}")),
            tasks.get_task_path(tasks.get_position()).bold(),
            tasks.get_prompt_suffix().italic(),
            "❯ ".dimmed()
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
                            event.created_at, event.id, event.kind, event.content, event.tags.iter().map(|tag| tag.as_slice()).collect_vec()
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
                } else {
                    relays.values_mut().for_each(|tasks| tasks.process_overflow());
                }

                let tasks = relays.get_mut(&selected_relay).unwrap();

                let operator = input.chars().next();
                let mut command = input;
                match operator {
                    None => {
                        debug!("Flushing Tasks because of empty command");
                        tasks.flush();
                        println!("{}", tasks);
                        continue 'repl;
                    }
                    Some('@') => {}
                    Some(_) => {
                        if let Some((left, arg)) = command.split_once("@") {
                            if let Some(time) = parse_hour(arg, 20)
                                .or_else(|| parse_date(arg).map(|utc| utc.with_timezone(&Local))) {
                                command = left.to_string();
                                tasks.custom_time = Some(time.to_timestamp());
                            }
                        }
                    }
                }

                let arg = if command.len() > 1 {
                    Some(command[1..].trim())
                } else {
                    None
                };
                let arg_default = arg.unwrap_or("");
                match operator {
                    Some(':') => {
                        if command.starts_with("://") {
                            if let Some((url, tasks)) = relays.iter().find(|(key, _)| key.as_ref().is_some_and(|url| url.as_str().contains(&command))) {
                                selected_relay.clone_from(url);
                                println!("{}", tasks);
                                continue 'repl;
                            }
                            warn!("No connected relay contains {:?}", command);
                            continue 'repl;
                        }

                        let mut iter = arg_default.chars();
                        let next = iter.next();
                        let remaining = iter.collect::<String>().trim().to_string();
                        if let Some(':') = next {
                            let props = remaining.split_whitespace().map(|s| s.to_string()).collect::<VecDeque<_>>();
                            if props.len() == 1 {
                                tasks.add_sorting_property(remaining)
                            } else {
                                tasks.set_sorting(props)
                            }
                        } else if let Some(digit) = next.and_then(|s| s.to_digit(10)) {
                            let index = (digit as usize).saturating_sub(1);
                            if remaining.is_empty() {
                                tasks.get_columns().remove_at(index);
                            } else {
                                tasks.get_columns().add_or_remove_at(remaining, index);
                            }
                        } else if let Some(arg) = arg {
                            tasks.get_columns().add_or_remove(arg.to_string());
                        } else {
                            println!("{}", PROPERTY_COLUMNS);
                            continue 'repl;
                        }
                    }

                    Some(',') =>
                        match arg {
                            None => {
                                if let Some(task) = tasks.get_current_task() {
                                    println!("Change History for {}:", task.get_id());
                                    for e in task.all_events() {
                                        println!("{} {} [{}]",
                                                 format_timestamp_full(&e.created_at),
                                                 match State::try_from(e.kind) {
                                                     Ok(state) => {
                                                         format!("State: {state}{}",
                                                                 if e.content.is_empty() { String::new() } else { format!(" - {}", e.content) })
                                                     }
                                                     Err(_) => {
                                                         e.content.to_string()
                                                     }
                                                 },
                                                 e.tags.iter().filter_map(|t| {
                                                     match match_event_tag(t) {
                                                         Some(et) =>
                                                             Some(et).take_if(|et| et.marker.as_ref().is_some_and(|m| m != MARKER_PROPERTY))
                                                                 .map(|et| format!("{}: {}", et.marker.as_ref().unwrap(), tasks.get_relative_path(et.id))),
                                                         None =>
                                                             Some(format_tag_basic(t)),
                                                     }
                                                 }).join(", ")
                                        )
                                    }
                                    continue 'repl;
                                } else {
                                    info!("With a task selected, use ,NOTE to attach NOTE and , to list all its updates");
                                    tasks.recurse_activities = !tasks.recurse_activities;
                                    info!("Toggled activities recursion to {}", tasks.recurse_activities);
                                }
                            }
                            Some(arg) => {
                                if arg.trim().len() < 2 {
                                    warn!("Needs at least 2 characters!");
                                    continue 'repl;
                                }
                                tasks.make_note(arg);
                            }
                        }

                    Some('>') => {
                        tasks.update_state(arg_default, State::Done);
                        if tasks.custom_time.is_none() { tasks.move_up(); }
                    }

                    Some('<') => {
                        tasks.update_state(arg_default, State::Closed);
                        if tasks.custom_time.is_none() { tasks.move_up(); }
                    }

                    Some('&') => {
                        match arg {
                            None => tasks.undo(),
                            Some(text) => {
                                if text == "&" {
                                    println!(
                                        "My History:\n{}",
                                        tasks.history_before_now()
                                            .take(9)
                                            .enumerate()
                                            .dropping(1)
                                            .map(|(c, e)| {
                                                format!("({}) {}",
                                                        c,
                                                        match referenced_event(e) {
                                                            Some(target) => tasks.get_task_path(Some(target)),
                                                            None => "---".to_string(),
                                                        },
                                                )
                                            })
                                            .join("\n")
                                    );
                                    continue 'repl;
                                }
                                match text.parse::<u8>() {
                                    Ok(int) => {
                                        tasks.move_back_by(int as usize);
                                    }
                                    _ => {
                                        if !tasks.move_back_to(text) {
                                            warn!("Did not find a match in history for \"{text}\"");
                                            continue 'repl;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    Some('@') => {
                        match arg {
                            None => {
                                let today = Timestamp::now() - 80_000;
                                info!("Filtering for tasks from the last 22 hours");
                                if !tasks.set_filter_since(today) {
                                    continue 'repl;
                                }
                            }
                            Some(arg) => {
                                if arg == "@" {
                                    tasks.reset_key_filter()
                                } else if let Some((key, name)) = tasks.find_user(arg) {
                                    info!("Showing {}'s tasks", name);
                                    tasks.set_key_filter(key)
                                } else {
                                    if parse_hour(arg, 1)
                                        .or_else(|| parse_date(arg)
                                            .map(|utc| utc.with_timezone(&Local)))
                                        .map(|time| {
                                            info!("Filtering for tasks from {}", format_datetime_relative(time));
                                            tasks.set_filter_since(time.to_timestamp())
                                        })
                                        .is_none_or(|b| !b) {
                                        continue 'repl;
                                    }
                                }
                            }
                        };
                    }

                    Some('*') => {
                        match arg {
                            None => match tasks.get_position() {
                                None => {
                                    info!("Showing only bookmarked tasks");
                                    tasks.set_view_bookmarks();
                                }
                                Some(pos) =>
                                    match or_warn!(tasks.toggle_bookmark(pos)) {
                                        Some(true) => info!("Bookmarking \"{}\"", tasks.get_task_title(&pos)),
                                        Some(false) => info!("Removing bookmark for \"{}\"", tasks.get_task_title(&pos)),
                                        None => {}
                                    }
                            },
                            Some(arg) => {
                                if arg == "*" {
                                    tasks.set_priority(None);
                                } else {
                                    tasks.set_priority(arg.parse()
                                        .inspect_err(|e| warn!("Invalid Priority {arg}: {e}")).ok()
                                        .map(|p: Prio| p * (if arg.len() < 2 { 10 } else { 1 })));
                                }
                            }
                        }
                    }

                    Some('|') =>
                        match arg {
                            None => match tasks.get_position() {
                                None => {
                                    info!("Use | to create dependent sibling task and || to create a procedure");
                                    tasks.set_state_filter(
                                        StateFilter::State(State::Procedure.to_string()));
                                }
                                Some(id) => {
                                    tasks.set_state_for(id, "", State::Procedure);
                                }
                            },
                            Some(arg) => 'arm: {
                                if !arg.starts_with('|') {
                                    if tasks.make_dependent_sibling(arg) {
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
                                info!("Usage: ![(Open|Procedure|Pending|Done|Closed): ][Statename] OR Time: Reason");
                            }
                            Some(id) => {
                                'block: {
                                    if let Some((left, right)) = arg_default.split_once(": ") {
                                        if let Ok(state) = left.try_into() {
                                            tasks.set_state_for(id, right, state);
                                            break 'block;
                                        }
                                        if let Some(time) = parse_hour(left, 20)
                                            .map(|dt| dt.to_utc())
                                            .or_else(|| parse_date(left)) {
                                            let stamp = time.to_timestamp();
                                            let state = tasks.get_by_id(&id).and_then(Task::state);
                                            tasks.set_state_for(id, right, State::Pending);
                                            tasks.custom_time = Some(stamp);
                                            tasks.set_state_for(id,
                                                                &state.as_ref().map(StateChange::get_label).unwrap_or_default(),
                                                                State::from(state));
                                            break 'block;
                                        }
                                    }
                                    tasks.set_state_for_with(id, arg_default);
                                }
                                tasks.custom_time = None;
                                tasks.move_up();
                            }
                        }

                    Some('#') => {
                        if !tasks.update_tags(arg_default.split_whitespace().map(Hashtag::from)) {
                            continue;
                        }
                    }

                    Some('+') =>
                        match arg {
                            Some(arg) => tasks.add_tag(arg),
                            None => {
                                tasks.print_hashtags();
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
                            let (first, remaining) = arg.split_at(1);
                            if first == "(" {
                                let mut max = usize::MAX;
                                if remaining.len() > 0 {
                                    match remaining.parse::<usize>() {
                                        Ok(number) => max = number,
                                        Err(e) => warn!("Ignoring extra {:?}: {}\nSyntax: ((INT", remaining, e),
                                    }
                                }
                                println!("{}", tasks.times_tracked(max));
                            } else if let Some((key, _)) = tasks.find_user(arg) {
                                let (label, mut times) = tasks.times_tracked_for(&key);
                                println!("{}\n{}", label.italic(), times.join("\n"));
                            } else {
                                if tasks.track_from(arg) {
                                    println!("{}", tasks.times_tracked(15));
                                }
                            }
                        } else {
                            println!("{}", tasks.times_tracked(60));
                        }
                        continue 'repl;
                    }

                    Some(')') => {
                        match arg {
                            None => tasks.move_to(None),
                            Some(arg) => {
                                let pos = tasks.get_position_timestamped();
                                let time = pos.1.and_then(|_| Local.timestamp_millis_opt(pos.0.as_u64() as i64 * 1000).earliest());
                                if parse_tracking_stamp(arg, time)
                                    .and_then(|stamp| tasks.track_at(stamp, None)).is_some() {
                                    println!("{}", tasks.times_tracked(15));
                                }
                                // So the error message is not covered up
                                continue 'repl;
                            }
                        }
                    }

                    Some('.') => {
                        let (remaining, dots) = trim_start_count(&command, '.');
                        let pos = tasks.up_by(dots - 1);

                        if remaining.is_empty() {
                            tasks.move_to(pos);
                            if dots > 1 {
                                info!("Moving up {} tasks", dots - 1)
                            } else {
                                tasks.clear_filters();
                            }
                        } else {
                            match remaining.parse::<usize>() {
                                Ok(depth) if depth < 10 => {
                                    if pos != tasks.get_position() {
                                        tasks.move_to(pos);
                                    }
                                    tasks.set_view_depth(depth);
                                }
                                _ => {
                                    tasks.filter_or_create(pos, &remaining).map(|id| tasks.move_to(Some(id)));
                                }
                            }
                        }
                    }

                    Some('/') => if arg.is_none() {
                        tasks.move_to(None);
                    } else {
                        let (remaining, dots) = trim_start_count(&command, '/');
                        let pos = tasks.up_by(dots - 1);

                        if remaining.is_empty() {
                            tasks.move_to(pos);
                            if dots > 1 {
                                info!("Moving up {} tasks", dots - 1)
                            }
                        } else if let Ok(depth) = remaining.parse::<usize>() {
                            if pos != tasks.get_position() {
                                tasks.move_to(pos);
                            }
                            tasks.set_search_depth(depth);
                        } else {
                            // TODO regex match
                            let mut transform: Box<dyn Fn(&str) -> String> = Box::new(|s: &str| s.to_string());
                            if !remaining.chars().any(|c| c.is_ascii_uppercase()) {
                                // Smart-case - case-sensitive if any uppercase char is entered
                                transform = Box::new(|s| s.to_ascii_lowercase());
                            }

                            let filtered =
                                tasks.get_filtered(pos, |t| {
                                    transform(&t.get_title()).contains(&remaining) ||
                                        t.list_hashtags().any(
                                            |tag| tag.contains(&remaining))
                                });
                            if filtered.len() == 1 {
                                tasks.move_to(filtered.into_iter().next());
                            } else {
                                tasks.move_to(pos);
                                if !tasks.set_view(filtered) {
                                    continue 'repl;
                                }
                            }
                        }
                    }

                    _ =>
                        if Regex::new("^wss?://").unwrap().is_match(command.trim()) {
                            if let Some((url, tasks)) = relays.iter().find(|(key, _)| key.as_ref().is_some_and(|url| url.as_str().starts_with(&command))) {
                                selected_relay.clone_from(url);
                                println!("{}", tasks);
                                continue 'repl;
                            }
                            or_warn!(RelayUrl::parse(&command), "Failed to parse url {}", command).map(|url| {
                                match tx.try_send(MostrMessage::NewRelay(url.clone())) {
                                    Err(e) => error!("Nostr communication thread failure, cannot add relay \"{url}\": {e}"),
                                    Ok(_) => {
                                        info!("Connecting to {url}");
                                        selected_relay = Some(url.clone());
                                        relays.insert(selected_relay.clone(), tasks_for_url(selected_relay.clone()));
                                    }
                                }
                            });
                            continue 'repl;
                        } else if command.contains('\n') {
                            command.split('\n').for_each(|line| {
                                if !line.trim().is_empty() {
                                    tasks.make_task(line);
                                }
                            });
                        } else {
                            tasks.filter_or_create(tasks.get_position(), &command);
                        }
                }
                tasks.custom_time = None;
                println!("{}", tasks);
            }
            Err(ReadlineError::Eof) => break 'repl,
            Err(ReadlineError::Interrupted) => break 'repl, // TODO exit only if prompt is empty, or clear
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
