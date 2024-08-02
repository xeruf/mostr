use itertools::Itertools;
use log::info;
use nostr_sdk::{Alphabet, EventBuilder, EventId, GenericTagValue, Kind, Tag};

pub const TASK_KIND: u64 = 1621;
pub const TRACKING_KIND: u64 = 1650;

pub(crate) fn build_tracking<I>(id: I) -> EventBuilder
where I: IntoIterator<Item=EventId> {
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
    tag.content().map(|c| {
        match c {
            GenericTagValue::PublicKey(key) => format!("Key: {}", key.to_string()[..8].to_string()),
            GenericTagValue::EventId(id) => format!("Parent: {}", id.to_string()[..8].to_string()),
            GenericTagValue::String(str) => {
                if is_hashtag(tag) {
                    format!("#{str}")
                } else {
                    str
                }
            }
        }
    }).unwrap_or_else(|| format!("Kind {}", tag.kind()))
}

pub(crate) fn is_hashtag(tag: &Tag) -> bool {
    tag.single_letter_tag()
        .is_some_and(|sltag| sltag.character == Alphabet::T)
}

