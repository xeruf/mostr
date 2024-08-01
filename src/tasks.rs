use std::collections::{BTreeSet, HashMap};
use std::io::{Error, stdout, Write};
use std::iter::once;
use std::ops::{Div, Rem};

use chrono::{Local, TimeZone};
use chrono::LocalResult::Single;
use colored::Colorize;
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use nostr_sdk::{Event, EventBuilder, EventId, Kind, PublicKey, Tag, Timestamp};
use nostr_sdk::Tag::Hashtag;

use crate::{EventSender, TASK_KIND, TRACKING_KIND};
use crate::task::{State, Task};

type TaskMap = HashMap<EventId, Task>;
#[derive(Debug, Clone)]
pub(crate) struct Tasks {
    /// The Tasks
    tasks: TaskMap,
    /// History of active tasks by PubKey
    history: HashMap<PublicKey, BTreeSet<Event>>,
    /// The task properties currently visible
    pub(crate) properties: Vec<String>,
    /// Negative: Only Leaf nodes
    /// Zero: Only Active node
    /// Positive: Go down the respective level
    pub(crate) depth: i8,

    /// Currently active task
    position: Option<EventId>,
    /// Currently active tags
    tags: BTreeSet<Tag>,
    /// Current active state
    state: Option<String>,
    /// A filtered view of the current tasks
    view: Vec<EventId>,

    sender: EventSender,
}

impl Tasks {
    pub(crate) fn from(sender: EventSender) -> Self {
        Tasks {
            tasks: Default::default(),
            history: Default::default(),
            properties: vec![
                "state".into(),
                "progress".into(),
                "rtime".into(),
                "hashtags".into(),
                "rpath".into(),
                "desc".into(),
            ],
            position: None,
            view: Default::default(),
            tags: Default::default(),
            state: None,
            depth: 1,
            sender,
        }
    }
}

impl Tasks {
    // Accessors

    #[inline]
    pub(crate) fn get_by_id(&self, id: &EventId) -> Option<&Task> { self.tasks.get(id) }

    #[inline]
    pub(crate) fn get_position(&self) -> Option<EventId> { self.position }

    #[inline]
    pub(crate) fn len(&self) -> usize { self.tasks.len() }

    /// Ids of all subtasks found for id, including itself
    fn get_subtasks(&self, id: EventId) -> Vec<EventId> {
        let mut children = Vec::with_capacity(32);
        let mut index = 0;

        children.push(id);
        while index < children.len() {
            self.tasks.get(&children[index]).map(|t| {
                children.reserve(t.children.len());
                for child in t.children.iter() {
                    children.push(child.clone());
                }
            });
            index += 1;
        }

        children
    }

    /// Total time tracked on this task by the current user.
    pub(crate) fn time_tracked(&self, id: &EventId) -> u64 {
        let mut total = 0;
        let mut start: Option<Timestamp> = None;
        for event in self.history.get(&self.sender.pubkey()).into_iter().flatten() {
            match event.tags.first() {
                Some(Tag::Event {
                         event_id,
                         ..
                     }) if event_id == id => {
                    start = start.or(Some(event.created_at))
                }
                _ => if let Some(stamp) = start {
                    total += (event.created_at - stamp).as_u64();
                }
            }
        }
        if let Some(start) = start {
            total += (Timestamp::now() - start).as_u64();
        }
        total
    }

