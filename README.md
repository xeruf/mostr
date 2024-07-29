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

### Command Syntax

TASK add syntax: `NAME: TAG1 TAG2 ...`

- `TASK` - create task
- `.` - clear filters and reload
- `.TASK` - filter / activate (by id or name) / create & activate task
- `.NUM` - set view depth - how many subtask levels to show

Dots can be repeated to move to parent tasks

- `:[IND][COL]` - add / remove property column COL to IND or end
- `>[TEXT]` - Complete active task and move to parent, with optional state description
- `<[TEXT]` - Close active task and move to parent, with optional state description
- `|TEXT` - Set state for current task from text (also aliased to `/` for now)
- `-TEXT` - add text note (comment / description)

Property Filters:

- `#TAG` - filter by tag
- `?TAG` - filter by state (type or description) - plain `?` to reset

State descriptions can be used for example for Kanban columns.
An active tag or state filter will also create new tasks with those corresponding attributes.

### Available Columns

- `id`
- `parentid`
- `name`
- `state`
- `tags`
- `desc` - accumulated notes of the task
- `path` - name including parent tasks
- `rpath` - name including parent tasks up to active task
- `time` - time tracked
- `rtime` - time tracked including subtasks
- TBI: `progress` - how many subtasks are complete
- TBI: `progressp` - subtask completion in percent

For debugging: `props` - Task Property Events

## Plans

- Expiry (no need to fetch potential years of history)
- Web Interface, Messenger integrations
- TUI - Clear terminal?