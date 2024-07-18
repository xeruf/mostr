use crate::{make_event, make_task};
use crate::task::{Task, State};
use nostr_sdk::{Event, EventId, Tag};
use std::collections::HashMap;

type TaskMap = HashMap<EventId, Task>;
pub(crate) struct Tasks {
    pub(crate) tasks: TaskMap,
    pub(crate) properties: Vec<String>,
    position: Option<EventId>,
}

impl Default for Tasks {
    fn default() -> Self {
        Tasks {
            tasks: Default::default(),
            properties: vec!["id".into(), "name".into(), "state".into()],
            position: None,
        }
    }
}

impl Tasks {
    pub(crate) fn get_position(&self) -> Option<EventId> {
        self.position
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

    pub(crate) fn add_task(&mut self, event: Event) {
        for tag in event.tags.iter() {
            match tag {
                Tag::Event { event_id, .. } => {
                    self.tasks
                        .get_mut(event_id)
                        .map(|t| t.children.push(event.id));
                }
                _ => {}
            }
        }
        self.tasks.insert(event.id, Task::new(event));
    }

    pub(crate) fn current_tasks(&self) -> Vec<&Task> {
        self.position.map_or(self.tasks.values().collect(), |p| {
            self.tasks.get(&p).map_or(Vec::new(), |t| {
                t.children
                    .iter()
                    .filter_map(|id| self.tasks.get(id))
                    .collect()
            })
        })
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
                        prop => task.get(prop).unwrap_or(String::new()),
                    })
                    .collect::<Vec<String>>()
                    .join(" ")
            );
        }
        println!();
    }

    pub(crate) fn move_up(&mut self) {
        self.move_to(
            self.position
                .and_then(|id| self.tasks.get(&id))
                .and_then(|t| t.parent_id()),
        )
    }

    pub(crate) fn move_to(&mut self, id: Option<EventId>) {
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
}
impl<'a> Iterator for ParentIterator<'a> {
    type Item = &'a Task;

    fn next(&mut self) -> Option<Self::Item> {
        self.current.and_then(|id| self.tasks.get(&id)).map(|t| {
            self.current = t.parent_id();
            t
        })
    }
}
