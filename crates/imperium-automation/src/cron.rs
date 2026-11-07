//! Five-field cron expressions (`minute hour day-of-month month day-of-week`) in local time:
//! numbers, `*`, lists (`1,15`), ranges (`MON-FRI`), and steps (`*/15`, `0-30/10`). As in
//! classic cron, when both day fields are restricted a day matching either one fires.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDateTime, TimeZone, Timelike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: Vec<bool>,
    hours: Vec<bool>,
    days: Vec<bool>,
    months: Vec<bool>,
    weekdays: Vec<bool>,
    days_restricted: bool,
    weekdays_restricted: bool,
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const WEEKDAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
/// How far ahead to look for the next match (covers 29 February).
const SEARCH_MINUTES: i64 = 60 * 24 * 366 * 5;

impl Cron {
    pub fn parse(expression: &str) -> Result<Self, String> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields.as_slice() else {
            return Err(format!(
                "{expression:?} needs five fields: minute hour day-of-month month day-of-week"
            ));
        };
        let mut weekdays = field(weekday, 0, 7, &WEEKDAYS)?;
        // 7 is also Sunday.
        if weekdays[7] {
            weekdays[0] = true;
        }
        weekdays.truncate(7);
        Ok(Cron {
            minutes: field(minute, 0, 59, &[])?,
            hours: field(hour, 0, 23, &[])?,
            days: field(day, 1, 31, &[])?,
            months: field(month, 1, 12, &MONTHS)?,
            weekdays,
            days_restricted: *day != "*",
            weekdays_restricted: *weekday != "*",
        })
    }

    fn matches(&self, time: &NaiveDateTime) -> bool {
        let day = self.days[time.day() as usize];
        let weekday = self.weekdays[time.weekday().num_days_from_sunday() as usize];
        let day_ok = match (self.days_restricted, self.weekdays_restricted) {
            (true, true) => day || weekday,
            _ => day && weekday,
        };
        self.minutes[time.minute() as usize]
            && self.hours[time.hour() as usize]
            && self.months[time.month() as usize]
            && day_ok
    }

    /// The first matching minute strictly after `after`.
    pub fn next_after(&self, after: DateTime<Local>) -> Option<DateTime<Local>> {
        let start = after.naive_local().with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        (0..SEARCH_MINUTES)
            .map(|offset| start + Duration::minutes(offset))
            .filter(|time| self.matches(time))
            // Local times skipped by a daylight-saving change do not exist; try the next match.
            .find_map(|time| Local.from_local_datetime(&time).earliest())
    }
}

/// A field's allowed values as flags indexed by value, from `min` to `max`.
fn field(text: &str, min: u32, max: u32, names: &[&str]) -> Result<Vec<bool>, String> {
    let mut allowed = vec![false; max as usize + 1];
    let value = |part: &str| -> Result<u32, String> {
        let upper = part.to_ascii_uppercase();
        // Month names count from 1 (JAN), weekday names from 0 (SUN).
        let first = if names.len() == MONTHS.len() { 1 } else { 0 };
        let number = match names.iter().position(|name| *name == upper) {
            Some(index) => index as u32 + first,
            None => part
                .parse()
                .map_err(|_| format!("{part:?} is not a number or name"))?,
        };
        if number < min || number > max {
            return Err(format!("{number} is outside {min}-{max}"));
        }
        Ok(number)
    };
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (
                range,
                step.parse::<u32>()
                    .ok()
                    .filter(|step| *step > 0)
                    .ok_or_else(|| format!("bad step in {part:?}"))?,
            ),
            None => (part, 1),
        };
        let (low, high) = match range {
            "*" => (min, max),
            range => match range.split_once('-') {
                Some((low, high)) => (value(low)?, value(high)?),
                None if step > 1 => (value(range)?, max),
                None => {
                    let single = value(range)?;
                    (single, single)
                }
            },
        };
        if low > high {
            return Err(format!("{part:?} runs backwards"));
        }
        for allowed_value in (low..=high).step_by(step as usize) {
            allowed[allowed_value as usize] = true;
        }
    }
    Ok(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Local> {
        let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
        Local.from_local_datetime(&naive).earliest().unwrap()
    }

    fn next(expression: &str, after: &str) -> String {
        Cron::parse(expression)
            .unwrap()
            .next_after(at(after))
            .unwrap()
            .format("%Y-%m-%d %H:%M %a")
            .to_string()
    }

    #[test]
    fn next_runs_follow_the_fields() {
        // 2026-11-07 is a Saturday.
        assert_eq!(
            next("0 8 * * MON-FRI", "2026-11-07 09:00"),
            "2026-11-09 08:00 Mon"
        );
        assert_eq!(
            next("*/15 * * * *", "2026-11-07 09:07"),
            "2026-11-07 09:15 Sat"
        );
        assert_eq!(
            next("30 9 1 * *", "2026-11-07 09:00"),
            "2026-12-01 09:30 Tue"
        );
        assert_eq!(
            next("0 0 29 2 *", "2026-11-07 00:00"),
            "2028-02-29 00:00 Tue"
        );
        assert_eq!(
            next("0 12 * jan,jul sun", "2026-11-07 00:00"),
            "2027-01-03 12:00 Sun"
        );
        assert_eq!(
            next("0 0 * * 7", "2026-11-07 00:00"),
            "2026-11-08 00:00 Sun"
        );
        // Both day fields restricted: either may match (the 13th, or any Friday).
        assert_eq!(
            next("0 0 13 * FRI", "2026-11-07 00:00"),
            "2026-11-13 00:00 Fri"
        );
        assert_eq!(
            next("0 0 13 * MON", "2026-11-07 00:00"),
            "2026-11-09 00:00 Mon"
        );
    }

    #[test]
    fn bad_expressions_are_explained() {
        assert!(Cron::parse("* * * *").unwrap_err().contains("five fields"));
        assert!(Cron::parse("61 * * * *").is_err());
        assert!(Cron::parse("*/0 * * * *").is_err());
        assert!(Cron::parse("5-1 * * * *").is_err());
        assert!(Cron::parse("0 0 * * FUNDAY").is_err());
    }
}
