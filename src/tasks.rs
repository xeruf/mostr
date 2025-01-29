mod nostr_users;
#[cfg(test)]
mod tests;
mod children_traversal;
mod durations;

use crate::event_sender::{EventSender, MostrMessage};
use crate::hashtag::Hashtag;
use crate::helpers::{
    format_timestamp_local, format_timestamp_relative, format_timestamp_relative_to,
    parse_tracking_stamp, some_non_empty, to_string_or_default, CHARACTER_THRESHOLD,
};
use crate::kinds::*;
use crate::task::{State, StateChange, Task, MARKER_DEPENDS, MARKER_PARENT, MARKER_PROPERTY};
use crate::tasks::children_traversal::ChildrenTraversal;
use crate::tasks::durations::{referenced_events, timestamps, Durations};
pub use crate::tasks::nostr_users::NostrUsers;

use chrono::{Local, TimeDelta, TimeZone};
use colored::Colorize;
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use nostr_sdk::{Alphabet, Event, EventBuilder, EventId, JsonUtil, Keys, Kind, Metadata, PublicKey, RelayUrl, SingleLetterTag, Tag, TagKind, Timestamp, Url};
use regex::bytes::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::{Display, Formatter};
use std::iter::{empty, once, FusedIterator};
use std::ops::{Deref, Div, Rem};
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::mpsc::Sender;

const DEFAULT_PRIO: Prio = 25;
const QUICK_PRIO: Prio = 35;
pub const HIGH_PRIO: Prio = 85;

/// Amount of seconds to treat as "now"
const MAX_OFFSET: u64 = 9;
pub(crate) fn now() -> Timestamp {
    Timestamp::now() + MAX_OFFSET
}

type TaskMap = HashMap<EventId, Task>;
pub(super) trait TaskMapMethods {
    fn children_of<'a>(&'a self, task: &'a Task) -> impl Iterator<Item=&Task> + 'a;
    fn children_for<'a>(&'a self, id: Option<EventId>) -> impl Iterator<Item=&Task> + 'a;
    fn children_ids_for<'a>(&'a self, id: EventId) -> impl Iterator<Item=EventId> + 'a;
}
impl TaskMapMethods for TaskMap {
    fn children_of<'a>(&'a self, task: &'a Task) -> impl Iterator<Item=&Task> + 'a {
        self.children_for(Some(task.get_id().clone()))
    }

    fn children_for<'a>(&'a self, id: Option<EventId>) -> impl Iterator<Item=&Task> + 'a {
        self.values().filter(move |t| t.parent_id() == id.as_ref())
    }
    fn children_ids_for<'a>(&'a self, id: EventId) -> impl Iterator<Item=EventId> + 'a {
        self.children_for(Some(id)).map(|t| t.get_id())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TasksRelay {
    /// The Tasks
    tasks: TaskMap,
    /// History of active tasks by PubKey
    history: HashMap<PublicKey, BTreeMap<Timestamp, Event>>,
    /// Index of known users with metadata
    users: NostrUsers,
    /// Own pinned tasks
    bookmarks: Vec<EventId>,

    /// The task properties currently visible
    properties: Vec<String>,
    /// The task properties sorted by
    sorting: VecDeque<String>, // TODO prefix +/- for asc/desc, no prefix for default

    /// A filtered view of the current tasks.
    /// Would like this to be Task references
    /// but that doesn't work unless I start meddling with Rc everywhere.
    view: Vec<EventId>,
    search_depth: usize,
    view_depth: usize,
    pub(crate) recurse_activities: bool,
    // Last position used in interface - needs auto-update
    //position: Option<EventId>,

    /// Currently active tags
    tags: BTreeSet<Hashtag>,
    /// Tags filtered out from view
    tags_excluded: BTreeSet<Hashtag>,
    /// Current active state
    state: StateFilter,
    /// Current priority for filtering and new tasks
    priority: Option<Prio>,
    keys: Vec<PublicKey>,
    own_keys: Vec<PublicKey>,

    sender: EventSender,
    overflow: VecDeque<Event>,
    pub(crate) custom_time: Option<Timestamp>,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum StateFilter {
    #[default]
    Default,
    All,
    State(String),
}
impl StateFilter {
    fn from(str: &str) -> Self {
        StateFilter::State(str.to_string())
    }

    fn indicator(&self) -> String {
        match self {
            StateFilter::Default => "".to_string(),
            StateFilter::All => " ?ALL".to_string(),
            StateFilter::State(str) => format!(" ?{str}"),
        }
    }

    fn matches(&self, task: &Task) -> bool {
        match self {
            StateFilter::Default => task.pure_state().is_open(),
            StateFilter::All => true,
            StateFilter::State(filter) => task.state().is_some_and(|t| t.matches_label(filter)),
        }
    }

    fn as_option(&self) -> Option<String> {
        if let StateFilter::State(str) = self {
            Some(str.to_string())
        } else {
            None
        }
    }
}
impl Display for StateFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                StateFilter::Default => "open tasks".to_string(),
                StateFilter::All => "all tasks".to_string(),
                StateFilter::State(s) => format!("state {s}"),
            }
        )
    }
}

impl TasksRelay {
    pub(crate) fn from(
        url: Option<RelayUrl>,
        tx: &Sender<MostrMessage>,
        keys: &Keys,
        metadata: Option<Metadata>,
    ) -> Self {
        let mut new = Self::with_sender(EventSender::from(url, tx, keys));
        metadata.map(|m| new.users.insert(keys.public_key(), m, Timestamp::now()));
        new
    }

    pub(crate) fn with_sender(sender: EventSender) -> Self {
        TasksRelay {
            tasks: Default::default(),
            history: Default::default(),
            users: Default::default(),
            bookmarks: Default::default(),

            properties: [
                "owner",
                "prio",
                "state",
                "rtime",
                "hashtags",
                "rpath",
                "desc",
            ].into_iter().map(|s| s.to_string()).collect(),
            sorting: [
                "priority",
                "status",
                "owner",
                "hashtags",
                "rtime",
                "name",
            ].into_iter().map(|s| s.to_string()).collect(),

            view: Default::default(),
            tags: Default::default(),
            tags_excluded: Default::default(),
            state: Default::default(),
            priority: None,
            keys: vec![sender.pubkey()],
            own_keys: vec![sender.pubkey()],

            search_depth: 4,
            view_depth: 0,
            recurse_activities: false,

            sender,
            overflow: Default::default(),
            custom_time: None,
        }
    }

