use std::borrow::Borrow;
use std::collections::HashMap;
use std::env::args;
use std::io::{stdin, stdout, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::ops::{Deref};
use std::time::Duration;
use once_cell::sync::Lazy;
use nostr_sdk::async_utility::futures_util::TryFutureExt;
use nostr_sdk::prelude::*;


static TASK_KIND: Lazy<Kind> = Lazy::new(||Kind::from(90002));

static MY_KEYS: Lazy<Keys> = Lazy::new(||Keys::generate());
static CLIENT: Lazy<Client> = Lazy::new(||Client::new(MY_KEYS.borrow().deref()));

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

    let filter = Filter::new().kind(*TASK_KIND);
    let sub_id: SubscriptionId = CLIENT.subscribe(vec![filter.clone()], None).await;

    for argument in args().skip(1) {
        let _ = send(&argument, &[]).await;
    }

    repl().await;

    println!("Finding existing events");
    let res = CLIENT.get_events_of(vec![filter], Option::from(timeout)).map_ok(|res|
    for event in res {
        println!("Found {} {:?}", event.content, event.tags)
    }).await;

    let mut notifications = CLIENT.notifications();
    println!("Listening for events...");
    while let Ok(notification) = notifications.recv().await {
        if let RelayPoolNotification::Event { subscription_id, event, .. } = notification {
            let kind = event.kind;
            let content = &event.content;
            println!("{kind}: {content}");
            //break; // Exit
        }
    }
}

fn make_event(text: &str, tags: &[Tag]) -> Event {
    EventBuilder::new(*TASK_KIND, text, tags.to_vec()).to_event(&MY_KEYS).unwrap()
}

async fn send(text: &str, tags: &[Tag]) -> (Event, Result<EventId, Error>) {
    println!("Sending {}", text);
    let event = EventBuilder::new(*TASK_KIND, text, tags.to_vec()).to_event(&MY_KEYS).unwrap();
    let result = CLIENT.send_event(event.clone()).await;
    return (event, result);
}

async fn repl() {
    let mut tasks: HashMap<EventId, Task> = HashMap::new();
    let mut position: Option<EventId> = None;
    loop {
        let mut prompt = String::from("");
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
                if input.trim() == "exit" {
                    break;
                }
                match input.split_once(": ") {
                    None => {
                        position = EventId::parse(input).ok();
                    }
                    Some(s) => {
                        let mut tags: Vec<Tag> = s.1.split(" ").map(|t| Tag::Hashtag(t.to_string())).collect();
                        if let Some(pos) = position {
                            tags.push(Tag::event(pos));
                        }
                        let event = make_event(s.0, &tags);
                        for tag in event.tags.iter() {
                            match tag {
                                Tag::Event { event_id, .. } => {
                                    tasks.get_mut(event_id).unwrap().children.push(event.clone());
                                }
                                _ => {}
                            }
                        }
                        tasks.insert(event.id, Task::new(event));
                    }
                };
                let events: Vec<&Event> = position.map_or(tasks.values().map(|t| &t.event).collect(),
                                                          |p| tasks.get(&p).map_or(Vec::new(), |t| t.children.iter().collect()));
                for event in events {
                    println!("{}: {}", event.id, event.content);
                }
            }
            _ => {}
        }
    }
    CLIENT.batch_event(tasks.into_values().map(|t| t.event).collect(), RelaySendOptions::new().skip_send_confirmation(true)).await.unwrap();
}

struct Task {
    event: Event,
    children: Vec<Event>,
}
impl Task {
    fn new(event: Event) -> Task {
        Task {
            event,
            children: Vec::new(),
        }
    }
}