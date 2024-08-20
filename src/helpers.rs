use std::fmt::Display;
use std::io::{stdin, stdout, Write};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use chrono::LocalResult::Single;
use log::{debug, error, info, trace, warn};
use nostr_sdk::Timestamp;

pub fn some_non_empty(str: &str) -> Option<String> {
    if str.is_empty() { None } else { Some(str.to_string()) }
}

pub fn prompt(prompt: &str) -> Option<String> {
    print!("{} ", prompt);
    stdout().flush().unwrap();
    match stdin().lines().next() {
        Some(Ok(line)) => Some(line),
        _ => None,
    }
}

pub fn parse_date(str: &str) -> Option<DateTime<Utc>> {
    // Using two libraries for better exhaustiveness, see https://github.com/uutils/parse_datetime/issues/84
    match interim::parse_date_string(str, Local::now(), interim::Dialect::Us) {
        Ok(date) => Some(date.to_utc()),
        Err(e) => {
            match parse_datetime::parse_datetime_at_date(Local::now(), str) {
                Ok(date) => Some(date.to_utc()),
                Err(_) => {
                    warn!("Could not parse date from {str}: {e}");
                    None
                }
            }
        }
    }
}

pub fn parse_tracking_stamp(str: &str) -> Option<Timestamp> {
    let stripped = str.trim().trim_start_matches('+').trim_start_matches("in ");
    if let Ok(num) = stripped.parse::<i64>() {
        return Some(Timestamp::from(Timestamp::now().as_u64().saturating_add_signed(num * 60)));
    }
    parse_date(str).and_then(|time| {
        if time.timestamp() > 0 {
            Some(Timestamp::from(time.timestamp() as u64))
        } else {
            warn!("Can only track times after 1970!");
            None
        }
    })
}

// For use in format strings but not possible, so need global find-replace
pub const MAX_TIMESTAMP_WIDTH: u8 = 15;
/// Format nostr Timestamp relative to local time 
/// with optional day specifier or full date depending on distance to today
pub fn relative_datetimestamp(stamp: &Timestamp) -> String {
    match Local.timestamp_opt(stamp.as_u64() as i64, 0) {
        Single(time) => {
            let date = time.date_naive();
            let prefix = match Local::now()
                .date_naive()
                .signed_duration_since(date)
                .num_days()
            {
                -1 => "tomorrow ".into(),
                0 => "".into(),
                1 => "yesterday ".into(),
                2..=6 => date.format("last %a ").to_string(),
                _ => date.format("%y-%m-%d ").to_string(),
            };
            format!("{}{}", prefix, time.format("%H:%M"))
        }
        _ => stamp.to_human_datetime(),
    }
}

/// Format a nostr timestamp in a sensible comprehensive format
pub fn local_datetimestamp(stamp: &Timestamp) -> String {
    format_stamp(stamp, "%y-%m-%d %a %H:%M")
}

/// Format a nostr timestamp with the given format
pub fn format_stamp(stamp: &Timestamp, format: &str) -> String {
    match Local.timestamp_opt(stamp.as_u64() as i64, 0) {
        Single(time) => time.format(format).to_string(),
        _ => stamp.to_human_datetime(),
    }
}