    pub(crate) fn process_overflow(&mut self) {
        let elements = self.overflow.len();
        let mut issues = 0;
        for _ in 0..elements {
            if let Some(event) = self.overflow.pop_back() {
                if let Some(event) = self.add_prop(event) {
                    warn!("Unable to sort Event {:?}", event);
                    issues += 1;
                    //self.overflow.push_back(event);
                }
            }
        }
        if elements > 0 {
            info!(
                "Reprocessed {elements} updates with {issues} issues{}",
                self.sender.url.as_ref().map(|url| format!(" from {url}")).unwrap_or_default()
            );
        }
    }

    // Accessors

    #[inline]
    pub(crate) fn get_by_id(&self, id: &EventId) -> Option<&Task> {
        self.tasks.get(id)
    }

    #[inline]
    pub(crate) fn len(&self) -> usize { self.tasks.len() }

    fn own_keys(&self) -> &Vec<PublicKey> { &self.own_keys }

    fn own_key(&self) -> PublicKey { self.sender.pubkey() }

    pub(crate) fn get_position(&self) -> Option<EventId> {
        self.get_position_at(now()).1
    }

    pub(crate) fn get_position_timestamped(&self) -> (Timestamp, Option<EventId>) {
        self.get_position_at(now())
    }

    pub(super) fn parse_tracking_stamp_relative(&self, input: &str) -> Option<Timestamp> {
        let pos = self.get_position_timestamped();
        let mut pos_time = pos.1.and_then(|_| Local.timestamp_opt(pos.0.as_u64() as i64, 0).earliest());
        parse_tracking_stamp(input, pos_time.take_if(|t| Local::now() - *t > TimeDelta::hours(6)))
    }

    fn sorting_key(&self, task: &Task) -> impl Ord {
        self.sorting
            .iter()
            .map(|p| self.get_property(task, p.as_str()))
            .collect_vec()
    }

    // TODO binary search
    /// Gets last position change before the given timestamp
    fn get_position_at(&self, timestamp: Timestamp) -> (Timestamp, Option<EventId>) {
        self.history_from(timestamp)
            .last()
            .filter(|e| e.created_at <= timestamp)
            .map_or_else(
                || (Timestamp::now(), None),
                |e| (e.created_at, referenced_event(e)),
            )
    }

    fn nonclosed_tasks(&self) -> impl Iterator<Item=&Task> {
        self.tasks.values()
            .filter(|t| t.pure_state() != State::Closed)
    }

    pub(crate) fn all_hashtags(&self) -> BTreeSet<Hashtag> {
        self.nonclosed_tasks().flat_map(|t| t.list_hashtags()).collect()
    }

    /// Dynamic time tracking overview for current task or current user.
    pub(crate) fn times_tracked(&self, limit: usize) -> String {
        let (label, times) = self.times_tracked_with(&self.own_key()); // TODO self.own_keys
        let times = times.collect_vec();
        format!("{}\n{}",
                if times.is_empty() {
                    label
                } else {
                    format!("{}{}",
                            if limit > times.len() || limit == usize::MAX { "All ".to_string() } else if limit < 20 { "Recent ".to_string() } else { format!("Latest {limit} Entries of ") },
                            label)
                }.italic(),
                &times[times.len().saturating_sub(limit)..].join("\n"))
    }

