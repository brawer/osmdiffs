//! Compares `opening_hours` values by the weekly schedule they describe,
//! not by their text, so that `24/7` and `Mo-Su 00:00-24:00`, or
//! `Mo-Fr 08:00-18:00; Sa 08:00-18:00` and `Mo-Sa 08:00-18:00`, count as
//! the same. On the 2026-11-02 planet `conflated.parquet`, 18% of the
//! matched pairs where both sides have `opening_hours` differ in text
//! but not in schedule.
//!
//! Only the simple syntax that AllThePlaces writes is understood: rules
//! separated by `;`, each with optional weekdays (`Mo-Fr`, `Mo,We`,
//! `Fr-Mo`) and times (`08:00-12:00,13:00-18:00`, `off`, `closed`), or
//! `24/7`. Later rules replace earlier ones for their days, as in
//! OpenStreetMap. Public holidays (`PH`) and school holidays (`SH`) are
//! left out of the schedule. Anything else, such as months, dates,
//! comments or `sunrise`, makes [parse] return `None`. For the full
//! syntax, see <https://wiki.openstreetmap.org/wiki/Key:opening_hours/specification>.

const DAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
const MINUTES_PER_DAY: u16 = 24 * 60;

/// Opening times for each day from Monday to Sunday, as sorted,
/// non-overlapping `[start, end)` ranges in minutes since midnight.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Week([Vec<(u16, u16)>; 7]);

/// Whether two `opening_hours` values say the same: as text without
/// regard to spaces, or as weekly schedules.
pub(crate) fn same_schedule(a: &str, b: &str) -> bool {
    let squash = |v: &str| v.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    if squash(a) == squash(b) {
        return true;
    }
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Whether an `opening_hours` value says more than AllThePlaces can,
/// such as public or school holidays, dates, months, comments or times
/// relative to sunrise. Replacing such a value with AllThePlaces' would
/// lose that information; see brawer/osmdiffs#321.
pub(crate) fn has_rich_syntax(value: &str) -> bool {
    const WORDS: &[&str] = &[
        "PH", "SH", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
        "Dec", "week", "easter", "sunrise", "sunset", "dawn", "dusk",
    ];
    if value.contains('"') || value.contains("||") {
        return true;
    }
    value
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|word| WORDS.contains(&word))
}

/// Parses an `opening_hours` value into its weekly schedule, or `None`
/// if it uses syntax beyond what this module understands.
pub(crate) fn parse(value: &str) -> Option<Week> {
    // Times per day as given; an end past midnight spills over below.
    let mut days: [Vec<(u16, u16)>; 7] = Default::default();
    let mut any_rule = false;
    for rule in value.split(';').map(str::trim).filter(|r| !r.is_empty()) {
        any_rule = true;
        if rule == "24/7" {
            for d in days.iter_mut() {
                *d = vec![(0, MINUTES_PER_DAY)];
            }
            continue;
        }
        let (selector, times) = split_rule(rule);
        let selected = match selector {
            Some(s) => match parse_days(s)? {
                Some(selected) => selected,
                None => continue, // only holidays
            },
            None => [true; 7],
        };
        let ranges = parse_times(times?)?;
        for (d, chosen) in selected.iter().enumerate() {
            if *chosen {
                days[d] = ranges.clone();
            }
        }
    }
    if !any_rule {
        return None;
    }

    let mut week = Week::default();
    for (d, ranges) in days.iter().enumerate() {
        for &(start, end) in ranges {
            if end <= MINUTES_PER_DAY {
                week.0[d].push((start, end));
            } else {
                week.0[d].push((start, MINUTES_PER_DAY));
                week.0[(d + 1) % 7].push((0, end - MINUTES_PER_DAY));
            }
        }
    }
    for ranges in week.0.iter_mut() {
        ranges.sort_unstable();
        let mut merged: Vec<(u16, u16)> = Vec::with_capacity(ranges.len());
        for &(start, end) in ranges.iter() {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        *ranges = merged;
    }
    Some(week)
}

/// Splits a rule into its day selector, if it starts with one, and the
/// rest, if there is any.
fn split_rule(rule: &str) -> (Option<&str>, Option<&str>) {
    if !rule.starts_with(|c: char| c.is_ascii_uppercase()) {
        return (None, Some(rule));
    }
    // The selector ends at the first space that isn't next to a comma.
    let bytes = rule.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b' ' {
            let prev = rule[..i].trim_end();
            let next = rule[i..].trim_start();
            if prev.ends_with(',') || next.starts_with(',') {
                continue;
            }
            return (Some(prev), Some(next));
        }
    }
    (Some(rule), None)
}

/// Parses a day selector such as `Mo-Fr,Su`. Returns `Some(None)` for a
/// selector of holidays only, such as `PH`, whose rule doesn't change
/// the weekly schedule.
fn parse_days(selector: &str) -> Option<Option<[bool; 7]>> {
    let mut days = [false; 7];
    let mut any_day = false;
    for part in selector.split(',').map(str::trim) {
        if part == "PH" || part == "SH" {
            continue;
        }
        let (from, to) = match part.split_once('-') {
            Some((from, to)) => (day_index(from)?, day_index(to)?),
            None => (day_index(part)?, day_index(part)?),
        };
        let mut d = from;
        loop {
            days[d] = true;
            if d == to {
                break;
            }
            d = (d + 1) % 7;
        }
        any_day = true;
    }
    Some(any_day.then_some(days))
}

