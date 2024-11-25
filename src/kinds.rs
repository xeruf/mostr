use crate::task::MARKER_PARENT;
use crate::tasks::nostr_users::NostrUsers;
use crate::tasks::HIGH_PRIO;
use itertools::Itertools;
use nostr_sdk::{EventBuilder, EventId, Kind, PublicKey, Tag, TagKind, TagStandard};
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
- `description` - all notes on the task
- `time` - time tracked on this task by you
Utilities:
- `state` - indicator of current progress
- `owner` - author or task assignee
- `rtime` - time tracked on this tasks and its subtree by everyone
- `progress` - recursive subtask completion in percent
- `subtasks` - how many direct subtasks are complete
- `path` - name including parent tasks
- `rpath` - name including parent tasks up to active task
- TBI `depends` - list all tasks this task depends on before it becomes actionable
Debugging: `kind`, `pubkey`, `props`, `alltags`, `descriptions`";

pub struct EventTag {
    pub id: EventId,
    pub marker: Option<String>,
}

/// Return event tag if existing
pub(crate) fn match_event_tag(tag: &Tag) -> Option<EventTag> {
    let mut vec = tag.as_slice().into_iter();
    if vec.next() == Some(&"e".to_string()) {
        if let Some(id) = vec.next().and_then(|v| EventId::parse(v).ok()) {
            vec.next();
            return Some(EventTag { id, marker: vec.next().cloned() });
        }
    }
    None
}

pub(crate) fn build_tracking<I>(id: I) -> EventBuilder
where
    I: IntoIterator<Item=EventId>,
{
    EventBuilder::new(Kind::from(TRACKING_KIND), "")
        .tags(id.into_iter().map(Tag::event))
}

/// Formats and joins the tags with commata
pub fn join_tags<'a, T>(tags: T) -> String
where
    T: IntoIterator<Item=&'a Tag>,
{
    tags.into_iter().map(format_tag).join(", ")
}

/// Return Hashtags embedded in the string.
pub(crate) fn extract_hashtags(input: &str) -> impl Iterator<Item=Tag> + '_ {
    input.split_ascii_whitespace()
        .filter(|s| s.starts_with('#'))
        .map(|s| s.trim_start_matches('#'))
        .map(to_hashtag_tag)
}

/// Extracts everything after a " # " as a list of tags 
/// as well as various embedded tags.
///
/// Expects sanitized input.
pub(crate) fn extract_tags(input: &str, users: &NostrUsers) -> (String, Vec<Tag>) {
    let words = input.split_ascii_whitespace();
    let mut tags = Vec::with_capacity(4);
    let result = words.filter(|s| {
        if s.starts_with('@') {
            if let Ok(key) = PublicKey::parse(&s[1..]) {
                tags.push(Tag::public_key(key));
                return false;
            } else if let Some((key, _)) = users.find_user(&s[1..]) {
                tags.push(Tag::public_key(*key));
                return false;
            }
        } else if s.starts_with('*') {
            if s.len() == 1 {
                tags.push(to_prio_tag(HIGH_PRIO));
                return false;
            }
            if let Ok(num) = s[1..].parse::<Prio>() {
               tags.push(to_prio_tag(num * (if s.len() > 2 { 1 } else { 10 })));
               return false
            }
        }
        true
    }).collect_vec();
    let mut split = result.split(|e| { e == &"#" });
    let main = split.next().unwrap().join(" ");
    let mut tags = extract_hashtags(&main)
        .chain(split.flatten().map(|s| to_hashtag_tag(&s)))
        .chain(tags)
        .collect_vec();
    tags.sort();
    tags.dedup();
    (main, tags)
}

pub fn to_hashtag_tag(tag: &str) -> Tag {
    TagStandard::Hashtag(tag.to_string()).into()
}

pub fn format_tag(tag: &Tag) -> String {
    if let Some(et) = match_event_tag(tag) {
        return format!("{}: {:.8}",
                       et.marker.as_ref().map(|m| m.to_string()).unwrap_or(MARKER_PARENT.to_string()),
                       et.id);
    }
    format_tag_basic(tag)
}

pub fn format_tag_basic(tag: &Tag) -> String {
    match tag.as_standardized() {
        Some(TagStandard::PublicKey {
                 public_key,
                 alias,
                 ..
             }) => format!("Key{}: {:.8}", public_key, alias.as_ref().map(|s| format!(" {s}")).unwrap_or_default()),
        Some(TagStandard::Hashtag(content)) =>
            format!("#{content}"),
        _ => tag.as_slice().join(" ")
    }
}

pub fn to_prio_tag(value: Prio) -> Tag {
    Tag::custom(TagKind::Custom(Cow::from(PRIO)), [value.to_string()])
}

#[test]
fn test_extract_tags() {
    assert_eq!(extract_tags("Hello from #mars with #greetings #yeah *4 # # yeah done-it", &Default::default()),
               ("Hello from #mars with #greetings #yeah".to_string(),
                std::iter::once(Tag::custom(TagKind::Custom(Cow::from(PRIO)), [40.to_string()]))
                    .chain(["done-it", "greetings", "mars", "yeah"].into_iter().map(to_hashtag_tag))
                    .collect()));
    assert_eq!(extract_tags("So tagless @hewo #", &Default::default()),
               ("So tagless @hewo".to_string(), vec![]));
}