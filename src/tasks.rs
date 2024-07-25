use std::collections::HashMap;
use std::iter::once;
use std::sync::mpsc;

use nostr_sdk::{Event, EventBuilder, EventId, Keys, Kind, Tag};

use crate::{EventSender, TASK_KIND};
use crate::task::{State, Task};

type TaskMap = HashMap<EventId, Task>;
pub(crate) struct Tasks {
    /// The Tasks
    tasks: TaskMap,
    /// The task properties currently visible
    pub(crate) properties: Vec<String>,
    /// Negative: Only Leaf nodes
    /// Zero: Only Active node
    /// Positive: Go down the respective level
    pub(crate) depth: i8,

    /// The task currently selected.
    position: Option<EventId>,
    /// A filtered view of the current tasks
    view: Vec<EventId>,

    sender: EventSender
}

impl Tasks {
    pub(crate) fn from(sender: EventSender) -> Self {
        Tasks {
            tasks: Default::default(),
            properties: vec!["id".into(), "name".into(), "state".into(), "ttime".into()],
            position: None,
            view: Default::default(),
            depth: 1,
            sender
        }
    }
}

impl Tasks {
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

    pub(crate) fn set_filter(&mut self, view: Vec<EventId>) {
        self.view = view
    }
    
    fn resolve_tasks<'a>(&self, iter: impl IntoIterator<Item=&'a EventId>) -> Vec<&Task> {
        self.resolve_tasks_rec(iter, self.depth)
    }

    fn resolve_tasks_rec<'a>(&self, iter: impl IntoIterator<Item=&'a EventId>, depth: i8) -> Vec<&Task> {
        iter.into_iter().filter_map(|id| self.tasks.get(&id)).flat_map(|task| {
            let new_depth = depth - 1;
            if new_depth < 0 {
                let tasks = self.resolve_tasks_rec(task.children.iter(), new_depth).into_iter().collect::<Vec<&Task>>();
                if tasks.is_empty() {
                    vec![task]
                } else {
                    tasks
                }
            } else if new_depth > 0 {
                self.resolve_tasks_rec(task.children.iter(), new_depth).into_iter().chain(once(task)).collect()
            } else {
                vec![task]
            }
        }).collect()
    }

    pub(crate) fn current_tasks(&self) -> Vec<&Task> {
        if self.depth == 0 {
            return self.position.and_then(|id| self.tasks.get(&id)).into_iter().collect();
        }
        let res: Vec<&Task> = self.resolve_tasks(self.view.iter());
        if res.len() > 0 {
            return res;
        }
        self.position.map_or_else(
            || {
                if self.depth > 8 {
                    self.tasks.values().collect()
                } else if self.depth == 1 {
                    self.tasks.values().filter(|t| t.parent_id() == None).collect()
                } else {
                    self.resolve_tasks(self.tasks.values().filter(|t| t.parent_id() == None).map(|t| &t.event.id))
                }
            },
            |p| self.tasks.get(&p).map_or(Vec::new(), |t| self.resolve_tasks(t.children.iter())),
        )
    }

    pub(crate) fn print_current_tasks(&self) {
        println!("{}", self.properties.join("\t"));
        for task in self.current_tasks() {
            println!(
                "{}",
                self.properties
                    .iter()
                    .map(|p| match p.as_str() {
                        "path" => self.taskpath(Some(task.event.id)),
                        "rpath" => join_tasks(self.traverse_up_from(Some(task.event.id)).take_while(|t| Some(t.event.id) != self.position)),
                        "ttime" => self.total_time_tracked(&task.event.id).to_string(),
                        prop => task.get(prop).unwrap_or(String::new()),
                    })
                    .collect::<Vec<String>>()
                    .join("\t")
            );
        }
        println!();
    }

    pub(crate) fn make_task(&mut self, input: &str) -> Option<EventId> {
        self.sender.submit(self.build_task(input)).map(|e| {
            let id = e.id;
            self.add_task(e);
            id
        })
    }

    pub(crate) fn build_task(&self, input: &str) -> EventBuilder {
        let mut tags: Vec<Tag> = Vec::new();
        self.position.inspect(|p| tags.push(Tag::event(*p)));
        return match input.split_once(": ") {
            None => EventBuilder::new(Kind::from(TASK_KIND), input, tags),
            Some(s) => {
                tags.append(
                    &mut s
                        .1
                        .split(" ")
                        .map(|t| Tag::Hashtag(t.to_string()))
                        .collect(),
                );
                EventBuilder::new(Kind::from(TASK_KIND), s.0, tags)
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
        self.referenced_tasks(&event, |t| { t.children.insert(event.id); });
        if self.tasks.contains_key(&event.id) {
            //eprintln!("Did not insert duplicate event {}", event.id);
        } else {
            self.tasks.insert(event.id, Task::new(event));
        }
    }
    
    pub(crate) fn add_prop(&mut self, event: &Event) {
        self.referenced_tasks(&event, |t| { t.props.insert(event.clone()); });
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
        join_tasks(self.traverse_up_from(id))
    }

    pub(crate) fn traverse_up_from(&self, id: Option<EventId>) -> ParentIterator {
        ParentIterator {
            tasks: &self.tasks,
            current: id,
            prev: None,
        }
    }

    pub(crate) fn update_state_for<F>(&mut self, id: &EventId, comment: &str, f: F) -> Option<Event>
    where
        F: FnOnce(&Task) -> Option<State>,
    {
        self.tasks.get_mut(id).and_then(|task| {
            f(task).and_then(|state| {
                self.sender.submit(EventBuilder::new(
                    state.kind(),
                    comment,
                    vec![Tag::event(task.event.id)],
                ))
            }).inspect(|e| {
                task.props.insert(e.clone());
            })
        })
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
            None => eprintln!("Cannot add note '{}' without active task", note),
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

pub(crate) fn join_tasks<'a>(iter: impl IntoIterator<Item=&'a Task>) -> String{
    iter.into_iter()
        .map(|t| t.event.content.clone())
        .fold(None, |acc, val| Some(acc.map_or_else(|| val.clone(), |cur| format!("{}>{}", val, cur))))
        .unwrap_or(String::new())
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
    let (tx, rx) = mpsc::channel();
    let mut tasks = Tasks::from(EventSender {
        tx,
        keys: Keys::generate(),
    });
    let t1 = tasks.make_task("t1");
    assert_eq!(tasks.depth, 1);
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = 0;
    assert_eq!(tasks.current_tasks().len(), 0);
    
    tasks.move_to(t1);
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 0);
    let t2 = tasks.make_task("t2");
    assert_eq!(tasks.current_tasks().len(), 1);
    let t3 = tasks.make_task("t3");
    assert_eq!(tasks.current_tasks().len(), 2);
    
    tasks.move_to(t2);
    assert_eq!(tasks.current_tasks().len(), 0);
    let t4 = tasks.make_task("t4");
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = 2;
    assert_eq!(tasks.current_tasks().len(), 1);
    tasks.depth = -1;
    assert_eq!(tasks.current_tasks().len(), 1);

    tasks.move_to(t1);
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
}