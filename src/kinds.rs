use itertools::Itertools;
use log::info;
use nostr_sdk::{Alphabet, EventBuilder, EventId, Kind, Tag, TagStandard};

pub const TASK_KIND: u16 = 1621;
pub const TRACKING_KIND: u16 = 1650;
pub const KINDS: [u16; 7] = [1, TASK_KIND, TRACKING_KIND, 1630, 1631, 1632, 1633];

pub const PROPERTY_COLUMNS: &str = "Available properties:
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
- `time` - time tracked on this task by you
- `rtime` - time tracked on this tasks and its subtree by everyone
- `progress` - recursive subtask completion in percent
- `subtasks` - how many direct subtasks are complete";

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

pub(crate) fn build_task(name: &str, tags: Vec<Tag>) -> EventBuilder {
    info!("Created task \"{name}\" with tags [{}]", tags.iter().map(|tag| format_tag(tag)).join(", "));
    EventBuilder::new(Kind::from(TASK_KIND), name, tags)
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

