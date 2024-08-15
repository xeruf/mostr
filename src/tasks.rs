use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt::{Display, Formatter};
use std::io::{Error, stdout, Write};
use std::iter::once;
use std::ops::{Div, Rem};
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::time::Duration;

use chrono::{DateTime, Local, TimeZone};
use chrono::LocalResult::Single;
use colored::Colorize;
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use nostr_sdk::{Event, EventBuilder, EventId, Keys, Kind, PublicKey, Tag, TagStandard, Timestamp, UncheckedUrl, Url};
use nostr_sdk::prelude::Marker;
use TagStandard::Hashtag;

use crate::{Events, EventSender, MostrMessage};
use crate::helpers::{format_stamp, local_datetimestamp, relative_datetimestamp, some_non_empty};
use crate::kinds::*;
use crate::task::{MARKER_DEPENDS, MARKER_PARENT, State, Task, TaskState};

type TaskMap = HashMap<EventId, Task>;
#[derive(Debug, Clone)]
pub(crate) struct Tasks {
    /// The Tasks
    tasks: TaskMap,
    /// History of active tasks by PubKey
    history: HashMap<PublicKey, BTreeSet<Event>>,

    /// The task properties currently visible
    properties: Vec<String>,
    /// The task properties sorted by
    sorting: VecDeque<String>,
    /// Negative: Only Leaf nodes
    /// Zero: Only Active node
    /// Positive: Go down the respective level
    depth: i8,

    /// Currently active task
    position: Option<EventId>,
    /// Currently active tags
    tags: BTreeSet<Tag>,
    /// Tags filtered out
    tags_excluded: BTreeSet<Tag>,
    /// Current active state
    state: StateFilter,
    /// A filtered view of the current tasks
    view: Vec<EventId>,

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
    pub(crate) fn from(url: Option<Url>, tx: &Sender<MostrMessage>, keys: &Keys) -> Self {
        Self::with_sender(EventSender {
            url,
            tx: tx.clone(),
            keys: keys.clone(),
            queue: Default::default(),
        })
    }

