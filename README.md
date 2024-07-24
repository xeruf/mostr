# mostr

A nested task chat, powered by nostr!

## Quickstart

First, start a nostr dev-relay like
https://github.com/coracle-social/bucket

```sh
cargo run # Listen to events
nostril --envelope --content "realtime message" --kind 90002 | websocat ws://localhost:4736 # Send a test event
```

## Plans

- TUI - Clear terminal?