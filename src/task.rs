use fmt::Display;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::string::ToString;

use colored::{ColoredString, Colorize};
use itertools::Either::{Left, Right};
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use nostr_sdk::{Event, EventId, Kind, Tag, TagStandard, Timestamp};

use crate::helpers::{format_timestamp_local, some_non_empty};
use crate::kinds::{is_hashtag, TASK_KIND};

pub static MARKER_PARENT: &str = "parent";
pub static MARKER_DEPENDS: &str = "depends";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Task {
    /// Event that defines this task
    pub(crate) event: Event,
    /// Cached sorted tags of the event with references remove - do not modify!
    pub(crate) tags: Option<BTreeSet<Tag>>,
    /// Task references derived from the event tags
    refs: Vec<(String, EventId)>,

    /// Reference to children, populated dynamically
    pub(crate) children: HashSet<EventId>,
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

impl Task {
    pub(crate) fn new(event: Event) -> Task {
        let (refs, tags) = event.tags.iter().partition_map(|tag| match tag.as_standardized() {
            Some(TagStandard::Event { event_id, marker, .. }) =>
                Left((marker.as_ref().map_or(MARKER_PARENT.to_string(), |m| m.to_string()), *event_id)),
            _ => Right(tag.clone()),
        });
        // Separate refs for dependencies
        Task {
            children: Default::default(),
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

    pub(crate) fn get_title(&self) -> String {
        Some(self.event.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.get_id().to_string())
    }

    pub(crate) fn description_events(&self) -> impl Iterator<Item=&Event> + '_ {
        self.props.iter().filter(|event| event.kind == Kind::TextNote)
    }

    pub(crate) fn descriptions(&self) -> impl Iterator<Item=&String> + '_ {
        self.description_events().map(|e| &e.content)
    }

    pub(crate) fn is_task(&self) -> bool {
        self.event.kind.as_u16() == TASK_KIND ||
            self.states().next().is_some()
    }

    fn states(&self) -> impl Iterator<Item=TaskState> + '_ {
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
        self.states().last()
    }

    pub(crate) fn pure_state(&self) -> State {
        self.state().map_or(State::Open, |s| s.state)
    }

    pub(crate) fn state_or_default(&self) -> TaskState {
        self.state().unwrap_or_else(|| self.default_state())
    }

    /// Returns None for a stateless task.
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

    fn filter_tags<P>(&self, predicate: P) -> Option<String>
    where
        P: FnMut(&&Tag) -> bool,
    {
        self.tags.as_ref().map(|tags| {
            tags.iter()
                .filter(predicate)
                .map(|t| t.content().unwrap().to_string())
                .join(" ")
        })
    }

    pub(crate) fn get(&self, property: &str) -> Option<String> {
        match property {
            // Static
            "id" => Some(self.event.id.to_string()),
            "parentid" => self.parent_id().map(|i| i.to_string()),
            "name" => Some(self.event.content.clone()),
            "pubkey" => Some(self.event.pubkey.to_string()),
            "created" => Some(format_timestamp_local(&self.event.created_at)),
            "kind" => Some(self.event.kind.to_string()),
            // Dynamic
            "status" => self.state_label().map(|c| c.to_string()),
            "desc" => self.descriptions().last().cloned(),
            "description" => Some(self.descriptions().join(" ")),
            "hashtags" => self.filter_tags(|tag| { is_hashtag(tag) }),
            "tags" => self.filter_tags(|_| true),
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

pub const PROCEDURE_KIND: u16 = 1639;
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
    Procedure = PROCEDURE_KIND as isize,
}
impl From<&str> for State {
    fn from(value: &str) -> Self {
        match value {
            "Closed" => State::Closed,
            "Done" => State::Done,
            "Pending" => State::Pending,
            "Proc" | "Procedure" | "List" => State::Procedure,
            _ => State::Open,
        }
    }
}
impl TryFrom<Kind> for State {
    type Error = ();

    fn try_from(value: Kind) -> Result<Self, Self::Error> {
        match value.as_u16() {
            1630 => Ok(State::Open),
            1631 => Ok(State::Done),
            1632 => Ok(State::Closed),
            1633 => Ok(State::Pending),
            PROCEDURE_KIND => Ok(State::Procedure),
            _ => Err(()),
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
