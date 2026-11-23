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

/// How two `opening_hours` values differ, for the `kinds` of
/// `spider-stats.parquet`; see brawer/osmdiffs#1000.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Difference {
    /// Different text, same schedule: they agree, in other notation.
    SameSchedule,
    /// Open on the same days, and on each day the times differ by at
    /// most [SMALL_MINUTES] in all.
    Small,
    /// Open on the same days, but on some day the times differ by more
    /// than [SMALL_MINUTES].
    Over60Min,
    /// Open on some day in one value and closed in the other.
    OtherDays,
    /// One of them doesn't [parse].
    Unparsed,
}

/// The most minutes a day by which a [Difference::Small] may differ,
/// counting the minutes open in only one of the two values.
pub(crate) const SMALL_MINUTES: u32 = 60;

impl Difference {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Difference::SameSchedule => "same_schedule",
            Difference::Small => "small",
            Difference::Over60Min => "over_60_min",
            Difference::OtherDays => "other_days",
            Difference::Unparsed => "unparsed",
        }
    }
}

/// How `a` and `b` differ, or `None` if they are the same text,
/// without regard to spaces. Values that [same_schedule] considers the
/// same are [Difference::SameSchedule].
pub(crate) fn difference(a: &str, b: &str) -> Option<Difference> {
    let squash = |v: &str| v.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    if squash(a) == squash(b) {
        return None;
    }
    let (Some(a), Some(b)) = (parse(a), parse(b)) else {
        return Some(Difference::Unparsed);
    };
    Some(week_difference(&a, &b).unwrap_or(Difference::SameSchedule))
}

/// The minutes that lie in exactly one of the sorted, non-overlapping
/// ranges `a` and `b`.
fn minutes_in_one(a: &[(u16, u16)], b: &[(u16, u16)]) -> u32 {
    let total = |r: &[(u16, u16)]| r.iter().map(|&(s, e)| u32::from(e - s)).sum::<u32>();
    let mut both = 0;
    for &(s1, e1) in a {
        for &(s2, e2) in b {
            let (s, e) = (s1.max(s2), e1.min(e2));
            if s < e {
                both += u32::from(e - s);
            }
        }
    }
    total(a) + total(b) - 2 * both
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

    Some(normalize(&days))
}

/// An `opening_hours` value split into its weekly schedule and the
/// rules on top of it, which hold for some days of the year only: on
/// public holidays (`PH off`), school holidays (`SH Mo-Fr 09:00-12:00`)
/// or fixed dates (`Dec 24 08:00-12:00`, `Dec 25,26 off`, `easter
/// off`). See [split].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Schedule {
    pub week: Week,
    /// The times of each day from Monday to Sunday before they spill
    /// over midnight, as the rules give them: to tell whether a
    /// public-holiday rule just repeats Sunday's, and to write times
    /// that a holiday shares with weekdays.
    pub days: [Vec<(u16, u16)>; 7],
    /// The times on public holidays, from the last rule that names
    /// `PH`, such as `PH off` or `Su,PH 10:00-16:00`.
    pub public_holidays: Option<Vec<(u16, u16)>>,
    /// The rules on top of the week, in their order. A rule that names
    /// weekdays and holidays, such as `Su,PH 10:00-16:00`, gives its
    /// week part to [Schedule::week] and its holiday part,
    /// `PH 10:00-16:00`, to this list.
    pub exceptions: Vec<Exception>,
}

/// A rule that holds on top of the week; see [Schedule].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Exception {
    /// The rule as written, or its holiday part.
    pub rule: String,
    /// Whether the rule names public holidays.
    pub public_holidays: bool,
    /// For the holiday part of a rule that also names weekdays, such
    /// as `Su,PH 10:00-16:00`: the holidays (`PH`) and the weekdays.
    pub shared: Option<(String, [bool; 7])>,
}