    pub(crate) fn with_sender(sender: EventSender) -> Self {
        Tasks {
            tasks: Default::default(),
            history: Default::default(),
            properties: vec![
                "state".into(),
                "rtime".into(),
                "hashtags".into(),
                "rpath".into(),
                "desc".into(),
            ],
            sorting: VecDeque::from([
                "state".into(),
                "hashtags".into(),
                "rtime".into(),
                "name".into(),
            ]),
            position: None, // TODO persist position
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
    pub(crate) fn get_position(&self) -> Option<EventId> { self.position }

    #[inline]
    pub(crate) fn len(&self) -> usize { self.tasks.len() }

    /// Ids of all subtasks recursively found for id, including itself
    pub(crate) fn get_task_tree<'a>(&'a self, id: &'a EventId) -> ChildIterator {
        ChildIterator::from(&self.tasks, id)
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
    pub(crate) fn times_tracked(&self) -> String {
        match self.get_position() {
            None => {
                let hist = self.history.get(&self.sender.pubkey());
                if let Some(set) = hist {
                    let mut full = String::with_capacity(set.len() * 40);
                    let mut last: Option<String> = None;
                    full.push_str("Your Time Tracking History:\n");
                    for event in set {
                        let new = some_non_empty(&event.tags.iter()
                            .filter_map(|t| t.content())
                            .map(|str| EventId::from_str(str).ok().map_or(str.to_string(), |id| self.get_task_title(&id)))
                            .join(" "));
                        if new != last {
                            full.push_str(&format!("{:>15} {}\n", relative_datetimestamp(&event.created_at), new.as_ref().unwrap_or(&"---".to_string())));
                            last = new;
                        }
                    }
                    full
                } else {
                    String::from("You have nothing tracked yet")
                }
            }
            Some(id) => {
                let ids = vec![&id];
                once(format!("Times tracked on {}", self.get_task_title(&id))).chain(
                    self.history.iter().flat_map(|(key, set)| {
                        let mut vec = Vec::with_capacity(set.len() / 2);
                        let mut iter = timestamps(set.iter(), &ids).tuples();
                        while let Some(((start, _), (end, _))) = iter.next() {
                            vec.push(format!("{} - {} by {}",
                                             local_datetimestamp(start),
                                             // Only use full stamp when ambiguous (>1day)
                                             if end.as_u64() - start.as_u64() > 86400 {
                                                 local_datetimestamp(end)
                                             } else {
                                                 format_stamp(end, "%H:%M")
                                             },
                                             key))
                        }
                        iter.into_buffer()
                            .for_each(|(stamp, _)|
                            vec.push(format!("{} started by {}", local_datetimestamp(stamp), key)));
                        vec
                    }).sorted_unstable() // TODO sorting depends on timestamp format - needed to interleave different people
                ).join("\n")
            }
        }
    }

    /// Total time in seconds tracked on this task by the current user.
    pub(crate) fn time_tracked(&self, id: EventId) -> u64 {
        TimesTracked::from(self.history.get(&self.sender.pubkey()).into_iter().flatten(), &vec![&id]).sum::<Duration>().as_secs()
    }


    /// Total time in seconds tracked on this task and its subtasks by all users.
    fn total_time_tracked(&self, id: EventId) -> u64 {
        let mut total = 0;

        let children = self.get_task_tree(&id).get_all();
        for user in self.history.values() {
            total += TimesTracked::from(user, &children).into_iter().sum::<Duration>().as_secs();
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

    pub(crate) fn get_parent(&self, id: Option<EventId>) -> Option<&EventId> {
        id.and_then(|id| self.get_by_id(&id))
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
                .take_while(|t| Some(t.event.id) != self.position),
            false,
        ).unwrap_or(id.to_string())
    }

    // Helpers

    fn resolve_tasks<'a>(&self, iter: impl IntoIterator<Item=&'a EventId>) -> Vec<&Task> {
        self.resolve_tasks_rec(iter, self.depth)
    }

    fn resolve_tasks_rec<'a>(
        &self,
        iter: impl IntoIterator<Item=&'a EventId>,
        depth: i8,
    ) -> Vec<&Task> {
        iter.into_iter()
            .filter_map(|id| self.get_by_id(&id))
            .flat_map(|task| {
                let new_depth = depth - 1;
                if new_depth < 0 {
                    let tasks = self
                        .resolve_tasks_rec(task.children.iter(), new_depth)
                        .into_iter()
                        .collect::<Vec<&Task>>();
                    if tasks.is_empty() {
                        vec![task]
                    } else {
                        tasks
                    }
                } else if new_depth > 0 {
                    self.resolve_tasks_rec(task.children.iter(), new_depth)
                        .into_iter()
                        .chain(once(task))
                        .collect()
                } else {
                    vec![task]
                }
            })
            .collect()
    }

    pub(crate) fn referenced_tasks<F: Fn(&mut Task)>(&mut self, event: &Event, f: F) {
        for tag in event.tags.iter() {
            if let Some(TagStandard::Event { event_id, .. }) = tag.as_standardized() {
                self.tasks.get_mut(event_id).map(|t| f(t));
            }
        }
    }

    #[inline]
    pub(crate) fn get_current_task(&self) -> Option<&Task> {
        self.position.and_then(|id| self.get_by_id(&id))
    }

    pub(crate) fn children_of(&self, id: Option<EventId>) -> impl Iterator<Item=&EventId> + '_ {
        self.tasks
            .values()
            .filter(move |t| t.parent_id() == id.as_ref())
            .map(|t| t.get_id())
    }

