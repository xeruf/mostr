# Mostr Design & Internals

## Nostr Reference

All used nostr kinds are listed on the top of [kinds.rs](./src/kinds.rs)

Mostr mainly uses the following [NIPs](https://github.com/nostr-protocol/nips):

- Kind 1 for task descriptions and permanent tasks, can contain task property updates (tags, priority)
- Issue Tracking: https://github.com/nostr-protocol/nips/blob/master/34.md
    + Tasks have Kind 1621 (originally: git issue - currently no markdown support implemented)
    + TBI: Kind 1622 for task comments
    + Kind 1630-1633: Task Status (1630 Open, 1631 Done, 1632 Closed, 1633 Pending)
- Own Kind 1650 for time-tracking

Considering to use Calendar: https://github.com/nostr-protocol/nips/blob/master/52.md

- Kind 31922 for GANTT, since it has only Date
- Kind 31923 for Calendar, since it has a time

## Immutability

Apart from user-specific temporary utilities such as the Bookmark List (Kind 10003),
all shared data is immutable, and modifications are recorded as separate events,
providing full audit security.
Deletions are not considered.

### Timestamps

Mostr provides convenient helpers to backdate an action to a limited extent.
But when closing one task with `)10` at 10:00 of the current day
and starting another with `(10` on the same day,
depending on the order of the event ids,
the started task would be terminated immediately
due to the equal timestamp.

That is why I decided to subtract one second from the timestamp
whenever timetracking is stopped,
making sure that the stop event always happens before the start event
when the same timestamp is provided in the interface.
Since the user interface is anyways focused on comprehensible output
and thus slightly fuzzy,
I then also add one second to each timestamp displayed
to make the displayed timestamps more intuitive.