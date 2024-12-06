use super::*;
use nostr_sdk::{EventBuilder, Keys, Tag, Timestamp};

#[test]
fn test_state() {
    let keys = Keys::generate();
    let mut task = Task::new(
        EventBuilder::new(Kind::GitIssue, "task").tags([Tag::hashtag("tag1")])
            .sign_with_keys(&keys).unwrap());
    assert_eq!(task.pure_state(), State::Open);
    assert_eq!(task.list_hashtags().count(), 1);

    let now = Timestamp::now();
    task.props.insert(
        EventBuilder::new(State::Done.into(), "")
            .custom_created_at(now)
            .sign_with_keys(&keys).unwrap());
    assert_eq!(task.pure_state(), State::Done);
    task.props.insert(
        EventBuilder::new(State::Open.into(), "Ready").tags([Tag::hashtag("tag2")])
            .custom_created_at(now - 2)
            .sign_with_keys(&keys).unwrap());
    assert_eq!(task.pure_state(), State::Done);
    assert_eq!(task.list_hashtags().count(), 2);
    task.props.insert(
        EventBuilder::new(State::Closed.into(), "")
            .custom_created_at(now + 9)
            .sign_with_keys(&keys).unwrap());
    assert_eq!(task.pure_state(), State::Closed);
    assert_eq!(task.state_at(now), Some(StateChange {
        state: State::Done,
        name: None,
        time: now,
    }));
    assert_eq!(task.state_at(now - 1), Some(StateChange {
        state: State::Open,
        name: Some("Ready".to_string()),
        time: now - 2,
    }));
}