    pub(crate) fn filtered_tasks(&self, position: Option<EventId>) -> impl Iterator<Item=&Task> {
        // TODO use ChildrenIterator
        self.resolve_tasks(self.children_of(position)).into_iter()
            .filter(|t| {
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
            return self.get_current_task().into_iter().collect();
        }
        if self.view.len() > 0 {
            return self.resolve_tasks(self.view.iter());
        }
        self.filtered_tasks(self.position).collect()
    }

    pub(crate) fn print_tasks(&self) -> Result<(), Error> {
        let mut lock = stdout().lock();
        if let Some(t) = self.get_current_task() {
            let state = t.state_or_default();
            let now = &Timestamp::now();
            let mut tracking_stamp: Option<Timestamp> = None;
            for elem in
                timestamps(self.history.get(&self.sender.pubkey()).into_iter().flatten(), &vec![t.get_id()])
                    .map(|(e, o)| e) {
                if tracking_stamp.is_some() && elem > now {
                    break;
                }
                tracking_stamp = Some(elem.clone())
            }
            writeln!(
                lock,
                "Tracking since {} (total tracked time {}m) - {} since {}",
                tracking_stamp.map_or("?".to_string(), |t| relative_datetimestamp(&t)),
                self.time_tracked(*t.get_id()) / 60,
                state.get_label(),
                relative_datetimestamp(&state.time)
            )?;
            writeln!(lock, "{}", t.descriptions().join("\n"))?;
        }
        // TODO proper column alignment
        // TODO hide empty columns
        writeln!(lock, "{}", self.properties.join("\t").bold())?;
        let mut total_time = 0;
        let mut tasks = self.visible_tasks();
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
            if self.depth < 2 || task.parent_id() == self.position.as_ref() {
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
                let state = task.state_or_default();
                if state.state.is_open() && progress.is_some_and(|p| p > 0.1) {
                    state.state.colorize(&prog_string)
                } else {
                    state.get_colored_label()
                }.to_string()
            }
            "progress" => prog_string.clone(),
            "path" => self.get_task_path(Some(task.event.id)),
            "rpath" => self.relative_path(task.event.id),
            // TODO format strings configurable
            "time" => display_time("MMMm", self.time_tracked(*task.get_id())),
            "rtime" => display_time("HH:MM", self.total_time_tracked(*task.get_id())),
            prop => task.get(prop).unwrap_or(String::new()),
        }
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

    /// Returns ids of tasks matching the filter.
    pub(crate) fn get_filtered(&self, arg: &str) -> Vec<EventId> {
        if let Ok(id) = EventId::parse(arg) {
            return vec![id];
        }
        let mut filtered: Vec<EventId> = Vec::with_capacity(32);
        let lowercase_arg = arg.to_ascii_lowercase();
        let mut filtered_more: Vec<EventId> = Vec::with_capacity(32);
        for task in self.filtered_tasks(self.position) {
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
    pub(crate) fn filter_or_create(&mut self, arg: &str) -> Option<EventId> {
        let filtered = self.get_filtered(arg);
        match filtered.len() {
            0 => {
                // No match, new task
                self.view.clear();
                if arg.len() > 2 {
                    Some(self.make_task(arg))
                } else {
                    warn!("Name of a task needs to have at least 3 characters");
                    None
                }
            }
            1 => {
                // One match, activate
                self.move_to(filtered.into_iter().nth(0));
                None
            }
            _ => {
                // Multiple match, filter
                self.set_filter(filtered);
                None
            }
        }
    }

    pub(crate) fn move_to(&mut self, id: Option<EventId>) {
        self.view.clear();
        if id == self.position {
            debug!("Flushing Tasks because of move in place");
            self.flush();
            return;
        }
        self.submit(build_tracking(id));
        if !id.and_then(|id| self.tasks.get(&id)).is_some_and(|t| t.parent_id() == self.position.as_ref()) {
            debug!("Flushing Tasks because of move beyond child");
            self.flush();
        }
        self.position = id;
    }

    // Updates

    /// Expects sanitized input
    pub(crate) fn parse_task(&self, input: &str) -> EventBuilder {
        let mut tags: Vec<Tag> = self.tags.iter().cloned().collect();
        match input.split_once(": ") {
            None => build_task(input, tags),
            Some(s) => {
                tags.append(
                    &mut s
                        .1
                        .split_ascii_whitespace()
                        .map(|t| Hashtag(t.to_string()).into())
                        .collect(),
                );
                build_task(s.0, tags)
            }
        }
    }

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
        self.position.map(|p| self.make_event_tag_from_id(p, MARKER_PARENT))
    }

    pub(crate) fn position_tags(&self) -> Vec<Tag> {
        let mut tags = Vec::with_capacity(2);
        self.parent_tag().map(|t| tags.push(t));
        self.get_current_task()
            .map(|t| {
                if t.pure_state() == State::Procedure {
                    t.children.iter()
                        .filter_map(|id| self.get_by_id(id))
                        .max()
                        .map(|t| tags.push(self.make_event_tag(&t.event, MARKER_DEPENDS)));
                }
            });
        tags
    }

    /// Creates a task following the current state
    /// Sanitizes input
    pub(crate) fn make_task(&mut self, input: &str) -> EventId {
        self.make_task_with(input, self.position_tags(), true)
    }

    pub(crate) fn make_task_and_enter(&mut self, input: &str, state: State) {
        let id = self.make_task_with(input, self.position_tags(), false);
        self.set_state_for(id, "", state);
        self.move_to(Some(id));
    }

    /// Creates a task
    /// Sanitizes input
    pub(crate) fn make_task_with(&mut self, input: &str, tags: impl IntoIterator<Item=Tag>, set_state: bool) -> EventId {
        let id = self.submit(
            self.parse_task(input.trim())
                .add_tags(tags.into_iter())
        );
        if set_state {
            self.state.as_option().inspect(|s| self.set_state_for_with(id, s));
        }
        id
    }

    pub(crate) fn build_prop(
        &mut self,
        kind: Kind,
        comment: &str,
        id: EventId,
    ) -> EventBuilder {
        EventBuilder::new(
            kind,
            comment,
            vec![Tag::event(id)],
        )
    }

    pub(crate) fn get_task_title(&self, id: &EventId) -> String {
        self.tasks.get(id).map_or(id.to_string(), |t| t.get_title())
    }

    /// Parse string and set tracking
    /// Returns false if parsing failed
    pub(crate) fn track_from(&mut self, str: &str) -> bool {
        if let Ok(num) = str.parse::<i64>() {
            self.track_at(Timestamp::from(Timestamp::now().as_u64().saturating_add_signed(num * 60)));
        } else if let Ok(date) = DateTime::parse_from_rfc3339(str) {
            self.track_at(Timestamp::from(date.to_utc().timestamp() as u64));
        } else {
            warn!("Cannot parse time from  {str}");
            return false;
        }
        true
    }

    pub(crate) fn track_at(&mut self, time: Timestamp) -> EventId {
        info!("{} from {}", self.position.map_or(String::from("Stopping time-tracking"), |id| format!("Tracking \"{}\"", self.get_task_title(&id))), relative_datetimestamp(&time));
        let pos = self.get_position();
        let tracking = build_tracking(pos);
        // TODO this can lead to funny deletions
        self.get_own_history().map(|events| {
            if let Some(event) = events.pop_last() {
                if event.kind.as_u16() == TRACKING_KIND &&
                    (pos == None && event.tags.is_empty()) ||
                    event.tags.iter().all(|t| t.content().map(|str| str.to_string()) == pos.map(|id| id.to_string())) {
                    // Replace last for easier calculation
                } else {
                    events.insert(event);
                }
            }
        });
        self.submit(tracking.custom_created_at(time))
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
            _ => self.add_prop(&event),
        }
    }

    pub(crate) fn add_task(&mut self, event: Event) {
        if self.tasks.contains_key(&event.id) {
            debug!("Did not insert duplicate event {}", event.id); // TODO warn in next sdk version
        } else {
            let id = event.id;
            let task = Task::new(event);
            task.find_refs(MARKER_PARENT).for_each(|parent| {
                self.tasks.get_mut(parent).map(|t| { t.children.insert(id); });
            });
            self.tasks.insert(id, task);
        }
    }

    fn add_prop(&mut self, event: &Event) {
        self.referenced_tasks(&event, |t| {
            t.props.insert(event.clone());
        });
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
        if let Some(pos) = self.position {
            if pos == event.id {
                self.move_up()
            }
        }
        self.tasks.remove(&event.id);
        self.get_own_history().map(|t| t.remove(event));
        self.referenced_tasks(event, |t| { t.props.remove(event); });
    }

    pub(crate) fn set_state_for_with(&mut self, id: EventId, comment: &str) {
        self.set_state_for(id, comment, comment.into());
    }

    pub(crate) fn set_state_for(&mut self, id: EventId, comment: &str, state: State) -> EventId {
        let prop = self.build_prop(
            state.into(),
            comment,
            id,
        );
        info!("Task status {} set for \"{}\"", TaskState::get_label_for(&state, comment), self.get_task_title(&id));
        self.submit(prop)
    }

    pub(crate) fn update_state(&mut self, comment: &str, state: State) {
        self.position
            .map(|id| self.set_state_for(id, comment, state));
    }

    pub(crate) fn make_note(&mut self, note: &str) {
        match self.position {
            None => warn!("Cannot add note \"{}\" without active task", note),
            Some(id) => {
                let prop = self.build_prop(Kind::TextNote, note, id);
                self.submit(prop);
            }
        }
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
struct TimesTracked<'a> {
    events: Box<dyn Iterator<Item=&'a Event> + 'a>,
    ids: &'a Vec<&'a EventId>,
    threshold: Option<Timestamp>,
}
impl TimesTracked<'_> {
    fn from<'b>(events: impl IntoIterator<Item=&'b Event> + 'b, ids: &'b Vec<&EventId>) -> TimesTracked<'b> {
        TimesTracked {
            events: Box::new(events.into_iter()),
            ids,
            threshold: Some(Timestamp::now()),
        }
    }
}

