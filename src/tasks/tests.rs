use super::*;
use crate::event_sender::EventSender;
use crate::hashtag::Hashtag;
use crate::kinds::{extract_tags, to_hashtag_tag, TASK_KIND};
use crate::task::{State, Task, MARKER_DEPENDS, MARKER_PARENT};
use itertools::Itertools;
use nostr_sdk::{EventBuilder, EventId, Keys, Kind, Tag, Timestamp};
use std::collections::HashSet;

fn stub_tasks() -> TasksRelay {
    use nostr_sdk::Keys;
    use tokio::sync::mpsc;

    let (tx, _rx) = mpsc::channel(16);
    TasksRelay::with_sender(EventSender {
        url: None,
        tx,
        keys: Keys::generate(),
        queue: Default::default(),
    })
}

macro_rules! assert_position {
        ($tasks:expr, $id:expr $(,)?) => {
            let pos = $tasks.get_position();
            assert_eq!(pos, Some($id),
                "Current: {:?}\nExpected: {:?}",
                $tasks.get_task_path(pos),
                $tasks.get_task_path(Some($id)),
            )
        };
    }

macro_rules! assert_tasks_visible {
        ($tasks:expr, $expected:expr $(,)?) => {
            assert_tasks!($tasks, $tasks.visible_tasks(), $expected,
                "\nQuick Access: {:?}",
                $tasks.quick_access_raw().map(|id| $tasks.get_task_path(Some(id))).collect_vec());
        };
    }

macro_rules! assert_tasks_view {
        ($tasks:expr, $expected:expr $(,)?) => {
            assert_tasks!($tasks, $tasks.viewed_tasks(), $expected, "");
        };
    }

macro_rules! assert_tasks {
        ($tasks:expr, $tasklist:expr, $expected:expr $(, $($arg:tt)*)?) => {
            assert_eq!(
                $tasklist
                    .iter()
                    .map(|t| t.get_id())
                    .collect::<HashSet<EventId>>(),
                HashSet::from_iter($expected.clone()),
                "Tasks Visible: {:?}\nExpected: {:?}{}",
                $tasklist.iter().map(|t| t.get_id()).map(|id| $tasks.get_task_path(Some(id))).collect_vec(),
                $expected.into_iter().map(|id| $tasks.get_task_path(Some(id))).collect_vec(),
                format!($($($arg)*)?)
            );
        };
    }

#[test]
fn test_recursive_closing() {
    let mut tasks = stub_tasks();

    tasks.custom_time = Some(Timestamp::zero());
    let parent = tasks.make_task_unwrapped("parent #tag1");
    tasks.move_to(Some(parent));
    let sub = tasks.make_task_unwrapped("sub #oi # tag2");
    assert_eq!(
        tasks.all_hashtags(),
        ["oi", "tag1", "tag2"].into_iter().map(Hashtag::from).collect()
    );
    tasks.make_note("note with #tag3 # yeah");
    let all_tags = ["oi", "tag1", "tag2", "tag3", "yeah"].into_iter().map(Hashtag::from).collect();
    assert_eq!(tasks.all_hashtags(), all_tags);

    tasks.custom_time = Some(Timestamp::now());
    tasks.update_state("Finished #YeaH # oi", State::Done);
    assert_eq!(
        tasks.get_by_id(&parent).unwrap().list_hashtags().collect_vec(),
        ["YeaH", "oi", "tag3", "yeah", "tag1"].map(Hashtag::from)
    );
    assert_eq!(tasks.all_hashtags(), all_tags);

    tasks.custom_time = Some(now());
    tasks.update_state("Closing Down", State::Closed);
    assert_eq!(tasks.get_by_id(&sub).unwrap().pure_state(), State::Closed);
    assert_eq!(tasks.get_by_id(&parent).unwrap().pure_state(), State::Closed);
    assert_eq!(tasks.nonclosed_tasks().next(), None);
    assert_eq!(tasks.all_hashtags(), Default::default());
}

