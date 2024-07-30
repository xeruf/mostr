use std::collections::{BTreeSet, HashMap};
use std::io::{Error, stdout, Write};
use std::iter::once;

use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use nostr_sdk::{Event, EventBuilder, EventId, Keys, Kind, Tag};
use nostr_sdk::Tag::Hashtag;

use crate::{EventSender, TASK_KIND};
use crate::task::{State, Task};

type TaskMap = HashMap<EventId, Task>;
#[derive(Debug, Clone)]
pub(crate) struct Tasks {
    /// The Tasks
    tasks: TaskMap,
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
            properties: vec![
                "state".into(),
                "rtime".into(),
                "rpath".into(),
                "tags".into(),
                "desc".into(),
            ],
            position: None,
            view: Default::default(),
            tags: Default::default(),
            state: Some(State::Open.to_string()),
            depth: 1,
            sender,
        }
    }
}

impl Tasks {
    // Accessors

    pub(crate) fn get_by_id(&self, id: &EventId) -> Option<&Task> {
        self.tasks.get(id)
    }

    pub(crate) fn get_position(&self) -> Option<EventId> {
        self.position
    }

    /// Total time this task and its subtasks have been active
    fn total_time_tracked(&self, task: &EventId) -> u64 {
        self.tasks.get(task).map_or(0, |t| {
            t.time_tracked()
                + t.children
                    .iter()
                    .map(|e| self.total_time_tracked(e))
                    .sum::<u64>()
        })
    }

    // Parents

    pub(crate) fn get_parent(&self, id: Option<EventId>) -> Option<EventId> {
        id.and_then(|id| self.tasks.get(&id))
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

    fn resolve_tasks<'a>(&self, iter: impl IntoIterator<Item = &'a EventId>) -> Vec<&Task> {
        self.resolve_tasks_rec(iter, self.depth)
    }