/// Splits an `opening_hours` value into its weekly schedule and the
/// rules on top of it, or `None` if it has a rule that changes the
/// week itself, such as seasons (`Apr-Oct`), week numbers, times
/// relative to `sunrise`, comments or fallback rules (`||`), if it has
/// no rule for weekdays at all, or if it doesn't parse. Unlike [parse],
/// which leaves holiday rules out, this keeps them, so that an edit
/// can carry them over.
pub(crate) fn split(value: &str) -> Option<Schedule> {
    if value.contains('"') || value.contains("||") {
        return None;
    }
    let mut days: [Vec<(u16, u16)>; 7] = Default::default();
    let mut any_week_rule = false;
    let mut public_holidays = None;
    let mut exceptions = Vec::new();
    let rules = value
        .split(';')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .flat_map(|r| additional_rules(r).into_iter().enumerate());
    for (k, rule) in rules {
        // An additional rule, after a comma, adds to the times of its
        // days rather than replacing them.
        let additive = k > 0;
        let set = |day: &mut Vec<(u16, u16)>, ranges: &[(u16, u16)]| {
            if !additive {
                day.clear();
            }
            day.extend_from_slice(ranges);
        };
        if rule == "24/7" {
            for d in days.iter_mut() {
                set(d, &[(0, MINUTES_PER_DAY)]);
            }
            any_week_rule = true;
            continue;
        }
        if is_date_rule(rule)? {
            exceptions.push(Exception {
                rule: rule.to_string(),
                public_holidays: false,
                shared: None,
            });
            continue;
        }
        let (selector, times) = split_rule(rule);
        let Some(selector) = selector else {
            let ranges = parse_times(times?)?;
            for d in days.iter_mut() {
                set(d, &ranges);
            }
            any_week_rule = true;
            continue;
        };
        let holidays: Vec<&str> = selector
            .split(',')
            .map(str::trim)
            .filter(|p| *p == "PH" || *p == "SH")
            .collect();
        let names_ph = holidays.contains(&"PH");
        match parse_days(selector)? {
            Some(selected) => {
                let ranges = parse_times(times?)?;
                for (d, chosen) in selected.iter().enumerate() {
                    if *chosen {
                        set(&mut days[d], &ranges);
                    }
                }
                any_week_rule = true;
                if !holidays.is_empty() {
                    exceptions.push(Exception {
                        rule: format!("{} {}", holidays.join(","), times?.trim()),
                        public_holidays: names_ph,
                        shared: Some((holidays.join(","), selected)),
                    });
                    if names_ph {
                        public_holidays = Some(ranges);
                    }
                }
            }
            None => {
                // Holidays only, maybe narrowed down by weekdays, as
                // in `SH Mo-Fr 09:00-12:00`.
                let times = times?;
                let (weekdays, times) = match split_rule(times) {
                    (Some(w), Some(t)) => (Some(w), t),
                    _ => (None, times),
                };
                if let Some(w) = weekdays {
                    parse_days(w)??;
                }
                let ranges = parse_times(times)?;
                if names_ph && weekdays.is_none() {
                    public_holidays = Some(ranges);
                }
                exceptions.push(Exception {
                    rule: rule.to_string(),
                    public_holidays: names_ph,
                    shared: None,
                });
            }
        }
    }
    if !any_week_rule {
        return None;
    }
    Some(Schedule {
        week: normalize(&days),
        days,
        public_holidays,
        exceptions,
    })
}

/// Splits a rule at the commas that start an additional rule, as in
/// `Mo-Fr 08:00-12:00, Sa 09:00-12:00`: those followed by a day or
/// holiday rather than a time. Commas within a selector (`Mo,We
/// 08:00-12:00`) or between times (`08:00-12:00, 13:00-18:00`) stay.
fn additional_rules(rule: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, _) in rule.match_indices(',') {
        let before = &rule[start..i];
        let after = rule[i + 1..].trim_start();
        let has_times =
            before.contains(':') || before.ends_with("off") || before.ends_with("closed");
        if has_times && after.starts_with(|c: char| c.is_ascii_uppercase()) {
            parts.push(before.trim());
            start = i + 1;
        }
    }
    parts.push(rule[start..].trim());
    parts
}