    pub(crate) fn history_for(
        &self,
        key: &PublicKey,
    ) -> Option<impl DoubleEndedIterator<Item=String> + '_> {
        self.history.get(key).map(|hist| {
            let mut last = None;
            // TODO limit history to active tags
            hist.values().filter_map(move |event| {
                let new = some_non_empty(&event.tags.iter()
                    .filter_map(|t| t.content())
                    .map(|str|
                        EventId::from_str(str).ok()
                            .map_or(str.to_string(), |id| self.get_task_path(Some(id))))
                    .join(" "));
                if new != last {
                    // TODO omit intervals <2min - but I think I need threeway variable tracking for that
                    // TODO alternate color with grey between days
                    last = new;
                    return Some(format!(
                        "{} {}",
                        format_timestamp_local(&event.created_at),
                        last.as_ref().unwrap_or(&"---".to_string())
                    ));
                }
                None
            })
        })
    }

    pub(crate) fn times_tracked_for(
        &self,
        key: &PublicKey,
    ) -> (String, Box<dyn DoubleEndedIterator<Item=String> + '_>) {
        match self.history_for(key) {
            Some(hist) => (
                format!(
                    "Time-Tracking History for {}:",
                    self.users.get_displayname(&key)
                ),
                Box::from(hist),
            ),
            None => ("Nothing time-tracked yet".to_string(), Box::from(empty())),
        }
    }

    /// Time tracked for current position or key
    /// Note: reversing the iterator skips most recent timetracking stops
    pub(crate) fn times_tracked_with(
        &self,
        key: &PublicKey,
    ) -> (String, Box<dyn DoubleEndedIterator<Item=String> + '_>) {
        match self.get_position() {
            None => self.times_tracked_for(key),
            Some(id) => {
                // TODO show current recursive if there is a pubkey
                let ids = [id];
                let mut history =
                    self.history.iter().flat_map(|(key, set)| {
                        let mut vec = Vec::with_capacity(set.len() / 2);
                        let mut iter = timestamps(set.values(), &ids).tuples();
                        while let Some(((start, _), (end, _))) = iter.next() {
                            // Filter out intervals <2 mins
                            if start.as_u64() + 120 < end.as_u64() {
                                vec.push(format!(
                                    "{} - {} by {}",
                                    format_timestamp_local(start),
                                    format_timestamp_relative_to(end, start),
                                    self.users.get_displayname(key)
                                ))
                            }
                        }
                        iter.into_buffer().for_each(|(stamp, _)| {
                            vec.push(format!(
                                "{} started by {}",
                                format_timestamp_local(stamp),
                                self.users.get_displayname(key)
                            ))
                        });
                        vec
                    }).collect_vec();
                // TODO sorting depends on timestamp format - needed to interleave different people
                history.sort_unstable();
                (
                    format!("Times Tracked on {:?}", self.get_task_title(&id)),
                    Box::from(history.into_iter()),
                )
            }
        }
    }

    /// Total time in seconds tracked on this task by the current user.
    pub(crate) fn time_tracked(&self, id: EventId) -> u64 {
        Durations::from(self.get_own_events_history(), &[id])
            .sum::<Duration>()
            .as_secs()
    }

    /// Total time in seconds tracked on this task and its subtasks by all users.
    fn total_time_tracked(&self, id: EventId) -> u64 {
        let mut total = 0;

        let children = ChildrenTraversal::from(&self, id).get_all();
        for user in self.history.values() {
            total += Durations::from(user.values(), &children)
                .sum::<Duration>()
                .as_secs();
        }
        total
    }

    fn total_progress(&self, id: EventId) -> Option<f32> {
        self.get_by_id(&id).and_then(|task| match task.pure_state() {
            State::Closed => None,
            State::Done => Some(1.0),
            _ => {
                let mut sum = 0f32;
                let mut count = 0;
                for prog in self.tasks
                    .children_ids_for(task.get_id())
                    .filter_map(|e| self.total_progress(e))
                {
                    sum += prog;
                    count += 1;
                }
                Some(if count > 0 { sum / (count as f32) } else { 0.0 })
            }
        })
    }

    // Parents

    /// Move up `count` parent tasks from current position
    pub(crate) fn up_by(&self, count: usize) -> Option<EventId> {
        let pos = self.get_position();
        let mut result = pos.as_ref();
        for _ in 0..count {
            result = self.get_parent(result);
        }
        result.cloned()
    }

    pub(crate) fn get_parent(&self, id: Option<&EventId>) -> Option<&EventId> {
        id.and_then(|id| self.get_by_id(id))
            .and_then(|t| t.parent_id())
    }

    pub(crate) fn pubkey_str(&self) -> Option<String> {
        match self.keys.first() {
            None => Some("ALL".to_string()),
            Some(key) => {
                if &self.keys != self.own_keys() {
                    Some(self.users.get_username(&key))
                } else {
                    None
                }
            }
        }
    }

    // TODO test with context elements
    /// Visual representation of current context
    pub(crate) fn get_prompt_suffix(&self) -> String {
        let mut prompt = String::with_capacity(128);
        for tag in self.tags.iter() {
            prompt.push_str(&format!(" #{}", tag));
        }
        for tag in self.tags_excluded.iter() {
            prompt.push_str(&format!(" -#{}", tag));
        }
        prompt.push_str(&self.state.indicator());
        self.priority
            .map(|p| prompt.push_str(&format!(" *{:02}", p)));
        prompt
    }

    pub(crate) fn get_task_path(&self, id: Option<EventId>) -> String {
        join_tasks(self.traverse_up_from(id), true)
            .filter(|s| !s.is_empty())
            .or_else(|| id.map(|id| id.to_string()))
            .unwrap_or_default()
    }

    pub(crate) fn get_relative_path(&self, id: EventId) -> String {
        join_tasks(
            self.traverse_up_from(Some(id))
                .take_while(|t| Some(t.get_id()) != self.get_position()),
            false,
        ).unwrap_or(id.to_string())
    }

    /// Iterate over the task referenced by the given id and all its available parents.
    fn traverse_up_from(&self, id: Option<EventId>) -> ParentIterator {
        ParentIterator {
            tasks: &self.tasks,
            current: id,
        }
    }

    // Helpers

    fn resolve_tasks_rec<'a>(
        &'a self,
        iter: impl Iterator<Item=&'a Task>,
        sparse: bool,
        depth: usize,
    ) -> Vec<&'a Task> {
        iter.sorted_by_cached_key(|task| self.sorting_key(task))
            .flat_map(move |task| {
                if !self.state.matches(task) {
                    return vec![];
                }
                let mut new_depth = depth;
                if depth > 0 && (!self.recurse_activities || task.is_task()) {
                    new_depth = depth - 1;
                    if sparse && new_depth > self.view_depth && self.filter(task) {
                        new_depth = self.view_depth;
                    }
                }
                if new_depth > 0 {
                    let mut children =
                        self.resolve_tasks_rec(self.tasks.children_of(&task), sparse, new_depth);
                    if !children.is_empty() {
                        if !sparse {
                            children.push(task);
                        }
                        return children;
                    }
                }
                return if self.filter(task) { vec![task] } else { vec![] };
            })
            .collect_vec()
    }

    /// Executes the given function with each task referenced by this event with no or property marker.
    /// Returns true if any task was found.
    pub(crate) fn referenced_tasks<F: Fn(&mut Task)>(&mut self, event: &Event, f: F) -> bool {
        let mut found = false;
        for tag in event.tags.iter() {
            if let Some(event_tag) = match_event_tag(tag) {
                if event_tag.marker.as_ref()
                    .is_none_or(|m| m.to_string() == MARKER_PROPERTY)
                {
                    self.tasks.get_mut(&event_tag.id).map(|t| {
                        found = true;
                        f(t);
                    });
                }
            }
        }
        found
    }

    #[inline]
    pub(crate) fn get_current_task(&self) -> Option<&Task> {
        self.get_position().and_then(|id| self.get_by_id(&id))
    }

    fn filter(&self, task: &Task) -> bool {
        self.state.matches(task) &&
            (!task.is_task() || self.keys.is_empty() ||
                self.keys.iter().any(|p| p == &task.get_owner() ||
                    task.list_hashtags().any(|t| t.matches(&self.users.get_username(&p))))) &&
            self.priority.is_none_or(|prio| {
                task.priority().unwrap_or(DEFAULT_PRIO) >= prio
            }) &&
            !task.list_hashtags().any(|tag| self.tags_excluded.contains(&tag)) &&
            (self.tags.is_empty() || {
                let mut iter = task.list_hashtags().sorted_unstable();
                self.tags.iter().all(|tag| iter.any(|t| &t == tag))
            })
    }

    // TODO sparse is deprecated and only left for tests
    pub(crate) fn filtered_tasks(&self, position: Option<EventId>, sparse: bool) -> Vec<&Task> {
        let roots = self.tasks.children_for(position);
        let mut current =
            self.resolve_tasks_rec(roots, sparse, self.search_depth + self.view_depth);
        if current.is_empty() {
            if !self.tags.is_empty() {
                let mut children = self.tasks.children_for(position).peekable();
                if children.peek().is_some() {
                    current = self.resolve_tasks_rec(children, true, 9);
                    if sparse {
                        if current.is_empty() {
                            println!("No tasks here matching{}", self.get_prompt_suffix());
                        } else {
                            println!("Found matching tasks beyond specified search depth:");
                        }
                    }
                }
            }
        }

        let ids = current.iter().map(|t| t.get_id()).collect_vec();
        let mut bookmarks =
            if sparse && current.is_empty() {
                vec![]
            } else {
                // TODO highlight bookmarks
                self.bookmarks.iter()
                    .filter(|id| !position.is_some_and(|p| &&p == id) && !ids.contains(id))
                    .filter_map(|id| self.get_by_id(id))
                    .filter(|t| self.filter(t))
                    .collect_vec()
            };
        current.append(&mut bookmarks);

        current
    }

    fn visible_tasks(&self) -> Vec<&Task> {
        let tasks = self.viewed_tasks();
        if self.view.is_empty() && !tasks.is_empty() {
            let bookmarks = self.bookmarked_tasks_deduped(&tasks);
            return bookmarks.chain(tasks.into_iter()).collect_vec();
        }
        tasks
    }

    fn viewed_tasks(&self) -> Vec<&Task> {
        let view = self.view.iter()
            .flat_map(|id| self.get_by_id(id))
            .collect_vec();
        if self.search_depth > 0 && view.is_empty() {
            self.resolve_tasks_rec(
                self.tasks.children_for(self.get_position()),
                true,
                self.search_depth + self.view_depth,
            )
        } else {
            self.resolve_tasks_rec(view.into_iter(), true, self.view_depth + 1)
        }
    }

    fn quick_access_raw(&self) -> impl Iterator<Item=EventId> + '_ {
        // TODO add recent tasks (most time tracked + recently created)
        self.bookmarks.iter().cloned()
            .chain(
                // Latest
                self.tasks.values()
                    .sorted_unstable()
                    .take(3).map(|t| t.get_id()))
            .chain(
                // Highest Prio
                self.tasks.values()
                    .filter_map(|t| t.priority().filter(|p| *p >= QUICK_PRIO)
                        .map(|p| (p, t)))
                    .sorted_unstable().rev()
                    .take(3).map(|(_, t)| t.get_id())
            )
    }

    fn bookmarked_tasks_deduped(&self, visible: &[&Task]) -> impl Iterator<Item=&Task> {
        let tree = visible.iter()
            .flat_map(|task| self.traverse_up_from(Some(task.get_id())))
            .unique();
        let pos = self.get_position();
        let ids: HashSet<EventId> = tree.map(|t| t.get_id()).chain(pos).collect();
        self.quick_access_raw()
            .filter(|id| !ids.contains(id))
            .filter_map(|id| self.get_by_id(&id))
            .filter(|t| self.filter(t))
            .sorted_by_cached_key(|t| self.sorting_key(t))
            .dedup()
    }

    fn get_property(&self, task: &Task, str: &str) -> String {
        let mut children = self.tasks.children_of(task).peekable();
        // Only show progress for non-activities with children
        let progress =
            children.peek()
                .filter(|_| task.is_task())
                .and_then(|_| self.total_progress(task.get_id()));
        let prog_string = progress.map_or(String::new(), |p| format!("{:2.0}%", p * 100.0));
        match str {
            "subtasks" => {
                let mut total = 0;
                let mut done = 0;
                for subtask in children {
                    let state = subtask.pure_state();
                    total += &(state != State::Closed).into();
                    done += &(state == State::Done).into();
                }
                if total > 0 {
                    format!("{done}/{total}")
                } else {
                    "".to_string()
                }
            }
            "state" => {
                if let Some(task) = task
                    .find_dependents()
                    .iter()
                    .filter_map(|id| self.get_by_id(id))
                    .find(|t| t.pure_state().is_open())
                {
                    return format!("Blocked by \"{}\"", task.get_title())
                        .bright_red()
                        .to_string();
                }
                let state = task.pure_state();
                if state.is_open() && progress.is_some_and(|p| p > 0.1) {
                    state.colorize(&prog_string)
                } else {
                    task.state_label().unwrap_or_default()
                }.to_string()
            }
            "progress" => prog_string.clone(),

            "owner" => format!("{:.6}", self.users.get_username(&task.get_owner())),
            "assignee" => format!("{:.6}", task.get_assignee().map(|u| self.users.get_username(&u)).unwrap_or_default()),
            "author" | "creator" => format!("{:.6}", self.users.get_username(&task.event.pubkey)), // FIXME temporary until proper column alignment
            "prio" => self
                .traverse_up_from(Some(task.get_id()))
                .find_map(Task::priority_raw)
                .map(|p| p.to_string())
                .unwrap_or_else(|| {
                    if self.priority.is_some() {
                        DEFAULT_PRIO.to_string().dimmed().to_string()
                    } else {
                        "".to_string()
                    }
                }),
            "path" => self.get_task_path(Some(task.get_id())),
            "rpath" => self.get_relative_path(task.get_id()),
            // TODO format strings configurable
            "time" => display_time("MMMm", self.time_tracked(task.get_id())),
            "rtime" => display_time("HH:MM", self.total_time_tracked(task.get_id())),
            prop => task.get(prop).unwrap_or_default(),
        }
    }

    pub(super) fn find_users(&self, name: &str) -> Vec<(PublicKey, String)> {
        self.users.find_user_with_displayname(name).collect()
    }

    // Movement and Selection

    /// Toggle bookmark on the given id.
    /// Returns whether it was added (true) or removed (false).
    pub(crate) fn toggle_bookmark(&mut self, id: EventId) -> nostr_sdk::Result<bool> {
        let added = match self.bookmarks.iter().position(|b| b == &id) {
            None => {
                self.bookmarks.push(id);
                true
            }
            Some(pos) => {
                self.bookmarks.remove(pos);
                false
            }
        };
        self.sender.submit(
            EventBuilder::new(Kind::Bookmarks, "mostr pins")
                .tags(self.bookmarks.iter().map(|id| Tag::event(*id))),
        )?;
        Ok(added)
    }

    pub(crate) fn reset_key_filter(&mut self) {
        if self.keys == self.own_keys {
            self.view.clear();
            info!("Showing everybody's tasks");
            self.keys.clear()
        } else {
            info!("Showing own tasks");
            self.keys = self.own_keys().clone();
        }
    }

    pub(crate) fn set_key_filter(&mut self, key: Vec<PublicKey>) {
        self.keys = key
    }

    pub(crate) fn set_filter_since(&mut self, time: Timestamp) -> bool {
        // TODO filter at both ends
        self.set_filter(|t| t.last_state_update() > time)
    }

    pub(crate) fn get_filtered<P>(&self, position: Option<EventId>, predicate: P) -> Vec<EventId>
    where
        P: Fn(&&Task) -> bool,
    {
        self.filtered_tasks(position, false)
            .into_iter()
            .filter(predicate)
            .map(|t| t.get_id())
            .collect()
    }

    pub(crate) fn set_filter<P>(&mut self, predicate: P) -> bool
    where
        P: Fn(&&Task) -> bool,
    {
        self.set_view(self.get_filtered(self.get_position(), predicate))
    }

    pub(crate) fn set_view_bookmarks(&mut self) -> bool {
        self.set_view(self.bookmarks.clone())
    }

    /// Set currently visible tasks.
    /// Returns whether there are any.
    pub(crate) fn set_view(&mut self, view: Vec<EventId>) -> bool {
        if view.is_empty() {
            warn!("No match for filter!");
            self.view = view;
            return false;
        }
        self.view = view;
        true
    }

    pub(crate) fn clear_filters(&mut self) {
        self.state = StateFilter::Default;
        self.keys = self.own_keys().clone();
        self.priority = None;
        self.view.clear();
        self.tags.clear();
        self.tags_excluded.clear();
        info!("Reset all filters");
    }

    pub(crate) fn has_tag_filter(&self) -> bool {
        !self.tags.is_empty() || !self.tags_excluded.is_empty()
    }

    pub(crate) fn print_hashtags(&self) {
        println!(
            "Hashtags of all known tasks:\n{}",
            self.all_hashtags().into_iter().join(" ").italic()
        );
    }

    /// Returns true if tags have been updated, false if it printed something
    pub(crate) fn update_tags(&mut self, tags: impl IntoIterator<Item=Hashtag>) -> bool {
        let mut peekable = tags.into_iter().peekable();
        if self.tags.is_empty() && peekable.peek().is_none() {
            if !self.tags_excluded.is_empty() {
                self.tags_excluded.clear();
            }
            self.print_hashtags();
            false
        } else {
            self.set_tags(peekable);
            true
        }
    }

    fn set_tags(&mut self, tags: impl IntoIterator<Item=Hashtag>) {
        self.tags.clear();
        self.tags.extend(tags);
    }

    pub(crate) fn add_tag(&mut self, tag: &str) {
        self.view.clear();
        info!("Added tag filter for #{tag}");
        let tag = Hashtag::from(tag);
        self.tags_excluded.remove(&tag);
        self.tags.insert(tag);
    }

    pub(crate) fn remove_tag(&mut self, tag: &str) {
        self.view.clear();
        let len = self.tags.len();
        self.tags.retain(|t| !t.contains(tag));
        if self.tags.len() < len {
            info!("Removed tag filters containing {tag}");
        } else {
            self.tags_excluded.insert(Hashtag::from(tag).into());
            info!("Excluding #{tag} from view");
        }
    }

    pub(crate) fn set_priority(&mut self, priority: Option<Prio>) {
        self.view.clear();
        match priority {
            None => info!("Removing priority filter"),
            Some(prio) => info!("Filtering for priority {}", prio),
        }
        self.priority = priority;
    }

    pub(crate) fn set_state_filter(&mut self, state: StateFilter) {
        self.view.clear();
        info!("Filtering for {}", state);
        self.state = state;
    }

    pub(crate) fn move_up(&mut self) {
        self.move_to(self.get_current_task().and_then(|t| t.parent_id()).cloned());
    }

    pub(crate) fn flush(&self) {
        self.sender.force_flush();
    }

    /// Returns ids of tasks matching the given string.
    ///
    /// Tries, in order:
    /// - single case-insensitive exact name match in visible tasks
    /// - single case-insensitive exact name match in all tasks
    /// - visible tasks starting with given arg case-sensitive
    /// - visible tasks where any word starts with given arg case-insensitive
    pub(crate) fn get_matching(&self, position: Option<EventId>, arg: &str) -> Vec<EventId> {
        if let Ok(id) = EventId::parse(arg) {
            return vec![id];
        }
        let lowercase_arg = arg.to_ascii_lowercase();
        // TODO apply regex to all matching, parse as plain match
        let regex = Regex::new(&format!(r"\b{}", lowercase_arg));

        let mut filtered: Vec<EventId> = Vec::with_capacity(32);
        let mut filtered_fuzzy: Vec<EventId> = Vec::with_capacity(32);
        for task in self.filtered_tasks(position, false) {
            let content = task.get_filter_title();
            let lowercase = content.to_ascii_lowercase();
            if lowercase == lowercase_arg {
                return vec![task.get_id()];
            } else if content.starts_with(arg) {
                filtered.push(task.get_id())
            } else if regex.as_ref()
                .map(|r| r.is_match(lowercase.as_bytes()))
                .unwrap_or_else(|_| lowercase.starts_with(&lowercase_arg)) {
                filtered_fuzzy.push(task.get_id())
            }
        }
        // Find global exact match
        for task in self.tasks.values() {
            if task.get_filter_title().to_ascii_lowercase() == lowercase_arg &&
                // exclude closed tasks and their subtasks
                !self.traverse_up_from(Some(task.get_id())).any(|t| !t.pure_state().is_open())
            {
                return vec![task.get_id()];
            }
        }

        if filtered.is_empty() {
            filtered = filtered_fuzzy;
        }
        let immediate = filtered.iter().filter(
            |t| self.get_by_id(t).is_some_and(|t| t.parent_id() == position.as_ref())).collect_vec();
        if immediate.len() == 1 {
            return immediate.into_iter().cloned().collect_vec();
        }
        filtered
    }

    /// Finds out what to do with the given string, one of:
    /// - filtering the visible tasks
    /// - entering the only matching task
    /// - creating a new task
    /// Returns an EventId if a new Task was created.
    pub(crate) fn filter_or_create(
        &mut self,
        position: Option<EventId>,
        arg: &str,
    ) -> Option<EventId> {
        let filtered = self.get_matching(position, arg);
        match filtered.len() {
            0 => {
                // No match, new task
                self.view.clear();
                if arg.len() < CHARACTER_THRESHOLD {
                    warn!("New task name needs at least {CHARACTER_THRESHOLD} characters");
                    return None;
                }
                self.make_task_with(arg, self.position_tags_for(position), true)
            }
            1 => {
                // One match, activate
                self.move_to(filtered.into_iter().nth(0));
                None
            }
            _ => {
                // Multiple match, filter
                self.move_to(position);
                self.set_view(filtered);
                None
            }
        }
    }

    /// Returns all recent events from history until the first event at or before the given timestamp.
    fn history_from(&self, stamp: Timestamp) -> impl Iterator<Item=&Event> {
        self.history.get(&self.sender.pubkey())
            .map(|hist| {
                hist.values().rev().take_while_inclusive(move |e| e.created_at > stamp)
            }).into_iter().flatten()
    }

    pub(crate) fn move_to(&mut self, target: Option<EventId>) {
        if let Some(time) = self.custom_time {
            self.track_at(time, target);
            return;
        }

        self.view.clear();
        let pos = self.get_position();
        if target == pos {
            debug!("Flushing Tasks because of move in place");
            self.sender.flush();
            return;
        }

        if !target
            .and_then(|id| self.get_by_id(&id))
            .is_some_and(|t| t.parent_id() == pos.as_ref())
        {
            debug!("Flushing Tasks because of move beyond child");
            self.sender.flush();
        }

        let now = Timestamp::now();
        let offset: u64 = self
            .history_from(now)
            .skip_while(|e| e.created_at.as_u64() > now.as_u64() + MAX_OFFSET)
            .count() as u64;
        if offset >= MAX_OFFSET {
            warn!("Whoa you are moving around quickly! Give me a few seconds to process.")
        }
        self.submit(
            build_tracking(target)
                .custom_created_at(now + offset),
        );
    }

    // Updates

    pub(crate) fn make_event_tag_from_id(&self, id: EventId, marker: &str) -> Tag {
        Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::E)),
            [
                id.to_string(),
                to_string_or_default(self.sender.url.as_ref()),
                marker.to_string(),
            ],
        )
    }

    pub(crate) fn make_event_tag(&self, event: &Event, marker: &str) -> Tag {
        Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::E)),
            [
                event.id.to_string(),
                to_string_or_default(self.sender.url.as_ref()),
                marker.to_string(),
                event.pubkey.to_string(),
            ],
        )
    }

    pub(crate) fn parent_tag(&self) -> Option<Tag> {
        self.get_position()
            .map(|p| self.make_event_tag_from_id(p, MARKER_PARENT))
    }

    pub(crate) fn position_tags(&self) -> Vec<Tag> {
        self.position_tags_for(self.get_position())
    }

    pub(crate) fn position_tags_for(&self, position: Option<EventId>) -> Vec<Tag> {
        position.map_or(vec![], |pos| {
            let mut tags = Vec::with_capacity(2);
            tags.push(self.make_event_tag_from_id(pos, MARKER_PARENT));
            self.get_by_id(&pos).map(|task| {
                if task.pure_state() == State::Procedure {
                    self.tasks
                        .children_of(task)
                        .max()
                        .map(|t| tags.push(self.make_event_tag(&t.event, MARKER_DEPENDS)));
                }
            });
            tags
        })
    }

    fn context_hashtags(&self) -> impl Iterator<Item=Tag> + '_ {
        self.tags.iter().map(Tag::from)
    }

    /// Moves up and creates a sibling task dependent on the current one
    ///
    /// Returns true if successful, false if there is no current task
    pub(crate) fn make_dependent_sibling(&mut self, input: &str) -> bool {
        if let Some(pos) = self.get_position() {
            self.move_up();
            self.make_task_with(
                input,
                self.get_position()
                    .map(|par| self.make_event_tag_from_id(par, MARKER_PARENT))
                    .into_iter()
                    .chain(once(self.make_event_tag_from_id(pos, MARKER_DEPENDS))),
                true,
            );
            return true;
        }
        false
    }

    /// Creates a task including current tag filters if content is not empty.
    ///
    /// @param set_state: whether to set context state - use false if setting state manually
    ///
    /// Sanitizes input
    pub(crate) fn make_task_with(
        &mut self,
        input: &str,
        tags: impl IntoIterator<Item=Tag>,
        set_state: bool,
    ) -> Option<EventId> {
        let (input, mut input_tags) = extract_tags(input.trim(), &self.users);
        if input.is_empty() {
            warn!("Task content must not be empty!");
            return None;
        }
        info!(
            "Created task \"{input}\" with tags [{}]",
            join_tags(&input_tags)
        );
        input_tags.extend(tags);
        let id = self.make_task_unchecked(&input, input_tags);
        if set_state {
            self.state
                .as_option()
                .inspect(|s| self.set_state_for_with(id, s));
        }
        Some(id)
    }

    /// Create the task, only adding context tags
    fn make_task_unchecked(
        &mut self,
        input: &str,
        tags: Vec<Tag>,
    ) -> EventId {
        let assignee =
            if tags.iter().any(|t| t.kind() == TagKind::p()) {
                None
            } else {
                self.keys.first().map(|p| Tag::public_key(*p))
            };
        let prio =
            if tags.iter().any(|t| t.kind().to_string() == PRIO) {
                None
            } else {
                self.priority.map(|p| to_prio_tag(p))
            };
        debug!("Making task {} with Assignee {:?} Prio {:?} Tags {:?}", input, assignee, prio, tags);
        let builder =
            EventBuilder::new(TASK_KIND, input)
                .tags(self.context_hashtags())
                .tags(tags)
                .tags(prio)
                .tags(assignee);
        debug!("{:?}", builder);
        self.submit(builder)
    }

    /// Primarily for tests
    fn make_task_unwrapped(&mut self, input: &str) -> EventId {
        self.make_task(input).unwrap()
    }

    /// Creates a task following the current state if input is sufficient.
    ///
    /// Sanitizes input
    pub(crate) fn make_task(&mut self, input: &str) -> Option<EventId> {
        self.make_task_with(input, self.position_tags(), true)
    }

    pub(crate) fn make_task_and_enter(&mut self, input: &str, state: State) {
        if let Some(id) = self.make_task_with(input, self.position_tags(), false) {
            self.set_state_for(id, "", state);
            self.move_to(Some(id));
        }
    }


    pub(crate) fn get_task_title(&self, id: &EventId) -> String {
        self.get_by_id(id).map_or(id.to_string(), |t| t.get_title())
    }

    /// Parse relative time string and track for current position
    ///
    /// Returns false and prints a message if parsing failed
    pub(crate) fn track_from(&mut self, str: &str) -> bool {
        parse_tracking_stamp(str, None)
            .and_then(|stamp| self.track_at(stamp, self.get_position()))
            .is_some()
    }

    pub(crate) fn track_at(
        &mut self,
        mut time: Timestamp,
        target: Option<EventId>,
    ) -> Option<EventId> {
        if target.is_none() {
            // Prevent random overlap with tracking started in the same second
            time = time - 1;
        } else if let Some(hist) = self.history.get(&self.sender.pubkey()) {
            while hist.get(&time).is_some() {
                time = time + 1;
            }
        }
        let pos_at = self.get_position_at(time);
        if (time < Timestamp::now() || target.is_none()) && pos_at.1 == target {
            warn!(
                "Already {} from {}",
                target.map_or("stopped time-tracking".to_string(), |id| format!(
                    "tracking \"{}\"",
                    self.get_task_title(&id)
                )),
                format_timestamp_relative(&pos_at.0),
            );
            return None;
        }
        info!("{}", match target {
            None => format!(
                "Stopping time-tracking of \"{}\" at {}",
                pos_at.1.map_or("???".to_string(), |id| self.get_task_title(&id)),
                format_timestamp_relative(&time)
            ),
            Some(new_id) => format!(
                "Tracking \"{}\" from {}{}",
                self.get_task_title(&new_id),
                format_timestamp_relative(&time),
                pos_at.1.filter(|id| id != &new_id).map(|id|
                    format!(" replacing \"{}\"", self.get_task_title(&id)))
                    .unwrap_or_default()
            )
        });
        self.submit(
            build_tracking(target)
                .custom_created_at(time)
        ).into()
    }

    /// Sign and queue the event to the relay, returning its id
    fn submit(&mut self, mut builder: EventBuilder) -> EventId {
        if let Some(stamp) = self.custom_time {
            builder = builder.custom_created_at(stamp);
        }
        let event = self.sender.submit(builder).unwrap();
        let id = event.id;
        self.add(event);
        id
    }

    pub(crate) fn add(&mut self, event: Event) {
        let author = event.pubkey;
        self.users.create(author);
        match event.kind {
            Kind::GitIssue => self.add_task(event),
            Kind::Metadata => match Metadata::from_json(event.content.as_str()) {
                Ok(metadata) => { self.users.insert(event.pubkey, metadata, event.created_at); }
                Err(e) => warn!("Cannot parse metadata: {} from {:?}", e, event),
            },
            Kind::Bookmarks => {
                if event.pubkey == self.sender.pubkey() {
                    self.bookmarks = referenced_events(&event).collect_vec()
                }
            }
            _ => {
                if event.kind == TRACKING_KIND {
                    match self.history.get_mut(&event.pubkey) {
                        Some(c) => {
                            c.insert(event.created_at, event);
                        }
                        None => {
                            self.history
                                .insert(event.pubkey, BTreeMap::from([(event.created_at, event)]));
                        }
                    }
                } else {
                    if let Some(event) = self.add_prop(event) {
                        debug!("Requeueing unknown Event {:?}", event);
                        self.overflow.push_back(event);
                    }
                }
            }
        }
    }

    pub(crate) fn add_task(&mut self, event: Event) {
        if self.tasks.contains_key(&event.id) {
            warn!("Did not insert duplicate event {}", event.id);
        } else {
            let id = event.id;
            let task = Task::new(event);
            self.tasks.insert(id, task);
        }
    }

    /// Add event as prop, returning it if not processable
    fn add_prop(&mut self, event: Event) -> Option<Event> {
        let found = self.referenced_tasks(&event, |t| {
            t.props.insert(event.clone());
        });
        if !found {
            if event.kind == Kind::TextNote {
                self.add_task(event);
            } else {
                return Some(event);
            }
        }
        None
    }

    fn get_own_history(&self) -> Option<&BTreeMap<Timestamp, Event>> {
        self.history.get(&self.sender.pubkey())
    }

    fn get_own_events_history(&self) -> impl DoubleEndedIterator<Item=&Event> + '_ {
        self.get_own_history().into_iter().flat_map(|t| t.values())
    }

    pub(super) fn history_before_now(&self) -> impl Iterator<Item=&Event> {
        self.get_own_history().into_iter().flat_map(|hist| {
            let now = now();
            hist.values().rev()
                .skip_while(move |e| e.created_at > now)
                .dedup_by(|e1, e2| e1.id == e2.id)
        })
    }

    pub(crate) fn move_back_to(&mut self, str: &str) -> bool {
        let lower = str.to_ascii_lowercase();
        let found =
            self.history_before_now()
                .find(|e| {
                    referenced_event(e)
                        .and_then(|id| self.get_by_id(&id))
                        .is_some_and(|t| t.get_title().to_ascii_lowercase().contains(&lower))
                });
        if let Some(event) = found {
            self.move_to(referenced_event(event));
            return true;
        }
        false
    }

    pub(crate) fn move_back_by(&mut self, steps: usize) {
        let id = self
            .history_before_now()
            .nth(steps)
            .and_then(|e| referenced_event(e));
        self.move_to(id)
    }

    pub(crate) fn undo(&mut self) {
        let mut count = 0;
        self.sender.clear().into_iter().rev().for_each(|event| {
            count += 1;
            self.remove(&event)
        });
        info!("Reverted last {count} actions!")
    }

    fn remove(&mut self, event: &Event) {
        self.tasks.remove(&event.id);
        self.history.get_mut(&self.sender.pubkey())
            .map(|t| {
                t.retain(|_, e| e != event && !referenced_event(e).is_some_and(|id| id == event.id))
            });
        self.referenced_tasks(event, |t| {
            t.props.remove(event);
        });
    }

    pub(crate) fn set_state_for_with(&mut self, id: EventId, comment: &str) {
        self.set_state_for(id, comment, comment.try_into().unwrap_or(State::Open));
    }

    pub(crate) fn set_state_for(&mut self, id: EventId, comment: &str, state: State) -> EventId {
        let ids =
            if state == State::Closed {
                // Close whole subtree
                ChildrenTraversal::from(self, id).get_all()
            } else {
                vec![id]
            };
        let (desc, tags) = extract_tags(comment, &self.users);
        let prop =
            EventBuilder::new(state.into(), desc)
                .tags(ids.into_iter()
                    .map(|e| self.make_event_tag_from_id(e, MARKER_PROPERTY)))
                .tags(tags);
        info!(
            "Task status {} set for \"{}\"{}{}",
            StateChange::get_label_for(&state, comment),
            self.get_task_title(&id),
            self.custom_time
                .map(|ts| format!(" at {}", format_timestamp_relative(&ts)))
                .unwrap_or_default(),
            self.get_by_id(&id)
                .and_then(|task| task.state_at(self.custom_time.unwrap_or_default()))
                .map(|ts| format!(" from {}", ts))
                .unwrap_or_default()
        );
        self.submit(prop)
    }

    pub(crate) fn update_state(&mut self, comment: &str, state: State) -> Option<EventId> {
        let id = self.get_position()?;
        Some(self.set_state_for(id, comment, state))
    }

    /// Creates a note or activity, depending on whether the parent is a task.
    /// Sanitizes Input.
    pub(crate) fn make_note(&mut self, note: &str) -> EventId {
        let (name, tags) = extract_tags(note.trim(), &self.users);
        let format = format!("\"{name}\" with tags [{}]", join_tags(&tags));
        let mut prop =
            EventBuilder::new(Kind::TextNote, name)
                .tags(tags);
        let marker =
            if self.get_current_task().is_some_and(|t| t.is_task()) {
                MARKER_PROPERTY
            } else {
                // Activity if parent is not a task
                prop = prop.tags(self.context_hashtags());
                MARKER_PARENT
            };
        info!("Created {} {format}", if marker == MARKER_PROPERTY { "note" } else { "activity" } );
        self.submit(
            prop.tags(
                self.get_position()
                    .map(|pos| self.make_event_tag_from_id(pos, marker))))
    }

    // Properties

    pub(crate) fn set_view_depth(&mut self, depth: usize) {
        info!("Showing {depth} subtask levels");
        self.view_depth = depth;
    }

    pub(crate) fn set_search_depth(&mut self, depth: usize) {
        if !self.view.is_empty() {
            self.view.clear();
            info!("Cleared search and changed search depth to {depth}");
        } else {
            info!("Changed search depth to {depth}");
        }
        self.search_depth = depth;
    }

    pub(crate) fn get_columns(&mut self) -> &mut Vec<String> {
        &mut self.properties
    }

    pub(crate) fn set_sorting(&mut self, vec: VecDeque<String>) {
        self.sorting = vec;
        info!("Now sorting by {:?}", self.sorting);
    }

    pub(crate) fn add_sorting_property(&mut self, property: String) {
        // TODO reverse order if already present
        self.sorting.push_front(property);
        self.sorting.truncate(4);
        info!("Now sorting by {:?}", self.sorting);
    }
}

