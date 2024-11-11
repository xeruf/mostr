use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::{Display, Formatter};
use std::io::Write;
use std::iter::{empty, once, FusedIterator};
use std::ops::{Div, Rem};
use std::str::FromStr;
use std::time::Duration;

use crate::event_sender::{EventSender, MostrMessage};
use crate::helpers::{format_timestamp_local, format_timestamp_relative, format_timestamp_relative_to, parse_tracking_stamp, some_non_empty, CHARACTER_THRESHOLD};
use crate::kinds::*;
use crate::task::{State, Task, TaskState, MARKER_DEPENDS, MARKER_PARENT, MARKER_PROPERTY};
use colored::Colorize;
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use nostr_sdk::prelude::Marker;
use nostr_sdk::{Event, EventBuilder, EventId, JsonUtil, Keys, Kind, Metadata, PublicKey, Tag, TagStandard, Timestamp, UncheckedUrl, Url};
use regex::bytes::Regex;
use tokio::sync::mpsc::Sender;
use TagStandard::Hashtag;

const DEFAULT_PRIO: Prio = 25;
pub const HIGH_PRIO: Prio = 85;

/// Amount of seconds to treat as "now"
const MAX_OFFSET: u64 = 9;
pub(crate) fn now() -> Timestamp {
    Timestamp::now() + MAX_OFFSET
}

type TaskMap = HashMap<EventId, Task>;
trait TaskMapMethods {
    fn children_of<'a>(&'a self, task: &'a Task) -> impl Iterator<Item=&Task> + 'a;
    fn children_for<'a>(&'a self, id: Option<&'a EventId>) -> impl Iterator<Item=&Task> + 'a;
    fn children_ids_for<'a>(&'a self, id: &'a EventId) -> impl Iterator<Item=&EventId> + 'a;
}
impl TaskMapMethods for TaskMap {
    fn children_of<'a>(&'a self, task: &'a Task) -> impl Iterator<Item=&Task> + 'a {
        self.children_for(Some(task.get_id()))
    }

    fn children_for<'a>(&'a self, id: Option<&'a EventId>) -> impl Iterator<Item=&Task> + 'a {
        self.values()
            .filter(move |t| t.parent_id() == id)
    }