    /// Total time tracked on this task and its subtasks by all users.
    /// TODO needs testing!
    fn total_time_tracked(&self, id: EventId) -> u64 {
        let mut total = 0;

        let children = self.get_subtasks(id);
        for user in self.history.values() {
            let mut start: Option<Timestamp> = None;
            for event in user {
                match event.tags.first() {
                    Some(Tag::Event {
                             event_id,
                             ..
                         }) if children.contains(event_id) => {
                        start = start.or(Some(event.created_at))
                    }
                    _ => if let Some(stamp) = start {
                        total += (event.created_at - stamp).as_u64();
                    }
                }
            }
            if let Some(start) = start {
                total += (Timestamp::now() - start).as_u64();
            }
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
        self.tags
            .iter()
            .map(|t| format!(" #{}", t.content().unwrap()))
            .chain(self.state.as_ref().map(|s| format!(" ?{s}")).into_iter())
            .collect::<Vec<String>>()
            .join("")
    }

    pub(crate) fn get_task_path(&self, id: Option<EventId>) -> String {
        join_tasks(self.traverse_up_from(id), true)
            .filter(|s| !s.is_empty())
            .or_else(|| id.map(|id| id.to_string()))
            .unwrap_or(String::new())
    }

    pub(crate) fn traverse_up_from(&self, id: Option<EventId>) -> ParentIterator {
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
            if let Tag::Event { event_id, .. } = tag {
                self.tasks.get_mut(event_id).map(|t| f(t));
            }
        }
    }

    #[inline]
    fn current_task(&self) -> Option<&Task> {
        self.position.and_then(|id| self.get_by_id(&id))
    }

    pub(crate) fn current_tasks(&self) -> Vec<&Task> {
        if self.depth == 0 {
            return self.current_task().into_iter().collect();
        }
        let res: Vec<&Task> = self.resolve_tasks(self.view.iter());
        if res.len() > 0 {
            // Currently ignores filter when it matches nothing
            return res;
        }
        self.resolve_tasks(
            self.tasks
                .values()
                .filter(|t| t.parent_id() == self.position.as_ref())
                .map(|t| t.get_id()),
        ).into_iter()
            .filter(|t| {
                let state = t.pure_state();
                self.state.as_ref().map_or_else(|| {
                    state == State::Open || (
                        state == State::Done &&
                            t.parent_id() != None
                    )
                }, |filter| {
                    t.state().is_some_and(|t| t.matches_label(filter))
                }) && (self.tags.is_empty()
                    || t.tags.as_ref().map_or(false, |tags| {
                    let mut iter = tags.iter();
                    self.tags.iter().all(|tag| iter.any(|t| t == tag))
                }))
            })
            .collect()
    }

