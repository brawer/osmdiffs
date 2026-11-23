//! Edits of `opening_hours`: AllThePlaces' weekly schedule, on top of
//! which we keep OSM's rules for holidays and fixed dates. See "Modify:
//! opening hours" and "Recognizing minor differences" in
//! `docs/notes/2026-11-23-edits-parquet.md`.
//!
//! The rule is that we never propose an edit that only rewrites a value
//! in other notation, and an edit never removes detail that OSM has and
//! AllThePlaces lacks:
//!
//! - Values are compared as weekly schedules, so `Mo-Fr 08:00-18:00;
//!   Sa 08:00-18:00` agrees with `Mo-Sa 08:00-18:00`, and `24/7` with
//!   `Mo-Su 00:00-24:00`.
//! - OSM's rules for public and school holidays (`PH off`, `SH Mo-Fr
//!   09:00-12:00`) and fixed dates (`Dec 24 08:00-12:00`, `easter off`)
//!   are exceptions on top of the week. They are kept verbatim, after
//!   AllThePlaces' week.
//! - AllThePlaces' `PH` rule counts only if it differs from its Sunday:
//!   many sources summarize public holidays as Sundays, so `Su,PH
//!   10:00-16:00` says nothing about OSM's `PH off`.
//! - An OSM value with rules that change the week itself, such as
//!   seasons, comments or `sunrise`, or one we can't parse, gets no
//!   machine-made value: the change has the reason `needs_review`, and
//!   is left for a person.
//!
//! Only spiders whose opening hours agree with OSM often enough on
//! matched pairs, and that don't give one schedule for every branch,
//! make edits; see [Gate].

use super::{Change, Reason, Tags};
use crate::utils::opening_hours::{
    Difference, Exception, Schedule, days_as_written, format_times, format_week, split,
    week_difference,
};
use anyhow::{Context, Result};
use arrow::array::{Array, Float32Array, ListArray, StringArray, StructArray, UInt64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

const KEY: &str = "opening_hours";

/// The fewest matched pairs with opening hours on both sides, ignoring
/// values that don't parse, on which a spider must be judged before
/// its opening hours make edits.
pub(super) const MIN_COMPARED: u64 = 10;

/// The smallest share of those pairs that must agree, in schedule or
/// within an hour a day ([Difference::Small]). On the 2026-11-02 planet
/// file, the median spider has 0.85. Spiders that give one schedule for
/// every branch, or the hours of a drive-through for the restaurant,
/// fall below: `wendys` has 0.09, `taco_bell_us` 0.37,
/// `total_energies` 0.35.
pub(super) const MIN_AGREEMENT: f64 = 0.7;

/// The largest share of a spider's features with `opening_hours` that
/// may have the same value. A spider above it gives one schedule for
/// every branch, which says nothing about the one place, even where it
/// agrees with OSM: on the 2026-11-02 planet file, `maes_dkv` (32,267
/// fuel stations) and `shell` (31,114) say `24/7` for all of them. This
/// also screens out chains that are open around the clock everywhere,
/// such as `wawa`, but OSM mostly has their hours already.
pub(super) const MAX_TOP_SHARE: f32 = 0.98;

/// Which spiders may edit opening hours, from the `tags.kinds` of
/// `spider-stats.parquet`.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    trusted: HashSet<String>,
}

impl Gate {
    pub(crate) fn allows(&self, spider: &str) -> bool {
        self.trusted.contains(spider)
    }

    /// Reads the opening-hours agreement of each spider.
    pub(crate) fn open(spider_stats: &Path) -> Result<Gate> {
        let file =
            File::open(spider_stats).with_context(|| format!("could not open {spider_stats:?}"))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
        let mut gate = Gate::default();
        let (mut passed, mut failed, mut too_few, mut template) = (0u64, 0u64, 0u64, 0u64);
        for batch in reader {
            let batch = batch?;
            let spiders = batch
                .column_by_name("spider")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .context("no column spider")?;
            let tags = batch
                .column_by_name("tags")
                .and_then(|c| c.as_any().downcast_ref::<ListArray>())
                .context("no column tags")?;
            let top_share = batch
                .column_by_name("opening_hours_top_share")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                .context("no column opening_hours_top_share")?;
            for row in 0..batch.num_rows() {
                let mut counts = opening_hours_counts(&tags.value(row))?;
                counts.top_share = top_share.is_valid(row).then(|| top_share.value(row));
                match counts.verdict() {
                    Verdict::Pass => {
                        passed += 1;
                        gate.trusted.insert(spiders.value(row).to_string());
                    }
                    Verdict::Fail => failed += 1,
                    Verdict::TooFew => too_few += 1,
                    Verdict::Template => template += 1,
                }
            }
        }
        log::info!(
            passed = passed,
            failed = failed,
            too_few = too_few,
            template = template;
            "make_edits: spiders allowed to edit opening_hours"
        );
        Ok(gate)
    }

