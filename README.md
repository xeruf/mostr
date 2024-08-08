# mostr

An immutable nested collaborative task manager, powered by nostr!

## Quickstart

First, start a nostr relay, such as
- https://github.com/coracle-social/bucket for local development
- https://github.com/rnostr/rnostr for production use

Run development build with:

    cargo run

A `relay` list and private `key` can be placed in config files
under `${XDG_CONFIG_HOME:-$HOME/.config}/mostr/`.
Currently, all relays are fetched and synced to,
separation is planned -
ideally for any project with different collaborators,
an own relay will be used.
If not saved, mostr will ask for a relay url
(entering none is fine too, but your data will not be persisted between sessions)
and a private key, alternatively generating one on the fly.
Both are currently saved in plain text to the above files.

Install latest build:

    cargo install --path .

Creating a test task externally:
`nostril --envelope --content "test task" --kind 1621 | websocat ws://localhost:4736`

To exit the application, press `Ctrl-D`.

## Basic Usage

### Navigation and Nesting

Create tasks and navigate using the shortcuts below.
Whichever task is active (selected)
will be the parent task for newly created tasks
and automatically has time-tracking running.
To track task progress,
simply subdivide the task -
checking off tasks will automatically update the progress
for all parent tasks.
Generally a flat hierarchy is recommended
with tags for filtering,
since hierarchies cannot be changed.
Filtering by a tag is just as easy
as activating a task and more flexible.

Using subtasks has two main advantages:
- ability to accumulate time tracked
- swiftly navigate between related tasks

Thus subtasks can be very useful for specific contexts,
for example a project or a specific place.
On the other hand, related tasks like chores
should be grouped with a tag instead.

### Collaboration

Since everything in mostr is inherently immutable,
live collaboration is easily possible.
After every command,
mostr checks if new updates arrived from the relay
and updates its display accordingly.

If a relay has a lot of events,
initial population of data can take a bit -
but you can already start creating events without issues,
updates will be fetched in the background.
For that reason,
it is recommended to leave mostr running
as you work.

### Time-Tracking

The currently active task is automatically time-tracked.
To stop time-tracking completely, simply move to the root of all tasks.

## Reference

### Command Syntax

`TASK` creation syntax: `NAME: TAG1 TAG2 ...`

- `TASK` - create task
- `.` - clear filters and reload
- `.TASK`
  + activate task by id
  + match by task name prefix: if one or more tasks match, filter / activate (tries case-sensitive then case-insensitive)
  + no match: create & activate task
- `.2` - set view depth to `2`, which can be substituted for any number (how many subtask levels to show, default 1)
- `/[TEXT]` - like `.`, but never creates a task

Dots can be repeated to move to parent tasks.

- `:[IND][COL]` - add property column COL at IND or end, if it already exists remove property column COL or IND (1-indexed)
- `*[TIME]` - add timetracking with the specified offset (empty: list tracked times)
- `>[TEXT]` - complete active task and move to parent, with optional state description
- `<[TEXT]` - close active task and move to parent, with optional state description
- `!TEXT` - set state for current task from text
- `,TEXT` - add text note (comment / description)
- `@` - undoes last action (moving in place or upwards or waiting a minute confirms pending actions)
- `wss://...` - switch or subscribe to relay

Property Filters:

- `#TAG` - set tag filter (empty: list all used tags)
- `+TAG` - add tag filter
- `-TAG` - remove tag filters
- `?STATE` - filter by state (type or description) - plain `?` to reset

State descriptions can be used for example for Kanban columns or review flows.
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
- `time` - time tracked on this task
- `rtime` - time tracked on this tasks and all recursive subtasks
- `progress` - recursive subtask completion in percent
- `subtasks` - how many direct subtasks are complete

For debugging: `props`, `alltags`, `descriptions`

## Nostr reference

Mostr mainly uses the following NIPs:
- Kind 1 for task descriptions
- Issue Tracking: https://github.com/nostr-protocol/nips/blob/master/34.md
  + Tasks have Kind 1621 (originally: git issue - currently no native markdown support)
  + Kind 1622 may be used for task comments or replace Kind 1 for descriptions
  + Kind 1630-1633: Task Status (1630 Open, 1631 Done, 1632 Closed, 1633 Pending)
- Implementing proprietary Kind 1650 for time-tracking

Considering to use Calendar: https://github.com/nostr-protocol/nips/blob/master/52.md
- Kind 31922 for GANTT, since it has only Date
- Kind 31923 for Calendar, since it has a time

## Plans

- Task markdown support? - colored
- Time tracking: Ability to postpone task and add planned timestamps (calendar entry)
- Parse Hashtag tags from task name
- Unified Filter object
  -> include subtasks of matched tasks
- Relay Switching
- Speedup: Offline caching & Expiry (no need to fetch potential years of history)
  + Fetch most recent tasks first
  + Relay: compress tracked time for old tasks, filter closed tasks
  + Relay: filter out task state updates within few seconds, also on client side
  
### Conceptual

The following features are not ready to be implemented
because they need conceptualization.
Suggestions welcome!

- Task Ownership
- Combined formatting and recursion specifiers
  + progress count/percentage and recursive or not
  + Subtask progress immediate/all/leafs
  + path full / leaf / top

### Interfaces

- TUI: Clear terminal? Refresh on empty prompt after timeout?
- Kanban, GANTT, Calendar
- Web Interface, Messenger integrations