#[test]
fn test_context() {
    let mut tasks = stub_tasks();
    tasks.update_tags(["dp", "yeah"].into_iter().map(Hashtag::from));
    assert_eq!(tasks.get_prompt_suffix(), " #dp #yeah");
    tasks.remove_tag("Y");
    assert_eq!(tasks.tags, ["dp"].into_iter().map(Hashtag::from).collect());

    tasks.set_priority(Some(HIGH_PRIO));
    assert_eq!(tasks.get_prompt_suffix(), " #dp *85");
    let id_hp = tasks.make_task_unwrapped("high prio tagged # tag");
    let hp = tasks.get_by_id(&id_hp).unwrap();
    assert_eq!(hp.priority(), Some(HIGH_PRIO));
    assert_eq!(
        hp.list_hashtags().collect_vec(),
        vec!["DP", "tag"].into_iter().map(Hashtag::from).collect_vec()
    );

    tasks.state = StateFilter::from("WIP");
    tasks.set_priority(Some(QUICK_PRIO));

    tasks.make_task_and_enter("another *4", State::Pending);
    let task2 = tasks.get_current_task().unwrap();
    assert_eq!(task2.priority(), Some(40));
    assert_eq!(task2.pure_state(), State::Pending);
    assert_eq!(task2.state().unwrap().get_label(), "Pending");
    tasks.make_note("*3");
    let task2 = tasks.get_current_task().unwrap();
    assert_eq!(task2.descriptions().next(), None);
    assert_eq!(task2.priority(), Some(30));
    let anid = task2.get_id();

    tasks.custom_time = Some(Timestamp::now() + 1);
    let s1 = tasks.make_task_unwrapped("sub1");
    tasks.custom_time = Some(Timestamp::now() + 2);
    tasks.set_priority(Some(QUICK_PRIO + 1));
    let s2 = tasks.make_task_unwrapped("sub2");
    let s3 = tasks.make_task_unwrapped("sub3");
    tasks.set_priority(Some(QUICK_PRIO));

    assert_tasks_visible!(tasks, [s1, s2, s3]);
    tasks.state = StateFilter::Default;
    assert_tasks_view!(tasks, [s1, s2, s3]);
    assert_tasks_visible!(tasks, [id_hp, s1, s2, s3]);
    tasks.move_up();
    tasks.set_search_depth(1);
    assert_tasks_view!(tasks, [id_hp]);
    assert_tasks_visible!(tasks, [s1, s2, s3, id_hp]);

    tasks.set_priority(None);
    let s4 = tasks.make_task_with("sub4", [tasks.make_event_tag_from_id(anid, MARKER_PARENT)], true).unwrap();
    assert_eq!(tasks.get_parent(Some(&s4)), Some(&anid));
    assert_tasks_view!(tasks, [anid, id_hp]);
    // s2-4 are newest while s2,s3,hp are highest prio
    assert_tasks_visible!(tasks, [s4, s2, s3, anid, id_hp]);

    tasks.pubkey = Some(Keys::generate().public_key);
}

#[test]
fn test_sibling_dependency() {
    let mut tasks = stub_tasks();
    let parent = tasks.make_task_unwrapped("parent");
    let sub = tasks.submit(
        EventBuilder::new(TASK_KIND, "sub")
            .tags([tasks.make_event_tag_from_id(parent, MARKER_PARENT)]),
    );
    assert_tasks_view!(tasks, [parent]);
    tasks.track_at(Timestamp::now(), Some(sub));
    assert_eq!(tasks.get_own_events_history().count(), 1);
    assert_tasks_view!(tasks, []);

    tasks.make_dependent_sibling("sibling");
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks.viewed_tasks().len(), 2);
}

#[test]
fn test_bookmarks() {
    let mut tasks = stub_tasks();
    let zero = EventId::all_zeros();
    let test = tasks.make_task_unwrapped("test # tag");
    let parent = tasks.make_task_unwrapped("parent");
    assert_eq!(tasks.viewed_tasks().len(), 2);
    tasks.move_to(Some(parent));
    let pin = tasks.make_task_unwrapped("pin");

    tasks.search_depth = 1;
    assert_eq!(tasks.filtered_tasks(None, true).len(), 2);
    assert_eq!(tasks.filtered_tasks(None, false).len(), 2);
    assert_eq!(tasks.filtered_tasks(Some(zero), false).len(), 0);
    assert_eq!(tasks.filtered_tasks(Some(parent), false).len(), 1);
    assert_eq!(tasks.filtered_tasks(Some(pin), false).len(), 0);
    assert_eq!(tasks.filtered_tasks(Some(zero), false).len(), 0);

    tasks.submit(
        EventBuilder::new(Kind::Bookmarks, "")
            .tags([Tag::event(pin), Tag::event(zero)])
    );
    assert_eq!(tasks.viewed_tasks().len(), 1);
    assert_eq!(tasks.filtered_tasks(Some(pin), true).len(), 0);
    assert_eq!(tasks.filtered_tasks(Some(pin), false).len(), 0);
    assert_eq!(tasks.filtered_tasks(Some(zero), true).len(), 0);
    assert_eq!(
        tasks.filtered_tasks(Some(zero), false),
        vec![tasks.get_by_id(&pin).unwrap()]
    );

    tasks.move_to(None);
    assert_eq!(tasks.view_depth, 0);
    assert_tasks_visible!(tasks, [pin, test, parent]);
    tasks.set_view_depth(1);
    assert_tasks_visible!(tasks, [pin, test]);
    tasks.add_tag("tag");
    assert_tasks_visible!(tasks, [test]);
    assert_eq!(
        tasks.filtered_tasks(None, true),
        vec![tasks.get_by_id(&test).unwrap()]
    );

    tasks.submit(EventBuilder::new(Kind::Bookmarks, ""));
    assert!(tasks.bookmarks.is_empty());
    tasks.clear_filters();
    assert_tasks_visible!(tasks, [pin, test]);
    tasks.set_view_depth(0);
    tasks.custom_time = Some(now());
    let mut new = (0..3).map(|t| tasks.make_task_unwrapped(t.to_string().as_str())).collect_vec();
    // Show the newest tasks in quick access and remove old pin
    new.extend([test, parent]);
    assert_tasks_visible!(tasks, new);
}

