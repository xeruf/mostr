use std::env::args;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use nostr_sdk::async_utility::futures_util::TryFutureExt;
use nostr_sdk::prelude::*;

#[tokio::main]
async fn main() {
    let my_keys: Keys = Keys::generate();
    let client = Client::new(&my_keys);

    let proxy = Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9050)));

    client.add_relay("ws://localhost:4736").await;
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

    let timeout = Duration::from_secs(3);
    let task_kind = Kind::from(90002);

    let filter = Filter::new().kind(task_kind);
    let sub_id: SubscriptionId = client.subscribe(vec![filter.clone()], None).await;

    for argument in args().skip(1) {
        println!("Sending {}", argument);
        let event = EventBuilder::new(task_kind, argument, []).to_event(&my_keys).unwrap();
        let _ = client.send_event(event).await;
    }

    println!("Finding existing events");
    let res = client.get_events_of(vec![filter], Option::from(timeout)).map_ok(|res|
    for event in res {
        println!("Found {} {:?}", event.content, event.tags)
    }).await;

    let mut notifications = client.notifications();
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