impl Display for TasksRelay {
    fn fmt(&self, lock: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(t) = self.get_current_task() {
            let state = t.state_or_default();
            let now = &now();
            let mut tracking_stamp: Option<Timestamp> = None;
            for elem in timestamps(self.get_own_events_history(), &[t.get_id()]).map(|(e, _)| e) {
                if tracking_stamp.is_some() && elem > now {
                    break;
                }
                tracking_stamp = Some(*elem)
            }
            writeln!(
                lock,
                "Active from {} (total tracked time {}m) - {} since {}",
                tracking_stamp.map_or("?".to_string(), |t| format_timestamp_relative(&t)),
                self.time_tracked(t.get_id()) / 60,
                state,
                format_timestamp_relative(&state.get_timestamp())
            )?;
            for d in t.descriptions().rev() { writeln!(lock, "{}", d)?; }
            writeln!(lock)?;
        }

        let visible = self.viewed_tasks();

        if visible.is_empty() {
            if self.tasks.children_for(self.get_position()).next().is_some() {
                writeln!(lock, "No tasks here matching{}", self.get_prompt_suffix())?;
            }
            writeln!(lock, "{}", self.times_tracked(6))?;
            return Ok(());
        }

        if self.view.is_empty() {
            let mut bookmarks = self.bookmarked_tasks_deduped(&visible).peekable();
            if bookmarks.peek().is_some() {
                writeln!(lock, "{}", Colorize::bold("Quick Access"))?;
                for task in bookmarks {
                    writeln!(
                        lock,
                        "{}",
                        self.properties.iter()
                            .map(|p| self.get_property(task, p.as_str()))
                            .join(" \t")
                    )?;
                }
            }
        }

        // TODO proper column alignment
        // TODO hide empty columns
        writeln!(lock, "{}", self.properties.join(" \t").bold())?;

        let count = visible.len();
        let mut total_time = 0;
        for task in visible {
            writeln!(lock, "{}",
                     self.properties.iter()
                         .map(|p| self.get_property(task, p.as_str()))
                         .join(" \t")
            )?;
            total_time += self.total_time_tracked(task.get_id()) // TODO include parent if it matches
        }

        writeln!(lock,
                 "{count} visible tasks{}",
                 display_time(" tracked a total of HHhMMm", total_time)
        )?;
        Ok(())
    }
}

