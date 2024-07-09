use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
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

    let filter = Filter::new().kind(Kind::from(90002));
    let sub_id: SubscriptionId = client.subscribe(vec![filter], None).await;

    let mut notifications = client.notifications();
    while let Ok(notification) = notifications.recv().await {
        if let RelayPoolNotification::Event { subscription_id, event, .. } = notification {
            let kind = event.kind;
            let content = &event.content;
            println!("{kind}: {content}");
            //break; // Exit
        }
    }
}