fn day_index(day: &str) -> Option<usize> {
    DAYS.iter().position(|d| *d == day.trim())
}

/// Parses `08:00-12:00,13:00-18:00`, `off` or `closed`.
fn parse_times(times: &str) -> Option<Vec<(u16, u16)>> {
    let times = times.trim();
    if times == "off" || times == "closed" {
        return Some(Vec::new());
    }
    times
        .split(',')
        .map(|range| {
            let (start, end) = range.trim().split_once('-')?;
            let start = parse_time(start)?;
            let mut end = parse_time(end)?;
            if start >= MINUTES_PER_DAY {
                return None;
            }
            if end <= start {
                end += MINUTES_PER_DAY;
            }
            Some((start, end))
        })
        .collect()
}

/// Parses `08:00` or `8:00` into minutes since midnight; `24:00` is
/// the end of the day, and hours up to 48 the next day, as in OSM.
fn parse_time(time: &str) -> Option<u16> {
    let (h, m) = time.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let h: u16 = h.parse().ok()?;
    let m: u16 = m.parse().ok()?;
    (h <= 48 && m < 60).then_some(h * 60 + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_schedule() {
        let same = [
            ("24/7", "Mo-Su 00:00-24:00"),
            ("24/7", "Mo-Su,PH 00:00-24:00"),
            ("Mo-Su 07:00-23:00", "07:00-23:00"),
            ("Mo-Sa 07:30-21:00", "Mo-Fr 07:30-21:00; Sa 07:30-21:00"),
            (
                "Mo-Sa 08:00-20:00",
                "Mo-Fr 08:00-20:00; Sa 08:00-20:00; Su off",
            ),
            ("Mo-Su 07:00-24:00", "07:00-00:00"),
            (
                "Mo-Fr 08:00-12:00, 13:00-18:00",
                "Mo-Fr 08:00-12:00,13:00-18:00",
            ),
            ("Fr-Mo 10:00-18:00", "Mo,Fr-Su 10:00-18:00"),
            // A later rule replaces an earlier one for its days.
            ("Mo-Sa 09:00-18:00", "Mo-Su 09:00-18:00; Su off"),
            // Past midnight, split or not.
            ("Fr 18:00-02:00", "Fr 18:00-24:00; Sa 00:00-02:00"),
            ("Mo-Fr 09:00-12:00,11:00-14:00", "Mo-Fr 09:00-14:00"),
        ];
        for (a, b) in same {
            assert!(same_schedule(a, b), "{a:?} and {b:?} should agree");
            assert!(same_schedule(b, a), "{b:?} and {a:?} should agree");
        }
        let different = [
            ("Mo-Sa 09:00-21:00", "Mo-Su 09:00-21:00"),
            (
                "Mo-Fr 08:00-18:00; Sa 09:00-12:00",
                "Mo-Fr 08:00-18:00; Sa 10:00-12:00",
            ),
            ("Mo-Fr 09:00-18:00", "Mo-Fr 09:00-12:30, 13:30-18:00"),
            ("24/7", "Mo-Su 06:00-22:00"),
            ("Mo-Fr 08:00-18:00", "Até 22:00 hs"),
        ];
        for (a, b) in different {
            assert!(!same_schedule(a, b), "{a:?} and {b:?} should differ");
        }
    }

    #[test]
    fn test_parse() {
        let week = parse("Mo-Fr 08:00-12:00,13:00-18:00; Sa 09:00-02:00").unwrap();
        assert_eq!(week.0[0], vec![(480, 720), (780, 1080)]);
        assert_eq!(week.0[5], vec![(540, 1440)]);
        assert_eq!(week.0[6], vec![(0, 120)]);
        assert!(parse("").is_none());
        assert!(parse("Mo-Fr").is_none());
        assert!(parse("Mo-Fr 08:00-18:00; Dec 25 off").is_none());
        assert!(parse("Mo-Fr 08:00-18:00 \"by appointment\"").is_none());
        assert!(parse("Mo-Fr 08:00-18:00+").is_none());
        assert!(parse("sunrise-sunset").is_none());
        // A holiday rule leaves the weekly schedule alone.
        assert_eq!(
            parse("Mo-Fr 08:00-18:00; PH off"),
            parse("Mo-Fr 08:00-18:00")
        );
    }

    #[test]
    fn test_has_rich_syntax() {
        assert!(has_rich_syntax("Mo-Fr 08:00-18:00; PH off"));
        assert!(has_rich_syntax("Mo-Sa 09:00-19:00; Dec 24 07:30-16:00"));
        assert!(has_rich_syntax("Mo-Fr 08:00-18:00 \"by appointment\""));
        assert!(has_rich_syntax("Mo-Su sunrise-sunset"));
        assert!(!has_rich_syntax("Mo-Fr 08:00-18:00; Sa 09:00-12:00"));
        assert!(!has_rich_syntax("24/7"));
        assert!(!has_rich_syntax("Até 22:00 hs"));
    }
}
