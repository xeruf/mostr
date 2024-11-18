use fmt::Display;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::iter::once;
use std::string::ToString;

use colored::{ColoredString, Colorize};
use itertools::Either::{Left, Right};
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use nostr_sdk::{Alphabet, Event, EventId, Kind, Tag, Timestamp};

use crate::helpers::{format_timestamp_local, some_non_empty};
use crate::kinds::{is_hashtag, match_event_tag, Prio, PRIO, PROCEDURE_KIND, PROCEDURE_KIND_ID, TASK_KIND};
use crate::tasks::now;

pub static MARKER_PARENT: &str = "parent";
pub static MARKER_DEPENDS: &str = "depends";
pub static MARKER_PROPERTY: &str = "property";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Task {
    /// Event that defines this task
    pub(crate) event: Event,
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

    pub(crate) fn get_id(&self) -> &EventId {
        &self.event.id
    }

    pub(crate) fn find_refs<'a>(&'a self, marker: &'a str) -> impl Iterator<Item=&'a EventId> {
        self.refs.iter().filter_map(move |(str, id)| Some(id).filter(|_| str == marker))
    }

    pub(crate) fn parent_id(&self) -> Option<&EventId> {
        self.find_refs(MARKER_PARENT).next()
    }

    pub(crate) fn get_dependendees(&self) -> Vec<&EventId> {
        self.find_refs(MARKER_DEPENDS).collect()
    }

    /// Trimmed event content or stringified id
    pub(crate) fn get_title(&self) -> String {
        some_non_empty(self.event.content.trim())
            .unwrap_or_else(|| self.get_id().to_string())
    }

    pub(crate) fn get_filter_title(&self) -> String {
        self.event.content.trim().trim_start_matches('#').to_string()
    }

    pub(crate) fn description_events(&self) -> impl Iterator<Item=&Event> + '_ {
        self.props.iter().filter(|event| event.kind == Kind::TextNote)
    }

    pub(crate) fn descriptions(&self) -> impl Iterator<Item=&String> + '_ {
        self.description_events().map(|e| &e.content)
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

    fn states(&self) -> impl DoubleEndedIterator<Item=TaskState> + '_ {
        self.props.iter().filter_map(|event| {
            event.kind.try_into().ok().map(|s| TaskState {
                name: some_non_empty(&event.content),
                state: s,
                time: event.created_at,
            })
        })
    }

    pub(crate) fn last_state_update(&self) -> Timestamp {
        self.state().map(|s| s.time).unwrap_or(self.event.created_at)
    }

    pub(crate) fn state(&self) -> Option<TaskState> {
        let now = now();
        // TODO do not iterate constructed state objects
        let state = self.states().take_while_inclusive(|ts| ts.time > now);
        state.last().map(|ts| {
            if ts.time <= now {
                ts
            } else {
                self.default_state()
            }
        })
    }

    pub(crate) fn pure_state(&self) -> State {
        self.state().map_or(State::Open, |s| s.state)
    }

    pub(crate) fn state_or_default(&self) -> TaskState {
        self.state().unwrap_or_else(|| self.default_state())
    }

    /// Returns None for activities.
    pub(crate) fn state_label(&self) -> Option<ColoredString> {
        self.state()
            .or_else(|| Some(self.default_state()).filter(|_| self.is_task()))
            .map(|state| state.get_colored_label())
    }

    fn default_state(&self) -> TaskState {
        TaskState {
            name: None,
            state: State::Open,
            time: self.event.created_at,
        }
    }

    pub(crate) fn get_hashtags(&self) -> impl Iterator<Item=&Tag> {
        self.tags().filter(|t| is_hashtag(t))
    }

    fn tags(&self) -> impl Iterator<Item=&Tag> {
        self.tags.iter().flatten().chain(
            self.props.iter().flat_map(|e| e.tags.iter()
                .filter(|t| t.single_letter_tag().is_none_or(|s| s.character != Alphabet::E)))
        )
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
            "id" => Some(self.event.id.to_string()),
            "parentid" => self.parent_id().map(|i| i.to_string()),
            "name" => Some(self.event.content.clone()),
            "key" | "pubkey" => Some(self.event.pubkey.to_string()),
            "created" => Some(format_timestamp_local(&self.event.created_at)),
            "kind" => Some(self.event.kind.to_string()),
            // Dynamic
            "priority" => self.priority_raw().map(|c| c.to_string()),
            "status" => self.state_label().map(|c| c.to_string()),
            "desc" => self.descriptions().last().cloned(),
            "description" => Some(self.descriptions().join(" ")),
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
            "descriptions" => Some(format!(
                "{:?}",
                self.descriptions().collect_vec()
            )),
            _ => {
                warn!("Unknown task property {}", property);
                None
            }
        }
    }
}

