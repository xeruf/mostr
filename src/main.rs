use std::borrow::Borrow;
use std::env::args;
use std::fmt::Display;
use std::fs;
use std::io::{Read, stdin, stdout, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::ops::Deref;
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::mpsc::Sender;

use nostr_sdk::async_utility::futures_util::TryFutureExt;
use nostr_sdk::prelude::*;
use once_cell::sync::Lazy;

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

static MY_KEYS: Lazy<Keys> = Lazy::new(|| {
    match fs::read_to_string("keys") {
        Ok(key) => {
            Keys::from_str(&key).unwrap()
        }
        Err(e) => {
            eprintln!("{}", e);
            let keys = Keys::generate();
            fs::write("keys", keys.secret_key().unwrap().to_string());
            keys
        },
    }
});

struct EventSender {
    tx: Sender<Event>,
    keys: Keys,
}
impl EventSender {
    fn submit(&self, event_builder: EventBuilder) -> Option<Event> {
        or_print(event_builder.to_event(MY_KEYS.deref())).inspect(|event| {
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

#[tokio::main]
async fn main() {
    let proxy = Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9050)));

    let client = Client::new(MY_KEYS.deref());
    client.add_relay("ws://localhost:4736").await;
    println!("My key: {}", MY_KEYS.public_key());
    //client.add_relay("wss://relay.damus.io").await;
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
        keys: MY_KEYS.clone(),
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
    loop {
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
        tasks.print_current_tasks();

        print!(" {}> ", tasks.taskpath(tasks.get_position()));
        stdout().flush().unwrap();
        match stdin().lines().next() {
            Some(Ok(input)) => {
                let mut iter = input.chars();
                let op = iter.next();
                match op {
                    None => {}

                    Some(':') => match input[1..2].parse::<usize>() {
                        Ok(index) => {
                            if input.len() == 2 {
                                tasks.properties.remove(index);
                                continue;
                            }
                            let value = input[2..].to_string();
                            if tasks.properties.get(index) == Some(&value) {
                                tasks.properties.remove(index);
                            } else {
                                tasks.properties.insert(index, value);
                            }
                        }
                        Err(_) => {
                            if input.chars().nth(1) == Some(':') {
                                tasks.recursive = !tasks.recursive;
                                continue
                            }
                            let prop = &input[1..];
                            let pos = tasks.properties.iter().position(|s| s == &prop);
                            match pos {
                                None => {
                                    tasks.properties.push(prop.to_string());
                                }
                                Some(i) => {
                                    tasks.properties.remove(i);
                                }
                            }
                        }
                    },

                    Some('>') | Some('<') => {
                        tasks.update_state(&input[1..], |_| {
                            Some(if op.unwrap() == '<' {
                                State::Closed
                            } else {
                                State::Done
                            })
                        });
                        tasks.move_up()
                    }

                    Some('.') => {
                        let mut dots = 1;
                        let mut pos = tasks.get_position();
                        for _ in iter.take_while(|c| c == &'.') {
                            dots += 1;
                            pos = tasks.parent(pos);
                        }
                        let slice = &input[dots..];
                        if !slice.is_empty() {
                            pos = EventId::parse(slice).ok().or_else(|| {
                                // TODO check what is more intuitive:
                                // currently resets filters before filtering again, maybe keep them
                                tasks.move_to(pos);
                                let filtered: Vec<EventId> = tasks
                                    .current_tasks()
                                    .iter()
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
                        } else {
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
