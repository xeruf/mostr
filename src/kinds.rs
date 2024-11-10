use crate::task::{State, MARKER_PARENT};
use crate::tasks::HIGH_PRIO;
use itertools::Itertools;
use log::info;
use nostr_sdk::TagStandard::Hashtag;
use nostr_sdk::{Alphabet, EventBuilder, EventId, Kind, Tag, TagKind, TagStandard};
use std::borrow::Cow;

pub const TASK_KIND: Kind = Kind::GitIssue;
pub const PROCEDURE_KIND_ID: u16 = 1639;
pub const PROCEDURE_KIND: Kind = Kind::Regular(PROCEDURE_KIND_ID);
pub const TRACKING_KIND: Kind = Kind::Regular(1650);
pub const BASIC_KINDS: [Kind; 4] = [
    Kind::Metadata,
    Kind::TextNote,
    TASK_KIND,
    Kind::Bookmarks,
];
pub const PROP_KINDS: [Kind; 6] = [
    TRACKING_KIND,
    Kind::GitStatusOpen,
    Kind::GitStatusApplied,
    Kind::GitStatusClosed,
    Kind::GitStatusDraft,
    PROCEDURE_KIND,
];

pub type Prio = u16;
pub const PRIO: &str = "priority";

// TODO: use formatting - bold / heading / italics - and generate from code
/// Helper for available properties.
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
        id.into_iter().map(Tag::event),
    )
}

/// Build a task with informational output and optional labeled kind
pub(crate) fn build_task(name: &str, tags: Vec<Tag>, kind: Option<(&str, Kind)>) -> EventBuilder {
    info!("Created {} \"{name}\" with tags [{}]",
        kind.map(|k| k.0).unwrap_or("task"),
        tags.iter().map(format_tag).join(", "));
    EventBuilder::new(kind.map(|k| k.1).unwrap_or(TASK_KIND), name, tags)
}

/// Return Hashtags embedded in the string.
pub(crate) fn extract_hashtags(input: &str) -> impl Iterator<Item=Tag> + '_ {
    input.split_ascii_whitespace()
        .filter(|s| s.starts_with('#'))
        .map(|s| s.trim_start_matches('#'))
        .map(to_hashtag)
}

/// Extracts everything after a " # " as a list of tags 
/// as well as various embedded tags.
///
/// Expects sanitized input.
pub(crate) fn extract_tags(input: &str) -> (String, Vec<Tag>) {
    let words = input.split_ascii_whitespace();
    let mut prio = None;
    let result = words.filter(|s| {
        if s.starts_with('*') {
            if s.len() == 1 {
                prio = Some(HIGH_PRIO);
                return false
            }
            return match s[1..].parse::<Prio>() {
                Ok(num) => {
                    prio = Some(num * (if s.len() > 2 { 1 } else { 10 }));
                    false
                },
                _ => true,
            }
        }
        true
    }).collect_vec();
    let mut split = result.split(|e| { e == &"#" });
    let main = split.next().unwrap().join(" ");
    let tags = extract_hashtags(&main)
        .chain(split.flatten().map(|s| to_hashtag(&s)))
        .chain(prio.map(|p| to_prio_tag(p))).collect();
    (main, tags)
}

fn to_hashtag(tag: &str) -> Tag {
    Hashtag(tag.to_string()).into()
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
             }) => format!("Key{}: {:.8}", public_key, alias.as_ref().map(|s| format!(" {s}")).unwrap_or_default()),
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

pub(crate) fn to_prio_tag(value: Prio) -> Tag {
    Tag::custom(TagKind::Custom(Cow::from(PRIO)), [value.to_string()])
}

#[test]
fn test_extract_tags() {
    assert_eq!(extract_tags("Hello from #mars with #greetings *4 # # yeah done-it"),
               ("Hello from #mars with #greetings".to_string(),
                ["mars", "greetings", "yeah", "done-it"].into_iter().map(to_hashtag)
                    .chain(std::iter::once(Tag::custom(TagKind::Custom(Cow::from(PRIO)), [40.to_string()]))).collect()));
    assert_eq!(extract_tags("So tagless #"),
               ("So tagless".to_string(), vec![]));
}