pub(super) trait PropertyCollection<T> {
    fn remove_at(&mut self, index: usize);
    fn add_or_remove(&mut self, value: T);
    fn add_or_remove_at(&mut self, value: T, index: usize);
}
impl<T> PropertyCollection<T> for Vec<T>
where
    T: Display + Eq + Clone,
{
    fn remove_at(&mut self, index: usize) {
        let col = self.remove(index);
        info!("Removed property column \"{col}\"");
    }

    fn add_or_remove(&mut self, property: T) {
        match self.iter().position(|s| s == &property) {
            None => {
                info!("Added property column \"{property}\"");
                self.push(property);
            }
            Some(index) => {
                self.remove_at(index);
            }
        }
    }

    fn add_or_remove_at(&mut self, property: T, index: usize) {
        if self.get(index) == Some(&property) {
            self.remove_at(index);
        } else {
            info!("Added property column \"{property}\" at position {}",index + 1);
            self.insert(index, property);
        }
    }
}

/// Formats the given seconds according to the given format.
/// - MMM - minutes
/// - MM - minutes of the hour
/// - HH - hours
///
/// Returns an empty string if under one minute.
fn display_time(format: &str, secs: u64) -> String {
    Some(secs / 60)
        .filter(|t| t > &0)
        .map_or(String::new(), |mins| {
            format
                .replace("MMM", &format!("{:3}", mins))
                .replace("HH", &format!("{:02}", mins.div(60)))
                .replace("MM", &format!("{:02}", mins.rem(60)))
        })
}

