use std::collections::HashMap;

use nostr_sdk::{Event, EventId, Tag};

use crate::make_task;
use crate::task::{State, Task};

type TaskMap = HashMap<EventId, Task>;
pub(crate) struct Tasks {
    /// The Tasks
    pub(crate) tasks: TaskMap,
    /// The task properties currently visible
    pub(crate) properties: Vec<String>,
    /// The task currently selected.
    position: Option<EventId>,
    /// A filtered view of the current tasks
    view: Vec<EventId>,
}

impl Default for Tasks {
    fn default() -> Self {
        Tasks {
            tasks: Default::default(),
            properties: vec!["id".into(), "name".into(), "state".into()],
            position: None,
            view: Default::default(),
        }
    }
}

impl Tasks {
    pub(crate) fn get_position(&self) -> Option<EventId> {
        self.position
    }

    fn collect_tasks(&self, tasks: &Vec<EventId>) -> Vec<&Task> {
        tasks.iter().filter_map(|id| self.tasks.get(id)).collect()
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

    pub(crate) fn set_filter(&mut self, view: Vec<EventId>) {
        self.view = view
    }

    pub(crate) fn current_tasks(&self) -> Vec<&Task> {
        let res = self.collect_tasks(&self.view);
        if res.len() > 0 {
            return res;
        }
        self.position.map_or_else(
            || self.tasks.values().collect(),
            |p| {
                self.tasks
                    .get(&p)
                    .map_or(Vec::new(), |t| self.collect_tasks(&t.children))
            },
        )
    }

    pub(crate) fn print_current_tasks(&self) {
        println!("{}", self.properties.join(" "));
        for task in self.current_tasks() {
            println!(
                "{}",
                self.properties
                    .iter()
                    .map(|p| match p.as_str() {
                        "path" => self.taskpath(Some(task.event.id)),
                        "ttime" => self.total_time_tracked(&task.event.id).to_string(),
                        prop => task.get(prop).unwrap_or(String::new()),
                    })
                    .collect::<Vec<String>>()
                    .join(" ")
            );
        }
        println!();
    }

    pub(crate) fn make_task(&self, input: &str) -> Event {
        let mut tags: Vec<Tag> = Vec::new();
        self.position.inspect(|p| tags.push(Tag::event(*p)));
        return match input.split_once(": ") {
            None => make_task(&input, &tags),
            Some(s) => {
                tags.append(
                    &mut s
                        .1
                        .split(" ")
                        .map(|t| Tag::Hashtag(t.to_string()))
                        .collect(),
                );
                make_task(s.0, &tags)
            }
        };
    }

    pub(crate) fn referenced_tasks<F: Fn(&mut Task)>(&mut self, event: &Event, f: F) {
        for tag in event.tags.iter() {
            if let Tag::Event { event_id, .. } = tag {
                self.tasks.get_mut(event_id).map(|t| f(t));
            }
        }
    }

    pub(crate) fn add(&mut self, event: Event) {
        if event.kind.as_u64() == 1621 {
            self.add_task(event)
        } else {
            self.add_prop(&event)
        }
    }

    pub(crate) fn add_task(&mut self, event: Event) {
        self.referenced_tasks(&event, |t| t.children.push(event.id));
        self.tasks.insert(event.id, Task::new(event));
    }
    
    pub(crate) fn add_prop(&mut self, event: &Event) {
        self.referenced_tasks(&event, |t| t.props.push(event.clone()));
    }

    pub(crate) fn move_up(&mut self) {
        self.move_to(
            self.position
                .and_then(|id| self.tasks.get(&id))
                .and_then(|t| t.parent_id()),
        )
    }

    pub(crate) fn move_to(&mut self, id: Option<EventId>) {
        self.view.clear();
        if id == self.position {
            return;
        }
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

    pub(crate) fn parent(&self, id: Option<EventId>) -> Option<EventId> {
        id.and_then(|id| self.tasks.get(&id))
            .and_then(|t| t.parent_id())
    }

    pub(crate) fn taskpath(&self, id: Option<EventId>) -> String {
        self.traverse_up_from(id)
            .map(|t| t.event.content.clone())
            .fold(String::new(), |acc, val| format!("{} {}", val, acc))
    }

    pub(crate) fn traverse_up_from(&self, id: Option<EventId>) -> ParentIterator {
        ParentIterator {
            tasks: &self.tasks,
            current: id,
            prev: None,
        }
    }

    pub(crate) fn update_state_for<F>(&mut self, id: &EventId, comment: &str, f: F)
    where
        F: FnOnce(&Task) -> Option<State>,
    {
        self.tasks.get_mut(id).map(|t| {
            f(t).map(|s| {
                t.update_state(s, comment);
            })
        });
    }

    pub(crate) fn update_state<F>(&mut self, comment: &str, f: F)
    where
        F: FnOnce(&Task) -> Option<State>,
    {
        self.position.inspect(|id| {
            self.update_state_for(id, comment, f);
        });
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
            self.current = t.parent_id();
            t
        })
    }
}
