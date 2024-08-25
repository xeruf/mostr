use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt::{Display, Formatter};
use std::io::{Error, stdout, Write};
use std::iter::{empty, once};
use std::ops::{Div, Rem};
use std::str::FromStr;
use std::time::Duration;

use colored::Colorize;
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use nostr_sdk::{Event, EventBuilder, EventId, JsonUtil, Keys, Kind, Metadata, PublicKey, Tag, TagStandard, Timestamp, UncheckedUrl, Url};
use nostr_sdk::prelude::Marker;
use TagStandard::Hashtag;

use crate::{EventSender, MostrMessage};
use crate::helpers::{CHARACTER_THRESHOLD, format_timestamp_local, format_timestamp_relative, format_timestamp_relative_to, parse_tracking_stamp, some_non_empty};
use crate::kinds::*;
use crate::task::{MARKER_DEPENDS, MARKER_PARENT, State, Task, TaskState};

type TaskMap = HashMap<EventId, Task>;
#[derive(Debug, Clone)]
pub(crate) struct Tasks {
    /// The Tasks
    tasks: TaskMap,
    /// History of active tasks by PubKey
    history: HashMap<PublicKey, BTreeSet<Event>>,
    /// Index of found users with metadata
    users: HashMap<PublicKey, Metadata>,

    /// The task properties currently visible
    properties: Vec<String>,
    /// The task properties sorted by
    sorting: VecDeque<String>,

    /// A filtered view of the current tasks
    view: Vec<EventId>,
    /// Zero: Only Active node
    /// Positive: Go down the respective level
    depth: i8,

    /// Currently active tags
    tags: BTreeSet<Tag>,
    /// Tags filtered out
    tags_excluded: BTreeSet<Tag>,
    /// Current active state
    state: StateFilter,

    sender: EventSender,
}

#[derive(Clone, Debug)]
pub(crate) enum StateFilter {
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
            StateFilter::Default => {
                let state = task.pure_state();
                state.is_open() || (state == State::Done && task.parent_id() != None)
            }
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
impl Default for StateFilter {
    fn default() -> Self {
        StateFilter::Default
    }
}
impl Display for StateFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                StateFilter::Default => "relevant tasks".to_string(),
                StateFilter::All => "all tasks".to_string(),
                StateFilter::State(s) => format!("state {s}"),
            }
        )
    }
}

impl Tasks {
    pub(crate) fn from(url: Option<Url>, tx: &tokio::sync::mpsc::Sender<MostrMessage>, keys: &Keys, metadata: Option<Metadata>) -> Self {
        let mut new = Self::with_sender(EventSender {
            url,
            tx: tx.clone(),
            keys: keys.clone(),
            queue: Default::default(),
        });
        metadata.map(|m| new.users.insert(keys.public_key(), m));
        new
    }