    fn children_ids_for<'a>(&'a self, id: &'a EventId) -> impl Iterator<Item=&EventId> + 'a {
        self.children_for(Some(id))
            .map(|t| t.get_id())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TasksRelay {
    /// The Tasks
    tasks: TaskMap,
    /// History of active tasks by PubKey
    history: HashMap<PublicKey, BTreeMap<Timestamp, Event>>,
    /// Index of known users with metadata
    users: HashMap<PublicKey, Metadata>,
    /// Own pinned tasks
    bookmarks: Vec<EventId>,

    /// The task properties currently visible
    properties: Vec<String>,
    /// The task properties sorted by
    sorting: VecDeque<String>, // TODO track boolean for reversal?

    /// A filtered view of the current tasks.
    /// Would like this to be Task references
    /// but that doesn't work unless I start meddling with Rc everywhere.
    view: Vec<EventId>,
    search_depth: usize,
    view_depth: usize,
    pub(crate) recurse_activities: bool,

    /// Currently active tags
    tags: BTreeSet<Tag>,
    /// Tags filtered out from view
    tags_excluded: BTreeSet<Tag>,
    /// Current active state
    state: StateFilter,
    /// Current priority for filtering and new tasks
    priority: Option<Prio>,

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
        url: Option<Url>,
        tx: &Sender<MostrMessage>,
        keys: &Keys,
        metadata: Option<Metadata>,
    ) -> Self {
        let mut new = Self::with_sender(EventSender::from(url, tx, keys));
        metadata.map(|m| new.users.insert(keys.public_key(), m));
        new
    }

    pub(crate) fn with_sender(sender: EventSender) -> Self {
        TasksRelay {
            tasks: Default::default(),
            history: Default::default(),
            users: Default::default(),
            bookmarks: Default::default(),

            properties: [
                "author",
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
                "author",
                "hashtags",
                "rtime",
                "name",
            ].into_iter().map(|s| s.to_string()).collect(),

            view: Default::default(),
            tags: Default::default(),
            tags_excluded: Default::default(),
            state: Default::default(),
            priority: None,

            search_depth: 4,
            view_depth: 0,
            recurse_activities: true,

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
            info!("Reprocessed {elements} updates with {issues} issues{}", self.sender.url.clone().map(|url| format!(" from {url}")).unwrap_or_default());
        }
    }

    // Accessors

    #[inline]
    pub(crate) fn get_by_id(&self, id: &EventId) -> Option<&Task> { self.tasks.get(id) }

    #[inline]
    pub(crate) fn len(&self) -> usize { self.tasks.len() }

    pub(crate) fn get_position(&self) -> Option<EventId> {
        self.get_position_ref().cloned()
    }

    pub(crate) fn get_position_ref(&self) -> Option<&EventId> {
        self.get_position_at(now()).1
    }

    fn sorting_key(&self, task: &Task) -> impl Ord {
        self.sorting
            .iter()
            .map(|p| self.get_property(task, p.as_str()))
            .collect_vec()
    }

    // TODO binary search
    /// Gets last position change before the given timestamp
    fn get_position_at(&self, timestamp: Timestamp) -> (Timestamp, Option<&EventId>) {
        self.history_from(timestamp)
            .last()
            .filter(|e| e.created_at <= timestamp)
            .map_or_else(
                || (Timestamp::now(), None),
                |e| (e.created_at, referenced_event(e)))
    }

    pub(crate) fn all_hashtags(&self) -> impl Iterator<Item=&str> {
        self.tasks.values()
            .filter(|t| t.pure_state() != State::Closed)
            .filter_map(|t| t.tags.as_ref()).flatten()
            .filter(|tag| is_hashtag(tag))
            .filter_map(|tag| tag.content().map(|s| s.trim()))
            .sorted_unstable()
            .dedup()
    }

    /// Dynamic time tracking overview for current task or current user.
    pub(crate) fn times_tracked(&self) -> (String, Box<dyn DoubleEndedIterator<Item=String>>) {
        self.times_tracked_for(&self.sender.pubkey())
    }

    pub(crate) fn times_tracked_for(&self, key: &PublicKey) -> (String, Box<dyn DoubleEndedIterator<Item=String>>) {
        match self.get_position_ref() {
            None => {
                if let Some(hist) = self.history.get(key) {
                    let mut last = None;
                    let mut full = Vec::with_capacity(hist.len());
                    for event in hist.values() {
                        let new = some_non_empty(&event.tags.iter()
                            .filter_map(|t| t.content())
                            .map(|str| EventId::from_str(str).ok().map_or(str.to_string(), |id| self.get_task_path(Some(id))))
                            .join(" "));
                        if new != last {
                            // TODO omit intervals <2min - but I think I need threeway for that
                            // TODO alternate color with grey between days
                            full.push(format!("{} {}", format_timestamp_local(&event.created_at), new.as_ref().unwrap_or(&"---".to_string())));
                            last = new;
                        }
                    }
                    // TODO show history for active tags
                    ("Your Time-Tracking History:".to_string(), Box::from(full.into_iter()))
                } else {
                    ("You have nothing time-tracked yet".to_string(), Box::from(empty()))
                }
            }
            Some(id) => {
                // TODO show current recursive with pubkey
                let ids = vec![id];
                let history =
                    self.history.iter().flat_map(|(key, set)| {
                        let mut vec = Vec::with_capacity(set.len() / 2);
                        let mut iter = timestamps(set.values(), &ids).tuples();
                        while let Some(((start, _), (end, _))) = iter.next() {
                            // Filter out intervals <2 mins
                            if start.as_u64() + 120 < end.as_u64() {
                                vec.push(format!("{} - {} by {}",
                                                 format_timestamp_local(start),
                                                 format_timestamp_relative_to(end, start),
                                                 self.get_username(key)))
                            }
                        }
                        iter.into_buffer()
                            .for_each(|(stamp, _)|
                                vec.push(format!("{} started by {}", format_timestamp_local(stamp), self.get_username(key))));
                        vec
                    }).sorted_unstable(); // TODO sorting depends on timestamp format - needed to interleave different people
                (format!("Times Tracked on {:?}", self.get_task_title(id)), Box::from(history))
            }
        }
    }

    /// Total time in seconds tracked on this task by the current user.
    pub(crate) fn time_tracked(&self, id: EventId) -> u64 {
        Durations::from(self.get_own_events_history(), &vec![&id]).sum::<Duration>().as_secs()
    }

    /// Total time in seconds tracked on this task and its subtasks by all users.
    fn total_time_tracked(&self, id: EventId) -> u64 {
        let mut total = 0;

        let children = ChildIterator::from(&self, &id).get_all();
        for user in self.history.values() {
            total += Durations::from(user.values(), &children).sum::<Duration>().as_secs();
        }
        total
    }

    fn total_progress(&self, id: &EventId) -> Option<f32> {
        self.get_by_id(id).and_then(|task| match task.pure_state() {
            State::Closed => None,
            State::Done => Some(1.0),
            _ => {
                let mut sum = 0f32;
                let mut count = 0;
                for prog in self.tasks.children_ids_for(task.get_id()).filter_map(|e| self.total_progress(e)) {
                    sum += prog;
                    count += 1;
                }
                Some(
                    if count > 0 {
                        sum / (count as f32)
                    } else {
                        0.0
                    }
                )
            }
        })
    }

    // Parents

    pub(crate) fn up_by(&self, count: usize) -> Option<&EventId> {
        let mut pos = self.get_position_ref();
        for _ in 0..count {
            pos = self.get_parent(pos);
        }
        pos
    }

    pub(crate) fn get_parent(&self, id: Option<&EventId>) -> Option<&EventId> {
        id.and_then(|id| self.get_by_id(id))
            .and_then(|t| t.parent_id())
    }

    pub(crate) fn get_prompt_suffix(&self) -> String {
        self.tags.iter()
            .map(|t| format!(" #{}", t.content().unwrap()))
            .chain(self.tags_excluded.iter()
                .map(|t| format!(" -#{}", t.content().unwrap())))
            .chain(once(self.state.indicator()))
            .chain(self.priority.map(|p| format!(" *{:02}", p)))
            .join("")
    }

    pub(crate) fn get_task_path(&self, id: Option<EventId>) -> String {
        join_tasks(self.traverse_up_from(id), true)
            .filter(|s| !s.is_empty())
            .or_else(|| id.map(|id| id.to_string()))
            .unwrap_or_default()
    }

    /// Iterate over the task referenced by the given id and all its available parents.
    fn traverse_up_from(&self, id: Option<EventId>) -> ParentIterator {
        ParentIterator {
            tasks: &self.tasks,
            current: id,
        }
    }

    fn relative_path(&self, id: EventId) -> String {
        join_tasks(
            self.traverse_up_from(Some(id))
                .take_while(|t| Some(&t.event.id) != self.get_position_ref()),
            false,
        ).unwrap_or(id.to_string())
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
                    let mut children = self.resolve_tasks_rec(self.tasks.children_of(&task), sparse, new_depth);
                    if !children.is_empty() {
                        if !sparse {
                            children.push(task);
                        }
                        return children;
                    }
                }
                return if self.filter(task) { vec![task] } else { vec![] };
            }).collect_vec()
    }

    /// Executes the given function with each task referenced by this event without marker.
    /// Returns true if any task was found.
    pub(crate) fn referenced_tasks<F: Fn(&mut Task)>(&mut self, event: &Event, f: F) -> bool {
        let mut found = false;
        for tag in event.tags.iter() {
            if let Some(TagStandard::Event { event_id, marker, .. }) = tag.as_standardized() {
                if marker.as_ref().is_none_or(|m| m.to_string() == MARKER_PROPERTY) {
                    self.tasks.get_mut(event_id).map(|t| {
                        found = true;
                        f(t)
                    });
                }
            }
        }
        found
    }

    #[inline]
    pub(crate) fn get_current_task(&self) -> Option<&Task> {
        self.get_position_ref().and_then(|id| self.get_by_id(id))
    }

    fn filter(&self, task: &Task) -> bool {
        self.state.matches(task) &&
            self.priority.is_none_or(|prio| {
                task.priority().unwrap_or(DEFAULT_PRIO) >= prio
            }) &&
            task.tags.as_ref().map_or(true, |tags| {
                !tags.iter().any(|tag| self.tags_excluded.contains(tag))
            }) &&
            (self.tags.is_empty() ||
                task.tags.as_ref().map_or(false, |tags| {
                    let mut iter = tags.iter();
                    self.tags.iter().all(|tag| iter.any(|t| t == tag))
                }))
    }

    pub(crate) fn filtered_tasks<'a>(&'a self, position: Option<&'a EventId>, sparse: bool) -> Vec<&'a Task> {
        let roots = self.tasks.children_for(position);
        let mut current = self.resolve_tasks_rec(roots, sparse, self.search_depth + self.view_depth);
        if current.is_empty() {
            if !self.tags.is_empty() {
                let mut children = self.tasks.children_for(self.get_position_ref()).peekable();
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
                    .filter(|id| !position.is_some_and(|p| &p == id) && !ids.contains(id))
                    .filter_map(|id| self.get_by_id(id))
                    .filter(|t| self.filter(t))
                    .collect_vec()
            };
        current.append(&mut bookmarks);

        current
    }

    // TODO this is a relict for tests
    fn visible_tasks(&self) -> Vec<&Task> {
        if self.search_depth == 0 {
            return vec![];
        }
        if !self.view.is_empty() {
            return self.view.iter().flat_map(|id| self.get_by_id(id)).collect();
        }
        self.filtered_tasks(self.get_position_ref(), true)
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
                if let Some(task) = task.get_dependendees().iter().filter_map(|id| self.get_by_id(id)).find(|t| t.pure_state().is_open()) {
                    return format!("Blocked by \"{}\"", task.get_title()).bright_red().to_string();
                }
                let state = task.pure_state();
                if state.is_open() && progress.is_some_and(|p| p > 0.1) {
                    state.colorize(&prog_string)
                } else {
                    task.state_label().unwrap_or_default()
                }.to_string()
            }
            "progress" => prog_string.clone(),

            "author" | "creator" => format!("{:.6}", self.get_username(&task.event.pubkey)), // FIXME temporary until proper column alignment
            "prio" => self.traverse_up_from(Some(*task.get_id())).find_map(Task::priority_raw).map(|p| p.to_string()).unwrap_or_else(||
                if self.priority.is_some() {
                    DEFAULT_PRIO.to_string().dimmed().to_string()
                } else {
                    "".to_string()
                }),
            "path" => self.get_task_path(Some(task.event.id)),
            "rpath" => self.relative_path(task.event.id),
            // TODO format strings configurable
            "time" => display_time("MMMm", self.time_tracked(*task.get_id())),
            "rtime" => display_time("HH:MM", self.total_time_tracked(*task.get_id())),
            prop => task.get(prop).unwrap_or_default(),
        }
    }

    pub(crate) fn find_user(&self, term: &str) -> Option<(&PublicKey, &Metadata)> {
        self.users.iter().find(|(_, v)|
            // TODO regex word boundary
            v.name.as_ref().is_some_and(|n| n.starts_with(term)) ||
                v.display_name.as_ref().is_some_and(|n| n.starts_with(term)))
    }

    pub(crate) fn get_username(&self, pubkey: &PublicKey) -> String {
        self.users.get(pubkey)
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| format!("{:.6}", pubkey.to_string()))
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
            EventBuilder::new(Kind::Bookmarks, "mostr pins",
                              self.bookmarks.iter().map(|id| Tag::event(*id))))?;
        Ok(added)
    }

    pub(crate) fn set_filter_author(&mut self, key: PublicKey) -> bool {
        self.set_filter(|t| t.event.pubkey == key)
    }

    pub(crate) fn set_filter_from(&mut self, time: Timestamp) -> bool {
        // TODO filter at both ends
        self.set_filter(|t| t.last_state_update() > time)
    }

    pub(crate) fn get_filtered<P>(&self, position: Option<&EventId>, predicate: P) -> Vec<EventId>
    where
        P: Fn(&&Task) -> bool,
    {
        self.filtered_tasks(position, false)
            .into_iter()
            .filter(predicate)
            .map(|t| t.event.id)
            .collect()
    }

    pub(crate) fn set_filter<P>(&mut self, predicate: P) -> bool
    where
        P: Fn(&&Task) -> bool,
    {
        self.set_view(self.get_filtered(self.get_position_ref(), predicate))
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
        self.view.clear();
        self.tags.clear();
        self.tags_excluded.clear();
        info!("Removed all filters");
    }

    pub(crate) fn has_tag_filter(&self) -> bool {
        !self.tags.is_empty() || !self.tags_excluded.is_empty()
    }

    pub(crate) fn print_hashtags(&self) {
        println!("Hashtags of all known tasks:\n{}", self.all_hashtags().join(" ").italic());
    }

    /// Returns true if tags have been updated, false if it printed something
    pub(crate) fn update_tags(&mut self, tags: impl IntoIterator<Item=Tag>) -> bool {
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

    fn set_tags(&mut self, tags: impl IntoIterator<Item=Tag>) {
        self.tags.clear();
        self.tags.extend(tags);
    }

    pub(crate) fn add_tag(&mut self, tag: String) {
        self.view.clear();
        info!("Added tag filter for #{tag}");
        let tag: Tag = Hashtag(tag).into();
        self.tags_excluded.remove(&tag);
        self.tags.insert(tag);
    }

    pub(crate) fn remove_tag(&mut self, tag: &str) {
        self.view.clear();
        let len = self.tags.len();
        self.tags.retain(|t| !t.content().is_some_and(|value| value.to_string().starts_with(tag)));
        if self.tags.len() < len {
            info!("Removed tag filters starting with {tag}");
        } else {
            self.tags_excluded.insert(Hashtag(tag.to_string()).into());
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
        self.sender.flush();
    }

    /// Returns ids of tasks matching the given string.
    ///
    /// Tries, in order:
    /// - single case-insensitive exact name match in visible tasks
    /// - single case-insensitive exact name match in all tasks
    /// - visible tasks starting with given arg case-sensitive
    /// - visible tasks where any word starts with given arg case-insensitive
    pub(crate) fn get_matching(&self, position: Option<&EventId>, arg: &str) -> Vec<EventId> {
        if let Ok(id) = EventId::parse(arg) {
            return vec![id];
        }
        let lowercase_arg = arg.to_ascii_lowercase();
        // TODO apply regex to all matching
        let regex = Regex::new(&format!(r"\b{}", lowercase_arg)).unwrap();

        let mut filtered: Vec<EventId> = Vec::with_capacity(32);
        let mut filtered_fuzzy: Vec<EventId> = Vec::with_capacity(32);
        for task in self.filtered_tasks(position, false) {
            let content = task.get_filter_title();
            let lowercase = content.to_ascii_lowercase();
            if lowercase == lowercase_arg {
                return vec![task.event.id];
            } else if content.starts_with(arg) {
                filtered.push(task.event.id)
            } else if regex.is_match(lowercase.as_bytes()) {
                filtered_fuzzy.push(task.event.id)
            }
        }
        // Find global exact match
        for task in self.tasks.values() {
            if task.get_filter_title().to_ascii_lowercase() == lowercase_arg &&
                // exclude closed tasks and their subtasks
                !self.traverse_up_from(Some(*task.get_id())).any(|t| !t.pure_state().is_open()) {
                return vec![task.event.id];
            }
        }

        if filtered.is_empty() {
            filtered = filtered_fuzzy;
        }
        let immediate = filtered.iter().filter(
            |t| self.get_by_id(t).is_some_and(|t| t.parent_id() == position)).collect_vec();
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
    pub(crate) fn filter_or_create(&mut self, position: Option<&EventId>, arg: &str) -> Option<EventId> {
        let filtered = self.get_matching(position, arg);
        match filtered.len() {
            0 => {
                // No match, new task
                self.view.clear();
                if arg.len() < CHARACTER_THRESHOLD {
                    warn!("New task name needs at least {CHARACTER_THRESHOLD} characters");
                    return None;
                }
                Some(self.make_task_with(arg, self.position_tags_for(position), true))
            }
            1 => {
                // One match, activate
                self.move_to(filtered.into_iter().nth(0));
                None
            }
            _ => {
                // Multiple match, filter
                self.move_to(position.cloned());
                self.set_view(filtered);
                None
            }
        }
    }

    /// Returns all recent events from history until the first event at or before the given timestamp.
    fn history_from(&self, stamp: Timestamp) -> impl Iterator<Item=&Event> {
        self.history.get(&self.sender.pubkey()).map(|hist| {
            hist.values().rev().take_while_inclusive(move |e| e.created_at > stamp)
        }).into_iter().flatten()
    }

    pub(crate) fn move_to(&mut self, target: Option<EventId>) {
        self.view.clear();
        let pos = self.get_position_ref();
        if target.as_ref() == pos {
            debug!("Flushing Tasks because of move in place");
            self.flush();
            return;
        }

        if !target.and_then(|id| self.tasks.get(&id)).is_some_and(|t| t.parent_id() == pos) {
            debug!("Flushing Tasks because of move beyond child");
            self.flush();
        }

        let now = Timestamp::now();
        let offset: u64 = self.history_from(now).skip_while(|e| e.created_at.as_u64() > now.as_u64() + MAX_OFFSET).count() as u64;
        if offset >= MAX_OFFSET {
            warn!("Whoa you are moving around quickly! Give me a few seconds to process.")
        }
        self.submit(
            build_tracking(target)
                .custom_created_at(Timestamp::from(now.as_u64() + offset))
        );
    }

    // Updates

    pub(crate) fn make_event_tag_from_id(&self, id: EventId, marker: &str) -> Tag {
        Tag::from(TagStandard::Event {
            event_id: id,
            relay_url: self.sender.url.as_ref().map(|url| UncheckedUrl::new(url.as_str())),
            marker: Some(Marker::Custom(marker.to_string())),
            public_key: self.get_by_id(&id).map(|e| e.event.pubkey),
        })
    }

    pub(crate) fn make_event_tag(&self, event: &Event, marker: &str) -> Tag {
        Tag::from(TagStandard::Event {
            event_id: event.id,
            relay_url: self.sender.url.as_ref().map(|url| UncheckedUrl::new(url.as_str())),
            marker: Some(Marker::Custom(marker.to_string())),
            public_key: Some(event.pubkey),
        })
    }

    pub(crate) fn parent_tag(&self) -> Option<Tag> {
        self.get_position_ref().map(|p| self.make_event_tag_from_id(*p, MARKER_PARENT))
    }

    pub(crate) fn position_tags(&self) -> Vec<Tag> {
        self.position_tags_for(self.get_position_ref())
    }

    pub(crate) fn position_tags_for(&self, position: Option<&EventId>) -> Vec<Tag> {
        position.map_or(vec![], |pos| {
            let mut tags = Vec::with_capacity(2);
            tags.push(self.make_event_tag_from_id(*pos, MARKER_PARENT));
            self.get_by_id(pos)
                .map(|task| {
                    if task.pure_state() == State::Procedure {
                        self.tasks.children_of(task)
                            .max()
                            .map(|t| tags.push(self.make_event_tag(&t.event, MARKER_DEPENDS)));
                    }
                });
            tags
        })
    }

    /// Creates a task following the current state
    ///
    /// Sanitizes input
    pub(crate) fn make_task(&mut self, input: &str) -> EventId {
        self.make_task_with(input, self.position_tags(), true)
    }

    pub(crate) fn make_task_and_enter(&mut self, input: &str, state: State) {
        let id = self.make_task_with(input, self.position_tags(), false);
        self.set_state_for(id, "", state);
        self.move_to(Some(id));
    }

    /// Moves up and creates a sibling task dependent on the current one
    ///
    /// Returns true if successful, false if there is no current task
    pub(crate) fn make_dependent_sibling(&mut self, input: &str) -> bool {
        if let Some(pos) = self.get_position() {
            self.move_up();
            self.make_task_with(
                input,
                self.get_position().map(|par| self.make_event_tag_from_id(par, MARKER_PARENT))
                    .into_iter().chain(once(self.make_event_tag_from_id(pos, MARKER_DEPENDS))),
                true);
            return true;
        }
        false
    }

    /// Creates a task including current tag filters
    ///
    /// Sanitizes input
    pub(crate) fn make_task_with(&mut self, input: &str, tags: impl IntoIterator<Item=Tag>, set_state: bool) -> EventId {
        let (input, input_tags) = extract_tags(input.trim());
        let prio =
            if input_tags.iter().any(|t| t.kind().to_string() == PRIO) { None } else { self.priority.map(|p| to_prio_tag(p)) };
        info!("Created task \"{input}\" with tags [{}]", join(&input_tags));
        let id = self.submit(
            EventBuilder::new(TASK_KIND, &input, input_tags)
                .add_tags(self.tags.iter().cloned())
                .add_tags(tags)
                .add_tags(prio)
        );
        if set_state {
            self.state
                .as_option()
                .inspect(|s| self.set_state_for_with(id, s));
        }
        id
    }

    pub(crate) fn get_task_title(&self, id: &EventId) -> String {
        self.tasks.get(id).map_or(id.to_string(), |t| t.get_title())
    }

    /// Parse relative time string and track for current position
    ///
    /// Returns false and prints a message if parsing failed
    pub(crate) fn track_from(&mut self, str: &str) -> bool {
        parse_tracking_stamp(str)
            .and_then(|stamp| self.track_at(stamp, self.get_position()))
            .is_some()
    }

    pub(crate) fn track_at(&mut self, mut time: Timestamp, target: Option<EventId>) -> Option<EventId> {
        if target.is_none() {
            // Prevent random overlap with tracking started in the same second
            time = time - 1;
        } else if let Some(hist) = self.history.get(&self.sender.pubkey()) {
            while hist.get(&time).is_some() {
                time = time + 1;
            }
        }
        let current_pos = self.get_position_at(time);
        if (time < Timestamp::now() || target.is_none()) && current_pos.1 == target.as_ref() {
            warn!("Already {} from {}",
                target.map_or("stopped time-tracking".to_string(), 
                    |id| format!("tracking \"{}\"", self.get_task_title(&id))),
                format_timestamp_relative(&current_pos.0),
            );
            return None;
        }
        info!("{}", match target {
            None => format!("Stopping time-tracking of \"{}\" at {}", 
                            current_pos.1.map_or("???".to_string(), |id| self.get_task_title(id)), 
                            format_timestamp_relative(&time)),
            Some(new_id) => format!("Tracking \"{}\" from {}{}", 
                                self.get_task_title(&new_id), 
                                format_timestamp_relative(&time),
                                current_pos.1.filter(|id| id != &&new_id).map(
                                     |id| format!(" replacing \"{}\"", self.get_task_title(id))).unwrap_or_default()),
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
        match event.kind {
            Kind::GitIssue => self.add_task(event),
            Kind::Metadata =>
                match Metadata::from_json(event.content()) {
                    Ok(metadata) => { self.users.insert(event.pubkey, metadata); }
                    Err(e) => warn!("Cannot parse metadata: {} from {:?}", e, event)
                }
            Kind::Bookmarks => {
                if event.pubkey == self.sender.pubkey() {
                    self.bookmarks = referenced_events(&event).cloned().collect_vec()
                }
            }
            _ => {
                if event.kind == TRACKING_KIND {
                    match self.history.get_mut(&event.pubkey) {
                        Some(c) => { c.insert(event.created_at, event); }
                        None => { self.history.insert(event.pubkey, BTreeMap::from([(event.created_at, event)])); }
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
        self.history.get(&self.sender.pubkey()).into_iter().flat_map(|t| t.values())
    }

    fn history_before_now(&self) -> impl Iterator<Item=&Event> {
        self.get_own_history().into_iter().flat_map(|hist| {
            let now = now();
            hist.values().rev().skip_while(move |e| e.created_at > now)
        })
    }

    pub(crate) fn move_back_to(&mut self, str: &str) -> bool {
        let lower = str.to_ascii_lowercase();
        let found = self.history_before_now()
            .find(|e| referenced_event(e)
                .and_then(|id| self.get_by_id(id))
                .is_some_and(|t| t.event.content.to_ascii_lowercase().contains(&lower)));
        if let Some(event) = found {
            self.move_to(referenced_event(event).cloned());
            return true;
        }
        false
    }

    pub(crate) fn move_back_by(&mut self, steps: usize) {
        let id = self.history_before_now().nth(steps)
            .and_then(|e| referenced_event(e));
        self.move_to(id.cloned())
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
            .map(|t| t.retain(|t, e| e != event &&
                !referenced_event(e).is_some_and(|id| id == &event.id)));
        self.referenced_tasks(event, |t| { t.props.remove(event); });
    }

    pub(crate) fn set_state_for_with(&mut self, id: EventId, comment: &str) {
        self.set_state_for(id, comment, comment.try_into().unwrap_or(State::Open));
    }

    pub(crate) fn set_state_for(&mut self, id: EventId, comment: &str, state: State) -> EventId {
        let ids =
            if state == State::Closed {
                // Close whole subtree
                ChildIterator::from(self, &id).get_all()
            } else {
                vec![&id]
            };
        let prop = EventBuilder::new(
            state.into(),
            comment,
            ids.into_iter().map(|e| self.make_event_tag_from_id(*e, MARKER_PROPERTY)),
        );
        // if self.custom_time.is_none() && self.get_by_id(id).map(|task| {}) {}
        info!("Task status {} set for \"{}\"{}",
            TaskState::get_label_for(&state, comment),
            self.get_task_title(&id),
            self.custom_time.map(|ts| format!(" at {}", format_timestamp_relative(&ts))).unwrap_or_default());
        self.submit(prop)
    }

    pub(crate) fn update_state(&mut self, comment: &str, state: State) -> Option<EventId> {
        let id = self.get_position_ref()?;
        Some(self.set_state_for(*id, comment, state))
    }

    /// Creates a note or activity, depending on whether the parent is a task.
    /// Sanitizes Input.
    pub(crate) fn make_note(&mut self, note: &str) -> EventId {
        let (name, tags) = extract_tags(note.trim());
        let format = format!("\"{name}\" with tags [{}]", join(&tags));
        let mut prop =
            EventBuilder::new(Kind::TextNote, name, tags);
        //.filter(|id| self.get_by_id(id).is_some_and(|t| t.is_task()))
        //.map(|id| 
        let marker =
            if self.get_current_task().is_some_and(|t| t.is_task()) {
                MARKER_PROPERTY
            } else {
                // Activity if parent is not a task
                prop = prop.add_tags(self.tags.iter().cloned());
                MARKER_PARENT
            };
        info!("Created {} {format}", if marker == MARKER_PROPERTY { "note" } else { "activity" } );
        self.submit(
            prop.add_tags(
                self.get_position().map(|pos| self.make_event_tag_from_id(pos, marker))))
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
            for elem in
                timestamps(self.get_own_events_history(), &[t.get_id()])
                    .map(|(e, _)| e) {
                if tracking_stamp.is_some() && elem > now {
                    break;
                }
                tracking_stamp = Some(*elem)
            }
            writeln!(
                lock,
                "Active from {} (total tracked time {}m) - {} since {}",
                tracking_stamp.map_or("?".to_string(), |t| format_timestamp_relative(&t)),
                self.time_tracked(*t.get_id()) / 60,
                state.get_label(),
                format_timestamp_relative(&state.time)
            )?;
            writeln!(lock, "{}", t.descriptions().join("\n"))?;
        }

        let position = self.get_position_ref();
        let mut current: Vec<&Task>;
        let roots = self.view.iter().flat_map(|id| self.get_by_id(id)).collect_vec();
        if self.search_depth > 0 && roots.is_empty() {
            current = self.resolve_tasks_rec(self.tasks.children_for(position), true, self.search_depth + self.view_depth);
            if current.is_empty() {
                if !self.tags.is_empty() {
                    let mut children = self.tasks.children_for(position).peekable();
                    if children.peek().is_some() {
                        current = self.resolve_tasks_rec(children, true, 9);
                        if current.is_empty() {
                            writeln!(lock, "No tasks here matching{}", self.get_prompt_suffix())?;
                        } else {
                            writeln!(lock, "Found matching tasks beyond specified search depth:")?;
                        }
                    }
                }
            }
        } else {
            current = self.resolve_tasks_rec(roots.iter().cloned(), true, self.view_depth);
        }

        if current.is_empty() {
            let (label, times) = self.times_tracked();
            let mut times_recent = times.rev().take(6).collect_vec();
            times_recent.reverse();
            // TODO Add recent prefix
            writeln!(lock, "{}\n{}", label.italic(), times_recent.join("\n"))?;
            return Ok(());
        }

        let tree = current.iter().flat_map(|task| self.traverse_up_from(Some(task.event.id))).unique();
        let ids: HashSet<&EventId> = tree.map(|t| t.get_id()).chain(position).collect();
        if self.view.is_empty() {
            let mut bookmarks =
                // TODO add recent tasks (most time tracked + recently created)
                self.bookmarks.iter()
                    .chain(self.tasks.values().sorted_unstable().take(3).map(|t| t.get_id()))
                    .filter(|id| !ids.contains(id))
                    .filter_map(|id| self.get_by_id(id))
                    .filter(|t| self.filter(t))
                    .sorted_by_cached_key(|t| self.sorting_key(t))
                    .dedup()
                    .peekable();
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

        let count = current.len();
        let mut total_time = 0;
        for task in current {
            writeln!(
                lock,
                "{}",
                self.properties.iter()
                    .map(|p| self.get_property(task, p.as_str()))
                    .join(" \t")
            )?;
            total_time += self.total_time_tracked(task.event.id) // TODO include parent if it matches
        }

        writeln!(lock, "{} visible tasks{}", count, display_time(" tracked a total of HHhMMm", total_time))?;
        Ok(())
    }
}

pub trait PropertyCollection<T> {
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
            info!("Added property column \"{property}\" at position {}", index + 1);
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
        .map_or(String::new(), |mins| format
            .replace("MMM", &format!("{:3}", mins))
            .replace("HH", &format!("{:02}", mins.div(60)))
            .replace("MM", &format!("{:02}", mins.rem(60))),
        )
}

pub(crate) fn join_tasks<'a>(
    iter: impl Iterator<Item=&'a Task>,
    include_last_id: bool,
) -> Option<String> {
    let tasks: Vec<&Task> = iter.collect();
    tasks
        .iter()
        .map(|t| t.get_title())
        .chain(if include_last_id {
            tasks
                .last()
                .and_then(|t| t.parent_id())
                .map(|id| id.to_string())
                .into_iter()
        } else {
            None.into_iter()
        })
        .fold(None, |acc, val| {
            Some(acc.map_or_else(|| val.clone(), |cur| format!("{}{}{}", val, ">".dimmed(), cur)))
        })
}

fn referenced_events(event: &Event) -> impl Iterator<Item=&EventId> {
    event.tags.iter().filter_map(|tag| match tag.as_standardized() {
        Some(TagStandard::Event { event_id, .. }) => Some(event_id),
        _ => None
    })
}

fn referenced_event(event: &Event) -> Option<&EventId> {
    referenced_events(event).next()
}

/// Returns the id of a referenced event if it is contained in the provided ids list.
fn matching_tag_id<'a>(event: &'a Event, ids: &'a [&'a EventId]) -> Option<&'a EventId> {
    referenced_events(event).find(|id| ids.contains(id))
}

/// Filters out event timestamps to those that start or stop one of the given events
fn timestamps<'a>(events: impl Iterator<Item=&'a Event>, ids: &'a [&'a EventId]) -> impl Iterator<Item=(&Timestamp, Option<&EventId>)> {
    events.map(|event| (&event.created_at, matching_tag_id(event, ids)))
        .dedup_by(|(_, e1), (_, e2)| e1 == e2)
        .skip_while(|element| element.1.is_none())
}

/// Iterates Events to accumulate times tracked
/// Expects a sorted iterator
struct Durations<'a> {
    events: Box<dyn Iterator<Item=&'a Event> + 'a>,
    ids: &'a Vec<&'a EventId>,
    threshold: Option<Timestamp>,
}
impl Durations<'_> {
    fn from<'b>(events: impl IntoIterator<Item=&'b Event> + 'b, ids: &'b Vec<&EventId>) -> Durations<'b> {
        Durations {
            events: Box::new(events.into_iter()),
            ids,
            threshold: Some(Timestamp::now()), // TODO consider offset?
        }
    }
}
impl Iterator for Durations<'_> {
    type Item = Duration;

    fn next(&mut self) -> Option<Self::Item> {
        let mut start: Option<u64> = None;
        while let Some(event) = self.events.next() {
            if matching_tag_id(event, self.ids).is_some() {
                if self.threshold.is_some_and(|th| event.created_at > th) {
                    continue;
                }
                start = start.or(Some(event.created_at.as_u64()))
            } else {
                if let Some(stamp) = start {
                    return Some(Duration::from_secs(event.created_at.as_u64() - stamp));
                }
            }
        }
        let now = self.threshold.unwrap_or(Timestamp::now()).as_u64();
        start.filter(|t| t < &now).map(|stamp| Duration::from_secs(now.saturating_sub(stamp)))
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ChildIteratorFilter {
    Reject = 0b00,
    TakeSelf = 0b01,
    TakeChildren = 0b10,
    Take = 0b11,
}
impl ChildIteratorFilter {
    fn takes_children(&self) -> bool {
        self == &ChildIteratorFilter::Take ||
            self == &ChildIteratorFilter::TakeChildren
    }
    fn takes_self(&self) -> bool {
        self == &ChildIteratorFilter::Take ||
            self == &ChildIteratorFilter::TakeSelf
    }
}

/// Breadth-First Iterator over Tasks and recursive children
struct ChildIterator<'a> {
    tasks: &'a TaskMap,
    /// Found Events
    queue: Vec<&'a EventId>,
    /// Index of the next element in the queue
    index: usize,
    /// Depth of the next element
    depth: usize,
    /// Element with the next depth boundary
    next_depth_at: usize,
}
impl<'a> ChildIterator<'a> {
    fn rooted(tasks: &'a TaskMap, id: Option<&EventId>) -> Self {
        let mut queue = Vec::with_capacity(tasks.len());
        queue.append(
            &mut tasks
                .values()
                .filter(move |t| t.parent_id() == id)
                .map(|t| t.get_id())
                .collect_vec()
        );
        Self::with_queue(tasks, queue)
    }

