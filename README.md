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

Managing a project with subtasks makes it continuously visible,
which is helpful if you want to be able to track time on the project itself
without a specific task,
Thus subtasks can be very useful for specific contexts,
for example a project or a specific place.

On the other hand, related tasks like chores
should be grouped with a tag instead.
Similarly for projects which are only sporadically worked on
when a specific task comes up, so they do not clutter the list.

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
  + prefix with space if you want a task to start with a command character
  + copy in text with newlines to create one task per line
- `.` - clear all filters
- `.TASK`
  + activate task by id
  + match by task name prefix: if one or more tasks match, filter / activate (tries case-sensitive then case-insensitive)
  + no match: create & activate task
- `.2` - set view depth to the given number (how many subtask levels to show, default is 1)
- `/[TEXT]` - activate task or filter by smart-case substring match (empty: move to root)
- `||TASK` - create and activate a new task procedure (where subtasks automatically depend on the previously created task)
- `|[TASK]` - (un)mark current task as procedure or create a sibling task depending on the current one and move up

Dot or slash can be repeated to move to parent tasks before acting.
Append `@TIME` to any task creation or change command to record the action with the given time.

- `:[IND][PROP]` - add property column PROP at IND or end,
  if it already exists remove property column PROP or IND; empty: list properties
- `::[PROP]` - sort by property PROP (multiple space-separated values allowed)
- `([TIME]` - list tracked times or insert timetracking with the specified offset (double to view all history)
  such as `-1d`, `-15 minutes`, `yesterday 17:20`, `in 2 fortnights`
- `)[TIME]` - stop timetracking with optional offset - also convenience helper to move to root
- `>[TEXT]` - complete active task and move up, with optional status description
- `<[TEXT]` - close active task and move up, with optional status description
- `!TEXT` - set status for current task from text and move up; empty: Open
- `!TIME: REASON` - defer current task to date
- TBI: `*[INT]` - set priority - can also be used in task creation, with any digit
- `,[TEXT]` - list notes or add text note (stateless task / task description)
- TBI: `;[TEXT]` - list comments or comment on task
- TBI: show status history and creation with attribution
- `&` - revert
  - with string argument, find first matching task in history
  - with int argument, jump back X tasks in history
  - undo last action (moving in place or upwards confirms pending actions)
- `wss://...` - switch or subscribe to relay (prefix with space to forcibly add a new one)

Property Filters:

- `#TAG1 TAG2` - set tag filter
- `+TAG` - add tag filter (empty: list all used tags)
- `-TAG` - remove tag filters (by prefix)
- `?STATUS` - filter by status (type or description) - plain `?` to reset, `??` to show all
- `@[AUTHOR|TIME]` - filter by time or author (pubkey, or `@` for self, TBI: id prefix, name prefix)
- TBI: `**INT` - filter by priority

Status descriptions can be used for example for Kanban columns or review flows.
An active tag or status filter will also set that attribute for newly created tasks.

### Notes

- TBI = To Be Implemented
- `. TASK` - create and enter a new task even if the name matches an existing one

## Nostr reference

Mostr mainly uses the following NIPs:

- Kind 1 for task descriptions and permanent tasks, can contain task property updates (tags, priority)
- Issue Tracking: https://github.com/nostr-protocol/nips/blob/master/34.md
  + Tasks have Kind 1621 (originally: git issue - currently no markdown support implemented)
  + TBI: Kind 1622 for task comments
  + Kind 1630-1633: Task Status (1630 Open, 1631 Done, 1632 Closed, 1633 Pending)
- Own Kind 1650 for time-tracking

Considering to use Calendar: https://github.com/nostr-protocol/nips/blob/master/52.md
- Kind 31922 for GANTT, since it has only Date
- Kind 31923 for Calendar, since it has a time

## Plans

- Local Database Cache, Negentropy Reconciliation
  -> Offline Use!
- Scheduling
- Remove status filter when moving up?
- Task markdown support? - colored
- Time tracking: Ability to postpone task and add planned timestamps (calendar entry)
- Speedup: Offline caching & Expiry (no need to fetch potential years of history)
  + Fetch most recent tasks first
  + Relay: compress tracked time for old tasks, filter closed tasks
  + Relay: filter out task status updates within few seconds, also on client side

### Fixes

- Handle event sending rejections (e.g. permissions)
- Recursive filter handling

### Command

- Open Command characters: `_^\=$%~'"`, `{}[]`
- Remove colon from task creation syntax
- reassign undo to `&` and use `@` for people
  
### Conceptual

The following features are not ready to be implemented
because they need conceptualization.
Suggestions welcome!

- Queueing tasks
- Allow adding new parent via description?
- Special commands: help, exit, tutorial, change log level
- Duplicate task (subtasks? timetracking?)
- What if I want to postpone a procedure, i.e. make it pending, or move it across kanban, does this make sense?
- Dependencies (change from tags to properties so they can be added later? or maybe as a status?)
- Templates
- Ownership
- Combined formatting and recursion specifiers
  + progress count/percentage and recursive or not
  + Subtask progress immediate/all/leafs
  + path full / leaf / top

### Interfaces

- TUI: Clear Terminal? Refresh on empty prompt after timeout?
- Kanban, GANTT, Calendar
- Web Interface
- Messenger Integrations (Telegram Bot)
- n8n node
- Caldav Feed: Scheduled (planning) / Tracked (events, timetracking) with args for how far back/forward

## Exemplary Workflows

- Freelancer
- Family Chore Management
- Inter-Disciplinary Project Team -> Company with multiple projects and multiple relays
  + Permissions via status or assignment (reassignment?)
  + Tasks can be blocked while having a status (e.g. kanban column)
  + A meeting can be worked on (tracked) before it starts
  + Schedule for multiple people
- Tracking Daily Routines / Habits

### Contexts

A context is a custom set of filters such as status, tags, assignee
so that the visible tasks are always relevant
and newly created tasks are less of a hassle to type out
since they will automatically take on that context.
By automating these contexts based on triggers, scripts or time,
relevant tasks can be surfaced automatically.

#### Example

In the morning, your groggy brain is good at divergent thinking,
and you like to do sports in the morning.
So for that time, mostr can show you tasks tagged for divergent thinking,
since you are easily distracted filter out those that require the internet,
as well as anything sportsy.
After you come back from sports and had breakfast,
for example detected through a period of inactivity on your device,
you are ready for work, so the different work projects are shown and you delve into one.
After 90 minutes you reach a natural low in your focus,
so mostr surfaces break activities -
such as a short walk, a small workout, some instrument practice
or simply grabbing a snack and drink.
After lunch you like to take an extended afternoon break,
so your call list pops up -
you can give a few people a call as you make a market run,
before going for siesta.
