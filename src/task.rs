use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::ops::Div;

use nostr_sdk::{Alphabet, Event, EventBuilder, EventId, Kind, Tag, Timestamp};

use crate::EventSender;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Task {
    pub(crate) event: Event,
    pub(crate) children: HashSet<EventId>,
    pub(crate) props: BTreeSet<Event>,
    /// Cached sorted tags of the event
    pub(crate) tags: Option<BTreeSet<Tag>>,
}

impl Task {
    pub(crate) fn new(event: Event) -> Task {
        Task {
            children: Default::default(),
            props: Default::default(),
            tags: if event.tags.is_empty() {
                None
            } else {
                Some(event.tags.iter().cloned().collect())
            },
            event,
        }
    }

    pub(crate) fn get_id(&self) -> &EventId {
        &self.event.id
    }

    pub(crate) fn parent_id(&self) -> Option<EventId> {
        for tag in self.event.tags.iter() {
            match tag {
                Tag::Event { event_id, .. } => return Some(*event_id),
                _ => {}
            }
        }
        None
    }

    pub(crate) fn get_title(&self) -> String {
        Some(self.event.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.get_id().to_string())
    }

    fn descriptions(&self) -> impl Iterator<Item = &String> + '_ {
        self.props.iter().filter_map(|event| {
            if event.kind == Kind::TextNote {
                Some(&event.content)
            } else {
                None
            }
        })
    }

    fn states(&self) -> impl Iterator<Item = TaskState> + '_ {
        self.props.iter().filter_map(|event| {
            event.kind.try_into().ok().map(|s| TaskState {
                name: if event.content.is_empty() {
                    None
                } else {
                    Some(event.content.clone())
                },
                state: s,
                time: event.created_at.clone(),
            })
        })
    }

    pub(crate) fn state(&self) -> Option<TaskState> {
        self.states().max_by_key(|t| t.time)
    }

    pub(crate) fn pure_state(&self) -> State {
        self.state().map_or(State::Open, |s| s.state)
    }

    pub(crate) fn set_state(
        &mut self,
        sender: &EventSender,
        state: State,
        comment: &str,
    ) -> Option<Event> {
        sender
            .submit(EventBuilder::new(
                state.kind(),
                comment,
                vec![Tag::event(self.event.id)],
            ))
            .inspect(|e| {
                self.props.insert(e.clone());
            })
    }

    fn default_state(&self) -> TaskState {
        TaskState {
            name: None,
            state: State::Open,
            time: self.event.created_at,
        }
    }

    /// Total time this task has been active.
    /// TODO: Consider caching
    pub(crate) fn time_tracked(&self) -> u64 {
        let mut total = 0;
        let mut start: Option<Timestamp> = None;
        for state in self.states() {
            match state.state {
                State::Active => start = start.or(Some(state.time)),
                _ => {
                    if let Some(stamp) = start {
                        total += (state.time - stamp).as_u64();
                        start = None;
                    }
                }
            }
        }
        if let Some(start) = start {
            total += (Timestamp::now() - start).as_u64();
        }
        total
    }
    
    fn filter_tags<P>(&self, predicate: P) -> Option<String> 
    where P: FnMut(&&Tag) -> bool{
        self.tags.as_ref().map(|tags| {
            tags.into_iter()
                .filter(predicate)
                .map(|t| format!("{}", t.content().unwrap()))
                .collect::<Vec<String>>()
                .join(" ")
        })
    }

    pub(crate) fn get(&self, property: &str) -> Option<String> {
        match property {
            "id" => Some(self.event.id.to_string()),
            "parentid" => self.parent_id().map(|i| i.to_string()),
            "state" => self.state().map(|s| s.to_string()),
            "name" => Some(self.event.content.clone()),
            "time" => Some(format!("{}m", self.time_tracked().div(60))),
            "hashtags" => self.filter_tags(|tag| tag.single_letter_tag().is_some_and(|sltag| sltag.character == Alphabet::T)),
            "tags" => self.filter_tags(|tag| !tag.single_letter_tag().is_some_and(|sltag| sltag.character == Alphabet::E)),
            "props" => Some(format!(
                "{:?}",
                self.props
                    .iter()
                    .map(|e| format!("{} kind {} '{}'", e.created_at, e.kind, e.content))
                    .collect::<Vec<String>>()
            )),
            "descriptions" => Some(format!("{:?}", self.descriptions().collect::<Vec<&String>>())),
            "desc" | "description" => self.descriptions().last().cloned(),
            _ => {
                eprintln!("Unknown task property {}", property);
                None
            }
        }
    }
}

pub(crate) struct TaskState {
    state: State,
    name: Option<String>,
    time: Timestamp,
}
impl TaskState {
    pub(crate) fn get_label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.state.to_string())
    }
    pub(crate) fn matches_label(&self, label: &str) -> bool {
        self.state == State::Active
            || self
                .name
                .as_ref()
                .is_some_and(|n| n.eq_ignore_ascii_case(label))
            || self.state.to_string().eq_ignore_ascii_case(label)
    }
}
impl fmt::Display for TaskState {
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

#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) enum State {
    Closed,
    Open,
    Active,
    Done,
}
impl TryFrom<Kind> for State {
    type Error = ();

    fn try_from(value: Kind) -> Result<Self, Self::Error> {
        match value.as_u32() {
            1630 => Ok(State::Open),
            1631 => Ok(State::Done),
            1632 => Ok(State::Closed),
            1633 => Ok(State::Active),
            _ => Err(()),
        }
    }
}
impl State {
    pub(crate) fn kind(&self) -> Kind {
        match self {
            State::Open => Kind::from(1630),
            State::Done => Kind::from(1631),
            State::Closed => Kind::from(1632),
            State::Active => Kind::from(1633),
        }
    }
}
impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
