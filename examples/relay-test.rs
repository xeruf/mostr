use nostr_sdk::prelude::*;

#[tokio::main]
async fn main() {
    //tracing_subscriber::fmt::init();
    let client = Client::new(Keys::generate());
    //let result = client.subscribe(vec![Filter::new()], None).await;
    //println!("{:?}", result);
    let mut notifications = client.notifications();

    let result = client.add_relay("ws://localhost:3333").await;
    println!("{:?}", result);
    let result = client.connect_relay("ws://localhost:3333").await;
    println!("{:?}", result);

    //let _thread = tokio::spawn(async move {
    //    let result = client.add_relay("ws://localhost:4736").await;
    //    println!("{:?}", result);
    //    let result = client.connect_relay("ws://localhost:4736").await;
    //    println!("{:?}", result);

    //    // Block b
    //    //let result = client.add_relay("ws://localhost:54736").await;
    //    //println!("{:?}", result);
    //    //let result = client.connect_relay("ws://localhost:54736").await;
    //    //println!("{:?}", result);

    //    tokio::time::sleep(Duration::from_secs(20)).await;
    //});

    loop {
        match notifications.recv().await {
            Ok(notification) => {
                if let RelayPoolNotification::Event { event, .. } = notification {
                    println!("At {} found {} kind {} content \"{}\"", event.created_at, event.id, event.kind, event.content);
                }
            }
            Err(e) => {
                println!("Aborting due to {:?}", e);
                break
            }
        }
    }
}