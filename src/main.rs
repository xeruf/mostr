use std::borrow::Borrow;
use std::env::args;
use std::io::{stdin, stdout, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::ops::Deref;
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
        let _ = send(argument, &[]).await;
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

async fn send(text: String, tags: &[Tag]) -> Result<EventId, Error> {
    println!("Sending {}", text);
    let event = EventBuilder::new(*TASK_KIND, text, tags.to_vec()).to_event(&MY_KEYS).unwrap();
    return CLIENT.send_event(event).await;
}

async fn repl() {
    loop {
        print!("> ");
        stdout().flush().unwrap();
        match stdin().lines().next() {
            Some(Ok(input)) => {
                if input.trim() == "exit" {
                    break;
                }
                if input.trim().is_empty() {
                    continue;
                }
                let fut = match input.split_once(": ") {
                    None => {
                        send(input, &[Tag::Name("default".to_string())]).await; 
                    }
                    Some(s) => {
                        let tags: Vec<Tag> = s.1.split(" ").map(|t|Tag::Hashtag(t.to_string())).collect();
                        send(s.0.to_string(), &tags).await;
                    }
                };
            }
            _ => {}
        }
    }
}
