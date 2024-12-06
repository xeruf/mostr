mod state;
#[cfg(test)]
mod tests;

use fmt::Display;
use std::cmp::Ordering;
use std::collections::btree_set::Iter;
use std::collections::BTreeSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::iter::{once, Chain, Once};
use std::str::FromStr;
use std::string::ToString;

use crate::hashtag::{is_hashtag, Hashtag};
use crate::helpers::{format_timestamp_local, some_non_empty};
use crate::kinds::{match_event_tag, Prio, PRIO, PROCEDURE_KIND, PROCEDURE_KIND_ID, TASK_KIND};
use crate::tasks::now;

pub use crate::task::state::State;
pub use crate::task::state::StateChange;

use colored::{ColoredString, Colorize};
use itertools::Either::{Left, Right};
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use nostr_sdk::{Alphabet, Event, EventId, Kind, PublicKey, SingleLetterTag, Tag, TagKind, Timestamp};

pub static MARKER_PARENT: &str = "parent";
pub static MARKER_DEPENDS: &str = "depends";
pub static MARKER_PROPERTY: &str = "property";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Task {
    /// Event that defines this task
    pub(super) event: Event, // TODO make private
    /// Cached sorted tags of the event with references removed
    tags: Option<BTreeSet<Tag>>,
    /// Task references derived from the event tags
    refs: Vec<(String, EventId)>,
    /// Events belonging to this task, such as state updates and notes
    pub(crate) props: BTreeSet<Event>,
}

impl PartialOrd<Self> for Task {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.event.partial_cmp(&other.event)
    }
}

impl Ord for Task {
    fn cmp(&self, other: &Self) -> Ordering {
        self.event.cmp(&other.event)
    }
}

impl Hash for Task {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.event.id.hash(state);
    }
}

impl Task {
    pub(crate) fn new(event: Event) -> Task {
        let (refs, tags) = event.tags.iter().partition_map(|tag| if let Some(et) = match_event_tag(tag) {
            Left((et.marker.as_ref().map_or(MARKER_PARENT.to_string(), |m| m.to_string()), et.id))
        } else {
            Right(tag.clone())
        });
        // Separate refs for dependencies
        Task {
            props: Default::default(),
            tags: Some(tags).filter(|t: &BTreeSet<Tag>| !t.is_empty()),
            refs,
            event,
        }
    }

    /// All Events including the task and its props in chronological order
    pub(crate) fn all_events(&self) -> impl DoubleEndedIterator<Item=&Event> {
        once(&self.event).chain(self.props.iter().rev())
    }

    pub(crate) fn get_id(&self) -> EventId {
        self.event.id
    }

