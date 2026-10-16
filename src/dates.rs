//! Reading the dates and durations people type into a search, the way Godspeed's date picker
//! does: `3h`, `2 days`, `in 3 days`, `tomorrow 3pm`, `friday`, `Oct 14th 12am`, `2026-10-31`.
//!
//! All times are local. A date with no time of day is a whole day.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Weekday};

/// What kind of moment a date query named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A calendar day, such as `tomorrow` or `Oct 14`.
    Day,
    /// A day and a time of day, such as `Oct 14th 12am`.
    Time,
    /// An exact moment relative to now, such as `3h` or `in 2 days`.
    Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed {
    pub at: NaiveDateTime,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Minute,
    Hour,
    Day,
    Week,
}

fn unit_of(word: &str) -> Option<Unit> {
    Some(match word {
        "m" | "min" | "mins" | "minute" | "minutes" => Unit::Minute,
        "h" | "hr" | "hrs" | "hour" | "hours" => Unit::Hour,
        "d" | "day" | "days" => Unit::Day,
        "w" | "wk" | "wks" | "week" | "weeks" => Unit::Week,
        _ => return None,
    })
}

fn unit_seconds(unit: Unit) -> f64 {
    match unit {
        Unit::Minute => 60.0,
        Unit::Hour => 3600.0,
        Unit::Day => 86_400.0,
        Unit::Week => 604_800.0,
    }
}

/// Reads `1h30m`, `1.5 hours`, `1 hour and 30 minutes`, `90 minutes`, `2d, 3h`: a run of numbers
/// each followed by a unit, with spaces, commas and "and" allowed between them. Returns the total
/// in seconds, or None if the text isn't made only of those.
fn amounts_in_seconds(text: &str) -> Option<f64> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut total = 0.0;
    let mut terms = 0;
    loop {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        if terms > 0 && chars[i..].iter().collect::<String>().starts_with("and ") {
            i += 4;
            continue;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
            i += 1;
        }
        let number: f64 = chars[start..i].iter().collect::<String>().parse().ok()?;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let unit_start = i;
        while i < chars.len() && chars[i].is_alphabetic() {
            i += 1;
        }
        let unit = unit_of(&chars[unit_start..i].iter().collect::<String>())?;
        total += number * unit_seconds(unit);
        terms += 1;
    }
    (terms > 0).then_some(total)
}

/// A duration in minutes: a bare number of minutes, or amounts with units, as above.
pub fn parse_minutes(text: &str) -> Option<i64> {
    let text = text.trim().to_lowercase();
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Some(text.parse::<f64>().ok()?.round() as i64);
    }
    Some((amounts_in_seconds(&text)? / 60.0).round() as i64)
}

/// `3h`, `-2d`, `in 3 days`, `3 hours ago`: a moment relative to now.
fn parse_relative(text: &str, now: NaiveDateTime) -> Option<NaiveDateTime> {
    let mut rest = text.trim();
    let mut sign = 1.0;
    if let Some(r) = rest.strip_prefix('-') {
        sign = -1.0;
        rest = r;
    } else if let Some(r) = rest.strip_prefix('+') {
        rest = r;
    }
    if let Some(r) = rest.strip_prefix("in ") {
        rest = r;
    }
    if let Some(r) = rest.strip_suffix(" ago") {
        sign = -sign;
        rest = r;
    }
    let seconds = amounts_in_seconds(rest)?;
    Some(now + Duration::seconds((sign * seconds).round() as i64))
}

