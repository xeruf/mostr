use std::collections::{BTreeSet, HashSet};
use std::fmt;

use nostr_sdk::{Event, EventId, Kind, Tag, Timestamp};

pub(crate) struct Task {
    pub(crate) event: Event,
    pub(crate) children: HashSet<EventId>,
    pub(crate) props: BTreeSet<Event>,
}
impl Task {
    pub(crate) fn new(event: Event) -> Task {
        Task {
            event,
            children: Default::default(),
            props: Default::default(),
        }
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

    fn descriptions(&self) -> impl Iterator<Item = String> + '_ {
        self.props.iter().filter_map(|event| {
            if event.kind == Kind::TextNote {
                Some(event.content.clone())
            } else {
                None
            }
        })
    }

    fn states(&self) -> impl Iterator<Item = TaskState> + '_ {
        self.props.iter().filter_map(|event| {
            match event.kind.as_u32() {
                1630 => Some(State::Open),
                1631 => Some(State::Done),
                1632 => Some(State::Closed),
                1633 => Some(State::Active),
                _ => None,
            }
            .map(|s| TaskState {
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

    fn state(&self) -> Option<TaskState> {
        self.states().max_by_key(|t| t.time)
    }

    pub(crate) fn pure_state(&self) -> State {
        self.state().map_or(State::Open, |s| s.state)
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
        total
    }

    pub(crate) fn get(&self, property: &str) -> Option<String> {
        match property {
            "id" => Some(self.event.id.to_string()),
            "parentid" => self.parent_id().map(|i| i.to_string()),
            "state" => self.state().map(|s| s.to_string()),
            "name" => Some(self.event.content.clone()),
            "time" => Some(self.time_tracked().to_string()), // TODO: format properly
            "props" => Some(format!(
                "{:?}",
                self.props
                    .iter()
                    .map(|e| format!("{} kind {} '{}'", e.created_at, e.kind, e.content))
                    .collect::<Vec<String>>()
            )),
            "desc" | "description" => self.descriptions().fold(None, |total, s| {
                Some(match total {
                    None => s,
                    Some(i) => i + " " + &s,
                })
            }),
            _ => {
                eprintln!("Unknown column {}", property);
                None
            }
        }
    }
}

struct TaskState {
    name: Option<String>,
    state: State,
    time: Timestamp,
}
impl fmt::Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            self.state,
            self.name
                .as_ref()
                .map_or(String::new(), |s| format!(": {}", s))
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