    pub(crate) fn get_participants(&self) -> impl Iterator<Item=PublicKey> + '_ {
        self.tags()
            .filter(|t| t.kind() == TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::P)))
            .filter_map(|t| t.content()
                .and_then(|c| PublicKey::from_str(c).inspect_err(|e| warn!("Unparseable pubkey in {:?}", t)).ok()))
    }

    pub(crate) fn get_owner(&self) -> PublicKey {
        self.get_participants().next()
            .unwrap_or_else(|| self.event.pubkey)
    }

    /// Trimmed event content or stringified id
    pub(crate) fn get_title(&self) -> String {
        some_non_empty(self.event.content.trim())
            .unwrap_or_else(|| self.get_id().to_string())
    }

    /// Title with leading hashtags removed
    pub(crate) fn get_filter_title(&self) -> String {
        self.event.content.trim().trim_start_matches('#').to_string()
    }

    pub(crate) fn find_refs<'a>(&'a self, marker: &'a str) -> impl Iterator<Item=&'a EventId> {
        self.refs.iter().filter_map(move |(str, id)|
            Some(id).filter(|_| str == marker))
    }

    pub(crate) fn parent_id(&self) -> Option<&EventId> {
        self.find_refs(MARKER_PARENT).next()
    }

    pub(crate) fn find_dependents(&self) -> Vec<&EventId> {
        self.find_refs(MARKER_DEPENDS).collect()
    }

    fn description_events(&self) -> impl DoubleEndedIterator<Item=&Event> + '_ {
        self.props.iter().filter(|event| event.kind == Kind::TextNote)
    }

    /// Description items, ordered newest to oldest
    pub(crate) fn descriptions(&self) -> impl DoubleEndedIterator<Item=&String> + '_ {
        self.description_events()
            .filter_map(|e| Some(&e.content).take_if(|s| !s.trim().is_empty()))
    }

    pub(crate) fn is_task_kind(&self) -> bool {
        self.event.kind == TASK_KIND
    }

    /// Whether this is an actionable task - false if stateless activity
    pub(crate) fn is_task(&self) -> bool {
        self.is_task_kind() ||
            self.props.iter().any(|event| State::try_from(event.kind).is_ok())
    }

    pub(crate) fn priority(&self) -> Option<Prio> {
        self.priority_raw().and_then(|s| s.parse().ok())
    }

    pub(crate) fn priority_raw(&self) -> Option<&str> {
        self.props.iter()
            .chain(once(&self.event))
            .find_map(|p| {
                p.tags.iter().find_map(|t|
                    t.content().take_if(|_| { t.kind().to_string() == PRIO }))
            })
    }

    fn states(&self) -> impl DoubleEndedIterator<Item=StateChange> + '_ {
        self.props.iter().filter_map(|event| {
            event.kind.try_into().ok().map(|s| StateChange {
                name: some_non_empty(&event.content),
                state: s,
                time: event.created_at,
            })
        })
    }

    pub fn last_state_update(&self) -> Timestamp {
        self.state().map(|s| s.time).unwrap_or(self.event.created_at)
    }

    pub fn state_at(&self, time: Timestamp) -> Option<StateChange> {
        // TODO do not iterate constructed state objects
        let state = self.states().take_while_inclusive(|ts| ts.time > time);
        state.last().map(|ts| {
            if ts.time <= time {
                ts
            } else {
                self.default_state()
            }
        })
    }

    /// Returns the current state if this is a task rather than an activity
    pub fn state(&self) -> Option<StateChange> {
        let now = now();
        self.state_at(now)
    }

    pub(crate) fn pure_state(&self) -> State {
        State::from(self.state())
    }

    pub(crate) fn state_or_default(&self) -> StateChange {
        self.state().unwrap_or_else(|| self.default_state())
    }

    /// Returns None for activities.
    pub(crate) fn state_label(&self) -> Option<ColoredString> {
        self.state()
            .or_else(|| Some(self.default_state()).filter(|_| self.is_task()))
            .map(|state| state.get_colored_label())
    }

    fn default_state(&self) -> StateChange {
        StateChange {
            name: None,
            state: State::Open,
            time: self.event.created_at,
        }
    }

    pub(crate) fn list_hashtags(&self) -> impl Iterator<Item=Hashtag> + '_ {
        self.tags().filter_map(|t| Hashtag::try_from(t).ok())
    }

    /// Tags of this task that are not event references, newest to oldest
    fn tags(&self) -> impl Iterator<Item=&Tag> {
        self.props.iter()
            .flat_map(|e| e.tags.iter()
                .filter(|t| t.single_letter_tag().is_none_or(|s| s.character != Alphabet::E)))
            .chain(self.tags.iter().flatten())
    }

    fn join_tags<P>(&self, predicate: P) -> String
    where
        P: FnMut(&&Tag) -> bool,
    {
        self.tags()
            .filter(predicate)
            .map(|t| t.content().unwrap().to_string())
            .sorted_unstable()
            .dedup()
            .join(" ")
    }

    pub(crate) fn get(&self, property: &str) -> Option<String> {
        match property {
            // Static
            "id" => Some(self.get_id().to_string()),
            "parentid" => self.parent_id().map(|i| i.to_string()),
            "name" => Some(self.event.content.clone()),
            "key" | "pubkey" => Some(self.event.pubkey.to_string()),
            "created" => Some(format_timestamp_local(&self.event.created_at)),
            "kind" => Some(self.event.kind.to_string()),
            // Dynamic
            "priority" => self.priority_raw().map(|c| c.to_string()),
            "status" => self.state_label().map(|c| c.to_string()),
            "desc" => self.descriptions().next().cloned(),
            "description" => Some(self.descriptions().rev().join(" ")),
            "hashtags" => Some(self.join_tags(|tag| { is_hashtag(tag) })),
            "tags" => Some(self.join_tags(|_| true)), // TODO test these!
            "alltags" => Some(format!("{:?}", self.tags)),
            "refs" => Some(format!("{:?}", self.refs.iter().map(|re| format!("{}: {}", re.0, re.1)).collect_vec())),
            "props" => Some(format!(
                "{:?}",
                self.props
                    .iter()
                    .map(|e| format!("{} kind {} \"{}\"", e.created_at, e.kind, e.content))
                    .collect_vec()
            )),
            "descriptions" => Some(format!("{:?}", self.descriptions().collect_vec())),
            _ => {
                warn!("Unknown task property {}", property);
                None
            }
        }
    }
}
