use std::time::Duration;
use nostr_sdk::prelude::*;

#[tokio::main]
async fn main() {
    //tracing_subscriber::fmt::init();
    let client = Client::new(Keys::generate());
    let result = client.subscribe(vec![Filter::new()], None).await;
    println!("subscribe: {:?}", result);
    let result = client.add_relay("ws://localhost:4736").await;
    println!("add relay: {:?}", result);
    client.connect().await;

    let mut notifications = client.notifications();
    let _thread = tokio::spawn(async move {
        client.send_event_builder(EventBuilder::new(Kind::TextNote, "test")).await;
        tokio::time::sleep(Duration::from_secs(20)).await;
    });
    while let Ok(notification) = notifications.recv().await {
        if let RelayPoolNotification::Event { event, .. } = notification {
            println!("At {} found {} kind {} content \"{}\"", event.created_at, event.id, event.kind, event.content);
        }
    }
}