    fn with_queue(tasks: &'a TaskMap, queue: Vec<&'a EventId>) -> Self {
        ChildIterator {
            tasks: &tasks,
            next_depth_at: queue.len(),
            index: 0,
            depth: 1,
            queue,
        }
    }

    fn from(tasks: &'a TasksRelay, id: &'a EventId) -> Self {
        let mut queue = Vec::with_capacity(30);
        queue.push(id);
        ChildIterator {
            tasks: &tasks.tasks,
            queue,
            index: 0,
            depth: 0,
            next_depth_at: 1,
        }
    }

    /// Process until the given depth
    /// Returns true if that depth was reached
    fn process_depth(&mut self, depth: usize) -> bool {
        while self.depth < depth {
            if self.next().is_none() {
                return false;
            }
        }
        true
    }

    /// Get all children
    fn get_all(mut self) -> Vec<&'a EventId> {
        while self.next().is_some() {}
        self.queue
    }

    /// Get all tasks until the specified depth
    fn get_depth(mut self, depth: usize) -> Vec<&'a EventId> {
        self.process_depth(depth);
        self.queue
    }

    /// Get all tasks until the specified depth matching the filter
    fn get_depth_filtered<F>(mut self, depth: usize, filter: F) -> Vec<&'a EventId>
    where
        F: Fn(&Task) -> ChildIteratorFilter,
    {
        while self.depth < depth {
            if self.next_filtered(&filter).is_none() {
                // TODO this can easily recurse beyond the intended depth
                break;
            }
        }
        while self.index < self.queue.len() {
            if let Some(task) = self.tasks.get(self.queue[self.index]) {
                if !filter(task).takes_self() {
                    self.queue.remove(self.index);
                    continue;
                }
            }
            self.index += 1;
        }
        self.queue
    }

    fn check_depth(&mut self) {
        if self.next_depth_at == self.index {
            self.depth += 1;
            self.next_depth_at = self.queue.len();
        }
    }

    /// Get next id and advance, without adding children
    fn next_task(&mut self) -> Option<&'a EventId> {
        if self.index >= self.queue.len() {
            return None;
        }
        let id = self.queue[self.index];
        self.index += 1;
        Some(id)
    }

    /// Get the next known task and run it through the filter
    fn next_filtered<F>(&mut self, filter: &F) -> Option<&'a Task>
    where
        F: Fn(&Task) -> ChildIteratorFilter,
    {
        self.next_task().and_then(|id| {
            if let Some(task) = self.tasks.get(id) {
                let take = filter(task);
                if take.takes_children() {
                    self.queue_children_of(&task);
                }
                if take.takes_self() {
                    self.check_depth();
                    return Some(task);
                }
            }
            self.check_depth();
            self.next_filtered(filter)
        })
    }

    fn queue_children_of(&mut self, task: &'a Task) {
        self.queue.extend(self.tasks.children_ids_for(task.get_id()));
    }
}
impl FusedIterator for ChildIterator<'_> {}
impl<'a> Iterator for ChildIterator<'a> {
    type Item = &'a EventId;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_task().inspect(|id| {
            match self.tasks.get(id) {
                None => {
                    // Unknown task, might still find children, just slower
                    for task in self.tasks.values() {
                        if task.parent_id().is_some_and(|i| i == *id) {
                            self.queue.push(task.get_id());
                        }
                    }
                }
                Some(task) => {
                    self.queue_children_of(&task);
                }
            }
            self.check_depth();
        })
    }
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

