use chrono::LocalResult::Single;
use chrono::{DateTime, Local, NaiveTime, TimeDelta, TimeZone, Utc};
use log::{debug, error, info, trace, warn};
use nostr_sdk::Timestamp;

pub const CHARACTER_THRESHOLD: usize = 3;

pub fn to_string_or_default(arg: Option<impl ToString>) -> String {
    arg.map(|arg| arg.to_string()).unwrap_or_default()
}

pub fn some_non_empty(str: &str) -> Option<String> {
    if str.is_empty() { None } else { Some(str.to_string()) }
}

pub fn trim_start_count(str: &str, char: char) -> (&str, usize) {
    let len = str.len();
    let result = str.trim_start_matches(char);
    let dots = len - result.len();
    (result, dots)
}

pub trait ToTimestamp {
    fn to_timestamp(&self) -> Timestamp;
}
impl<T: TimeZone> ToTimestamp for DateTime<T> {
    fn to_timestamp(&self) -> Timestamp {
        let stamp = self.to_utc().timestamp();
        if let Some(t) = 0u64.checked_add_signed(stamp) {
            Timestamp::from(t)
        } else { Timestamp::zero() }
    }
}


/// Parses the hour optionally with minute from a plain number in a String,
/// with max of max_future hours into the future.
pub fn parse_hour(str: &str, max_future: i64) -> Option<DateTime<Local>> {
    parse_hour_after(str, Local::now() - TimeDelta::hours(24 - max_future))
}

/// Parses the hour optionally with minute from a plain number in a String.
pub fn parse_hour_after<T: TimeZone>(str: &str, after: DateTime<T>) -> Option<DateTime<T>> {
    str.parse::<u32>().ok().and_then(|number| {
        #[allow(deprecated)]
        after.date().and_hms_opt(
            if str.len() > 2 { number / 100 } else { number },
            if str.len() > 2 { number % 100 } else { 0 },
            0,
        ).map(|time| {
            if time < after {
                time + TimeDelta::days(1)
            } else {
                time
            }
        })
    })
}

pub fn parse_date(str: &str) -> Option<DateTime<Utc>> {
    parse_date_with_ref(str, Local::now())
}

pub fn parse_date_with_ref(str: &str, reference: DateTime<Local>) -> Option<DateTime<Utc>> {
    // Using two libraries for better exhaustiveness, see https://github.com/uutils/parse_datetime/issues/84
    match interim::parse_date_string(str, reference, interim::Dialect::Us) {
        Ok(date) => Some(date.to_utc()),
        Err(e) => {
            match parse_datetime::parse_datetime_at_date(reference, str) {
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
/// - Plain number as hour after given date, if none 18 hours back or 6 hours forward
/// - Number with prefix as minute offset
/// - Otherwise try to parse a relative date
pub fn parse_tracking_stamp(str: &str, after: Option<DateTime<Local>>) -> Option<Timestamp> {
    if let Some(num) = parse_hour_after(str, after.unwrap_or(Local::now() - TimeDelta::hours(18))) {
        return Some(num.to_timestamp());
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
            //-3..=3 => date.format("%a ").to_string(),
            -10..=10 => date.format("%d. %a ").to_string(),
            -100..=100 => date.format("%a %b %d ").to_string(),
            _ => date.format("%y-%m-%d %a ").to_string(),
        };
    format!("{}{}", prefix, time.format("%H:%M"))
}

/// Format a nostr timestamp with the given formatting function.
pub fn format_as_datetime<F>(stamp: &Timestamp, formatter: F) -> String
where
    F: Fn(DateTime<Local>) -> String,
{
    match Local.timestamp_opt(stamp.as_u64() as i64 + 1, 0) {
        Single(time) => formatter(time),
        _ => stamp.to_human_datetime().to_string(),
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

/// Format nostr timestamp with seconds precision.
pub fn format_timestamp_full(stamp: &Timestamp) -> String {
    format_timestamp(stamp, "%y-%m-%d %a %H:%M:%S")
}

pub fn format_timestamp_relative_to(stamp: &Timestamp, reference: &Timestamp) -> String {
    // Rough difference in days
    match (stamp.as_u64() as i64 - reference.as_u64() as i64) / 80_000 {
        0 => format_timestamp(stamp, "%H:%M"),
        -3..=3 => format_timestamp(stamp, "%a %H:%M"),
        _ => format_timestamp_local(stamp),
    }
}

mod test {
    use super::*;
    use chrono::{FixedOffset, NaiveDate, Timelike};
    use interim::datetime::DateTime;

    #[test]
    fn parse_hours() {
        let now = Local::now();
        #[allow(deprecated)]
        let date = now.date();
        if now.hour() > 2 {
            assert_eq!(
                parse_hour("23", 22).unwrap(),
                date.and_hms_opt(23, 0, 0).unwrap()
            );
        }
        if now.hour() < 22 {
            assert_eq!(
                parse_hour("02", 2).unwrap(),
                date.and_hms_opt(2, 0, 0).unwrap()
            );
            assert_eq!(
                parse_hour("2301", 1).unwrap(),
                (date - TimeDelta::days(1)).and_hms_opt(23, 01, 0).unwrap()
            );
        }

        let date = NaiveDate::from_ymd_opt(2020, 10, 10).unwrap();
        let time = Utc.from_utc_datetime(
            &date.and_hms_opt(10, 1,0).unwrap()
        );
        assert_eq!(parse_hour_after("2201", time).unwrap(), Utc.from_utc_datetime(&date.and_hms_opt(22, 1, 0).unwrap()));
        assert_eq!(parse_hour_after("10", time).unwrap(), Utc.from_utc_datetime(&(date + TimeDelta::days(1)).and_hms_opt(10, 0, 0).unwrap()));

        // TODO test timezone offset issues
    }

    #[test]
    fn test_timezone() {
        assert_eq!(
            FixedOffset::east_opt(7200).unwrap().timestamp_millis_opt(1000).unwrap().time(),
            NaiveTime::from_hms_opt(2, 0, 1).unwrap()
        );
    }
}