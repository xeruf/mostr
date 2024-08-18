use itertools::Itertools;
use log::info;
use nostr_sdk::{Alphabet, EventBuilder, EventId, Kind, Tag, TagStandard};
use nostr_sdk::TagStandard::Hashtag;

pub const METADATA_KIND: u16 = 0;
pub const NOTE_KIND: u16 = 1;
pub const TASK_KIND: u16 = 1621;
pub const PROCEDURE_KIND: u16 = 1639;
pub const TRACKING_KIND: u16 = 1650;
pub const KINDS: [u16; 9] = [
    METADATA_KIND,
    NOTE_KIND,
    TASK_KIND,
    TRACKING_KIND,
    PROCEDURE_KIND,
    1630, 1631, 1632, 1633];

/// Helper for available properties.
/// TODO: use formatting - bold / heading / italics - and generate from code
pub const PROPERTY_COLUMNS: &str =
    "# Available Properties
Immutable:
- `id` - unique task id
- `parentid` - unique task id of the parent, if any
- `name` - initial name of the task
- `created` - task creation timestamp
- `author` - name of the task creator
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
Debugging: `pubkey`, `props`, `alltags`, `descriptions`";

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
                 ..
             }) => format!("Parent: {}", event_id.to_string()[..8].to_string()),
        Some(TagStandard::PublicKey {
                 public_key,
                 ..
             }) => format!("Key: {}", public_key.to_string()[..8].to_string()),
        Some(TagStandard::Hashtag(content)) => format!("#{content}"),
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

