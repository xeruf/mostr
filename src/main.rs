mod tasks;

use crate::tasks::Tasks;
use crate::State::*;
use nostr_sdk::async_utility::futures_util::TryFutureExt;
use nostr_sdk::prelude::*;
use once_cell::sync::Lazy;
use std::borrow::Borrow;
use std::env::args;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::io::{stdin, stdout, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::ops::Deref;
use std::time::Duration;

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

    let timeout = Duration::from_secs(3);

    let filter = Filter::new();
    let sub_id: SubscriptionId = CLIENT.subscribe(vec![filter.clone()], None).await;

    repl().await;

    println!("Finding existing events");
    let res = CLIENT
        .get_events_of(vec![filter], Option::from(timeout))
        .map_ok(|res| {
            for event in res {
                println!("Found {} '{}' {:?}", event.kind, event.content, event.tags)
            }
        })
        .await;

    let mut notifications = CLIENT.notifications();
    println!("Listening for events...");
    while let Ok(notification) = notifications.recv().await {
        if let RelayPoolNotification::Event {
            subscription_id,
            event,
            ..
        } = notification
        {
            let kind = event.kind;
            let content = &event.content;
            println!("{kind}: {content}");
            //break; // Exit
        }
    }
}

fn make_task(text: &str, tags: &[Tag]) -> Event {
    make_event(Kind::from(1621), text, tags)
}
fn make_event(kind: Kind, text: &str, tags: &[Tag]) -> Event {
    EventBuilder::new(kind, text, tags.to_vec())
        .to_event(&MY_KEYS)
        .unwrap()
}

async fn repl() {
    let mut tasks: Tasks = Default::default();
    for argument in args().skip(1) {
        tasks.add_task(make_task(&argument, &[Tag::Hashtag("arg".to_string())]));
    }

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
                            Some(if op.unwrap() == '<' { Closed } else { Done })
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
                                let task = tasks.make_task(slice);
                                let ret = Some(task.id);
                                tasks.add_task(task);
                                ret
                            });
                            tasks.move_to(pos);
                        }
                        tasks.move_to(pos);
                    }

                    _ => {
                        tasks.add_task(tasks.make_task(&input));
                    }
                }

                tasks.print_current_tasks();
            }
            Some(Err(e)) => eprintln!("{}", e),
            None => break,
        }
    }

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

struct Task {
    event: Event,
    children: Vec<EventId>,
    props: Vec<Event>,
}
impl Task {
    fn new(event: Event) -> Task {
        Task {
            event,
            children: Vec::new(),
            props: Vec::new(),
        }
    }

    fn parent_id(&self) -> Option<EventId> {
        for tag in self.event.tags.iter() {
            match tag {
                Tag::Event { event_id, .. } => return Some(*event_id),
                _ => {}
            }
        }
        None
    }

    fn descriptions(&self) -> impl Iterator<Item = String> + '_ {
        self.props.iter().filter_map(|event| {
            if event.kind == Kind::TextNote {
                Some(event.content.clone())
            } else {
                None
            }
        })
    }

    fn states(&self) -> impl Iterator<Item = TaskState> + '_ {
        self.props.iter().filter_map(|event| {
            match event.kind.as_u32() {
                1630 => Some(Open),
                1631 => Some(Done),
                1632 => Some(Closed),
                1633 => Some(Active),
                _ => None,
            }
            .map(|s| TaskState {
                name: if event.content.is_empty() {
                    None
                } else {
                    Some(event.content.clone())
                },
                state: s,
                time: event.created_at.clone(),
            })
        })
    }

    fn state(&self) -> Option<TaskState> {
        self.states().max_by_key(|t| t.time)
    }

    fn pure_state(&self) -> State {
        self.state().map_or(Open, |s| s.state)
    }

    fn default_state(&self) -> TaskState {
        TaskState {
            name: None,
            state: Open,
            time: self.event.created_at,
        }
    }

    fn update_state(&mut self, state: State, comment: &str) {
        self.props.push(make_event(
            state.kind(),
            comment,
            &[Tag::event(self.event.id)],
        ))
    }

    fn get(&self, property: &str) -> Option<String> {
        match property {
            "id" => Some(self.event.id.to_string()),
            "parentid" => self.parent_id().map(|i| i.to_string()),
            "state" => self.state().map(|s| s.to_string()),
            "name" => Some(self.event.content.clone()),
            "desc" | "description" => self.descriptions().fold(None, |total, s| {
                Some(match total {
                    None => s,
                    Some(i) => i + " " + &s,
                })
            }),
            _ => {
                eprintln!("Unknown column {}", property);
                None
            }
        }
    }
}

struct TaskState {
    name: Option<String>,
    state: State,
    time: Timestamp,
}
impl Display for TaskState {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            self.state,
            self.name
                .as_ref()
                .map_or(String::new(), |s| format!(": {}", s))
        )
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
enum State {
    Closed,
    Open,
    Active,
    Done,
}
impl State {
    fn kind(&self) -> Kind {
        match self {
            Open => Kind::from(1630),
            Done => Kind::from(1631),
            Closed => Kind::from(1632),
            Active => Kind::from(1633),
        }
    }
}
static STATES: [State; 4] = [Closed, Open, Active, Done];
impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
