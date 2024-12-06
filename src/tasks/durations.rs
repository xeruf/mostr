use std::time::Duration;
use itertools::Itertools;
use nostr_sdk::{Event, EventId, Timestamp};
use crate::kinds::match_event_tag;

pub(super) fn referenced_events(event: &Event) -> impl Iterator<Item=EventId> + '_ {
    event.tags.iter().filter_map(|tag| match_event_tag(tag).map(|t| t.id))
}

/// Returns the id of a referenced event if it is contained in the provided ids list.
fn matching_tag_id<'a>(event: &'a Event, ids: &'a [EventId]) -> Option<EventId> {
    referenced_events(event).find(|id| ids.contains(id))
}

/// Filters out event timestamps to those that start or stop one of the given events
pub(super) fn timestamps<'a>(
    events: impl Iterator<Item=&'a Event>,
    ids: &'a [EventId],
) -> impl Iterator<Item=(&Timestamp, Option<EventId>)> {
    events
        .map(|event| (&event.created_at, matching_tag_id(event, ids)))
        .dedup_by(|(_, e1), (_, e2)| e1 == e2)
        .skip_while(|element| element.1.is_none())
}

/// Iterates Events to accumulate times tracked
/// Expects a sorted iterator
pub(super) struct Durations<'a> {
    events: Box<dyn Iterator<Item=&'a Event> + 'a>,
    ids: &'a [EventId],
    threshold: Option<Timestamp>,
}
impl Durations<'_> {
    pub(super) fn from<'b>(
        events: impl IntoIterator<Item=&'b Event> + 'b,
        ids: &'b [EventId],
    ) -> Durations<'b> {
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
        start.filter(|t| t < &now)
            .map(|stamp| Duration::from_secs(now.saturating_sub(stamp)))
    }
}

#[test]
#[ignore]
fn test_timestamps() {
    let mut tasks = crate::tasks::tests::stub_tasks();
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