pub(crate) struct TaskState {
    pub(crate) state: State,
    name: Option<String>,
    pub(crate) time: Timestamp,
}
impl TaskState {
    pub(crate) fn get_label_for(state: &State, comment: &str) -> String {
        some_non_empty(comment).unwrap_or_else(|| state.to_string())
    }
    pub(crate) fn get_label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.state.to_string())
    }
    pub(crate) fn get_colored_label(&self) -> ColoredString {
        self.state.colorize(&self.get_label())
    }
    pub(crate) fn matches_label(&self, label: &str) -> bool {
        self.name.as_ref().is_some_and(|n| n.eq_ignore_ascii_case(label))
            || self.state.to_string().eq_ignore_ascii_case(label)
    }
}
impl Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state_str = self.state.to_string();
        write!(
            f,
            "{}",
            self.name
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.eq_ignore_ascii_case(&state_str))
                .map_or(state_str, |s| format!("{}: {}", self.state, s))
        )
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Ord, PartialOrd, Eq)]
pub(crate) enum State {
    /// Actionable
    Open = 1630,
    /// Completed
    Done,
    /// Not Actionable (anymore)
    Closed,
    /// Temporarily not actionable
    Pending,
    /// Actionable ordered task list
    Procedure = PROCEDURE_KIND_ID as isize,
}
impl TryFrom<&str> for State {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_ascii_lowercase().as_str() {
            "closed" => Ok(State::Closed),
            "done" => Ok(State::Done),
            "pending" => Ok(State::Pending),
            "proc" | "procedure" | "list" => Ok(State::Procedure),
            "open" => Ok(State::Open),
            _ => Err(()),
        }
    }
}
impl TryFrom<Kind> for State {
    type Error = ();

    fn try_from(value: Kind) -> Result<Self, Self::Error> {
        match value {
            Kind::GitStatusOpen => Ok(State::Open),
            Kind::GitStatusApplied => Ok(State::Done),
            Kind::GitStatusClosed => Ok(State::Closed),
            Kind::GitStatusDraft => Ok(State::Pending),
            _ => {
                if value == PROCEDURE_KIND {
                    Ok(State::Procedure)
                } else {
                    Err(())
                }
            }
        }
    }
}
impl State {
    pub(crate) fn is_open(&self) -> bool {
        matches!(self, State::Open | State::Pending | State::Procedure)
    }

    pub(crate) fn kind(self) -> u16 {
        self as u16
    }

    pub(crate) fn colorize(&self, str: &str) -> ColoredString {
        match self {
            State::Open => str.green(),
            State::Done => str.bright_black(),
            State::Closed => str.magenta(),
            State::Pending => str.yellow(),
            State::Procedure => str.blue(),
        }
    }
}
impl From<State> for Kind {
    fn from(value: State) -> Self {
        Kind::from(value.kind())
    }
}
impl Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[cfg(test)]
mod tasks_test {
    use super::*;
    use nostr_sdk::{EventBuilder, Keys};

    #[test]
    fn test_state() {
        let keys = Keys::generate();
        let mut task = Task::new(
            EventBuilder::new(TASK_KIND, "task").tags([Tag::hashtag("tag1")])
                .sign_with_keys(&keys).unwrap());
        assert_eq!(task.pure_state(), State::Open);
        assert_eq!(task.get_hashtags().count(), 1);
        task.props.insert(
            EventBuilder::new(State::Done.into(), "")
                .sign_with_keys(&keys).unwrap());
        assert_eq!(task.pure_state(), State::Done);
        task.props.insert(
            EventBuilder::new(State::Open.into(), "").tags([Tag::hashtag("tag2")])
                .custom_created_at(Timestamp::from(Timestamp::now() - 2))
                .sign_with_keys(&keys).unwrap());
        assert_eq!(task.pure_state(), State::Done);
        assert_eq!(task.get_hashtags().count(), 2);
        task.props.insert(
            EventBuilder::new(State::Closed.into(), "")
                .custom_created_at(Timestamp::from(Timestamp::now() + 1))
                .sign_with_keys(&keys).unwrap());
        assert_eq!(task.pure_state(), State::Closed);
    }
}