    pub(crate) fn with_sender(sender: EventSender) -> Self {
        Tasks {
            tasks: Default::default(),
            history: Default::default(),
            users: Default::default(),
            properties: [
                "author",
                "state",
                "rtime",
                "hashtags",
                "rpath",
                "desc",
            ].into_iter().map(|s| s.to_string()).collect(),
            sorting: [
                "state",
                "author",
                "hashtags",
                "rtime",
                "name",
            ].into_iter().map(|s| s.to_string()).collect(),
            view: Default::default(),
            tags: Default::default(),
            tags_excluded: Default::default(),
            state: Default::default(),
            depth: 1,
            sender,
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

    fn now() -> Timestamp {
        Timestamp::from(Timestamp::now() + Self::MAX_OFFSET)
    }

    pub(crate) fn get_position_ref(&self) -> Option<&EventId> {
        self.history_from(Self::now())
            .last()
            .and_then(|e| referenced_events(e))
    }

    /// Ids of all subtasks recursively found for id, including itself
    pub(crate) fn get_task_tree<'a>(&'a self, id: &'a EventId) -> ChildIterator {
        ChildIterator::from(self, id)
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
        match self.get_position_ref() {
            None => {
                if let Some(set) = self.history.get(&self.sender.pubkey()) {
                    let mut last = None;
                    let mut full = Vec::with_capacity(set.len());
                    for event in set {
                        let new = some_non_empty(&event.tags.iter()
                            .filter_map(|t| t.content())
                            .map(|str| EventId::from_str(str).ok().map_or(str.to_string(), |id| self.get_task_title(&id)))
                            .join(" "));
                        if new != last {
                            // TODO alternate color with grey between days
                            full.push(format!("{} {}", format_timestamp_local(&event.created_at), new.as_ref().unwrap_or(&"---".to_string())));
                            last = new;
                        }
                    }
                    ("Your Time-Tracking History:".to_string(), Box::from(full.into_iter()))
                } else {
                    ("You have nothing tracked yet".to_string(), Box::from(empty()))
                }
            }
            Some(id) => {
                let ids = vec![id];
                let history =
                    self.history.iter().flat_map(|(key, set)| {
                        let mut vec = Vec::with_capacity(set.len() / 2);
                        let mut iter = timestamps(set.iter(), &ids).tuples();
                        while let Some(((start, _), (end, _))) = iter.next() {
                            vec.push(format!("{} - {} by {}",
                                             format_timestamp_local(start),
                                             format_timestamp_relative_to(end, start),
                                             self.get_author(key)))
                        }
                        iter.into_buffer()
                            .for_each(|(stamp, _)|
                            vec.push(format!("{} started by {}", format_timestamp_local(stamp), self.get_author(key))));
                        vec
                    }).sorted_unstable(); // TODO sorting depends on timestamp format - needed to interleave different people
                (format!("Times Tracked on {:?}", self.get_task_title(&id)), Box::from(history))
            }
        }
    }

    /// Total time in seconds tracked on this task by the current user.
    pub(crate) fn time_tracked(&self, id: EventId) -> u64 {
        Durations::from(self.history.get(&self.sender.pubkey()).into_iter().flatten(), &vec![&id]).sum::<Duration>().as_secs()
    }


    /// Total time in seconds tracked on this task and its subtasks by all users.
    fn total_time_tracked(&self, id: EventId) -> u64 {
        let mut total = 0;

        let children = self.get_task_tree(&id).get_all();
        for user in self.history.values() {
            total += Durations::from(user, &children).sum::<Duration>().as_secs();
        }
        total
    }

    fn total_progress(&self, id: &EventId) -> Option<f32> {
        self.get_by_id(id).and_then(|t| match t.pure_state() {
            State::Closed => None,
            State::Done => Some(1.0),
            _ => {
                let mut sum = 0f32;
                let mut count = 0;
                for prog in t.children.iter().filter_map(|e| self.total_progress(e)) {
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
            .join("")
    }

    pub(crate) fn get_task_path(&self, id: Option<EventId>) -> String {
        join_tasks(self.traverse_up_from(id), true)
            .filter(|s| !s.is_empty())
            .or_else(|| id.map(|id| id.to_string()))
            .unwrap_or(String::new())
    }

    fn traverse_up_from(&self, id: Option<EventId>) -> ParentIterator {
        ParentIterator {
            tasks: &self.tasks,
            current: id,
            prev: None,
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

    fn resolve_tasks<'a>(&'a self, iter: impl Iterator<Item=&'a EventId>) -> impl Iterator<Item=&'a Task> {
        self.resolve_tasks_rec(iter, self.depth)
    }

    fn resolve_tasks_rec<'a>(
        &'a self,
        iter: impl Iterator<Item=&'a EventId>,
        depth: i8,
    ) -> Box<impl Iterator<Item=&'a Task>> {
        iter.filter_map(|id| self.get_by_id(&id))
            .flat_map(move |task| {
                let new_depth = depth - 1;
                if new_depth == 0 {
                    vec![task]
                } else {
                    let tasks_iter = self.resolve_tasks_rec(task.children.iter(), new_depth);
                    if new_depth < 0 {
                        let tasks: Vec<&Task> = tasks_iter.collect();
                        if tasks.is_empty() {
                            vec![task]
                        } else {
                            tasks
                        }
                    } else {
                        tasks_iter.chain(once(task)).collect()
                    }
                }
            })
            .into()
    }

    /// Executes the given function with each task referenced by this event without marker.
    /// Returns true if any task was found.
    pub(crate) fn referenced_tasks<F: Fn(&mut Task)>(&mut self, event: &Event, f: F) -> bool {
        let mut found = false;
        for tag in event.tags.iter() {
            if let Some(TagStandard::Event { event_id, marker, .. }) = tag.as_standardized() {
                if marker.is_none() {
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

    pub(crate) fn children_of<'a>(&'a self, id: Option<&'a EventId>) -> impl Iterator<Item=&EventId> + 'a {
        self.tasks
            .values()
            .filter(move |t| t.parent_id() == id)
            .map(|t| t.get_id())
    }

    pub(crate) fn filtered_tasks<'a>(&'a self, position: Option<&'a EventId>) -> impl Iterator<Item=&Task> + 'a {
        // TODO use ChildIterator
        self.resolve_tasks(self.children_of(position))
            .filter(move |t| {
                // TODO apply filters in transit
                self.state.matches(t) &&
                    t.tags.as_ref().map_or(true, |tags| {
                        tags.iter().find(|tag| self.tags_excluded.contains(tag)).is_none()
                    }) &&
                    (self.tags.is_empty() ||
                        t.tags.as_ref().map_or(false, |tags| {
                            let mut iter = tags.iter();
                            self.tags.iter().all(|tag| iter.any(|t| t == tag))
                        }))
            })
    }

    pub(crate) fn visible_tasks(&self) -> Vec<&Task> {
        if self.depth == 0 {
            return vec![];
        }
        if self.view.len() > 0 {
            return self.resolve_tasks(self.view.iter()).collect();
        }
        self.filtered_tasks(self.get_position_ref()).collect()
    }

    pub(crate) fn print_tasks(&self) -> Result<(), Error> {
        let mut lock = stdout().lock();
        if let Some(t) = self.get_current_task() {
            let state = t.state_or_default();
            let now = &Self::now();
            let mut tracking_stamp: Option<Timestamp> = None;
            for elem in
                timestamps(self.history.get(&self.sender.pubkey()).into_iter().flatten(), &vec![t.get_id()])
                    .map(|(e, _)| e) {
                if tracking_stamp.is_some() && elem > now {
                    break;
                }
                tracking_stamp = Some(elem.clone())
            }
            writeln!(
                lock,
                "Tracking since {} (total tracked time {}m) - {} since {}",
                tracking_stamp.map_or("?".to_string(), |t| format_timestamp_relative(&t)),
                self.time_tracked(*t.get_id()) / 60,
                state.get_label(),
                format_timestamp_relative(&state.time)
            )?;
            writeln!(lock, "{}", t.descriptions().join("\n"))?;
        }

        let mut tasks = self.visible_tasks();
        if tasks.is_empty() {
            let (label, times) = self.times_tracked();
            let mut times_recent = times.rev().take(6).collect_vec();
            times_recent.reverse();
            // TODO Add recent prefix
            writeln!(lock, "{}\n{}", label.italic(), times_recent.join("\n"))?;
            return Ok(());
        }

        // TODO proper column alignment
        // TODO hide empty columns
        writeln!(lock, "{}", self.properties.join("\t").bold())?;
        let mut total_time = 0;
        let count = tasks.len();
        tasks.sort_by_cached_key(|task| {
            self.sorting
                .iter()
                .map(|p| self.get_property(task, p.as_str()))
                .collect_vec()
        });
        for task in tasks {
            writeln!(
                lock,
                "{}",
                self.properties.iter()
                    .map(|p| self.get_property(task, p.as_str()))
                    .join(" \t")
            )?;
            if self.depth < 2 || task.parent_id() == self.get_position_ref() {
                total_time += self.total_time_tracked(task.event.id)
            }
        }
        if total_time > 0 {
            writeln!(lock, "{} visible tasks{}", count, display_time(" tracked a total of HHhMMm", total_time))?;
        }
        Ok(())
    }

    fn get_property(&self, task: &Task, str: &str) -> String {
        let progress =
            self
                .total_progress(task.get_id())
                .filter(|_| task.children.len() > 0);
        let prog_string = progress.map_or(String::new(), |p| format!("{:2.0}%", p * 100.0));
        match str {
            "subtasks" => {
                let mut total = 0;
                let mut done = 0;
                for subtask in task.children.iter().filter_map(|id| self.get_by_id(id)) {
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

            "author" => format!("{:.6}", self.get_author(&task.event.pubkey)), // FIXME temporary until proper column alignment
            "path" => self.get_task_path(Some(task.event.id)),
            "rpath" => self.relative_path(task.event.id),
            // TODO format strings configurable
            "time" => display_time("MMMm", self.time_tracked(*task.get_id())),
            "rtime" => display_time("HH:MM", self.total_time_tracked(*task.get_id())),
            prop => task.get(prop).unwrap_or(String::new()),
        }
    }

    pub(crate) fn get_author(&self, pubkey: &PublicKey) -> String {
        self.users.get(pubkey)
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| format!("{:.6}", pubkey.to_string()))
    }

    // Movement and Selection

    pub(crate) fn set_filter(&mut self, view: Vec<EventId>) {
        if view.is_empty() {
            warn!("No match for filter!")
        }
        self.view = view;
    }

    pub(crate) fn clear_filter(&mut self) {
        self.view.clear();
        self.tags.clear();
        self.tags_excluded.clear();
        info!("Removed all filters");
    }

    pub(crate) fn set_tags(&mut self, tags: impl IntoIterator<Item=Tag>) {
        self.tags_excluded.clear();
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

    /// Returns ids of tasks starting with the given string.
    pub(crate) fn get_filtered(&self, position: Option<&EventId>, arg: &str) -> Vec<EventId> {
        if let Ok(id) = EventId::parse(arg) {
            return vec![id];
        }
        let mut filtered: Vec<EventId> = Vec::with_capacity(32);
        let lowercase_arg = arg.to_ascii_lowercase();
        let mut filtered_more: Vec<EventId> = Vec::with_capacity(32);
        for task in self.filtered_tasks(position) {
            let lowercase = task.event.content.to_ascii_lowercase();
            if lowercase == lowercase_arg {
                return vec![task.event.id];
            } else if task.event.content.starts_with(arg) {
                filtered.push(task.event.id)
            } else if lowercase.starts_with(&lowercase_arg) {
                filtered_more.push(task.event.id)
            }
        }
        if filtered.len() == 0 {
            return filtered_more;
        }
        return filtered;
    }

    /// Finds out what to do with the given string.
    /// Returns an EventId if a new Task was created.
    pub(crate) fn filter_or_create(&mut self, position: Option<&EventId>, arg: &str) -> Option<EventId> {
        let filtered = self.get_filtered(position, arg);
        match filtered.len() {
            0 => {
                // No match, new task
                self.view.clear();
                if arg.len() < CHARACTER_THRESHOLD {
                    warn!("New task name needs at least {CHARACTER_THRESHOLD} characters");
                    return None
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
                self.set_filter(filtered);
                None
            }
        }
    }

    /// Returns all recent events from history until the first event at or before the given timestamp.
    fn history_from(&self, stamp: Timestamp) -> impl Iterator<Item=&Event> {
        self.history.get(&self.sender.pubkey()).map(|hist| {
            hist.iter().rev().take_while_inclusive(move |e| e.created_at > stamp)
        }).into_iter().flatten()
    }

    const MAX_OFFSET: u64 = 9;

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
        let offset: u64 = self.history_from(now).skip_while(|e| e.created_at.as_u64() > now.as_u64() + Self::MAX_OFFSET).count() as u64;
        if offset >= Self::MAX_OFFSET {
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
                .map(|t| {
                    if t.pure_state() == State::Procedure {
                        t.children.iter()
                            .filter_map(|id| self.get_by_id(id))
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

    /// Creates a task including current tag filters
    ///
    /// Sanitizes input
    pub(crate) fn make_task_with(&mut self, input: &str, tags: impl IntoIterator<Item=Tag>, set_state: bool) -> EventId {
        let (input, input_tags) = extract_tags(input.trim());
        let id = self.submit(
            build_task(input, input_tags, None)
                .add_tags(self.tags.iter().cloned())
                .add_tags(tags.into_iter())
        );
        if set_state {
            self.state.as_option().inspect(|s| self.set_state_for_with(id, s));
        }
        id
    }

    pub(crate) fn get_task_title(&self, id: &EventId) -> String {
        self.tasks.get(id).map_or(id.to_string(), |t| t.get_title())
    }

    /// Parse string and set tracking
    /// Returns false and prints a message if parsing failed
    pub(crate) fn track_from(&mut self, str: &str) -> bool {
        parse_tracking_stamp(str)
            .map(|stamp| self.track_at(stamp, self.get_position()))
            .is_some()
    }

    pub(crate) fn track_at(&mut self, time: Timestamp, task: Option<EventId>) -> EventId {
        info!("{} {}", task.map_or(
            String::from("Stopping time-tracking at"),
            |id| format!("Tracking \"{}\" from", self.get_task_title(&id))), format_timestamp_relative(&time));
        self.submit(
            build_tracking(task)
                .custom_created_at(time)
        )
    }

    /// Sign and queue the event to the relay, returning its id
    fn submit(&mut self, builder: EventBuilder) -> EventId {
        let event = self.sender.submit(builder).unwrap();
        let id = event.id;
        self.add(event);
        id
    }

    pub(crate) fn add(&mut self, event: Event) {
        match event.kind.as_u16() {
            TASK_KIND => self.add_task(event),
            TRACKING_KIND =>
                match self.history.get_mut(&event.pubkey) {
                    Some(c) => { c.insert(event); }
                    None => { self.history.insert(event.pubkey, BTreeSet::from([event])); }
                },
            METADATA_KIND =>
                match Metadata::from_json(event.content()) {
                    Ok(metadata) => { self.users.insert(event.pubkey, metadata); }
                    Err(e) => warn!("Cannot parse metadata: {} from {:?}", e, event)
                }
            _ => self.add_prop(event),
        }
    }

    pub(crate) fn add_task(&mut self, event: Event) {
        if self.tasks.contains_key(&event.id) {
            warn!("Did not insert duplicate event {}", event.id);
        } else {
            let id = event.id;
            let task = Task::new(event);
            task.find_refs(MARKER_PARENT).for_each(|parent| {
                self.tasks.get_mut(parent).map(|t| { t.children.insert(id); });
            });
            self.tasks.insert(id, task);
        }
    }

    fn add_prop(&mut self, event: Event) {
        let found = self.referenced_tasks(&event, |t| {
            t.props.insert(event.clone());
        });
        if !found {
            if event.kind.as_u16() == NOTE_KIND {
                self.add_task(event);
                return;
            }
            warn!("Unknown event {:?}", event)
        }
    }

    fn get_own_history(&mut self) -> Option<&mut BTreeSet<Event>> {
        self.history.get_mut(&self.sender.pubkey())
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
        self.get_own_history().map(
            |t| t.retain(|e| e != event &&
                !referenced_events(e).is_some_and(|id| id == &event.id)));
        self.referenced_tasks(event, |t| { t.props.remove(event); });
    }

    pub(crate) fn set_state_for_with(&mut self, id: EventId, comment: &str) {
        self.set_state_for(id, comment, comment.into());
    }

    pub(crate) fn set_state_for(&mut self, id: EventId, comment: &str, state: State) -> EventId {
        let prop = build_prop(
            state.into(),
            comment,
            id,
        );
        info!("Task status {} set for \"{}\"", TaskState::get_label_for(&state, comment), self.get_task_title(&id));
        self.submit(prop)
    }

    pub(crate) fn update_state(&mut self, comment: &str, state: State) -> Option<EventId> {
        let id = self.get_position_ref()?;
        Some(self.set_state_for(id.clone(), comment, state))
    }

    pub(crate) fn make_note(&mut self, note: &str) {
        if let Some(id) = self.get_position_ref() {
            if self.get_by_id(id).is_some_and(|t| t.is_task()) {
                let prop = build_prop(Kind::TextNote, note.trim(), id.clone());
                self.submit(prop);
                return;
            }
        }
        let (input, tags) = extract_tags(note.trim());
        self.submit(
            build_task(input, tags, Some(("stateless ", Kind::TextNote)))
                .add_tags(self.parent_tag())
                .add_tags(self.tags.iter().cloned())
        );
    }

    // Properties

    pub(crate) fn set_depth(&mut self, depth: i8) {
        self.depth = depth;
        info!("Changed view depth to {depth}");
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
/// MMM - minutes
/// MM - minutes of the hour
/// HH - hours
/// Returns an empty string if under a minute.
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
            Some(acc.map_or_else(|| val.clone(), |cur| format!("{}>{}", val, cur)))
        })
}

fn referenced_events(event: &Event) -> Option<&EventId> {
    event.tags.iter().find_map(|tag| match tag.as_standardized() {
        Some(TagStandard::Event { event_id, .. }) => Some(event_id),
        _ => None
    })
}

fn matching_tag_id<'a>(event: &'a Event, ids: &'a Vec<&'a EventId>) -> Option<&'a EventId> {
    event.tags.iter().find_map(|tag| match tag.as_standardized() {
        Some(TagStandard::Event { event_id, .. }) if ids.contains(&event_id) => Some(event_id),
        _ => None
    })
}

/// Filters out event timestamps to those that start or stop one of the given events
fn timestamps<'a>(events: impl Iterator<Item=&'a Event>, ids: &'a Vec<&'a EventId>) -> impl Iterator<Item=(&Timestamp, Option<&EventId>)> {
    events.map(|event| (&event.created_at, matching_tag_id(event, ids)))
        .dedup_by(|(_, e1), (_, e2)| e1 == e2)
        .skip_while(|element| element.1 == None)
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
        return start.filter(|t| t < &now).map(|stamp| Duration::from_secs(now.saturating_sub(stamp)));
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
    fn from(tasks: &'a Tasks, id: &'a EventId) -> Self {
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

    fn get_depth(mut self, depth: usize) -> Vec<&'a EventId> {
        while self.depth < depth {
            self.next();
        }
        self.queue
    }

    fn get_all(mut self) -> Vec<&'a EventId> {
        while self.next().is_some() {}
        self.queue
    }
}
impl<'a> Iterator for ChildIterator<'a> {
    type Item = &'a EventId;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.queue.len() {
            return None;
        }
        let id = self.queue[self.index];
        if let Some(task) = self.tasks.get(&id) {
            self.queue.reserve(task.children.len());
            self.queue.extend(task.children.iter());
        } else {
            // Unknown task, might still find children, just slower
            for task in self.tasks.values() {
                if task.parent_id().is_some_and(|i| i == id) {
                    self.queue.push(task.get_id());
                }
            }
        }
        self.index += 1;
        if self.next_depth_at == self.index {
            self.depth += 1;
            self.next_depth_at = self.queue.len();
        }
        Some(id)
    }
}


struct ParentIterator<'a> {
    tasks: &'a TaskMap,
    current: Option<EventId>,
    /// Inexpensive helper to assert correctness
    prev: Option<EventId>,
}
impl<'a> Iterator for ParentIterator<'a> {
    type Item = &'a Task;

    fn next(&mut self) -> Option<Self::Item> {
        self.current.and_then(|id| self.tasks.get(&id)).map(|t| {
            self.prev.map(|id| assert!(t.children.contains(&id)));
            self.prev = self.current;
            self.current = t.parent_id().cloned();
            t
        })
    }
}

#[cfg(test)]
mod tasks_test {
    use std::collections::HashSet;

    use super::*;

    fn stub_tasks() -> Tasks {
        use tokio::sync::mpsc;
        use nostr_sdk::Keys;

        let (tx, _rx) = mpsc::channel(16);
        Tasks::with_sender(EventSender {
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

    #[test]
    fn test_procedures() {
        let mut tasks = stub_tasks();
        tasks.make_task_and_enter("proc: tags", State::Procedure);
        assert_eq!(tasks.get_own_history().unwrap().len(), 1);
        let side = tasks.submit(build_task("side", vec![tasks.make_event_tag(&tasks.get_current_task().unwrap().event, MARKER_DEPENDS)], None));
        assert_eq!(tasks.get_current_task().unwrap().children, HashSet::<EventId>::new());
        let sub_id = tasks.make_task("sub");
        assert_eq!(tasks.get_current_task().unwrap().children, HashSet::from([sub_id]));
        assert_eq!(tasks.len(), 3);
        let sub = tasks.get_by_id(&sub_id).unwrap();
        assert_eq!(sub.get_dependendees(), Vec::<&EventId>::new());
    }

    #[test]
    fn test_filter_or_create() {
        let mut tasks = stub_tasks();
        let zeros = EventId::all_zeros();
        let zero = Some(&zeros);

        let id1 = tasks.filter_or_create(zero, "new");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks.visible_tasks().len(), 0);
        assert_eq!(tasks.get_by_id(&id1.unwrap()).unwrap().parent_id(), zero);

        tasks.move_to(zero.cloned());
        assert_eq!(tasks.visible_tasks().len(), 1);
        let sub = tasks.make_task("test");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks.visible_tasks().len(), 2);
        assert_eq!(tasks.get_by_id(&sub).unwrap().parent_id(), zero);

        let id2 = tasks.filter_or_create(None, "new");
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks.visible_tasks().len(), 2);
        let new2 = tasks.get_by_id(&id2.unwrap()).unwrap();
        assert_eq!(new2.props, Default::default());
    }

    #[test]
    fn test_tracking() {
        let mut tasks = stub_tasks();
        let zero = EventId::all_zeros();

        tasks.track_at(Timestamp::from(0), None);
        assert_eq!(tasks.history.len(), 1);

        let almost_now: Timestamp = Timestamp::now() - 12u64;
        tasks.track_at(Timestamp::from(11), Some(zero));
        tasks.track_at(Timestamp::from(13), Some(zero));
        assert_position!(tasks, zero);
        assert!(tasks.time_tracked(zero) > almost_now.as_u64());

        tasks.track_at(Timestamp::from(22), None);
        assert_eq!(tasks.get_own_history().unwrap().len(), 4);
        assert_eq!(tasks.time_tracked(zero), 11);

        // TODO test received events
    }

    #[test]
    #[ignore]
    fn test_timestamps() {
        let mut tasks = stub_tasks();
        let zero = EventId::all_zeros();

        tasks.track_at(Timestamp::from(Timestamp::now().as_u64() + 100), Some(zero));
        assert_eq!(timestamps(tasks.history.values().nth(0).unwrap().into_iter(), &vec![&zero]).collect_vec().len(), 2)
        // TODO Does not show both future and current tracking properly, need to split by current time
    }


    #[test]
    fn test_depth() {
        let mut tasks = stub_tasks();

        let t1 = tasks.make_task("t1");
        let task1 = tasks.get_by_id(&t1).unwrap();
        assert_eq!(tasks.depth, 1);
        assert_eq!(task1.pure_state(), State::Open);
        debug!("{:?}", tasks);
        assert_eq!(tasks.visible_tasks().len(), 1);
        tasks.depth = 0;
        assert_eq!(tasks.visible_tasks().len(), 0);

        tasks.move_to(Some(t1));
        assert_position!(tasks, t1);
        tasks.depth = 2;
        assert_eq!(tasks.visible_tasks().len(), 0);
        let t2 = tasks.make_task("t2");
        assert_eq!(tasks.visible_tasks().len(), 1);
        assert_eq!(tasks.get_task_path(Some(t2)), "t1>t2");
        assert_eq!(tasks.relative_path(t2), "t2");
        let t3 = tasks.make_task("t3");
        assert_eq!(tasks.visible_tasks().len(), 2);

        tasks.move_to(Some(t2));
        assert_position!(tasks, t2);
        assert_eq!(tasks.visible_tasks().len(), 0);
        let t4 = tasks.make_task("t4");
        assert_eq!(tasks.visible_tasks().len(), 1);
        assert_eq!(tasks.get_task_path(Some(t4)), "t1>t2>t4");
        assert_eq!(tasks.relative_path(t4), "t4");
        tasks.depth = 2;
        assert_eq!(tasks.visible_tasks().len(), 1);
        tasks.depth = -1;
        assert_eq!(tasks.visible_tasks().len(), 1);

        assert_eq!(ChildIterator::from(&tasks, &EventId::all_zeros()).get_all().len(), 1);
        assert_eq!(ChildIterator::from(&tasks, &EventId::all_zeros()).get_depth(0).len(), 1);
        assert_eq!(ChildIterator::from(&tasks, &t1).get_depth(0).len(), 1);
        assert_eq!(ChildIterator::from(&tasks, &t1).get_depth(1).len(), 3);
        assert_eq!(ChildIterator::from(&tasks, &t1).get_depth(2).len(), 4);
        assert_eq!(ChildIterator::from(&tasks, &t1).get_all().len(), 4);

        tasks.move_to(Some(t1));
        assert_position!(tasks, t1);
        assert_eq!(tasks.get_own_history().unwrap().len(), 3);
        assert_eq!(tasks.relative_path(t4), "t2>t4");
        assert_eq!(tasks.visible_tasks().len(), 2);
        tasks.depth = 2;
        assert_eq!(tasks.visible_tasks().len(), 3);
        tasks.set_filter(vec![t2]);
        assert_eq!(tasks.visible_tasks().len(), 2);
        tasks.depth = 1;
        assert_eq!(tasks.visible_tasks().len(), 1);
        tasks.depth = -1;
        assert_eq!(tasks.visible_tasks().len(), 1);
        tasks.set_filter(vec![t2, t3]);
        assert_eq!(tasks.visible_tasks().len(), 2);
        tasks.depth = 2;
        assert_eq!(tasks.visible_tasks().len(), 3);
        tasks.depth = 1;
        assert_eq!(tasks.visible_tasks().len(), 2);

        tasks.move_to(None);
        assert_eq!(tasks.visible_tasks().len(), 1);
        tasks.depth = 2;
        assert_eq!(tasks.visible_tasks().len(), 3);
        tasks.depth = 3;
        assert_eq!(tasks.visible_tasks().len(), 4);
        tasks.depth = 9;
        assert_eq!(tasks.visible_tasks().len(), 4);
        tasks.depth = -1;
        assert_eq!(tasks.visible_tasks().len(), 2);
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