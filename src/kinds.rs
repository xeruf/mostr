use itertools::Itertools;
use log::info;
use nostr_sdk::{Alphabet, EventBuilder, EventId, Kind, Tag, TagStandard};
use nostr_sdk::TagStandard::Hashtag;

use crate::task::{MARKER_PARENT, State};

pub const METADATA_KIND: u16 = 0;
pub const NOTE_KIND: u16 = 1;
pub const TASK_KIND: u16 = 1621;
pub const TRACKING_KIND: u16 = 1650;
pub const KINDS: [u16; 3] = [
    METADATA_KIND,
    NOTE_KIND,
    TASK_KIND,
];
pub const PROP_KINDS: [u16; 6] = [
    TRACKING_KIND,
    State::Open as u16,
    State::Done as u16,
    State::Closed as u16,
    State::Pending as u16,
    State::Procedure as u16
];

/// Helper for available properties.
/// TODO: use formatting - bold / heading / italics - and generate from code
pub const PROPERTY_COLUMNS: &str =
    "# Available Properties
Immutable:
- `id` - unique task id
- `parentid` - unique task id of the parent, if any
- `name` - initial name of the task
- `created` - task creation timestamp
- `author` - name or abbreviated key of the task creator
Task:
- `status` - pure task status
- `hashtags` - list of hashtags set for the task
- `tags` - values of all nostr tags associated with the event, except event tags
- `desc` - last note on the task
- `description` - accumulated notes on the task
- `time` - time tracked on this task by you
Utilities:
- `state` - indicator of current progress
- `rtime` - time tracked on this tasks and its subtree by everyone
- `progress` - recursive subtask completion in percent
- `subtasks` - how many direct subtasks are complete
- `path` - name including parent tasks
- `rpath` - name including parent tasks up to active task
- TBI `depends` - list all tasks this task depends on before it becomes actionable
Debugging: `kind`, `pubkey`, `props`, `alltags`, `descriptions`";

pub(crate) fn build_tracking<I>(id: I) -> EventBuilder
where
    I: IntoIterator<Item=EventId>,
{
    EventBuilder::new(
        Kind::from(TRACKING_KIND),
        "",
        id.into_iter().map(|id| Tag::event(id)),
    )
}

/// Build a task with informational output and optional labeled kind
pub(crate) fn build_task(name: &str, tags: Vec<Tag>, kind: Option<(&str, Kind)>) -> EventBuilder {
    info!("Created {}task \"{name}\" with tags [{}]",
        kind.map(|k| k.0).unwrap_or_default(),
        tags.iter().map(|tag| format_tag(tag)).join(", "));
    EventBuilder::new(kind.map(|k| k.1).unwrap_or(Kind::from(TASK_KIND)), name, tags)
}

pub(crate) fn build_prop(
    kind: Kind,
    comment: &str,
    id: EventId,
) -> EventBuilder {
    EventBuilder::new(
        kind,
        comment,
        vec![Tag::event(id)],
    )
}

/// Expects sanitized input
pub(crate) fn extract_tags(input: &str) -> (&str, Vec<Tag>) {
    match input.split_once(": ") {
        None => (input, vec![]),
        Some(s) => {
            let tags = s
                .1
                .split_ascii_whitespace()
                .map(|t| Hashtag(t.to_string()).into())
                .collect();
            (s.0, tags)
        }
    }
}

fn format_tag(tag: &Tag) -> String {
    match tag.as_standardized() {
        Some(TagStandard::Event {
                 event_id,
                 marker,
                 ..
             }) => format!("{}: {:.8}", marker.as_ref().map(|m| m.to_string()).unwrap_or(MARKER_PARENT.to_string()), event_id),
        Some(TagStandard::PublicKey {
                 public_key,
                 alias,
                 ..
             }) => format!("Key{}: {:.8}", public_key.to_string(), alias.as_ref().map(|s| format!(" {s}")).unwrap_or_default()),
        Some(TagStandard::Hashtag(content)) =>
            format!("#{content}"),
        _ => tag.content().map_or_else(
            || format!("Kind {}", tag.kind()),
            |content| content.to_string(),
        )
    }
}

pub(crate) fn is_hashtag(tag: &Tag) -> bool {
    tag.single_letter_tag()
        .is_some_and(|letter| letter.character == Alphabet::T)
}