/// Whether `rule` is for fixed dates, such as `Dec 24 08:00-12:00`,
/// `Dec 24-26 off`, `Dec 25,Jan 01 off` or `easter off`, or `None` if it
/// is about dates in a way that changes the week, such as a season
/// (`Apr-Oct Mo-Fr 10:00-18:00`), a long stretch of dates that isn't
/// closed (`May 01-Sep 30 10:00-18:00`), or a year.
fn is_date_rule(rule: &str) -> Option<bool> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let first = rule
        .split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or("");
    let rest = if first == "easter" {
        rule["easter".len()..].trim()
    } else if MONTHS.contains(&first) {
        // Dates up to the first space that isn't followed by a day.
        let mut end = rule.len();
        for (i, c) in rule.char_indices() {
            if c == ' ' {
                let next = rule[i + 1..].trim_start();
                let after_month = rule[..i]
                    .trim_end()
                    .ends_with(|c: char| c.is_ascii_alphabetic());
                if after_month && next.starts_with(|c: char| c.is_ascii_digit()) {
                    continue;
                }
                end = i;
                break;
            }
        }
        let dates = &rule[..end];
        let ranges = dates.contains('-');
        for item in dates.split(',') {
            for date in item.split('-') {
                let date = date.trim();
                let (month, day) = match date.split_once(' ') {
                    Some((m, d)) => (Some(m), d),
                    None => (None, date),
                };
                if month.is_some_and(|m| !MONTHS.contains(&m)) {
                    return None;
                }
                // A month without a day is a season.
                let day: u8 = day.trim().parse().ok()?;
                if !(1..=31).contains(&day) {
                    return None;
                }
            }
        }
        let rest = rule[end..].trim();
        if ranges && !(rest == "off" || rest == "closed") {
            return None;
        }
        rest
    } else {
        return Some(false);
    };
    if rest.is_empty() {
        return None;
    }
    parse_times(rest)?;
    Some(true)
}

/// Spills times past midnight over to the next day and merges
/// overlapping ranges.
fn normalize(days: &[Vec<(u16, u16)>; 7]) -> Week {
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
    week
}

/// How two weekly schedules differ, or `None` if they are the same.
pub(crate) fn week_difference(a: &Week, b: &Week) -> Option<Difference> {
    if a == b {
        return None;
    }
    if (0..7).any(|d| a.0[d].is_empty() != b.0[d].is_empty()) {
        return Some(Difference::OtherDays);
    }
    let most = (0..7)
        .map(|d| minutes_in_one(&a.0[d], &b.0[d]))
        .max()
        .unwrap_or(0);
    Some(if most <= SMALL_MINUTES {
        Difference::Small
    } else {
        Difference::Over60Min
    })
}