    pub(crate) fn print_tasks(&self) -> Result<(), Error> {
        let mut lock = stdout().lock();
        if let Some(t) = self.current_task() {
            let state = t.state_or_default();
            writeln!(
                lock,
                "{} since {} (total tracked time {}m)",
                state.get_label(),
                match Local.timestamp_opt(state.time.as_i64(), 0) {
                    Single(time) => {
                        let date = time.date_naive();
                        let prefix = match Local::now()
                            .date_naive()
                            .signed_duration_since(date)
                            .num_days()
                        {
                            0 => "".into(),
                            1 => "yesterday ".into(),
                            2..=6 => date.format("%a ").to_string(),
                            _ => date.format("%y-%m-%d ").to_string(),
                        };
                        format!("{}{}", prefix, time.format("%H:%M"))
                    }
                    _ => state.time.to_human_datetime(),
                },
                self.time_tracked(t.get_id()) / 60
            )?;
            writeln!(lock, "{}", t.descriptions().join("\n"))?;
        }
        // TODO proper columns
        writeln!(lock, "{}", self.properties.join("\t").bold())?;
        for task in self.current_tasks() {
            writeln!(
                lock,
                "{}",
                self.properties
                    .iter()
                    .map(|p| match p.as_str() {
                        "subtasks" => {
                            let mut total = 0;
                            let mut done = 0;
                            for subtask in task.children.iter().filter_map(|id| self.get_by_id(id))
                            {
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
                        "progress" => self
                            .total_progress(task.get_id())
                            .filter(|_| task.children.len() > 0)
                            .map_or(String::new(), |p| format!("{:2.0}%", p * 100.0)),
                        "path" => self.get_task_path(Some(task.event.id)),
                        "rpath" => self.relative_path(task.event.id),
                        "time" => display_time("MMMm", self.time_tracked(task.get_id())),
                        "rtime" => display_time("HH:MMm", self.total_time_tracked(*task.get_id())),
                        prop => task.get(prop).unwrap_or(String::new()),
                    })
                    .collect::<Vec<String>>()
                    .join(" \t")
            )?;
        }
        writeln!(lock)?;
        Ok(())
    }

    // Movement and Selection

    pub(crate) fn set_filter(&mut self, view: Vec<EventId>) {
        self.view = view;
    }

    pub(crate) fn add_tag(&mut self, tag: String) {
        self.view.clear();
        self.tags.insert(Hashtag(tag));
    }

    pub(crate) fn remove_tag(&mut self, tag: String) {
        self.view.clear();
        self.tags.retain(|t| !t.content().is_some_and(|value| value.to_string().starts_with(&tag)));
    }

    pub(crate) fn set_state_filter(&mut self, state: Option<String>) {
        self.view.clear();
        self.state = state;
    }

    pub(crate) fn move_up(&mut self) {
        self.move_to(self.current_task().and_then(|t| t.parent_id()).cloned());
    }

    pub(crate) fn flush(&self) {
        self.sender.flush();
    }

    pub(crate) fn move_to(&mut self, id: Option<EventId>) {
        self.view.clear();
        if id == self.position {
            debug!("Flushing Tasks because of move in place");
            self.flush();
            return;
        }
        self.submit(
            EventBuilder::new(
                Kind::from(TRACKING_KIND),
                "",
                id.iter().map(|id| Tag::event(id.clone())),
            )
        );
        if !id.and_then(|id| self.tasks.get(&id)).is_some_and(|t| t.parent_id() == self.position.as_ref()) {
            debug!("Flushing Tasks because of move");
            self.flush();
        }
        self.position = id;
    }

    // Updates

    /// Expects sanitized input
    pub(crate) fn build_task(&self, input: &str) -> EventBuilder {
        let mut tags: Vec<Tag> = self.tags.iter().cloned().collect();
        self.position.inspect(|p| tags.push(Tag::event(*p)));
        return match input.split_once(": ") {
            None => EventBuilder::new(Kind::from(TASK_KIND), input, tags),
            Some(s) => {
                tags.append(
                    &mut s
                        .1
                        .split_ascii_whitespace()
                        .map(|t| Hashtag(t.to_string()))
                        .collect(),
                );
                EventBuilder::new(Kind::from(TASK_KIND), s.0, tags)
            }
        };
    }

    /// Sanitizes input
    pub(crate) fn make_task(&mut self, input: &str) -> EventId {
        self.submit(self.build_task(input.trim()))
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

    fn submit(&mut self, builder: EventBuilder) -> EventId {
        let event = self.sender.submit(builder).unwrap();
        let id = event.id;
        self.add(event);
        id
    }

    pub(crate) fn add(&mut self, event: Event) {
        match event.kind.as_u64() {
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
        self.referenced_tasks(&event, |t| {
            t.children.insert(event.id);
        });
        if self.tasks.contains_key(&event.id) {
            debug!("Did not insert duplicate event {}", event.id);
        } else {
            self.tasks.insert(event.id, Task::new(event));
        }
    }

    fn add_prop(&mut self, event: &Event) {
        self.referenced_tasks(&event, |t| {
            t.props.insert(event.clone());
        });
    }

    pub(crate) fn undo(&mut self) {
        self.sender.clear().into_iter().rev().for_each(|event| {
            self.remove(&event)
        });
    }

    fn remove(&mut self, event: &Event) {
        if let Some(pos) = self.position {
            if pos == event.id {
                self.move_up()
            }
        }
        self.tasks.remove(&event.id);
        self.history.get_mut(&self.sender.pubkey()).map(|t| t.remove(event));
        self.referenced_tasks(event, |t| { t.props.remove(event); });
    }

    pub(crate) fn set_state_for(&mut self, id: EventId, comment: &str, state: State) -> EventId {
        let prop = self.build_prop(
            state.into(),
            comment,
            id,
        );
        self.submit(prop)
    }

    pub(crate) fn update_state(&mut self, comment: &str, state: State)
    {
        self.position
            .map(|id| self.set_state_for(id, comment, state));
    }

    pub(crate) fn add_note(&mut self, note: &str) {
        match self.position {
            None => warn!("Cannot add note '{}' without active task", note),
            Some(id) => {
                let prop = self.build_prop(Kind::TextNote, note, id);
                self.submit(prop);
            }
        }
    }
}

fn display_time(format: &str, secs: u64) -> String {
    Some(secs / 60)
        .filter(|t| t > &0)
        .map_or(String::new(), |mins| format
            .replace("HH", &format!("{:02}", mins.div(60)))
            .replace("MM", &format!("{:02}", mins.rem(60)))
            .replace("MMM", &format!("{:3}", mins)),
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

#[test]
fn test_depth() {
    use std::sync::mpsc;
    use nostr_sdk::Keys;

    let (tx, _rx) = mpsc::channel();
    let mut tasks = Tasks::from(EventSender {
        tx,
        keys: Keys::generate(),
        queue: Default::default(),
    });

    let t1 = tasks.make_task("t1");
    let task1 = tasks.get_by_id(&t1).unwrap();
    assert_eq!(tasks.depth, 1);
    assert_eq!(task1.pure_state(), State::Open);
    debug!("{:?}", tasks);
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = 0;
    assert_eq!(tasks.current_tasks().len(), 0);

    tasks.move_to(Some(t1));
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 0);
    let t2 = tasks.make_task("t2");
    assert_eq!(tasks.current_tasks().len(), 1);
    assert_eq!(tasks.get_task_path(Some(t2)), "t1>t2");
    assert_eq!(tasks.relative_path(t2), "t2");
    let t3 = tasks.make_task("t3");
    assert_eq!(tasks.current_tasks().len(), 2);

    tasks.move_to(Some(t2));
    assert_eq!(tasks.current_tasks().len(), 0);
    let t4 = tasks.make_task("t4");
    assert_eq!(tasks.current_tasks().len(), 1);
    assert_eq!(tasks.get_task_path(Some(t4)), "t1>t2>t4");
    assert_eq!(tasks.relative_path(t4), "t4");
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = -1;
    assert_eq!(tasks.current_tasks().len(), 1);

    tasks.move_to(Some(t1));
    assert_eq!(tasks.relative_path(t4), "t2>t4");
    assert_eq!(tasks.current_tasks().len(), 2);
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 3);
    tasks.set_filter(vec![t2]);
    assert_eq!(tasks.current_tasks().len(), 2);
    tasks.depth = 1;
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = -1;
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.set_filter(vec![t2, t3]);
    assert_eq!(tasks.current_tasks().len(), 2);
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 3);
    tasks.depth = 1;
    assert_eq!(tasks.current_tasks().len(), 2);

    tasks.move_to(None);
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 3);
    tasks.depth = 3;
    assert_eq!(tasks.current_tasks().len(), 4);
    tasks.depth = 9;
    assert_eq!(tasks.current_tasks().len(), 4);
    tasks.depth = -1;
    assert_eq!(tasks.current_tasks().len(), 2);

    let empty = tasks.make_task("");
    let empty_task = tasks.get_by_id(&empty).unwrap();
    let empty_id = empty_task.event.id.to_string();
    assert_eq!(empty_task.get_title(), empty_id);
    assert_eq!(tasks.get_task_path(Some(empty)), empty_id);

    let zero = EventId::all_zeros();
    assert_eq!(tasks.get_task_path(Some(zero)), zero.to_string());
    tasks.move_to(Some(zero));
    let dangling = tasks.make_task("test");
    assert_eq!(
        tasks.get_task_path(Some(dangling)),
        "0000000000000000000000000000000000000000000000000000000000000000>test"
    );
    assert_eq!(tasks.relative_path(dangling), "test");

    use itertools::Itertools;
    assert_eq!("test  toast".split(' ').collect_vec().len(), 3);
    assert_eq!(
        "test  toast".split_ascii_whitespace().collect_vec().len(),
        2
    );
}