    #[cfg(test)]
    pub(crate) fn trusting(spiders: &[&str]) -> Gate {
        Gate {
            trusted: spiders.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// How a spider's `opening_hours` compare with OSM's on matched pairs.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Counts {
    pub agreeing: u64,
    pub differing: u64,
    pub small: u64,
    pub unparsed: u64,
    /// `opening_hours_top_share`: how many of the spider's features
    /// with opening hours have its most common value.
    pub top_share: Option<f32>,
}

#[derive(Debug, PartialEq)]
pub(super) enum Verdict {
    Pass,
    Fail,
    TooFew,
    Template,
}

impl Counts {
    pub(super) fn verdict(&self) -> Verdict {
        if self.top_share.is_some_and(|share| share > MAX_TOP_SHARE) {
            return Verdict::Template;
        }
        let compared = (self.agreeing + self.differing).saturating_sub(self.unparsed);
        if compared < MIN_COMPARED {
            return Verdict::TooFew;
        }
        let good = self.agreeing + self.small;
        if good as f64 >= MIN_AGREEMENT * compared as f64 {
            Verdict::Pass
        } else {
            Verdict::Fail
        }
    }
}

/// The counts of the `opening_hours` entry in one spider's `tags`.
fn opening_hours_counts(tags: &dyn Array) -> Result<Counts> {
    let tags = tags
        .as_any()
        .downcast_ref::<StructArray>()
        .context("tags is no struct")?;
    let u64s = |name: &str| {
        tags.column_by_name(name)
            .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
            .with_context(|| format!("no column tags.{name}"))
    };
    let tag = tags
        .column_by_name("tag")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .context("no column tags.tag")?;
    let (agreeing, differing) = (u64s("agreeing")?, u64s("differing")?);
    let kinds = tags
        .column_by_name("kinds")
        .and_then(|c| c.as_any().downcast_ref::<ListArray>())
        .context("no column tags.kinds")?;
    let mut counts = Counts::default();
    for i in 0..tags.len() {
        if tag.value(i) != KEY {
            continue;
        }
        counts.agreeing = agreeing.value(i);
        counts.differing = differing.value(i);
        let kinds = kinds.value(i);
        let kinds = kinds
            .as_any()
            .downcast_ref::<StructArray>()
            .context("kinds is no struct")?;
        let kind = kinds
            .column_by_name("kind")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .context("no column kinds.kind")?;
        let rows = kinds
            .column_by_name("rows")
            .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
            .context("no column kinds.rows")?;
        for k in 0..kinds.len() {
            match kind.value(k) {
                "small" => counts.small = rows.value(k),
                "unparsed" => counts.unparsed = rows.value(k),
                _ => {}
            }
        }
    }
    Ok(counts)
}

/// AllThePlaces' opening hours, as an edit uses them: the week, and
/// the times on public holidays if they aren't just Sunday's.
#[derive(Clone, Debug, PartialEq)]
struct AtpHours {
    schedule: Schedule,
    public_holidays: Option<Vec<(u16, u16)>>,
}

/// AllThePlaces' hours from `value`, or `None` if they don't parse,
/// or if they say closed all week: a closed place is for a delete, not
/// for `opening_hours=off`.
fn atp_hours(value: &str) -> Option<AtpHours> {
    let schedule = split(value)?;
    if schedule.days.iter().all(Vec::is_empty) {
        return None;
    }
    let public_holidays = schedule
        .public_holidays
        .clone()
        .filter(|ph| *ph != schedule.days[6] && *ph != days_as_written(&schedule.week)[6]);
    Some(AtpHours {
        schedule,
        public_holidays,
    })
}

fn get<'a>(tags: &'a Tags, key: &str) -> Option<&'a str> {
    tags.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

fn squash(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

/// An OSM holiday rule as it goes into the edit. One that OSM writes
/// together with weekdays, as in `Su,PH 10:00-16:00`, says "as on
/// those days": when AllThePlaces gives those days other times, the
/// holidays follow, if the days all have the same times, or else
/// Sunday's if the rule names Sunday. Otherwise, the rule is kept as
/// OSM has it.
fn follow_week(exception: &Exception, osm: &Schedule, atp: &Schedule) -> String {
    let Some((holidays, selected)) = &exception.shared else {
        return exception.rule.clone();
    };
    let days: Vec<usize> = (0..7).filter(|&d| selected[d]).collect();
    let (osm_days, atp_days) = (days_as_written(&osm.week), days_as_written(&atp.week));
    if days.iter().all(|&d| osm_days[d] == atp_days[d]) {
        return exception.rule.clone();
    }
    let first = &atp_days[days[0]];
    let times = if !first.is_empty() && days.iter().all(|&d| atp_days[d] == *first) {
        first
    } else if selected[6] && !atp_days[6].is_empty() {
        &atp_days[6]
    } else {
        return exception.rule.clone();
    };
    format!("{holidays} {}", format_times(times))
}

/// The `opening_hours` change for an OSM feature with tags `osm`, from
/// the tags of the AllThePlaces features that matched it as `same`,
/// each with its spider. Sources whose spider the [Gate] doesn't allow
/// are left out, as are values that don't parse.
pub(super) fn opening_hours_changes(
    osm: &Tags,
    sources: &[(&str, &Tags)],
    gate: &Gate,
    warnings: &mut Vec<String>,
) -> Vec<Change> {
    let mut atp: Option<(&str, AtpHours)> = None;
    for (spider, tags) in sources {
        if !gate.allows(spider) {
            continue;
        }
        let Some(value) = get(tags, KEY) else {
            continue;
        };
        let Some(hours) = atp_hours(value) else {
            continue;
        };
        match &atp {
            None => atp = Some((value, hours)),
            Some((_, first))
                if first.schedule.week == hours.schedule.week
                    && first.public_holidays == hours.public_holidays => {}
            Some(_) => {
                warnings.push(format!("sources_disagree:{KEY}"));
                return Vec::new();
            }
        }
    }
    let Some((atp_value, atp)) = atp else {
        return Vec::new();
    };
    let ph_rule = |ranges: &[(u16, u16)]| format!("PH {}", format_times(ranges));

    let Some(old) = get(osm, KEY) else {
        let exceptions: Vec<String> = atp.public_holidays.iter().map(|ph| ph_rule(ph)).collect();
        let value = join_rules(&format_week(&atp.schedule.week), exceptions);
        return vec![Change::new(KEY, None, Some(value), Reason::MissingInOsm)];
    };
    if squash(old) == squash(atp_value) {
        return Vec::new();
    }
    let Some(osm_hours) = split(old) else {
        // Seasons, comments, `sunrise` or what we can't parse: a
        // person must look.
        let mut change = Change::new(KEY, Some(old), Some(old.to_string()), Reason::NeedsReview);
        change.atp = Some(atp_value.to_string());
        return vec![change];
    };

    let mut exceptions: Vec<String> = Vec::new();
    match &atp.public_holidays {
        Some(ph) if osm_hours.public_holidays.as_ref() != Some(ph) => {
            exceptions.extend(
                osm_hours
                    .exceptions
                    .iter()
                    .filter(|e| !e.public_holidays)
                    .map(|e| e.rule.clone()),
            );
            exceptions.push(ph_rule(ph));
        }
        _ => exceptions.extend(
            osm_hours
                .exceptions
                .iter()
                .map(|e| follow_week(e, &osm_hours, &atp.schedule)),
        ),
    }
    let holidays_changed = exceptions
        .iter()
        .map(|e| squash(e))
        .ne(osm_hours.exceptions.iter().map(|e| squash(&e.rule)));
    let reason = match week_difference(&osm_hours.week, &atp.schedule.week) {
        None if !holidays_changed => return Vec::new(),
        None => Reason::PublicHolidays,
        Some(Difference::Small) => Reason::Small,
        Some(Difference::Over60Min) => Reason::Over60Min,
        Some(_) => Reason::OtherDays,
    };
    let value = join_rules(&format_week(&atp.schedule.week), exceptions);
    vec![Change::new(KEY, Some(old), Some(value), reason)]
}

/// Writes the week and the rules on top of it. When the first of
/// those is for holidays only and has the times of the week's last
/// rule, the two are written as one, `Su,PH 10:00-16:00` rather than
/// `Su 10:00-16:00; PH 10:00-16:00`. Only the last rule of the week
/// may take the holidays, since a later rule for weekdays would
/// override it on a holiday that falls on one of its days.
fn join_rules(week: &str, mut exceptions: Vec<String>) -> String {
    let mut rules: Vec<String> = week.split("; ").map(str::to_string).collect();
    let holidays = exceptions
        .first()
        .and_then(|e| e.split_once(' '))
        .filter(|(selector, _)| selector.split(',').all(|h| h == "PH" || h == "SH"));
    let last = rules.last().and_then(|r| r.split_once(' '));
    if let (Some((selector, times)), Some((days, last_times))) = (holidays, last)
        && times == last_times
        && days.starts_with(|c: char| c.is_ascii_uppercase())
    {
        let joined = format!("{days},{selector} {times}");
        *rules.last_mut().unwrap() = joined;
        exceptions.remove(0);
    }
    rules.extend(exceptions);
    rules.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(value: Option<&str>) -> Tags {
        value
            .map(|v| vec![(KEY.to_string(), v.to_string())])
            .unwrap_or_default()
    }

    /// The change for OSM's value and ATP's, with spider `s` trusted.
    fn change(osm: Option<&str>, atp: &str) -> Option<Change> {
        let atp = tags(Some(atp));
        let mut warnings = Vec::new();
        let mut changes = opening_hours_changes(
            &tags(osm),
            &[("s", &atp)],
            &Gate::trusting(&["s"]),
            &mut warnings,
        );
        assert!(warnings.is_empty());
        assert!(changes.len() <= 1);
        changes.pop()
    }

    fn new_value(osm: Option<&str>, atp: &str) -> Option<String> {
        change(osm, atp).and_then(|c| c.new)
    }

    /// The table in "Recognizing minor differences" of the plan, and
    /// in `docs/outputs/EDITS_PARQUET.md`.
    #[test]
    fn equivalence_table() {
        let cases: &[(&str, &str, &str, Option<&str>)] = &[
            (
                "notation only",
                "Mo-Fr 08:00-18:00; Sa 08:00-18:00",
                "Mo-Sa 08:00-18:00",
                None,
            ),
            ("always open", "24/7", "Mo-Su 00:00-24:00", None),
            (
                "OSM has PH, ATP doesn't",
                "Mo-Sa 08:00-20:00; Su,PH 10:00-16:00",
                "Mo-Sa 08:00-20:00; Su 10:00-16:00",
                None,
            ),
            (
                "ATP folds PH into Sunday",
                "Mo-Sa 08:00-20:00; Su 10:00-16:00; PH off",
                "Mo-Sa 08:00-20:00; Su,PH 10:00-16:00",
                None,
            ),
            (
                "week differs, OSM has PH",
                "Mo-Fr 08:00-18:00; PH off",
                "Mo-Fr 07:30-18:30",
                Some("Mo-Fr 07:30-18:30; PH off"),
            ),
            (
                "ATP has its own PH rule",
                "Mo-Fr 08:00-18:00",
                "Mo-Fr 08:00-18:00; PH 10:00-14:00",
                Some("Mo-Fr 08:00-18:00; PH 10:00-14:00"),
            ),
            (
                "fixed dates, same week",
                "Mo-Fr 08:00-18:00; Dec 24 off",
                "Mo-Fr 08:00-18:00",
                None,
            ),
            (
                "fixed dates, week differs",
                "Mo-Fr 08:00-18:00; Dec 24 08:00-12:00; Dec 25,26 off",
                "Mo-Fr 08:00-19:00",
                Some("Mo-Fr 08:00-19:00; Dec 24 08:00-12:00; Dec 25,26 off"),
            ),
            (
                "PH with weekday, ATP closed that day",
                "Mo-Fr 08:00-18:00; Tu,PH 09:00-11:00",
                "Mo,We-Fr 08:00-18:00",
                Some("Mo,We-Fr 08:00-18:00; PH 09:00-11:00"),
            ),
            (
                "PH as on Sunday, Sunday differs",
                "Mo-Sa 08:00-20:00; Su,PH 10:00-16:00",
                "Mo-Sa 08:00-20:00; Su 10:00-18:00",
                Some("Mo-Sa 08:00-20:00; Su,PH 10:00-18:00"),
            ),
            (
                "PH as on every day, week differs",
                "Mo-Su,PH 10:00-22:00",
                "Mo-Th,Su 09:00-22:00; Fr,Sa 09:00-23:30",
                Some("Mo-Th,Su 09:00-22:00; Fr,Sa 09:00-23:30; PH 09:00-22:00"),
            ),
            (
                "PH as on every day, past midnight, ATP in other notation",
                "Mo-Su,PH 07:00-01:00",
                "Mo 00:00-01:00,07:00-24:00; Tu-Su 00:00-01:00,07:00-24:00",
                None,
            ),
            (
                "PH as on every day, past midnight, week differs",
                "Mo-Su,PH 07:00-01:00",
                "Mo-Su 07:00-02:00",
                Some("Mo-Su,PH 07:00-02:00"),
            ),
            (
                "school holidays kept",
                "Mo-Fr 08:00-18:00; SH Mo-Fr 09:00-12:00",
                "Mo-Fr 07:00-18:00",
                Some("Mo-Fr 07:00-18:00; SH Mo-Fr 09:00-12:00"),
            ),
        ];
        for (case, osm, atp, want) in cases {
            assert_eq!(
                new_value(Some(osm), atp).as_deref(),
                *want,
                "{case}: OSM {osm:?}, ATP {atp:?}"
            );
        }
    }

    #[test]
    fn rules_that_change_the_week_need_review() {
        for osm in [
            "Apr-Oct Mo-Su 10:00-18:00; Nov-Mar Mo-Fr 10:00-16:00",
            "Mo-Fr 08:00-18:00 \"by appointment\"",
            "Mo-Su sunrise-sunset",
            "May 01-Sep 30 10:00-18:00",
            "Até 22:00 hs",
        ] {
            let c = change(Some(osm), "Mo-Fr 09:00-17:00").expect(osm);
            assert_eq!(c.reason, Reason::NeedsReview, "{osm}");
            assert_eq!(c.new.as_deref(), Some(osm), "a no-op if applied");
            assert_eq!(c.atp.as_deref(), Some("Mo-Fr 09:00-17:00"));
        }
        // Same text: nothing to look at.
        assert_eq!(
            change(Some("Mo-Su sunrise-sunset"), "Mo-Su sunrise-sunset"),
            None
        );
    }

    #[test]
    fn reasons() {
        let reason = |osm: Option<&str>, atp| change(osm, atp).map(|c| c.reason);
        assert_eq!(
            reason(None, "Mo-Fr 08:00-18:00"),
            Some(Reason::MissingInOsm)
        );
        assert_eq!(
            reason(Some("Mo-Fr 08:00-18:00"), "Mo-Fr 08:30-18:00"),
            Some(Reason::Small)
        );
        assert_eq!(
            reason(Some("Mo-Fr 08:00-18:00"), "Mo-Fr 08:00-20:00"),
            Some(Reason::Over60Min)
        );
        assert_eq!(
            reason(Some("Mo-Fr 08:00-18:00"), "Mo-Sa 08:00-18:00"),
            Some(Reason::OtherDays)
        );
        assert_eq!(
            reason(Some("Mo-Su 08:00-18:00"), "Mo-Su 08:00-18:00; PH off"),
            Some(Reason::PublicHolidays)
        );
    }

    #[test]
    fn missing_in_osm() {
        // ATP's notation is tidied up, and a PH rule that repeats
        // Sunday's is a summary, so it isn't added.
        assert_eq!(
            new_value(None, "Mo 08:00-18:00; Tu 08:00-18:00; We-Fr 08:00-18:00"),
            Some("Mo-Fr 08:00-18:00".into())
        );
        assert_eq!(
            new_value(None, "Mo-Sa 08:00-20:00; Su,PH 10:00-16:00"),
            Some("Mo-Sa 08:00-20:00; Su 10:00-16:00".into())
        );
        assert_eq!(
            new_value(None, "Mo-Su 08:00-20:00; PH off"),
            Some("Mo-Su 08:00-20:00; PH off".into())
        );
        assert_eq!(new_value(None, "Mo-Su 00:00-24:00"), Some("24/7".into()));
        // Closed all week is for a delete, not for an edit.
        assert_eq!(change(None, "Mo-Su closed"), None);
        assert_eq!(change(Some("Mo-Sa 08:00-20:00"), "Mo-Su off"), None);
        // What we can't parse isn't added.
        assert_eq!(change(None, "Mo-Fr 08:00-18:00 \"call first\""), None);
    }

    #[test]
    fn atp_ph_replaces_osm_ph() {
        assert_eq!(
            new_value(
                Some("Mo-Su 08:00-20:00; PH 10:00-14:00; Dec 25 off"),
                "Mo-Su 08:00-20:00; PH off"
            )
            .as_deref(),
            Some("Mo-Su 08:00-20:00; Dec 25 off; PH off")
        );
        // The same PH times in other notation: no edit.
        assert_eq!(
            change(
                Some("Mo-Su 08:00-20:00; PH closed"),
                "Mo-Su 08:00-20:00; PH off"
            ),
            None
        );
    }

    #[test]
    fn untrusted_spiders_are_left_out() {
        let atp = tags(Some("Mo-Fr 09:00-17:00"));
        let mut warnings = Vec::new();
        let changes = opening_hours_changes(
            &tags(Some("Mo-Fr 08:00-18:00")),
            &[("wendys", &atp)],
            &Gate::trusting(&["other"]),
            &mut warnings,
        );
        assert!(changes.is_empty());
    }

    #[test]
    fn sources_must_agree() {
        let a = tags(Some("Mo-Fr 09:00-17:00"));
        let b = tags(Some("Mo-Fr 09:00-17:00; Sa off"));
        let c = tags(Some("Mo-Sa 09:00-17:00"));
        let gate = Gate::trusting(&["a", "b", "c"]);
        let osm = tags(Some("Mo-Fr 08:00-18:00"));
        let mut warnings = Vec::new();
        let changes = opening_hours_changes(&osm, &[("a", &a), ("b", &b)], &gate, &mut warnings);
        assert_eq!(changes.len(), 1);
        assert!(warnings.is_empty());
        let changes = opening_hours_changes(&osm, &[("a", &a), ("c", &c)], &gate, &mut warnings);
        assert!(changes.is_empty());
        assert_eq!(warnings, ["sources_disagree:opening_hours"]);
    }

    #[test]
    fn test_join_rules() {
        let join = |week: &str, exceptions: &[&str]| {
            join_rules(week, exceptions.iter().map(|e| e.to_string()).collect())
        };
        assert_eq!(
            join("Mo-Fr 08:00-18:00", &["PH 08:00-18:00", "Dec 25 off"]),
            "Mo-Fr,PH 08:00-18:00; Dec 25 off"
        );
        // Only the last rule of the week, and only the first exception.
        assert_eq!(
            join("Mo-Fr 08:00-18:00; Sa 09:00-12:00", &["PH 08:00-18:00"]),
            "Mo-Fr 08:00-18:00; Sa 09:00-12:00; PH 08:00-18:00"
        );
        assert_eq!(
            join("Mo-Su 08:00-20:00", &["Dec 25 off", "PH 08:00-20:00"]),
            "Mo-Su 08:00-20:00; Dec 25 off; PH 08:00-20:00"
        );
        assert_eq!(join("24/7", &["PH off"]), "24/7; PH off");
        assert_eq!(
            join("Mo-Fr 08:00-18:00", &["SH Mo-Fr 08:00-18:00"]),
            "Mo-Fr 08:00-18:00; SH Mo-Fr 08:00-18:00"
        );
    }

    #[test]
    fn test_verdict() {
        let counts = |agreeing, differing, small, unparsed| Counts {
            agreeing,
            differing,
            small,
            unparsed,
            top_share: Some(0.5),
        };
        assert_eq!(counts(5, 4, 0, 0).verdict(), Verdict::TooFew);
        assert_eq!(counts(7, 3, 0, 0).verdict(), Verdict::Pass);
        assert_eq!(counts(5, 5, 2, 0).verdict(), Verdict::Pass);
        assert_eq!(counts(5, 5, 1, 0).verdict(), Verdict::Fail);
        // Values that don't parse don't count against the spider.
        assert_eq!(counts(7, 13, 0, 10).verdict(), Verdict::Pass);
        // One value for nearly every branch.
        let template = Counts {
            top_share: Some(0.99),
            ..counts(100, 0, 0, 0)
        };
        assert_eq!(template.verdict(), Verdict::Template);
    }
}