#[test]
fn test_procedures() {
    let mut tasks = stub_tasks();
    tasks.make_task_and_enter("proc # tags", State::Procedure);
    assert_eq!(tasks.get_own_events_history().count(), 1);
    let side = tasks.submit(
        EventBuilder::new(TASK_KIND, "side")
            .tags([tasks.make_event_tag(&tasks.get_current_task().unwrap().event, MARKER_DEPENDS)])
    );
    assert_eq!(tasks.viewed_tasks(), Vec::<&Task>::new());
    let sub_id = tasks.make_task_unwrapped("sub");
    assert_tasks_view!(tasks, [sub_id]);
    assert_eq!(tasks.len(), 3);
    let sub = tasks.get_by_id(&sub_id).unwrap();
    assert_eq!(sub.find_dependents(), Vec::<&EventId>::new());
}

#[test]
fn test_filter_or_create() {
    let mut tasks = stub_tasks();
    let zeros = EventId::all_zeros();
    let zero = Some(zeros);

    let id1 = tasks.filter_or_create(zero, "newer");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks.viewed_tasks().len(), 0);
    assert_eq!(tasks.get_by_id(&id1.unwrap()).unwrap().parent_id(), zero.as_ref());

    tasks.move_to(zero);
    assert_eq!(tasks.viewed_tasks().len(), 1);
    let sub = tasks.make_task_unwrapped("test");
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks.viewed_tasks().len(), 2);
    assert_eq!(tasks.get_by_id(&sub).unwrap().parent_id(), zero.as_ref());

    // Do not substring match invisible subtask
    let id2 = tasks.filter_or_create(None, "#new-is gold wrapped").unwrap();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks.viewed_tasks().len(), 2);
    let new2 = tasks.get_by_id(&id2).unwrap();
    assert_eq!(new2.props, Default::default());

    tasks.move_up();
    assert_eq!(tasks.get_matching(tasks.get_position(), "wrapped").len(), 1);
    assert_eq!(tasks.get_matching(tasks.get_position(), "new-i").len(), 1);
    tasks.filter_or_create(None, "is gold");
    assert_position!(tasks, id2);

    assert_eq!(tasks.get_own_events_history().count(), 3);
    // Global match
    assert_eq!(tasks.filter_or_create(None, "newer"), None);
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

    let some = tasks.make_task_unwrapped("some");
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

    tasks.track_at(Timestamp::now() + 100, Some(zero));
    assert_eq!(
        timestamps(tasks.get_own_events_history(), &[zero])
            .collect_vec()
            .len(),
        2
    )
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
    assert_eq!(tasks.viewed_tasks().len(), 1);
    tasks.search_depth = 0;
    assert_eq!(tasks.viewed_tasks().len(), 0);
    tasks.recurse_activities = false;
    assert_eq!(tasks.filtered_tasks(None, false).len(), 1);

    tasks.move_to(Some(t1));
    assert_position!(tasks, t1);
    tasks.search_depth = 2;
    assert_eq!(tasks.viewed_tasks().len(), 0);
    let t11 = tasks.make_task_unwrapped("t11 # tag");
    assert_eq!(tasks.viewed_tasks().len(), 1);
    assert_eq!(tasks.get_task_path(Some(t11)), "t1>t11");
    assert_eq!(tasks.get_relative_path(t11), "t11");
    let t12 = tasks.make_task_unwrapped("t12");
    assert_eq!(tasks.viewed_tasks().len(), 2);

    tasks.move_to(Some(t11));
    assert_position!(tasks, t11);
    assert_eq!(tasks.viewed_tasks().len(), 0);
    let t111 = tasks.make_task_unwrapped("t111");
    assert_tasks_view!(tasks, [t111]);
    assert_eq!(tasks.get_task_path(Some(t111)), "t1>t11>t111");
    assert_eq!(tasks.get_relative_path(t111), "t111");
    tasks.view_depth = 2;
    assert_tasks_view!(tasks, [t111]);

    assert_eq!(ChildIterator::from(&tasks, EventId::all_zeros()).get_all().len(), 1);
    assert_eq!(ChildIterator::from(&tasks, EventId::all_zeros()).get_depth(0).len(), 1);
    assert_eq!(ChildIterator::from(&tasks, t1).get_depth(0).len(), 1);
    assert_eq!(ChildIterator::from(&tasks, t1).get_depth(1).len(), 3);
    assert_eq!(ChildIterator::from(&tasks, t1).get_depth(2).len(), 4);
    assert_eq!(ChildIterator::from(&tasks, t1).get_depth(9).len(), 4);
    assert_eq!(ChildIterator::from(&tasks, t1).get_all().len(), 4);

    tasks.move_up();
    assert_position!(tasks, t1);
    assert_eq!(tasks.get_own_events_history().count(), 3);
    assert_eq!(tasks.get_relative_path(t111), "t11>t111");
    assert_eq!(tasks.view_depth, 2);
    tasks.set_search_depth(1);
    assert_tasks_view!(tasks, [t111, t12]);
    tasks.set_view_depth(0);
    assert_tasks_view!(tasks, [t11, t12]);
    tasks.set_view(vec![t11]);
    assert_tasks_view!(tasks, [t11]);
    tasks.set_view_depth(1);
    assert_tasks_view!(tasks, [t111]);
    tasks.set_search_depth(2); // resets view
    assert_tasks_view!(tasks, [t111, t12]);
    tasks.set_view_depth(0);
    assert_tasks_view!(tasks, [t11, t12]);

    tasks.move_to(None);
    tasks.recurse_activities = true;
    assert_tasks_view!(tasks, [t11, t12]);
    tasks.recurse_activities = false;
    assert_tasks_view!(tasks, [t1]);
    tasks.view_depth = 1;
    assert_tasks_view!(tasks, [t11, t12]);
    tasks.view_depth = 2;
    assert_tasks_view!(tasks, [t111, t12]);
    tasks.view_depth = 9;
    assert_tasks_view!(tasks, [t111, t12]);

    tasks.add_tag("tag");
    assert_eq!(tasks.get_prompt_suffix(), " #tag");
    tasks.view_depth = 0;
    assert_tasks_view!(tasks, [t11]);
    tasks.search_depth = 0;
    assert_eq!(tasks.view, []);
    assert_tasks_view!(tasks, []);

    // Upwards
    tasks.move_to(Some(t111));
    assert_eq!(tasks.get_task_path(tasks.get_position()), "t1>t11>t111");
    assert_eq!(tasks.up_by(1), Some(t11));
    assert_eq!(tasks.up_by(2), Some(t1));
    assert_eq!(tasks.up_by(4), None);
    tasks.move_to(Some(t12));
    assert_eq!(tasks.up_by(1), Some(t1));
    assert_eq!(tasks.up_by(2), None);
}