#[cfg(test)]
mod tasks_test {
    use std::collections::HashSet;

    use super::*;

    fn stub_tasks() -> TasksRelay {
        use tokio::sync::mpsc;
        use nostr_sdk::Keys;

        let (tx, _rx) = mpsc::channel(16);
        TasksRelay::with_sender(EventSender {
            url: None,
            tx,
            keys: Keys::generate(),
            queue: Default::default(),
        })
    }

    macro_rules! assert_position {
        ($left:expr, $right:expr $(,)?) => {
            assert_eq!($left.get_position_ref(), Some(&$right))
        };
    }

    macro_rules! assert_tasks {
        ($left:expr, $right:expr $(,)?) => {
            assert_eq!($left.visible_tasks().iter().map(|t| t.event.id).collect::<HashSet<EventId>>(), 
                       HashSet::from($right))
        };
    }


    #[test]
    fn test_recursive_closing() {
        let mut tasks = stub_tasks();
        let parent = tasks.make_task("parent #tag1");
        tasks.move_to(Some(parent));
        let sub = tasks.make_task("sub # tag2");
        assert_eq!(tasks.all_hashtags().collect_vec(), vec!["tag1", "tag2"]);
        tasks.update_state("Closing Down", State::Closed);
        assert_eq!(tasks.get_by_id(&sub).unwrap().pure_state(), State::Closed);
        assert_eq!(tasks.all_hashtags().next(), None);
    }