/// Joins the tasks of this upwards iterator.
/// * `include_last_id` whether to add the id of an unknown parent at the top
pub(crate) fn join_tasks<'a>(
    iter: impl Iterator<Item=&'a Task>,
    include_last_id: bool,
) -> Option<String> {
    let tasks: Vec<&Task> = iter.collect();
    tasks
        .iter()
        .map(|t| t.get_title())
        .chain(
            tasks.last()
                .take_if(|_| include_last_id)
                .and_then(|t| t.parent_id())
                .map(|id| id.to_string())
                .into_iter(),
        )
        .fold(None, |acc, val| {
            Some(acc.map_or_else(
                || val.clone(),
                |cur| format!("{}{}{}", val, ">".dimmed(), cur),
            ))
        })
}

pub fn referenced_event(event: &Event) -> Option<EventId> {
    referenced_events(event).next()
}

struct ParentIterator<'a> {
    tasks: &'a TaskMap,
    current: Option<EventId>,
}
impl<'a> Iterator for ParentIterator<'a> {
    type Item = &'a Task;

    fn next(&mut self) -> Option<Self::Item> {
        self.current.and_then(|id| self.tasks.get(&id)).map(|t| {
            self.current = t.parent_id().cloned();
            t
        })
    }
}
impl FusedIterator for ParentIterator<'_> {}
