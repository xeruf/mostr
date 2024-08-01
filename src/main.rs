use std::cell::RefCell;
use std::env::{args, var};
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, stdin, stdout, Write};
use std::ops::Sub;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::mpsc::Sender;

use colored::Colorize;
use log::{debug, error, info, trace, warn};
use nostr_sdk::prelude::*;
use xdg::BaseDirectories;

use crate::task::State;
use crate::tasks::Tasks;

mod task;
mod tasks;

const TASK_KIND: u64 = 1621;
const TRACKING_KIND: u64 = 1650;

type Events = Vec<Event>;

#[derive(Debug, Clone)]
struct EventSender {
    tx: Sender<Events>,
    keys: Keys,
    queue: RefCell<Events>,
}
impl EventSender {
    fn submit(&self, event_builder: EventBuilder) -> Result<Event> {
        if let Some(event) = self.queue.borrow().first() {
            // Flush if oldest event older than a minute
            if event.created_at < Timestamp::now().sub(60u64) {
                debug!("Flushing Event Queue because it is older than a minute");
                self.flush();
            }
        }
        let mut queue = self.queue.borrow_mut();
        Ok(event_builder.to_event(&self.keys).inspect(|event| {
            if event.kind.as_u64() == TRACKING_KIND {
                queue.retain(|e| {
                    e.kind.as_u64() != TRACKING_KIND
                });
            }
            queue.push(event.clone());
        })?)
    }
    fn flush(&self) {
        debug!("Flushing {} events from queue", self.queue.borrow().len());
        if self.queue.borrow().len() > 0 {
            or_print(self.tx.send(self.clear()));
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
        self.flush()
    }
}

fn or_print<T, U: Display>(result: Result<T, U>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            warn!("{}", error);
            None
        }
    }
}

fn prompt(prompt: &str) -> Option<String> {
    print!("{} ", prompt);
    stdout().flush().unwrap();
    match stdin().lines().next() {
        Some(Ok(line)) => Some(line),
        _ => None,
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

    let (tx, rx) = mpsc::channel();
    let mut tasks: Tasks = Tasks::from(EventSender {
        keys,
        tx,
        queue: Default::default(),
    });

    let sub_id: SubscriptionId = client.subscribe(vec![Filter::new()], None).await;
    info!("Subscribed with {}", sub_id);
    let mut notifications = client.notifications();

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
        while let Ok(e) = rx.recv() {
            trace!("Sending {:?}", e);
            // TODO batch up further
            let _ = client.batch_event(e, RelaySendOptions::new()).await;
        }
        info!("Stopping listeners...");
        client.unsubscribe_all().await;
    });
    for argument in args().skip(1) {
        tasks.make_task(&argument);
    }

    println!();
    let mut lines = stdin().lines();
    loop {
        or_print(tasks.print_tasks());

        print!(
            "{}",
            format!(
                " {}{}) ",
                tasks.get_task_path(tasks.get_position()),
                tasks.get_prompt_suffix()
            ).italic()
        );
        stdout().flush().unwrap();
        match lines.next() {
            Some(Ok(input)) => {
                let mut count = 0;
                while let Ok(notification) = notifications.try_recv() {
                    if let RelayPoolNotification::Event {
                        subscription_id,
                        event,
                        ..
                    } = notification
                    {
                        print_event(&event);
                        tasks.add(*event);
                        count += 1;
                    }
                }
                if count > 0 {
                    info!("Received {count} updates");
                }

                let mut iter = input.chars();
                let op = iter.next();
                let arg = if input.len() > 1 {
                    input[1..].trim()
                } else {
                    ""
                };
                match op {
                    None => {
                        tasks.flush()
                    }

                    Some(':') => match iter.next().and_then(|s| s.to_digit(10)) {
                        Some(digit) => {
                            let index = digit as usize;
                            let remaining = iter.collect::<String>().trim().to_string();
                            if remaining.is_empty() {
                                tasks.properties.remove(index);
                                continue;
                            }
                            let value = input[2..].trim().to_string();
                            if tasks.properties.get(index) == Some(&value) {
                                tasks.properties.remove(index);
                            } else {
                                tasks.properties.insert(index, value);
                            }
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
                            let pos = tasks.properties.iter().position(|s| s == arg);
                            match pos {
                                None => {
                                    tasks.properties.push(arg.to_string());
                                }
                                Some(i) => {
                                    tasks.properties.remove(i);
                                }
                            }
                        }
                    },

                    Some(',') => tasks.add_note(arg),

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
                        tasks.set_state_filter(Some(arg.to_string()).filter(|s| !s.is_empty()));
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
                    }
                    
                    Some('-') => {
                        tasks.remove_tag(arg.to_string())
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
                            tasks.depth = depth;
                            continue;
                        }
                        pos = EventId::parse(slice).ok().or_else(|| {
                            // TODO rebuild and use for plaintext too
                            let mut filtered: Vec<EventId> = tasks
                                .current_tasks()
                                .into_iter()
                                .filter(|t| t.event.content.starts_with(slice))
                                .map(|t| t.event.id)
                                .collect();
                            if filtered.is_empty() {
                                let lowercase = slice.to_ascii_lowercase();
                                filtered = tasks
                                    .current_tasks()
                                    .into_iter()
                                    .filter(|t| {
                                        t.event.content.to_ascii_lowercase().starts_with(&lowercase)
                                    })
                                    .map(|t| t.event.id)
                                    .collect();
                            }
                            match filtered.len() {
                                0 => {
                                    // No match, new task
                                    Some(tasks.make_task(slice))
                                }
                                1 => {
                                    // One match, activate
                                    Some(filtered.first().unwrap().clone())
                                }
                                _ => {
                                    // Multiple match, filter
                                    tasks.set_filter(filtered);
                                    None
                                }
                            }
                        });
                        if pos != None {
                            tasks.move_to(pos);
                        }
                    }

                    _ => {
                        tasks.make_task(&input);
                    }
                }
            }
            Some(Err(e)) => warn!("{}", e),
            None => break,
        }
    }
    println!();

    tasks.move_to(None);
    drop(tasks);

    info!("Submitting pending changes...");
    or_print(sender.await);
}

fn print_event(event: &Event) {
    debug!(
        "At {} found {} kind {} '{}' {:?}",
        event.created_at, event.id, event.kind, event.content, event.tags
    );
}