    fn resolve_tasks_rec<'a>(
        &self,
        iter: impl IntoIterator<Item = &'a EventId>,
        depth: i8,
    ) -> Vec<&Task> {
        iter.into_iter()
            .filter_map(|id| self.tasks.get(&id))
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

    fn current_task(&self) -> Option<&Task> {
        self.position.and_then(|id| self.tasks.get(&id))
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
                .filter(|t| t.parent_id() == self.position)
                .map(|t| t.get_id()),
        )
        .into_iter()
        .filter(|t| {
            self.state.as_ref().map_or(true, |state| {
                t.state().is_some_and(|t| t.matches_label(state))
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
            if let Some(state) = t.state() {
                writeln!(
                    lock,
                    "{} since {} (total time {}m)",
                    state.get_label(),
                    state.time.to_human_datetime(),
                    t.time_tracked() / 60
                )?;
            }
            writeln!(lock, "{}", t.descriptions().join("\n"))?;
        }
        writeln!(lock, "{}", self.properties.join("\t"))?; // TODO proper columns
        for task in self.current_tasks() {
            writeln!(
                lock,
                "{}",
                self.properties
                    .iter()
                    .map(|p| match p.as_str() {
                        "path" => self.get_task_path(Some(task.event.id)),
                        "rpath" => self.relative_path(task.event.id),
                        "rtime" => {
                            let time = self.total_time_tracked(&task.event.id);
                            format!("{:02}:{:02}", time / 3600, time / 60 % 60)
                        }
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

    pub(crate) fn set_state_filter(&mut self, state: Option<String>) {
        self.view.clear();
        self.state = state;
    }

    pub(crate) fn move_up(&mut self) {
        self.move_to(self.current_task().and_then(|t| t.parent_id()))
    }

    pub(crate) fn move_to(&mut self, id: Option<EventId>) {
        self.view.clear();
        self.tags.clear();
        if id == self.position {
            return;
        }
        // TODO: erases previous state comment - do not track active via state
        self.update_state("", |s| {
            if s.pure_state() == State::Active {
                Some(State::Open)
            } else {
                None
            }
        });
        self.position = id;
        self.update_state("", |s| {
            if s.pure_state() == State::Open {
                Some(State::Active)
            } else {
                None
            }
        });
    }

    // Updates

    /// Expects sanitized input
    pub(crate) fn build_task(&self, input: &str) -> EventBuilder {
        let mut tags: Vec<Tag> = self.tags.iter().cloned().collect();
        self.position.inspect(|p| tags.push(Tag::event(*p)));
        return match input.split_once(": ") {
            None => EventBuilder::new(Kind::from(TASK_KIND), input, tags),
            Some(s) => {
                tags.append(&mut s.1.split_ascii_whitespace().map(|t| Hashtag(t.to_string())).collect());
                EventBuilder::new(Kind::from(TASK_KIND), s.0, tags)
            }
        };
    }

    /// Sanitizes input
    pub(crate) fn make_task(&mut self, input: &str) -> Option<EventId> {
        self.sender.submit(self.build_task(input.trim())).map(|e| {
            let id = e.id;
            self.add_task(e);
            let state = self.state.clone().unwrap_or("Open".to_string());
            self.set_state_for(&id, &state);
            id
        })
    }

    pub(crate) fn add(&mut self, event: Event) {
        if event.kind.as_u64() == 1621 {
            self.add_task(event)
        } else {
            self.add_prop(&event)
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

    pub(crate) fn add_prop(&mut self, event: &Event) {
        self.referenced_tasks(&event, |t| {
            t.props.insert(event.clone());
        });
    }

    pub(crate) fn set_state_for(&mut self, id: &EventId, comment: &str) -> Option<Event> {
        let t = self.tasks.get_mut(id);
        t.and_then(|task| {
            task.set_state(
                &self.sender,
                match comment {
                    "Closed" => State::Closed,
                    "Done" => State::Done,
                    _ => State::Open,
                },
                comment,
            )
        })
    }

    pub(crate) fn update_state_for<F>(&mut self, id: &EventId, comment: &str, f: F) -> Option<Event>
    where
        F: FnOnce(&Task) -> Option<State>,
    {
        self.tasks
            .get_mut(id)
            .and_then(|task| f(task).and_then(|state| task.set_state(&self.sender, state, comment)))
    }

    pub(crate) fn update_state<F>(&mut self, comment: &str, f: F) -> Option<Event>
    where
        F: FnOnce(&Task) -> Option<State>,
    {
        self.position
            .and_then(|id| self.update_state_for(&id, comment, f))
    }

    pub(crate) fn add_note(&mut self, note: &str) {
        match self.position {
            None => warn!("Cannot add note '{}' without active task", note),
            Some(id) => {
                self.sender
                    .submit(EventBuilder::text_note(note, vec![]))
                    .map(|e| {
                        self.tasks.get_mut(&id).map(|t| {
                            t.props.insert(e.clone());
                        });
                    });
            }
        }
    }
}

pub(crate) fn join_tasks<'a>(iter: impl Iterator<Item = &'a Task>, include_last_id: bool) -> Option<String> {
    let tasks: Vec<&Task> = iter.collect();
    tasks
        .iter()
        .map(|t| t.get_title())
        .chain(
            if include_last_id {
                tasks
                    .last()
                    .and_then(|t| t.parent_id())
                    .map(|id| id.to_string())
                    .into_iter()
            } else {
                None.into_iter()
            }
        )
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
            self.current = t.parent_id();
            t
        })
    }
}

#[test]
fn test_depth() {
    use std::sync::mpsc;

    let (tx, _rx) = mpsc::channel();
    let mut tasks = Tasks::from(EventSender {
        tx,
        keys: Keys::generate(),
    });

    let t1 = tasks.make_task("t1");
    let task1 = tasks.get_by_id(&t1.unwrap()).unwrap();
    assert_eq!(tasks.depth, 1);
    assert_eq!(task1.state().unwrap().get_label(), "Open");
    debug!("{:?}", tasks);
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = 0;
    assert_eq!(tasks.current_tasks().len(), 0);

    tasks.move_to(t1);
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 0);
    let t2 = tasks.make_task("t2");
    assert_eq!(tasks.current_tasks().len(), 1);
    assert_eq!(tasks.get_task_path(t2), "t1>t2");
    assert_eq!(tasks.relative_path(t2.unwrap()), "t2");
    let t3 = tasks.make_task("t3");
    assert_eq!(tasks.current_tasks().len(), 2);

    tasks.move_to(t2);
    assert_eq!(tasks.current_tasks().len(), 0);
    let t4 = tasks.make_task("t4");
    assert_eq!(tasks.current_tasks().len(), 1);
    assert_eq!(tasks.get_task_path(t4), "t1>t2>t4");
    assert_eq!(tasks.relative_path(t4.unwrap()), "t4");
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = -1;
    assert_eq!(tasks.current_tasks().len(), 1);

    tasks.move_to(t1);
    assert_eq!(tasks.relative_path(t4.unwrap()), "t2>t4");
    assert_eq!(tasks.current_tasks().len(), 2);
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 3);
    tasks.set_filter(vec![t2.unwrap()]);
    assert_eq!(tasks.current_tasks().len(), 2);
    tasks.depth = 1;
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = -1;
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.set_filter(vec![t2.unwrap(), t3.unwrap()]);
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
    let empty_task = tasks.get_by_id(&empty.unwrap()).unwrap();
    let empty_id = empty_task.event.id.to_string();
    assert_eq!(empty_task.get_title(), empty_id);
    assert_eq!(tasks.get_task_path(empty), empty_id);

    let zero = EventId::all_zeros();
    assert_eq!(tasks.get_task_path(Some(zero)), zero.to_string());
    tasks.move_to(Some(zero));
    let dangling = tasks.make_task("test");
    assert_eq!(tasks.get_task_path(dangling), "0000000000000000000000000000000000000000000000000000000000000000>test");
    assert_eq!(tasks.relative_path(dangling.unwrap()), "test");

    use itertools::Itertools;
    assert_eq!("test  toast".split(' ').collect_vec().len(), 3);
    assert_eq!("test  toast".split_ascii_whitespace().collect_vec().len(), 2);
}
