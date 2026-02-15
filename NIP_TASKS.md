# NIP: Task Management Kinds and Processes
Status: Draft

This document describes a task-management profile that reuses existing NIPs and defines a small number of new kinds.
It is built on top of NIP-01 (events and tags), NIP-22 (comments), NIP-34 (git/issue tracking kinds), and NIP-51 (bookmarks lists).

**Kinds**

These kinds are defined in existing NIPs but are used here with task-management semantics:

- `Kind 1` (Text Note, NIP-01)
- `Kind 1111` (Comment, NIP-22)
- `Kind 1621` (Git Issue, NIP-34)
- `Kind 1630` (Git Status Open, NIP-34)
- `Kind 1631` (Git Status Applied, NIP-34)
- `Kind 1632` (Git Status Closed, NIP-34)
- `Kind 1633` (Git Status Draft, NIP-34)
- `Kind 10003` (Bookmarks list, NIP-51)

These kinds are added:

- `Kind 1639` (Procedure State)
- `Kind 1650` (Time Tracking)

**Tag Markers (NIP-01 Event Tags With Task Markers)**

This profile uses `e` tags (NIP-01) with explicit markers to describe task relationships. The marker is stored in the 4th field of the `e` tag.

- `parent` means the tagged event is the parent task.
- `depends` means the tagged event must be completed before this task is actionable.
- `property` means this event is a property update for the tagged task.

**Core Processes**

**Task Creation (NIP-34 Kind 1621)**

A task is a `Kind 1621` event (NIP-34) with the task title as its content.

- Optional `e` tag with marker `parent` to attach the task to a parent task.
- Optional `e` tag with marker `depends` to model dependencies.
- Optional `p` tag for assignee. If no assignee is provided, the creator is added as the assignee.
- Optional `#` hashtags and custom `priority` tags for filtering and sorting.

When creating a task under a Procedure (see below), the client may add a `depends` tag that references the most recent child of the Procedure, creating a linear dependency chain.

**Task State Updates (NIP-34 Status Kinds + Procedure Extension)**

Task state changes are recorded as separate events (immutability preserved, NIP-01).

- `Kind 1630` Open
- `Kind 1631` Done
- `Kind 1632` Closed
- `Kind 1633` Pending
- `Kind 1639` Procedure (new in this profile)

Each state event:

- Uses the status label or a user comment as content.
- Includes one or more `e` tags with marker `property` pointing at the task(s) being updated.

When closing a task, clients may emit `property` tags for the entire task subtree so all descendants are closed as well.

**Notes and Activities (NIP-01 Kind 1)**

Notes are stored as `Kind 1` events (NIP-01).

- If the current item is a task, the note is attached with an `e` tag marked `property`.
- If the current item is not a task, the note is treated as an activity and uses an `e` tag marked `parent`.

Notes form the task description history. The most recent note is used as the current description summary.
Only the task owner SHOULD add `Kind 1` description notes for that task.

**Comments (NIP-22 Kind 1111)**

Any participant MAY comment on a task using `Kind 1111` as defined in NIP-22 (plaintext content).
Comments MUST use NIP-22 scoping rules (uppercase tags for the root task, lowercase for the parent comment).
This enables discussion without granting edit rights to task state or description.

**Procedures (Ordered Task Lists, Kind 1639)**

A Procedure is a task with an explicit Procedure state (`Kind 1639`).

- Child tasks created under a Procedure automatically depend on the previous child.
- This yields an ordered, step-by-step task list without breaking the parent/child tree model.

**Time Tracking (Kind 1650)**

Clients track active work via `Kind 1650` events.

- Start tracking: emit `Kind 1650` with an `e` tag referencing the active task.
- Stop tracking: emit `Kind 1650` with no `e` tag.

Durations are computed by pairing tracking start events with the next tracking stop event or the current time.

**Bookmarks (NIP-51 Kind 10003)**

Pinned tasks are stored in `Kind 10003` bookmarks owned by the user. The event references the pinned task ids via `e` tags.

**Custom Tags**

- `priority` is a custom tag used to store numeric priority values.
- Hashtags are stored as standard `#` tags.

**Turning Other Kinds Into Tasks**

Any event of a suitable kind (for example an article or wiki entry) can be treated as a task by emitting a task state event
that references it via `e` tags marked `property`.
This allows progress tracking to be layered onto existing content without changing the original event kind.

**Collaboration-Focused Relays and Permissions**

This profile is intended primarily for collaboration-centric relays rather than social-media-style feeds.
Future relay implementations may enforce fine-grained permissions such as:

- Only the task owner may add or change task description notes (`Kind 1`) or emit state updates.
- Owner reassignment rules (for example, explicit reassignment events or multi-sig approval).
- Scoped moderation or role-based access for comment visibility and task updates.

Such relay policies are optional and do not change the event formats, but can provide stronger guarantees for shared task workflows.

**Compatibility Notes (Fit With Existing NIPs)**

- NIP-01 provides the event, tag, and relay mechanics used throughout this profile.
- NIP-34 provides the issue and status kinds and their semantics; this profile reuses those kinds for tasks.
- NIP-22 cleanly separates discussion (`kind:1111`) from task state and description updates.
- NIP-51 lists (`kind:10003`) are a natural fit for task bookmarking and pinning.
- The new kinds (1639, 1650) are narrow extensions that do not conflict with existing kinds or tag conventions.
