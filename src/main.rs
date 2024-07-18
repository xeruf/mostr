use crate::State::*;
use nostr_sdk::async_utility::futures_util::TryFutureExt;
use nostr_sdk::prelude::*;
use once_cell::sync::Lazy;
use std::borrow::Borrow;
use std::collections::HashMap;
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
                println!("Found {} {:?}", event.content, event.tags)
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

type TaskMap = HashMap<EventId, Task>;
fn add_task(tasks: &mut TaskMap, event: Event) -> Option<Task> {
    tasks.insert(event.id, Task::new(event))
}

async fn repl() {
    let mut tasks: TaskMap = HashMap::new();
    for argument in args().skip(1) {
        add_task(
            &mut tasks,
            make_task(&argument, &[Tag::Hashtag("arg".to_string())]),
        );
    }

    let mut properties: Vec<String> = vec!["id".into(), "name".into(), "state".into()];
    let mut position: Option<EventId> = None;
    let print_tasks = |tasks: Vec<&Task>, properties: &Vec<String>| {
        println!("{}", properties.join(" "));
        for task in tasks {
            println!("{}", properties.iter().map(|p| task.get(p).unwrap_or(String::new())).collect::<Vec<String>>().join(" "));
        }
        println!();
    };

    println!();
    print_tasks(tasks.values().collect(), &properties);

    loop {
        let mut prompt = String::with_capacity(64);
        let mut pos = position;
        while pos.is_some() {
            let id = pos.unwrap();
            let task = tasks.get(&id);
            prompt = task.map_or(id.to_string(), |t| t.event.content.clone()) + " " + &prompt;
            pos = task.and_then(|t| t.parent_id());
        }
        print!(" {}> ", prompt);
        stdout().flush().unwrap();
        match stdin().lines().next() {
            Some(Ok(input)) => {
                let mut iter = input.chars();
                let op = iter.next();
                match op {
                    None => {}

                    Some(':') => match input[1..2].parse::<usize>() {
                        Ok(index) => {
                            properties.insert(index, input[2..].to_string());
                        }
                        Err(_) => {
                            let prop = &input[1..];
                            let pos = properties.iter().position(|s| s == &prop);
                            match pos {
                                None => {
                                    properties.push(prop.to_string());
                                }
                                Some(i) => {
                                    properties.remove(i);
                                }
                            }
                        }
                    },

                    Some('>') | Some('<') => {
                        position.inspect(|e| {
                            let pos = tasks.get(e)
                                .and_then(|t| t.state())
                                .and_then(|state| STATES.iter().position(|s| s == &state.state))
                                .unwrap_or(1);
                            tasks.get_mut(e).map(|t| t.props.push(make_event(STATES[if op.unwrap() == '<' { pos - 1 } else { pos + 1 }].kind(), &input[1..], &[Tag::event(e.clone())])));
                        });
                    }

                    Some('.') => {
                        let mut dots = 1;
                        for _ in iter.take_while(|c| c == &'.') {
                            dots += 1;
                            position = position
                                .and_then(|id| tasks.get(&id))
                                .and_then(|t| t.parent_id());
                        }
                        let _ = EventId::parse(&input[dots..]).map(|p| position = Some(p));
                    }

                    _ => {
                        let mut tags: Vec<Tag> = Vec::new();
                        position.inspect(|p| tags.push(Tag::event(*p)));
                        let event = match input.split_once(": ") {
                            None => make_task(&input, &tags),
                            Some(s) => {
                                tags.append(
                                    &mut s.1.split(" ")
                                        .map(|t| Tag::Hashtag(t.to_string()))
                                        .collect());
                                make_task(s.0, &tags)
                            }
                        };
                        for tag in event.tags.iter() {
                            match tag {
                                Tag::Event { event_id, .. } => {
                                    tasks
                                        .get_mut(event_id)
                                        .map(|t| t.children.push(event.id));
                                }
                                _ => {}
                            }
                        }
                        let _ = add_task(&mut tasks, event);
                    }
                }

                let tasks: Vec<&Task> =
                    position.map_or(tasks.values().collect(),
                                    |p| {
                                        tasks.get(&p)
                                            .map_or(Vec::new(), |t| t.children.iter().filter_map(|id| tasks.get(id)).collect())
                                    });
                print_tasks(tasks, &properties);
            }
            Some(Err(e)) => eprintln!("{}", e),
            None => break,
        }
    }

    println!();
    let _ = CLIENT
        .batch_event(
            tasks.into_values().map(|t| t.event).collect(),
            RelaySendOptions::new().skip_send_confirmation(true),
        )
        .await;
}

struct Task {
    event: Event,
    children: Vec<EventId>,
    props: Vec<Event>
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
            }.map(|s| TaskState {
                name: if event.content.is_empty() { None } else { Some(event.content.clone()) },
                state: s,
                time: event.created_at.clone(),
            })
        })
    }

    fn state(&self) -> Option<TaskState> {
        self.states().max_by_key(|t| t.time)
    }

    fn default_state(&self) -> TaskState {
        TaskState {
            name: None,
            state: Open,
            time: self.event.created_at,
        }
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
            },
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
        write!(f, "{}{}", self.state, self.name.as_ref().map_or(String::new(), |s| format!(": {}", s)))
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
            Closed => Kind::from(1632),
            Open => Kind::from(1630),
            Active => Kind::from(1633),
            Done => Kind::from(1631),
        }
    }
}
static STATES: [State; 4] = [Closed, Open, Active, Done];
impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