#[test]
fn test_empty_task_title_fallback_to_id() {
    let mut tasks = stub_tasks();

    let empty = tasks.make_task_unchecked("", vec![]);
    let empty_task = tasks.get_by_id(&empty).unwrap();
    let empty_id = empty_task.get_id().to_string();
    assert_eq!(empty_task.get_title(), empty_id);
    assert_eq!(tasks.get_task_path(Some(empty)), empty_id);
}

#[test]
fn test_short_task() {
    let mut tasks = stub_tasks();
    let str = " # one";
    assert_eq!(extract_tags(str, &tasks.users), ("".to_string(), vec![to_hashtag_tag("one")]));
    assert_eq!(tasks.make_task(str), None);
}

#[test]
fn test_unknown_task() {
    let mut tasks = stub_tasks();

    let zero = EventId::all_zeros();
    assert_eq!(tasks.get_task_path(Some(zero)), zero.to_string());
    tasks.move_to(Some(zero));
    let dangling = tasks.make_task_unwrapped("test");
    assert_eq!(
        tasks.get_task_path(Some(dangling)),
        "0000000000000000000000000000000000000000000000000000000000000000>test"
    );
    assert_eq!(tasks.get_relative_path(dangling), "test");

    tasks.move_to(Some(dangling));
    assert_eq!(tasks.up_by(0), Some(dangling));
    assert_eq!(tasks.up_by(1), Some(zero));
    assert_eq!(tasks.up_by(2), None);
}

#[allow(dead_code)] // #[test]
fn test_itertools() {
    use itertools::Itertools;
    assert_eq!("test  toast".split(' ').collect_vec().len(), 3);
    assert_eq!("test  toast".split_ascii_whitespace().collect_vec().len(), 2);
}
