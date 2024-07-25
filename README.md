# mostr

A nested task chat, powered by nostr!

## Quickstart

First, start a nostr dev-relay like
https://github.com/coracle-social/bucket

```sh
cargo run # Listen to events
nostril --envelope --content "realtime message" --kind 90002 | websocat ws://localhost:4736 # Send a test event
```

## Principles

- active task is tracked automatically
- progress through subdivision rather than guessing
- TBI: show/hide closed/done tasks

Recommendation: Flat hierarchy, using tags for filtering (TBI)

## Reference

TASK add syntax: `NAME: TAG1 TAG2`

- `TASK` - create task
- `.` - clear filters and reload
- `.TASK` - filter / activate (by id or name) / create & activate task
- `.NUM` - set view depth - how many subtask levels to show

Dots can be repeated to move to parent tasks

- `:[IND][COL]` - add / remove property column COL to IND or end
- `>[TEXT]` - Complete active task and move to parent, with optional state description
- `<[TEXT]` - Close active task and move to parent, with optional state description
- `-TEXT` - add text note (comment / description)

State descriptions can be used for example for Kanban columns.

### Columns

- `id`
- `parentid`
- `name`
- `state`
- `desc` - accumulated notes of the task
- `path` - name including parent tasks
- `rpath` - name including parent tasks up to active task
- `time` - time tracked
- `ttime` - time tracked including subtasks
- TBI: `progress` - how many subtasks are complete

For debugging: `props` - Task Property Events

## Plans

- Expiry (no need to fetch potential years of history)
- Web Interface, Messenger bots
- TUI - Clear terminal?