/// Writes a weekly schedule in the usual notation: `24/7`, or one rule
/// per set of days with the same times, such as `Mo-Fr 08:00-18:00;
/// Sa 09:00-12:00`. Times past midnight are written as such, `Fr
/// 18:00-02:00`, rather than as two days. [parse] reads it back as the
/// same week.
pub(crate) fn format_week(week: &Week) -> String {
    let full = vec![(0, MINUTES_PER_DAY)];
    if week.0.iter().all(|d| *d == full) {
        return "24/7".to_string();
    }
    let days = days_as_written(week);
    let mut rules: Vec<(Vec<usize>, String)> = Vec::new();
    for (d, ranges) in days.iter().enumerate() {
        if ranges.is_empty() {
            continue;
        }
        let t = format_times(ranges);
        match rules.iter_mut().find(|(_, other)| *other == t) {
            Some((ds, _)) => ds.push(d),
            None => rules.push((vec![d], t)),
        }
    }
    if rules.is_empty() {
        return "off".to_string();
    }
    rules
        .iter()
        .map(|(ds, t)| {
            if ds.len() == 7 {
                return format!("Mo-Su {t}");
            }
            let mut parts = Vec::new();
            let mut i = 0;
            while i < ds.len() {
                let mut j = i;
                while j + 1 < ds.len() && ds[j + 1] == ds[j] + 1 {
                    j += 1;
                }
                match j - i {
                    0 => parts.push(DAYS[ds[i]].to_string()),
                    1 => {
                        parts.push(DAYS[ds[i]].to_string());
                        parts.push(DAYS[ds[j]].to_string());
                    }
                    _ => parts.push(format!("{}-{}", DAYS[ds[i]], DAYS[ds[j]])),
                }
                i = j + 1;
            }
            format!("{} {t}", parts.join(","))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The times of each day as [format_week] writes them: a range that
/// runs to midnight is joined with the next day's from midnight, so
/// that `Fr 18:00-02:00` is Friday's, not split over two days.
pub(crate) fn days_as_written(week: &Week) -> [Vec<(u16, u16)>; 7] {
    let full = vec![(0, MINUTES_PER_DAY)];
    // Join a day's last range that runs to midnight with the next
    // day's first one from midnight, unless that day is open all day.
    let mut days: [Vec<(u16, u16)>; 7] = week.0.clone();
    for d in 0..7 {
        let next = (d + 1) % 7;
        let Some(&(start, MINUTES_PER_DAY)) = days[d].last() else {
            continue;
        };
        if start == 0 || days[next] == full {
            continue;
        }
        if let Some(&(0, e)) = days[next].first() {
            days[d].last_mut().unwrap().1 = MINUTES_PER_DAY + e;
            days[next].remove(0);
        }
    }
    days
}

/// Writes times such as those of [Schedule::public_holidays]:
/// `10:00-14:00` or `off`.
pub(crate) fn format_times(ranges: &[(u16, u16)]) -> String {
    if ranges.is_empty() {
        return "off".to_string();
    }
    let time = |m: u16| format!("{:02}:{:02}", m / 60, m % 60);
    ranges
        .iter()
        .map(|&(s, e)| {
            let e = if e > MINUTES_PER_DAY {
                e - MINUTES_PER_DAY
            } else {
                e
            };
            format!("{}-{}", time(s), time(e))
        })
        .collect::<Vec<_>>()
        .join(",")
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
    fn test_difference() {
        use Difference::*;
        let cases = [
            ("Mo-Fr 08:00-18:00", " Mo-Fr 08:00-18:00", None),
            ("24/7", "Mo-Su 00:00-24:00", Some(SameSchedule)),
            ("Mo-Fr 08:00-18:00", "Mo-Fr 08:30-18:30", Some(Small)),
            // An hour in all, on one day.
            (
                "Mo-Fr 08:00-18:00",
                "Mo-Fr 08:00-18:00; Fr 08:00-19:00",
                Some(Small),
            ),
            ("Mo-Fr 08:00-18:00", "Mo-Fr 07:30-18:31", Some(Over60Min)),
            (
                "Mo-Fr 09:00-18:00",
                "Mo-Fr 09:00-12:30,13:30-18:00",
                Some(Small),
            ),
            ("Mo-Sa 09:00-21:00", "Mo-Su 09:00-21:00", Some(OtherDays)),
            ("Mo-Fr 08:00-18:00", "Até 22:00 hs", Some(Unparsed)),
        ];
        for (a, b, want) in cases {
            assert_eq!(difference(a, b), want, "{a:?} and {b:?}");
            assert_eq!(difference(b, a), want, "{b:?} and {a:?}");
        }
        // Whatever differs is no longer the same schedule.
        for (a, b) in [
            ("Mo-Sa 09:00-21:00", "Mo-Su 09:00-21:00"),
            ("24/7", "Mo-Su 06:00-22:00"),
        ] {
            assert!(!same_schedule(a, b));
            assert_ne!(difference(a, b), Some(SameSchedule));
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
    fn test_split() {
        let s = split("Mo-Sa 08:00-20:00; Su,PH 10:00-16:00; Dec 24 08:00-12:00").unwrap();
        assert_eq!(s.week, parse("Mo-Sa 08:00-20:00; Su 10:00-16:00").unwrap());
        assert_eq!(s.days[6], vec![(600, 960)]);
        let mut sunday = [false; 7];
        sunday[6] = true;
        assert_eq!(s.exceptions[0].shared, Some(("PH".to_string(), sunday)));
        assert_eq!(s.public_holidays, Some(vec![(600, 960)]));
        let rules: Vec<&str> = s.exceptions.iter().map(|e| e.rule.as_str()).collect();
        assert_eq!(rules, ["PH 10:00-16:00", "Dec 24 08:00-12:00"]);
        assert!(s.exceptions[0].public_holidays);
        assert!(!s.exceptions[1].public_holidays);

        let s = split("24/7; PH off").unwrap();
        assert_eq!(s.public_holidays, Some(vec![]));
        let s = split("Mo-Fr 08:00-18:00; SH Mo-Fr 09:00-12:00; PH,SH off").unwrap();
        assert_eq!(s.public_holidays, Some(vec![]));
        assert_eq!(s.exceptions.len(), 2);
        // Additional rules, after a comma, add to their days.
        let week = |v: &str| split(v).map(|s| s.week);
        assert_eq!(
            week("Mo-Fr 09:00-20:00, Sa 09:00-18:00"),
            parse("Mo-Fr 09:00-20:00; Sa 09:00-18:00")
        );
        assert_eq!(
            week("Mo,Tu,Th 10:00-12:30, We,Fr 10:00-14:00, Mo,Tu,Th 14:00-18:00; Sa,Su,PH off"),
            parse("Mo,Tu,Th 10:00-12:30,14:00-18:00; We,Fr 10:00-14:00")
        );
        let s = split("Mo-Fr 10:00-20:00, PH off").unwrap();
        assert_eq!(s.exceptions[0].rule, "PH off");
        assert_eq!(
            week("Mo-Fr 08:00-12:00, 13:00-18:00"),
            parse("Mo-Fr 08:00-12:00,13:00-18:00")
        );
        for value in [
            "Mo-Fr 08:00-18:00; Dec 25,26 off",
            "Mo-Fr 08:00-18:00; Dec 24-26 off",
            "Mo-Fr 08:00-18:00; Dec 24-Jan 02 off",
            "Mo-Fr 08:00-18:00; Dec 25,Jan 01 off",
            "Mo-Fr 08:00-18:00; easter off",
            "08:00-18:00",
        ] {
            assert!(split(value).is_some(), "{value}");
        }
        for value in [
            "",
            "PH off",
            "Dec 25 off",
            "Apr-Oct Mo-Su 10:00-18:00; Nov-Mar Mo-Fr 10:00-16:00",
            "Mo-Fr 08:00-18:00; May 01-Sep 30 Sa 10:00-12:00",
            "Mo-Fr 08:00-18:00; May 01-Sep 30 10:00-12:00",
            "Mo-Fr 08:00-18:00; 2026 Dec 24 off",
            "Mo-Fr 08:00-18:00; week 01-10 Sa 09:00-12:00",
            "Mo-Fr 08:00-18:00 \"by appointment\"",
            "Mo-Fr 08:00-18:00 || \"by appointment\"",
            "Mo-Su sunrise-sunset",
            "Mo-Fr 08:00-18:00; PH +1 day off",
            "Mo-Fr 08:00-18:00; easter +1 day off",
            "Até 22:00 hs",
        ] {
            assert!(split(value).is_none(), "{value}");
        }
    }

    #[test]
    fn test_format_week() {
        let cases = [
            ("Mo-Su 00:00-24:00", "24/7"),
            ("Mo-Fr 08:00-18:00; Sa 08:00-18:00", "Mo-Sa 08:00-18:00"),
            ("Mo 08:00-12:00; We 08:00-12:00", "Mo,We 08:00-12:00"),
            ("Sa,Su 10:00-16:00", "Sa,Su 10:00-16:00"),
            (
                "Mo-Fr 08:00-12:00,13:00-18:00; Sa 09:00-12:00; Su 09:00-12:00",
                "Mo-Fr 08:00-12:00,13:00-18:00; Sa,Su 09:00-12:00",
            ),
            ("Mo-Su 07:00-23:00; We off", "Mo,Tu,Th-Su 07:00-23:00"),
            ("Fr 18:00-02:00", "Fr 18:00-02:00"),
            ("Mo-Su 18:00-02:00", "Mo-Su 18:00-02:00"),
            ("Mo-Su 07:00-24:00", "Mo-Su 07:00-24:00"),
            (
                "Mo-Fr 00:00-24:00; Sa 08:00-12:00",
                "Mo-Fr 00:00-24:00; Sa 08:00-12:00",
            ),
            ("Mo-Su off", "off"),
        ];
        for (value, want) in cases {
            let week = parse(value).unwrap();
            let got = format_week(&week);
            assert_eq!(got, want, "{value}");
            assert_eq!(parse(&got), Some(week), "{value} reads back");
        }
    }
}
