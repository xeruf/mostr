use std::cell::RefCell;
use std::collections::HashMap;
use std::env::{args, var};
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, stdin, stdout, Write};
use std::ops::Sub;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::mpsc::Sender;

use chrono::DateTime;
use colored::Colorize;
use log::{debug, error, info, trace, warn};
use nostr_sdk::prelude::*;
use regex::Regex;
use xdg::BaseDirectories;

use crate::helpers::*;
use crate::kinds::{KINDS, TRACKING_KIND};
use crate::task::State;
use crate::tasks::Tasks;

mod helpers;
mod task;
mod tasks;
mod kinds;

type Events = Vec<Event>;

#[derive(Debug, Clone)]
struct EventSender {
    url: Option<Url>,
    tx: Sender<(Url, Events)>,
    keys: Keys,
    queue: RefCell<Events>,
}
impl EventSender {
    fn from(url: Option<Url>, tx: &Sender<(Url, Events)>, keys: &Keys) -> Self {
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
            let min = Timestamp::now().sub(60u64);
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
            or_print(self.tx.send((url.clone(), values)));
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

#[tokio::main]
async fn main() {
    colog::init();

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

    let sub_id = client.subscribe(vec![Filter::new().kinds(KINDS.into_iter().map(|k| Kind::from(k)))], None).await;
    info!("Subscribed with {:?}", sub_id);

    //let proxy = Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9050)));
    //client
    //    .add_relay_with_opts(
    //        "wss://relay.nostr.info",
    //        RelayOptions::new().proxy(proxy).flags(RelayServiceFlags::default().remove(RelayServiceFlags::WRITE)),
    //    )
    //    .await?;
    //client
    //    .add_relay_with_opts(
    //        "ws://jgqaglhautb4k6e6i2g34jakxiemqp6z4wynlirltuukgkft2xuglmqd.onion",
    //        RelayOptions::new().proxy(proxy),
    //    )
    //    .await?;

    //let metadata = Metadata::new()
    //    .name("username")
    //    .display_name("My Username")
    //    .about("Description")
    //    .picture(Url::parse("https://example.com/avatar.png")?)
    //    .banner(Url::parse("https://example.com/banner.png")?)
    //    .nip05("username@example.com")
    //    .lud16("yuki@getalby.com")
    //    .custom_field("custom_field", "my value");

    //client.set_metadata(&metadata).await?;

    client.connect().await;
    let mut notifications = client.notifications();

    let (tx, rx) = mpsc::channel();
    let tasks_for_url = |url: Option<Url>| Tasks::from(url, &tx, &keys);
    let mut relays: HashMap<Url, Tasks> =
        client.relays().await.into_keys().map(|url| (url.clone(), tasks_for_url(Some(url)))).collect();

    /*println!("Finding existing events");
    let _ = client
        .get_events_of(vec![Filter::new()], Some(Duration::from_secs(5)))
        .map_ok(|res| {
            println!("Found {} events", res.len());
            let (mut task_events, props): (Vec<Event>, Vec<Event>) =
                res.into_iter().partition(|e| e.kind.as_u32() == 1621);
            task_events.sort_unstable();
            for event in task_events {
                print_event(&event);
                tasks.add_task(event);
            }
            for event in props {
                print_event(&event);
                tasks.add_prop(&event);
            }
        })
        .await;*/

    let sender = tokio::spawn(async move {
        while let Ok((url, events)) = rx.recv() {
            trace!("Sending {:?}", events);
            // TODO batch up further
            let _ = client.batch_event_to(vec![url], events, RelaySendOptions::new()).await;
        }
        info!("Shutting down sender thread");
    });

    let mut local_tasks = Tasks::from(None, &tx, &keys);
    let mut selected_relay: Option<Url> = relays.keys().nth(0).cloned();

    {
        let tasks = selected_relay.as_ref().and_then(|url| relays.get_mut(&url)).unwrap_or_else(|| &mut local_tasks);
        for argument in args().skip(1) {
            tasks.make_task(&argument);
        }
    }

    println!();
    let mut lines = stdin().lines();
    loop {
        selected_relay.as_ref().and_then(|url| relays.get(url)).inspect(|tasks| {
            or_print(tasks.print_tasks());

            print!(
                "{}",
                format!(
                    "{} {}{}) ",
                    selected_relay.as_ref().map_or("local".to_string(), |url| url.to_string()),
                    tasks.get_task_path(tasks.get_position()),
                    tasks.get_prompt_suffix()
                ).italic()
            );
        });
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
                        print_event(&event);
                        match relays.get_mut(&relay_url) {
                            Some(tasks) => tasks.add(*event),
                            None => warn!("Event received from unknown relay {relay_url}: {:?}", event)
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
                    input[1..].trim()
                } else {
                    ""
                };
                let tasks = selected_relay.as_ref().and_then(|url| relays.get_mut(&url)).unwrap_or_else(|| &mut local_tasks);
                match op {
                    None => {
                        debug!("Flushing Tasks because of empty command");
                        tasks.flush()
                    }

                    Some(':') => match iter.next().and_then(|s| s.to_digit(10)) {
                        Some(digit) => {
                            let index = (digit as usize).saturating_sub(1);
                            let remaining = iter.collect::<String>().trim().to_string();
                            if remaining.is_empty() {
                                tasks.remove_column(index);
                                continue;
                            }
                            let value = input[2..].trim().to_string();
                            tasks.add_or_remove_property_column_at_index(value, index);
                        }
                        None => {
                            if arg.is_empty() {
                                println!("Available properties:
- `id`
- `parentid`
- `name`
- `state`
- `hashtags`
- `tags` - values of all nostr tags associated with the event, except event tags
- `desc` - last note on the task
- `description` - accumulated notes on the task
- `path` - name including parent tasks
- `rpath` - name including parent tasks up to active task
- `time` - time tracked on this task
- `rtime` - time tracked on this tasks and all recursive subtasks
- `progress` - recursive subtask completion in percent
- `subtasks` - how many direct subtasks are complete");
                                continue;
                            }
                            tasks.add_or_remove_property_column(arg);
                        }
                    },

                    Some(',') => tasks.make_note(arg),

                    Some('>') => {
                        tasks.update_state(arg, State::Done);
                        tasks.move_up();
                    }

                    Some('<') => {
                        tasks.update_state(arg, State::Closed);
                        tasks.move_up();
                    }

                    Some('@') => {
                        tasks.undo();
                    }

                    Some('?') => {
                        tasks.set_state_filter(some_non_empty(arg).filter(|s| !s.is_empty()));
                    }

                    Some('!') => match tasks.get_position() {
                        None => {
                            warn!("First select a task to set its state!");
                        }
                        Some(id) => {
                            tasks.set_state_for(id, arg, match arg {
                                "Closed" => State::Closed,
                                "Done" => State::Done,
                                _ => State::Open,
                            });
                        }
                    },

                    Some('#') | Some('+') => {
                        tasks.add_tag(arg.to_string());
                        info!("Added tag filter for #{arg}")
                    }

                    Some('-') => {
                        tasks.remove_tag(arg.to_string());
                        info!("Removed tag filter for #{arg}")
                    }

                    Some('*') => {
                        if let Ok(num) = arg.parse::<i64>() {
                            tasks.track_at(Timestamp::from(Timestamp::now().as_u64().saturating_add_signed(num)));
                        } else if let Ok(date) = DateTime::parse_from_rfc3339(arg) {
                            tasks.track_at(Timestamp::from(date.to_utc().timestamp() as u64));
                        } else {
                            warn!("Cannot parse {arg}");
                        }
                    }

                    Some('.') => {
                        let mut dots = 1;
                        let mut pos = tasks.get_position();
                        for _ in iter.take_while(|c| c == &'.') {
                            dots += 1;
                            pos = tasks.get_parent(pos).cloned();
                        }
                        let slice = &input[dots..];
                        if slice.is_empty() {
                            tasks.move_to(pos);
                            continue;
                        }
                        if let Ok(depth) = slice.parse::<i8>() {
                            tasks.move_to(pos);
                            tasks.set_depth(depth);
                        } else {
                            tasks.filter_or_create(slice).map(|id| tasks.move_to(Some(id)));
                        }
                    }

                    Some('/') => {
                        let mut dots = 1;
                        let mut pos = tasks.get_position();
                        for _ in iter.take_while(|c| c == &'/') {
                            dots += 1;
                            pos = tasks.get_parent(pos).cloned();
                        }
                        let slice = &input[dots..].to_ascii_lowercase();
                        if slice.is_empty() {
                            tasks.move_to(pos);
                            continue;
                        }
                        if let Ok(depth) = slice.parse::<i8>() {
                            tasks.move_to(pos);
                            tasks.set_depth(depth);
                        } else {
                            let filtered = tasks
                                .children_of(pos)
                                .into_iter()
                                .filter_map(|child| tasks.get_by_id(&child))
                                .filter(|t| t.event.content.to_ascii_lowercase().starts_with(slice))
                                .map(|t| t.event.id)
                                .collect::<Vec<_>>();
                            if filtered.len() == 1 {
                                tasks.move_to(filtered.into_iter().nth(0));
                            } else {
                                tasks.move_to(pos);
                                tasks.set_filter(filtered);
                            }
                        }
                    }

                    _ => {
                        if Regex::new("^wss?://").unwrap().is_match(&input) {
                            tasks.move_to(None);
                            let mut new_relay = relays.keys().find(|key| key.as_str().starts_with(&input)).cloned();
                            if new_relay.is_none() {
                                if let Some(url) = or_print(Url::parse(&input)) {
                                    warn!("Connecting to {url} while running not yet supported");
                                    //new_relay = Some(url.clone());
                                    //relays.insert(url.clone(), tasks_for_url(Some(url.clone())));
                                    //if client.add_relay(url).await.unwrap() {
                                    //    relays.insert(url.clone(), tasks_for_url(Some(url.clone())));
                                    //    client.connect().await;
                                    //}
                                }
                            }
                            if new_relay.is_some() {
                                selected_relay = new_relay;
                            }
                        } else {
                            tasks.filter_or_create(&input);
                        }
                    }
                }
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

fn print_event(event: &Event) {
    debug!(
        "At {} found {} kind {} \"{}\" {:?}",
        event.created_at, event.id, event.kind, event.content, event.tags
    );
}
