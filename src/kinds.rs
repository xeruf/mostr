use itertools::Itertools;
use log::info;
use nostr_sdk::{Alphabet, EventBuilder, EventId, Kind, Tag, TagStandard};

pub const TASK_KIND: u16 = 1621;
pub const TRACKING_KIND: u16 = 1650;

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
            |content| content.to_string()
        )
    }
}

pub(crate) fn is_hashtag(tag: &Tag) -> bool {
    tag.single_letter_tag()
        .is_some_and(|sltag| sltag.character == Alphabet::T)
}