fn weekday_of(word: &str) -> Option<Weekday> {
    Some(match word {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "weds" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

fn month_of(word: &str) -> Option<u32> {
    Some(match word {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    })
}

/// `12am`, `3pm`, `3:30pm`, `11:59pm`, or 24-hour `14:30`.
fn parse_clock(word: &str) -> Option<NaiveTime> {
    let (body, meridiem) = if let Some(b) = word.strip_suffix("am") {
        (b, Some(false))
    } else if let Some(b) = word.strip_suffix("pm") {
        (b, Some(true))
    } else {
        (word, None)
    };
    let (hour, minute) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        Some(_) => return None,
        // `1159pm` and `930am`: hours then two digits of minutes.
        None if meridiem.is_some() && (3..=4).contains(&body.len()) && body.bytes().all(|b| b.is_ascii_digit()) => {
            let (h, m) = body.split_at(body.len() - 2);
            (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)
        }
        None if meridiem.is_some() => (body.parse::<u32>().ok()?, 0),
        None => return None,
    };
    let hour = match meridiem {
        Some(pm) => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour % 12 + if pm { 12 } else { 0 }
        }
        None => hour,
    };
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn meridiem_clock(hour: u32, pm: bool) -> Option<NaiveTime> {
    parse_clock(&format!("{hour}{}", if pm { "pm" } else { "am" }))
}

/// The year, among last, this and next, in which the month and day fall closest to `today`.
fn nearest_year(month: u32, day: u32, today: NaiveDate) -> Option<NaiveDate> {
    (today.year() - 1..=today.year() + 1)
        .filter_map(|year| NaiveDate::from_ymd_opt(year, month, day))
        .min_by_key(|date| (*date - today).num_days().abs())
}

/// `ordinal("14th")` is 14.
fn number_with_ordinal(word: &str) -> Option<u32> {
    let digits = ["st", "nd", "rd", "th"].iter().find_map(|suffix| word.strip_suffix(suffix)).unwrap_or(word);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn parse_words(text: &str, now: NaiveDateTime) -> Option<Parsed> {
    let today = now.date();
    let cleaned = text.replace(',', " ");
    let mut date: Option<NaiveDate> = None;
    let mut time: Option<NaiveTime> = None;
    let mut month: Option<u32> = None;
    let mut weekday: Option<Weekday> = None;
    let mut next = false;
    let mut numbers: Vec<u32> = Vec::new();
    let mut recognised = false;

    for word in cleaned.split_whitespace() {
        if matches!(word, "at" | "on" | "the" | "of") {
            continue;
        }
        recognised = true;
        match word {
            "today" => date = Some(today),
            "tomorrow" => date = Some(today + Duration::days(1)),
            "yesterday" => date = Some(today - Duration::days(1)),
            "next" => next = true,
            "this" => {}
            "noon" => time = NaiveTime::from_hms_opt(12, 0, 0),
            "midnight" => time = NaiveTime::from_hms_opt(0, 0, 0),
            "am" | "pm" => {
                let hour = numbers.pop()?;
                time = Some(meridiem_clock(hour, word == "pm")?);
            }
            _ => {
                if let Some(day_of_week) = weekday_of(word) {
                    weekday = Some(day_of_week);
                } else if let Some(m) = month_of(word) {
                    month = Some(m);
                } else if word.len() == 8 && word.bytes().all(|b| b.is_ascii_digit()) {
                    date = Some(NaiveDate::parse_from_str(word, "%Y%m%d").ok()?);
                } else if let Ok(iso) = NaiveDate::parse_from_str(word, "%Y-%m-%d") {
                    date = Some(iso);
                } else if let Some(clock) = parse_clock(word) {
                    time = Some(clock);
                } else if let Some(number) = number_with_ordinal(word) {
                    numbers.push(number);
                } else {
                    return None;
                }
            }
        }
    }
    if !recognised {
        return None;
    }

    if let Some(m) = month {
        let day = numbers.iter().copied().find(|n| (1..=31).contains(n));
        let year = numbers.iter().copied().find(|n| *n >= 1000);
        let day = day?;
        date = Some(match year {
            Some(year) => NaiveDate::from_ymd_opt(year as i32, m, day)?,
            None => nearest_year(m, day, today)?,
        });
    } else if !numbers.is_empty() {
        return None;
    }

    if let Some(target) = weekday {
        let ahead = (target.num_days_from_monday() as i64 - today.weekday().num_days_from_monday() as i64).rem_euclid(7);
        let ahead = if next && ahead == 0 { 7 } else { ahead };
        date = Some(today + Duration::days(ahead));
    }

    match (date, time) {
        (Some(day), Some(clock)) => Some(Parsed { at: day.and_time(clock), kind: Kind::Time }),
        (Some(day), None) => Some(Parsed { at: day.and_hms_opt(0, 0, 0)?, kind: Kind::Day }),
        (None, Some(clock)) => Some(Parsed { at: today.and_time(clock), kind: Kind::Time }),
        (None, None) => None,
    }
}

/// Reads a date query. `now` is the current local time, which relative queries count from.
pub fn parse(text: &str, now: NaiveDateTime) -> Option<Parsed> {
    let text = text.trim().to_lowercase();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    if text == "now" {
        return Some(Parsed { at: now, kind: Kind::Instant });
    }
    if let Some(at) = parse_relative(&text, now) {
        return Some(Parsed { at, kind: Kind::Instant });
    }
    parse_words(&text, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Thursday 15 October 2026, 10:30.
    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(10, 30, 0).unwrap()
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap()
    }

    fn day(y: i32, m: u32, d: u32) -> Parsed {
        Parsed { at: at(y, m, d, 0, 0), kind: Kind::Day }
    }

    fn time(y: i32, m: u32, d: u32, h: u32, min: u32) -> Parsed {
        Parsed { at: at(y, m, d, h, min), kind: Kind::Time }
    }

    fn instant(dt: NaiveDateTime) -> Parsed {
        Parsed { at: dt, kind: Kind::Instant }
    }

    fn p(text: &str) -> Parsed {
        parse(text, now()).unwrap_or_else(|| panic!("could not parse {text:?}"))
    }

    #[test]
    fn minutes_from_plain_numbers_and_units() {
        assert_eq!(parse_minutes("15"), Some(15));
        assert_eq!(parse_minutes("15m"), Some(15));
        assert_eq!(parse_minutes("15 min"), Some(15));
        assert_eq!(parse_minutes("90 minutes"), Some(90));
        assert_eq!(parse_minutes("1.5h"), Some(90));
        assert_eq!(parse_minutes("2 hours"), Some(120));
        assert_eq!(parse_minutes("1h30m"), Some(90));
        assert_eq!(parse_minutes("1h 30m"), Some(90));
        assert_eq!(parse_minutes("1 hour and 30 minutes"), Some(90));
        assert_eq!(parse_minutes("1 hour, 30 minutes"), Some(90));
        assert_eq!(parse_minutes("1 day"), Some(1440));
        assert_eq!(parse_minutes("  45  "), Some(45));
        assert_eq!(parse_minutes("1 HOUR AND 30 MINUTES"), Some(90));
    }

    #[test]
    fn minutes_rejects_anything_else() {
        for bad in ["", "abc", "1x", "h", "1 hour and", "and 1h", "-5", "1.2.3", "5 apples", "1h abc"] {
            assert_eq!(parse_minutes(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn relative_amounts_count_from_now() {
        assert_eq!(p("now"), instant(now()));
        assert_eq!(p("3h"), instant(at(2026, 10, 15, 13, 30)));
        assert_eq!(p("30m"), instant(at(2026, 10, 15, 11, 0)));
        assert_eq!(p("2d"), instant(at(2026, 10, 17, 10, 30)));
        assert_eq!(p("1w"), instant(at(2026, 10, 22, 10, 30)));
        assert_eq!(p("-2d"), instant(at(2026, 10, 13, 10, 30)));
        assert_eq!(p("+3h"), instant(at(2026, 10, 15, 13, 30)));
        assert_eq!(p("in 3 days"), instant(at(2026, 10, 18, 10, 30)));
        assert_eq!(p("3 hours ago"), instant(at(2026, 10, 15, 7, 30)));
        assert_eq!(p("1h30m"), instant(at(2026, 10, 15, 12, 0)));
        assert_eq!(p("1 day and 2 hours"), instant(at(2026, 10, 16, 12, 30)));
        assert_eq!(p("1.5h"), instant(at(2026, 10, 15, 12, 0)));
    }

    #[test]
    fn day_keywords_are_whole_days() {
        assert_eq!(p("today"), day(2026, 10, 15));
        assert_eq!(p("tomorrow"), day(2026, 10, 16));
        assert_eq!(p("yesterday"), day(2026, 10, 14));
        assert_eq!(p("  Tomorrow "), day(2026, 10, 16));
    }

    #[test]
    fn weekdays_are_the_next_one_counting_today() {
        // 15 October 2026 is a Thursday.
        assert_eq!(p("thursday"), day(2026, 10, 15));
        assert_eq!(p("friday"), day(2026, 10, 16));
        assert_eq!(p("fri"), day(2026, 10, 16));
        assert_eq!(p("monday"), day(2026, 10, 19));
        assert_eq!(p("wednesday"), day(2026, 10, 21));
        assert_eq!(p("this friday"), day(2026, 10, 16));
        assert_eq!(p("next friday"), day(2026, 10, 16));
        assert_eq!(p("next thursday"), day(2026, 10, 22));
        assert_eq!(p("on monday"), day(2026, 10, 19));
    }

    #[test]
    fn numeric_dates() {
        assert_eq!(p("2026-10-31"), day(2026, 10, 31));
        assert_eq!(p("20261031"), day(2026, 10, 31));
        assert_eq!(parse("2026-13-01", now()), None);
        assert_eq!(parse("20261345", now()), None);
    }

    #[test]
    fn month_names_with_days_ordinals_and_years() {
        assert_eq!(p("oct 14"), day(2026, 10, 14));
        assert_eq!(p("Oct 14th"), day(2026, 10, 14));
        assert_eq!(p("october 14th"), day(2026, 10, 14));
        assert_eq!(p("14 oct"), day(2026, 10, 14));
        assert_eq!(p("the 3rd of november"), day(2026, 11, 3));
        assert_eq!(p("oct 14 2027"), day(2027, 10, 14));
        assert_eq!(p("oct 14, 2025"), day(2025, 10, 14));
        assert_eq!(p("sept 1"), day(2026, 9, 1));
        assert_eq!(p("dec 25"), day(2026, 12, 25));
    }

    #[test]
    fn a_month_and_day_without_a_year_is_the_closest_one() {
        // From 15 October 2026: January and March are nearer next year (under five months
        // ahead, against more than seven months back), April is nearer this year (178 days back
        // against 187 ahead).
        assert_eq!(p("jan 5"), day(2027, 1, 5));
        assert_eq!(p("mar 1"), day(2027, 3, 1));
        assert_eq!(p("apr 20"), day(2026, 4, 20));
        assert_eq!(p("sep 1"), day(2026, 9, 1));
    }

    #[test]
    fn times_of_day() {
        assert_eq!(p("oct 14th 12am"), time(2026, 10, 14, 0, 0));
        assert_eq!(p("Oct 14th 11:59pm"), time(2026, 10, 14, 23, 59));
        // The form used in Godspeed's own example.
        assert_eq!(p("Oct 14th 1159pm"), time(2026, 10, 14, 23, 59));
        assert_eq!(p("oct 14 930am"), time(2026, 10, 14, 9, 30));
    }

    #[test]
    fn clock_formats() {
        assert_eq!(p("tomorrow 3pm"), time(2026, 10, 16, 15, 0));
        assert_eq!(p("tomorrow 3:30pm"), time(2026, 10, 16, 15, 30));
        assert_eq!(p("tomorrow 15:30"), time(2026, 10, 16, 15, 30));
        assert_eq!(p("tomorrow at 9am"), time(2026, 10, 16, 9, 0));
        assert_eq!(p("tomorrow 12pm"), time(2026, 10, 16, 12, 0));
        assert_eq!(p("tomorrow 12 am"), time(2026, 10, 16, 0, 0));
        assert_eq!(p("tomorrow noon"), time(2026, 10, 16, 12, 0));
        assert_eq!(p("tomorrow midnight"), time(2026, 10, 16, 0, 0));
        assert_eq!(p("3pm tomorrow"), time(2026, 10, 16, 15, 0));
        assert_eq!(p("friday 5pm"), time(2026, 10, 16, 17, 0));
    }

    #[test]
    fn a_time_alone_is_today() {
        assert_eq!(p("3pm"), time(2026, 10, 15, 15, 0));
        assert_eq!(p("noon"), time(2026, 10, 15, 12, 0));
        assert_eq!(p("14:30"), time(2026, 10, 15, 14, 30));
    }

    #[test]
    fn unreadable_dates_are_none() {
        for bad in ["", "   ", "soon", "someday", "oct", "oct 32", "13pm", "0am", "25:00", "tomorrow banana", "42", "feb 30 2026", "at"] {
            assert_eq!(parse(bad, now()), None, "{bad:?}");
        }
    }
}
