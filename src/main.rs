use std::env::{args, var};
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, stdin, stdout, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::mpsc::Sender;

use nostr_sdk::prelude::*;
use xdg::BaseDirectories;

use crate::task::State;
use crate::tasks::Tasks;

mod task;
mod tasks;

/*
      1: Task Description
   Issue Tracking: https://github.com/nostr-protocol/nips/blob/master/34.md
   1621: MD Issue
   1622: MD Reply
   1630-1633: Status (Time-tracking, Kanban)
   Calendar: https://github.com/nostr-protocol/nips/blob/master/52.md
   31922 (GANTT, only Date)
   31923 (Calendar, with Time)
*/
static TASK_KIND: u64 = 1621;

#[derive(Debug, Clone)]
struct EventSender {
    tx: Sender<Event>,
    keys: Keys,
}
impl EventSender {
    fn submit(&self, event_builder: EventBuilder) -> Option<Event> {
        or_print(event_builder.to_event(&self.keys)).inspect(|event| {
            or_print(self.tx.send(event.clone()));
        })
    }
}

fn or_print<T, U: Display>(result: Result<T, U>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            eprintln!("{}", error);
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
    let config_dir = or_print(BaseDirectories::new())
        .and_then(|d| or_print(d.create_config_directory("mostr")))
        .unwrap_or(PathBuf::new());
    let keysfile = config_dir.join("key");
    let relayfile = config_dir.join("relays");

    let keys = match fs::read_to_string(&keysfile).map(|s| Keys::from_str(&s)) {
        Ok(Ok(key)) => key,
        _ => {
            eprintln!("Could not read keys from {}", keysfile.to_string_lossy());
            let keys = prompt("Secret Key?")
                .and_then(|s| or_print(Keys::from_str(&s)))
                .unwrap_or_else(|| Keys::generate());
            or_print(fs::write(&keysfile, keys.secret_key().unwrap().to_string()));
            keys
        }
    };

    let client = Client::new(&keys);
    println!("My public key: {}", keys.public_key());
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
                eprintln!("Could not read relays file: {}", e);
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

    let (tx, rx) = mpsc::channel::<Event>();
    let mut tasks: Tasks = Tasks::from(EventSender {
        keys: keys.clone(),
        tx,
    });

    let sub_id: SubscriptionId = client.subscribe(vec![Filter::new()], None).await;
    eprintln!("Subscribed with {}", sub_id);
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
            //eprintln!("Sending {}", e.id);
            let _ = client.send_event(e).await;
        }
        println!("Stopping listeners...");
        client.unsubscribe_all().await;
    });
    for argument in args().skip(1) {
        tasks.make_task(&argument);
    }

    println!();
    let mut lines = stdin().lines();
    loop {
        tasks.print_tasks();

        print!(
            " {}{}) ",
            tasks.get_task_path(tasks.get_position()),
            tasks.get_prompt_suffix()
        );
        stdout().flush().unwrap();
        match lines.next() {
            Some(Ok(input)) => {
                while let Ok(notification) = notifications.try_recv() {
                    if let RelayPoolNotification::Event {
                        subscription_id,
                        event,
                        ..
                    } = notification
                    {
                        print_event(&event);
                        tasks.add(*event);
                    }
                }

                let mut iter = input.chars();
                let op = iter.next();
                let arg = if input.len() > 1 {
                    input[1..].trim()
                } else {
                    ""
                };
                match op {
                    None => {}

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

                    Some('?') => {
                        tasks.set_state_filter(Some(arg.to_string()).filter(|s| !s.is_empty()));
                    }

                    Some('-') => tasks.add_note(arg),

                    Some('>') => {
                        tasks.update_state(arg, |_| Some(State::Done));
                        tasks.move_up();
                    }

                    Some('<') => {
                        tasks.update_state(arg, |_| Some(State::Closed));
                        tasks.move_up();
                    }

                    Some('|') | Some('/') => match tasks.get_position() {
                        None => {
                            println!("First select a task to set its state!");
                        }
                        Some(id) => {
                            tasks.set_state_for(&id, arg);
                            tasks.move_to(tasks.get_position());
                        }
                    },

                    Some('#') => {
                        tasks.add_tag(arg.to_string());
                    }

                    Some('.') => {
                        let mut dots = 1;
                        let mut pos = tasks.get_position();
                        for _ in iter.take_while(|c| c == &'.') {
                            dots += 1;
                            pos = tasks.get_parent(pos);
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
                            // TODO check what is more intuitive:
                            // currently resets filters before filtering again, maybe keep them
                            tasks.move_to(pos);
                            let filtered: Vec<EventId> = tasks
                                .current_tasks()
                                .into_iter()
                                .filter(|t| t.event.content.starts_with(slice))
                                .map(|t| t.event.id)
                                .collect();
                            match filtered.len() {
                                0 => {
                                    // No match, new task
                                    tasks.make_task(slice)
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
            Some(Err(e)) => eprintln!("{}", e),
            None => break,
        }
    }
    println!();

    tasks.update_state("", |t| {
        if t.pure_state() == State::Active {
            Some(State::Open)
        } else {
            None
        }
    });
    drop(tasks);

    eprintln!("Submitting pending changes...");
    or_print(sender.await);
}

fn print_event(event: &Event) {
    eprintln!(
        "At {} found {} kind {} '{}' {:?}",
        event.created_at, event.id, event.kind, event.content, event.tags
    );
}
