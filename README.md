# mostr

A nested task chat, powered by nostr!

## Quickstart

First, start a nostr relay, such as
- https://github.com/coracle-social/bucket for local development
- https://github.com/rnostr/rnostr for production use

Run development build with:

    cargo run

Creating a test task: 
`nostril --envelope --content "test task" --kind 1621 | websocat ws://localhost:4736`

Install latest build:

    cargo install --path . --offline

## Principles

- active task is tracked automatically
- progress through subdivision rather than guessing
- TBI: show/hide closed/done tasks

Recommendation: Flat hierarchy, using tags for filtering (TBI)

## Reference

### Command Syntax

`TASK` creation syntax: `NAME: TAG1 TAG2 ...`

- `TASK` - create task
- `.` - clear filters and reload
- `.TASK`
  + select task by id
  + match by task name prefix: if one or more tasks match, filter / activate (tries case-sensitive then case-insensitive)
  + no match: create & activate task
- `.2` - set view depth to `2`, which can be substituted for any number (how many subtask levels to show, default 1)

Dots can be repeated to move to parent tasks

- `:[IND][COL]` - add / remove property column COL to IND or end
- `>[TEXT]` - Complete active task and move to parent, with optional state description
- `<[TEXT]` - Close active task and move to parent, with optional state description
- `|TEXT` - Set state for current task from text (also aliased to `/` for now)
- `-TEXT` - add text note (comment / description)

Property Filters:

- `#TAG` - filter by tag
- `?STATE` - filter by state (type or description) - plain `?` to reset

State descriptions can be used for example for Kanban columns.
An active tag or state filter will also set that attribute for newly created tasks.

### Available Columns

- `id`
- `parentid`
- `name`
- `state`
- `hashtags`
- `tags` - values of all nostr tags associated with the event, except event tags
- `desc` - last note on the task
- `description` - accumulated notes on the task
- `path` - name including parent tasks
- `rpath` - name including parent tasks up to active task
- `time` - time tracked
- `rtime` - time tracked including subtasks
- TBI: `progress` - how many subtasks are complete
- TBI: `progressp` - subtask completion in percent

For debugging: `props`, `alltags`, `descriptions`

TODO: Combined formatting and recursion specifiers

## Plans

- Relay Selection, fetch most recent tasks first
- parse Hashtag tags from task name
- Personal time tracking
- Unified Filter object
  -> include sub
- Time tracking: Active not as task state, ability to postpone task and add planned timestamps (calendar entry)
- TUI - Clear terminal?
- Expiry (no need to fetch potential years of history)
- Offline caching
- Web Interface, Messenger integrations
- Relay: filter out task state updates within few seconds, also on client side
