use std::borrow::Borrow;
use std::env::args;
use std::io::{stdin, stdout, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::ops::Deref;
use std::time::Duration;

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

static MY_KEYS: Lazy<Keys> = Lazy::new(|| Keys::generate());
static CLIENT: Lazy<Client> = Lazy::new(|| Client::new(MY_KEYS.borrow().deref()));

#[tokio::main]
async fn main() {
    let proxy = Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9050)));

    CLIENT.add_relay("ws://localhost:4736").await;
    //CLIENT.add_relay("wss://relay.damus.io").await;
    //CLIENT
    //    .add_relay_with_opts(
    //        "wss://relay.nostr.info",
    //        RelayOptions::new().proxy(proxy).flags(RelayServiceFlags::default().remove(RelayServiceFlags::WRITE)),
    //    )
    //    .await?;
    //CLIENT
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

    //CLIENT.set_metadata(&metadata).await?;

    CLIENT.connect().await;

    repl().await;

}

fn make_task(text: &str, tags: &[Tag]) -> Event {
    make_event(Kind::from(1621), text, tags)
}
fn make_event(kind: Kind, text: &str, tags: &[Tag]) -> Event {
    EventBuilder::new(kind, text, tags.to_vec())
        .to_event(&MY_KEYS)
        .unwrap()
}

fn print_event(event: &Event) {
    println!("At {} found {} kind {} '{}' {:?}", event.created_at, event.id, event.kind, event.content, event.tags);
}

async fn repl() {
    let mut tasks: Tasks = Default::default();
    for argument in args().skip(1) {
        tasks.add_task(make_task(&argument, &[Tag::Hashtag("arg".to_string())]));
    }

    let sub_id: SubscriptionId = CLIENT.subscribe(vec![Filter::new()], None).await;
    let mut notifications = CLIENT.notifications();

    println!("Finding existing events");
    let res = CLIENT
        .get_events_of(vec![Filter::new()], None)
        .map_ok(|res| {
            println!("Found {} events", res.len());
            let (mut task_events, props): (Vec<Event>, Vec<Event>) = res.into_iter().partition(|e| e.kind.as_u32() == 1621);
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
        .await;


    println!();
    tasks.print_current_tasks();

    loop {
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
                            tasks.properties.insert(index, input[2..].to_string());
                        }
                        Err(_) => {
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
                                tasks.move_to(pos);
                                let filtered: Vec<EventId> = tasks.current_tasks().iter().filter(|t| t.event.content.starts_with(slice)).map(|t| t.event.id).collect();
                                match filtered.len() {
                                    0 => {
                                        // No match, new task
                                        let task = tasks.make_task(slice);
                                        let ret = Some(task.id);
                                        tasks.add_task(task);
                                        ret
                                    }
                                    1 => {
                                        // One match, select
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
                        tasks.add_task(tasks.make_task(&input));
                    }
                }
            }
            Some(Err(e)) => eprintln!("{}", e),
            None => break,
        }
        
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
    }

    tasks.update_state("", |t| if t.pure_state() == State::Active { Some(State::Open) } else { None });

    println!();
    println!("Submitting created events");
    let _ = CLIENT
        .batch_event(
            tasks
                .tasks
                .into_values()
                .flat_map(|t| {
                    let mut ev = t.props;
                    ev.push(t.event);
                    ev
                })
                .collect(),
            RelaySendOptions::new().skip_send_confirmation(true),
        )
        .await;
}
