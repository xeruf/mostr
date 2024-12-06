use std::iter::FusedIterator;
use itertools::Itertools;
use nostr_sdk::EventId;
use crate::task::Task;
use crate::tasks::{TaskMap, TaskMapMethods, TasksRelay};

#[derive(Clone, Debug, PartialEq)]
enum TraversalFilter {
    Reject = 0b00,
    TakeSelf = 0b01,
    TakeChildren = 0b10,
    Take = 0b11,
}
impl TraversalFilter {
    fn takes_children(&self) -> bool {
        self == &TraversalFilter::Take ||
            self == &TraversalFilter::TakeChildren
    }
    fn takes_self(&self) -> bool {
        self == &TraversalFilter::Take ||
            self == &TraversalFilter::TakeSelf
    }
}

/// Breadth-First Iterator over tasks with recursive children
pub(super) struct ChildrenTraversal<'a> {
    tasks: &'a TaskMap,
    /// Found Events
    queue: Vec<EventId>,
    /// Index of the next element in the queue
    index: usize,
    /// Depth of the next element
    depth: usize,
    /// Element with the next depth boundary
    next_depth_at: usize,
}
impl<'a> ChildrenTraversal<'a> {
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

    fn with_queue(tasks: &'a TaskMap, queue: Vec<EventId>) -> Self {
        ChildrenTraversal {
            tasks: &tasks,
            next_depth_at: queue.len(),
            index: 0,
            depth: 1,
            queue,
        }
    }

    pub(super) fn from(tasks: &'a TasksRelay, id: EventId) -> Self {
        let mut queue = Vec::with_capacity(64);
        queue.push(id);
        ChildrenTraversal {
            tasks: &tasks.tasks,
            queue,
            index: 0,
            depth: 0,
            next_depth_at: 1,
        }
    }

    /// Process until the given depth
    /// Returns true if that depth was reached
    pub(super) fn process_depth(&mut self, depth: usize) -> bool {
        while self.depth < depth {
            if self.next().is_none() {
                return false;
            }
        }
        true
    }

    /// Get all children
    pub(super) fn get_all(mut self) -> Vec<EventId> {
        while self.next().is_some() {}
        self.queue
    }

    /// Get all tasks until the specified depth
    pub(super) fn get_depth(mut self, depth: usize) -> Vec<EventId> {
        self.process_depth(depth);
        self.queue
    }

    fn check_depth(&mut self) {
        if self.next_depth_at == self.index {
            self.depth += 1;
            self.next_depth_at = self.queue.len();
        }
    }

    /// Get next id and advance, without adding children
    fn next_task(&mut self) -> Option<EventId> {
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
        F: Fn(&Task) -> TraversalFilter,
    {
        self.next_task().and_then(|id| {
            if let Some(task) = self.tasks.get(&id) {
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
impl FusedIterator for ChildrenTraversal<'_> {}
impl<'a> Iterator for ChildrenTraversal<'a> {
    type Item = EventId;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_task().inspect(|id| {
            match self.tasks.get(id) {
                None => {
                    // Unknown task, might still find children, just slower
                    for task in self.tasks.values() {
                        if task.parent_id().is_some_and(|i| i == id) {
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

