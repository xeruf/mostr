use crate::helpers::some_non_empty;
use crate::kinds::{PROCEDURE_KIND, PROCEDURE_KIND_ID};

use colored::{ColoredString, Colorize};
use nostr_sdk::{Kind, Timestamp};
use std::fmt;
use std::fmt::Display;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct StateChange {
    pub(super) state: State,
    pub(super) name: Option<String>,
    pub(super) time: Timestamp,
}
impl StateChange {
    pub fn get_label_for(state: &State, comment: &str) -> String {
        some_non_empty(comment).unwrap_or_else(|| state.to_string())
    }
    pub fn get_label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.state.to_string())
    }
    pub fn get_colored_label(&self) -> ColoredString {
        self.state.colorize(&self.get_label())
    }
    pub fn matches_label(&self, label: &str) -> bool {
        self.name.as_ref().is_some_and(|n| n.eq_ignore_ascii_case(label))
            || self.state.to_string().eq_ignore_ascii_case(label)
    }
    pub fn get_timestamp(&self) -> Timestamp {
        self.time
    }
}
impl Display for StateChange {
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
impl From<Option<StateChange>> for State {
    fn from(value: Option<StateChange>) -> Self {
        value.map_or(State::Open, |s| s.state)
    }
}


#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum State {
    /// Actionable
    Open = 1630,
    /// Completed
    Done,
    /// Not Actionable (anymore)
    Closed,
    /// Temporarily not actionable
    Pending,
    /// Ordered task list
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