    #[test]
    fn test_sibling_dependency() {
        let mut tasks = stub_tasks();
        let parent = tasks.make_task("parent");
        let sub = tasks.submit(EventBuilder::new(
            TASK_KIND,
            "sub",
            [tasks.make_event_tag_from_id(parent, MARKER_PARENT)],
        ));
        assert_eq!(tasks.visible_tasks().len(), 1);
        tasks.track_at(Timestamp::now(), Some(sub));
        assert_eq!(tasks.get_own_events_history().count(), 1);

        tasks.make_dependent_sibling("sibling");
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks.visible_tasks().len(), 2);
    }

    #[test]
    fn test_bookmarks() {
        let mut tasks = stub_tasks();
        let zero = EventId::all_zeros();
        let test = tasks.make_task("test # tag");
        let parent = tasks.make_task("parent");
        assert_eq!(tasks.visible_tasks().len(), 2);
        tasks.move_to(Some(parent));
        let pin = tasks.make_task("pin");

        tasks.search_depth = 1;
        assert_eq!(tasks.filtered_tasks(None, true).len(), 2);
        assert_eq!(tasks.filtered_tasks(None, false).len(), 2);
        assert_eq!(tasks.filtered_tasks(Some(&zero), false).len(), 0);
        assert_eq!(tasks.visible_tasks().len(), 1);
        assert_eq!(tasks.filtered_tasks(Some(&pin), false).len(), 0);
        assert_eq!(tasks.filtered_tasks(Some(&zero), false).len(), 0);

        tasks.submit(EventBuilder::new(
            Kind::Bookmarks,
            "",
            [Tag::event(pin), Tag::event(zero)],
        ));
        assert_eq!(tasks.visible_tasks().len(), 1);
        assert_eq!(tasks.filtered_tasks(Some(&pin), true).len(), 0);
        assert_eq!(tasks.filtered_tasks(Some(&pin), false).len(), 0);
        assert_eq!(tasks.filtered_tasks(Some(&zero), true).len(), 0);
        assert_eq!(tasks.filtered_tasks(Some(&zero), false), vec![tasks.get_by_id(&pin).unwrap()]);

        tasks.move_to(None);
        assert_eq!(tasks.view_depth, 0);
        assert_tasks!(tasks, [pin, test, parent]);
        tasks.set_view_depth(1);
        assert_tasks!(tasks, [pin, test]);
        tasks.add_tag("tag".to_string());
        assert_tasks!(tasks, [test]);
        assert_eq!(tasks.filtered_tasks(None, true), vec![tasks.get_by_id(&test).unwrap()]);

        tasks.submit(EventBuilder::new(Kind::Bookmarks, "", []));
        tasks.clear_filters();
        assert_tasks!(tasks, [pin, test]);
        tasks.set_view_depth(0);
        assert_tasks!(tasks, [test, parent]);
    }

    #[test]
    fn test_procedures() {
        let mut tasks = stub_tasks();
        tasks.make_task_and_enter("proc # tags", State::Procedure);
        assert_eq!(tasks.get_own_events_history().count(), 1);
        let side = tasks.submit(EventBuilder::new(
            TASK_KIND,
            "side",
            [tasks.make_event_tag(&tasks.get_current_task().unwrap().event, MARKER_DEPENDS)],
        ));
        assert_eq!(tasks.visible_tasks(), Vec::<&Task>::new());
        let sub_id = tasks.make_task("sub");
        assert_tasks!(tasks, [sub_id]);
        assert_eq!(tasks.len(), 3);
        let sub = tasks.get_by_id(&sub_id).unwrap();
        assert_eq!(sub.get_dependendees(), Vec::<&EventId>::new());
    }

    #[test]
    fn test_filter_or_create() {
        let mut tasks = stub_tasks();
        let zeros = EventId::all_zeros();
        let zero = Some(&zeros);

        let id1 = tasks.filter_or_create(zero, "newer");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks.visible_tasks().len(), 0);
        assert_eq!(tasks.get_by_id(&id1.unwrap()).unwrap().parent_id(), zero);

        tasks.move_to(zero.cloned());
        assert_eq!(tasks.visible_tasks().len(), 1);
        let sub = tasks.make_task("test");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks.visible_tasks().len(), 2);
        assert_eq!(tasks.get_by_id(&sub).unwrap().parent_id(), zero);

        // Do not substring match invisible subtask
        let id2 = tasks.filter_or_create(None, "#new-is gold wrapped").unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks.visible_tasks().len(), 2);
        let new2 = tasks.get_by_id(&id2).unwrap();
        assert_eq!(new2.props, Default::default());

        tasks.move_up();
        assert_eq!(tasks.get_matching(tasks.get_position_ref(), "wrapped").len(), 1);
        assert_eq!(tasks.get_matching(tasks.get_position_ref(), "new-i").len(), 1);
        tasks.filter_or_create(None, "is gold");
        assert_position!(tasks, id2);

        assert_eq!(tasks.get_own_events_history().count(), 3);
        // Global match
        let idagain = tasks.filter_or_create(None, "newer");
        assert_eq!(idagain, None);
        assert_position!(tasks, id1.unwrap());
        assert_eq!(tasks.get_own_events_history().count(), 4);
        assert_eq!(tasks.len(), 3);
    }

    #[test]
    fn test_tracking() {
        let mut tasks = stub_tasks();
        let zero = EventId::all_zeros();

        tasks.track_at(Timestamp::from(0), None);
        assert_eq!(tasks.history.len(), 0);

        let almost_now: Timestamp = Timestamp::now() - 12u64;
        tasks.track_at(Timestamp::from(11), Some(zero));
        tasks.track_at(Timestamp::from(13), Some(zero));
        assert_position!(tasks, zero);
        assert!(tasks.time_tracked(zero) > almost_now.as_u64());

        // Because None is backtracked by one to avoid conflicts
        tasks.track_at(Timestamp::from(22 + 1), None);
        assert_eq!(tasks.get_own_events_history().count(), 2);
        assert_eq!(tasks.time_tracked(zero), 11);
        tasks.track_at(Timestamp::from(22 + 1), Some(zero));
        assert_eq!(tasks.get_own_events_history().count(), 3);
        assert!(tasks.time_tracked(zero) > 999);

        let some = tasks.make_task("some");
        tasks.track_at(Timestamp::from(22 + 1), Some(some));
        assert_eq!(tasks.get_own_events_history().count(), 4);
        assert_eq!(tasks.time_tracked(zero), 12);
        assert!(tasks.time_tracked(some) > 999);

        // TODO test received events
    }

    #[test]
    #[ignore]
    fn test_timestamps() {
        let mut tasks = stub_tasks();
        let zero = EventId::all_zeros();

        tasks.track_at(Timestamp::from(Timestamp::now().as_u64() + 100), Some(zero));
        assert_eq!(timestamps(tasks.get_own_events_history(), &vec![&zero]).collect_vec().len(), 2)
        // TODO Does not show both future and current tracking properly, need to split by current time
    }


    #[test]
    fn test_depth() {
        let mut tasks = stub_tasks();

        let t1 = tasks.make_note("t1");
        let activity_t1 = tasks.get_by_id(&t1).unwrap();
        assert!(!activity_t1.is_task());
        assert_eq!(tasks.view_depth, 0);
        assert_eq!(activity_t1.pure_state(), State::Open);
        debug!("{:?}", tasks);
        assert_eq!(tasks.visible_tasks().len(), 1);
        tasks.search_depth = 0;
        assert_eq!(tasks.visible_tasks().len(), 0);
        tasks.recurse_activities = false;
        assert_eq!(tasks.filtered_tasks(None, false).len(), 1);

        tasks.move_to(Some(t1));
        assert_position!(tasks, t1);
        tasks.search_depth = 2;
        assert_eq!(tasks.visible_tasks().len(), 0);
        let t11 = tasks.make_task("t11 # tag");
        assert_eq!(tasks.visible_tasks().len(), 1);
        assert_eq!(tasks.get_task_path(Some(t11)), "t1>t11");
        assert_eq!(tasks.relative_path(t11), "t11");
        let t12 = tasks.make_task("t12");
        assert_eq!(tasks.visible_tasks().len(), 2);

        tasks.move_to(Some(t11));
        assert_position!(tasks, t11);
        assert_eq!(tasks.visible_tasks().len(), 0);
        let t111 = tasks.make_task("t111");
        assert_tasks!(tasks, [t111]);
        assert_eq!(tasks.get_task_path(Some(t111)), "t1>t11>t111");
        assert_eq!(tasks.relative_path(t111), "t111");
        tasks.view_depth = 2;
        assert_tasks!(tasks, [t111]);

        assert_eq!(ChildIterator::from(&tasks, &EventId::all_zeros()).get_all().len(), 1);
        assert_eq!(ChildIterator::from(&tasks, &EventId::all_zeros()).get_depth(0).len(), 1);
        assert_eq!(ChildIterator::from(&tasks, &t1).get_depth(0).len(), 1);
        assert_eq!(ChildIterator::from(&tasks, &t1).get_depth(1).len(), 3);
        assert_eq!(ChildIterator::from(&tasks, &t1).get_depth(2).len(), 4);
        assert_eq!(ChildIterator::from(&tasks, &t1).get_depth(9).len(), 4);
        assert_eq!(ChildIterator::from(&tasks, &t1).get_all().len(), 4);

        tasks.move_to(Some(t1));
        assert_position!(tasks, t1);
        assert_eq!(tasks.get_own_events_history().count(), 3);
        assert_eq!(tasks.relative_path(t111), "t11>t111");
        assert_eq!(tasks.view_depth, 2);
        assert_tasks!(tasks, [t111, t12]);
        tasks.set_view(vec![t11]);
        assert_tasks!(tasks, [t11]); // No more depth applied to view
        tasks.set_search_depth(1); // resets view
        assert_tasks!(tasks, [t111, t12]);
        tasks.set_view_depth(0);
        assert_tasks!(tasks, [t11, t12]);

        tasks.move_to(None);
        tasks.recurse_activities = true;
        assert_tasks!(tasks, [t11, t12]);
        tasks.recurse_activities = false;
        assert_tasks!(tasks, [t1]);
        tasks.view_depth = 1;
        assert_tasks!(tasks, [t11, t12]);
        tasks.view_depth = 2;
        assert_tasks!(tasks, [t111, t12]);
        tasks.view_depth = 9;
        assert_tasks!(tasks, [t111, t12]);

        tasks.add_tag("tag".to_string());
        tasks.view_depth = 0;
        assert_tasks!(tasks, [t11]);
        tasks.search_depth = 0;
        assert_eq!(tasks.view, []);
        assert_tasks!(tasks, []);
    }

    #[test]
    fn test_empty_task_title_fallback_to_id() {
        let mut tasks = stub_tasks();

        let empty = tasks.make_task("");
        let empty_task = tasks.get_by_id(&empty).unwrap();
        let empty_id = empty_task.event.id.to_string();
        assert_eq!(empty_task.get_title(), empty_id);
        assert_eq!(tasks.get_task_path(Some(empty)), empty_id);
    }

    #[test]
    fn test_unknown_task() {
        let mut tasks = stub_tasks();

        let zero = EventId::all_zeros();
        assert_eq!(tasks.get_task_path(Some(zero)), zero.to_string());
        tasks.move_to(Some(zero));
        let dangling = tasks.make_task("test");
        assert_eq!(
            tasks.get_task_path(Some(dangling)),
            "0000000000000000000000000000000000000000000000000000000000000000>test"
        );
        assert_eq!(tasks.relative_path(dangling), "test");
    }

    #[allow(dead_code)] // #[test]
    fn test_itertools() {
        use itertools::Itertools;
        assert_eq!(
            "test  toast".split(' ').collect_vec().len(),
            3
        );
        assert_eq!(
            "test  toast".split_ascii_whitespace().collect_vec().len(),
            2
        );
    }
}