impl Iterator for TimesTracked<'_> {
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
    queue: Vec<&'a EventId>,
    index: usize,
}
impl<'a> ChildIterator<'a> {
    fn from(tasks: &'a TaskMap, id: &'a EventId) -> Self {
        let mut queue = Vec::with_capacity(30);
        queue.push(id);
        ChildIterator {
            tasks,
            queue,
            index: 0,
        }
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
            // Unknown task, can still find children
            for task in self.tasks.values() {
                if task.parent_id().is_some_and(|i| i == id) {
                    self.queue.push(task.get_id());
                }
            }
        }
        self.index += 1;
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
        use std::sync::mpsc;
        use nostr_sdk::Keys;

        let (tx, _rx) = mpsc::channel();
        Tasks::with_sender(EventSender {
            url: None,
            tx,
            keys: Keys::generate(),
            queue: Default::default(),
        })
    }

    #[test]
    fn test_procedures() {
        let mut tasks = stub_tasks();
        tasks.make_task_and_enter("proc: tags", State::Procedure);
        let side = tasks.submit(build_task("side", vec![tasks.make_event_tag(&tasks.get_current_task().unwrap().event, MARKER_DEPENDS)]));
        assert_eq!(tasks.get_current_task().unwrap().children, HashSet::<EventId>::new());
        let sub_id = tasks.make_task("sub");
        assert_eq!(tasks.get_current_task().unwrap().children, HashSet::from([sub_id]));
        assert_eq!(tasks.len(), 3);
        let sub = tasks.get_by_id(&sub_id).unwrap();
        assert_eq!(sub.get_dependendees(), Vec::<&EventId>::new());
    }

    #[test]
    fn test_tracking() {
        let mut tasks = stub_tasks();
        let zero = EventId::all_zeros();

        tasks.track_at(Timestamp::from(0));
        assert_eq!(tasks.history.len(), 1);

        tasks.move_to(Some(zero));
        let now: Timestamp = Timestamp::now() - 2u64;
        tasks.track_at(Timestamp::from(1));
        assert!(tasks.time_tracked(zero) > now.as_u64());

        tasks.move_to(None);
        tasks.track_at(Timestamp::from(2));
        assert_eq!(tasks.get_own_history().unwrap().len(), 3);
        assert_eq!(tasks.time_tracked(zero), 1);

        // TODO test received events
    }

    #[test]
    #[ignore]
    fn test_timestamps() {
        let mut tasks = stub_tasks();
        let zero = EventId::all_zeros();

        tasks.move_to(Some(zero));
        tasks.track_at(Timestamp::from(Timestamp::now().as_u64() + 100));
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
        tasks.depth = 2;
        assert_eq!(tasks.visible_tasks().len(), 0);
        let t2 = tasks.make_task("t2");
        assert_eq!(tasks.visible_tasks().len(), 1);
        assert_eq!(tasks.get_task_path(Some(t2)), "t1>t2");
        assert_eq!(tasks.relative_path(t2), "t2");
        let t3 = tasks.make_task("t3");
        assert_eq!(tasks.visible_tasks().len(), 2);

        tasks.move_to(Some(t2));
        assert_eq!(tasks.visible_tasks().len(), 0);
        let t4 = tasks.make_task("t4");
        assert_eq!(tasks.visible_tasks().len(), 1);
        assert_eq!(tasks.get_task_path(Some(t4)), "t1>t2>t4");
        assert_eq!(tasks.relative_path(t4), "t4");
        tasks.depth = 2;
        assert_eq!(tasks.visible_tasks().len(), 1);
        tasks.depth = -1;
        assert_eq!(tasks.visible_tasks().len(), 1);

        tasks.move_to(Some(t1));
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

    #[allow(dead_code)]
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