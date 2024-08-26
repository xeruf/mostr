use std::ops::Sub;

use chrono::{DateTime, Local, NaiveTime, TimeDelta, TimeZone, Utc};
use chrono::LocalResult::Single;
use log::{debug, error, info, trace, warn};
use nostr_sdk::Timestamp;

pub const CHARACTER_THRESHOLD: usize = 3;

pub fn some_non_empty(str: &str) -> Option<String> {
    if str.is_empty() { None } else { Some(str.to_string()) }
}

/// Parses the hour from a plain number in the String,
/// with max of max_future hours into the future.
pub fn parse_hour(str: &str, max_future: i64) -> Option<DateTime<Local>> {
    str.parse::<u32>().ok().and_then(|hour| {
        let now = Local::now();
        #[allow(deprecated)]
        now.date().and_hms_opt(hour, 0, 0).map(|time| {
            if time - now > TimeDelta::hours(max_future) {
                time.sub(TimeDelta::days(1))
            } else {
                time
            }
        })
    })
}

pub fn parse_date(str: &str) -> Option<DateTime<Utc>> {
    // Using two libraries for better exhaustiveness, see https://github.com/uutils/parse_datetime/issues/84
    match interim::parse_date_string(str, Local::now(), interim::Dialect::Us) {
        Ok(date) => Some(date.to_utc()),
        Err(e) => {
            match parse_datetime::parse_datetime_at_date(Local::now(), str) {
                Ok(date) => Some(date.to_utc()),
                Err(_) => {
                    warn!("Could not parse date from \"{str}\": {e}");
                    None
                }
            }
        }
    }.map(|time| {
        // TODO properly map date without time to day start, also support intervals
        if str.chars().any(|c| c.is_numeric()) {
            time
        } else {
            #[allow(deprecated)]
            time.date().and_time(NaiveTime::default()).unwrap()
        }
    })
}

/// Turn a human-readable relative timestamp into a nostr Timestamp.
/// - Plain number as hour, 18 hours back or 6 hours forward
/// - Number with prefix as minute offset
/// - Otherwise try to parse a relative date
pub fn parse_tracking_stamp(str: &str) -> Option<Timestamp> {
    if let Some(num) = parse_hour(str, 6) {
        return Some(Timestamp::from(num.to_utc().timestamp() as u64));
    }
    let stripped = str.trim().trim_start_matches('+').trim_start_matches("in ");
    if let Ok(num) = stripped.parse::<i64>() {
        return Some(Timestamp::from(Timestamp::now().as_u64().saturating_add_signed(num * 60)));
    }
    parse_date(str).and_then(|time| {
        let stamp = time.to_utc().timestamp();
        if stamp > 0 {
            Some(Timestamp::from(stamp as u64))
        } else {
            warn!("Can only track times after 1970!");
            None
        }
    })
}

/// Format DateTime easily comprehensible for human but unambiguous.
/// Length may vary.
pub fn format_datetime_relative(time: DateTime<Local>) -> String {
    let date = time.date_naive();
    let prefix =
        match Local::now()
            .date_naive()
            .signed_duration_since(date)
            .num_days() {
            -1 => "tomorrow ".into(),
            0 => "".into(),
            1 => "yesterday ".into(),
            -3..=3 => date.format("%a ").to_string(),
            //-10..=10 => date.format("%d. %a ").to_string(),
            -100..=100 => date.format("%b %d ").to_string(),
            _ => date.format("%y-%m-%d ").to_string(),
        };
    format!("{}{}", prefix, time.format("%H:%M"))
}

/// Format a nostr timestamp with the given formatting function.
pub fn format_as_datetime<F>(stamp: &Timestamp, formatter: F) -> String
where
    F: Fn(DateTime<Local>) -> String,
{
    match Local.timestamp_opt(stamp.as_u64() as i64, 0) {
        Single(time) => formatter(time),
        _ => stamp.to_human_datetime(),
    }
}

/// Format nostr Timestamp relative to local time
/// with optional day specifier or full date depending on distance to today.
pub fn format_timestamp_relative(stamp: &Timestamp) -> String {
    format_as_datetime(stamp, format_datetime_relative)
}

/// Format nostr timestamp with the given format.
pub fn format_timestamp(stamp: &Timestamp, format: &str) -> String {
    format_as_datetime(stamp, |time| time.format(format).to_string())
}

/// Format nostr timestamp in a sensible comprehensive format with consistent length and consistent sorting.
///
/// Currently: 18 characters
pub fn format_timestamp_local(stamp: &Timestamp) -> String {
    format_timestamp(stamp, "%y-%m-%d %a %H:%M")
}

pub fn format_timestamp_relative_to(stamp: &Timestamp, reference: &Timestamp) -> String {
    // Rough difference in days
    match (stamp.as_u64() as i64 - reference.as_u64() as i64) / 80_000 {
        0 => format_timestamp(stamp, "%H:%M"),
        -3..=3 => format_timestamp(stamp, "%a %H:%M"),
        _ => format_timestamp_local(stamp),
    }
}