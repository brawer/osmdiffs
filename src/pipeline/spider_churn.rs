//! Logs how the AllThePlaces spiders changed since last week, so that
//! whoever reviews a production run sees it in `pipeline.log` without
//! comparing two weeks of `spider-stats.parquet` by hand; see
//! brawer/osmdiffs#1020.
//!
//! Compares this week's `spider-stats.parquet` with the one that its
//! key-value metadata names as the previous file, which
//! `fetch_inputs.previous` downloads into the workdir (see
//! [super::previous_release]). Logs one INFO line with totals, and one
//! WARN line per spider that lost many features or much of its match
//! rate. A spider counts as broken by the same test that picks spiders
//! for older data ([fallback::breakage]), and the lines say which
//! spiders use older data.
//!
//! Like `fetch_inputs.previous`, this never fails the pipeline: if the
//! previous file is missing or can't be read, it logs that once and
//! carries on.

use super::atp::fallback::{self, Breakage};
use anyhow::{Context, Result, anyhow};
use arrow::array::{Array, RecordBatch, StringArray, UInt64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::Path;

/// The fewest features that a spider must have had last week to get a
/// WARN line of its own; smaller ones only count in the totals.
const MIN_LOGGED_FEATURES: u64 = 100;

/// The fewest conflated rows that a spider must have had last week for
/// a drop in its match rate to count.
const MIN_RATE_ROWS: u64 = 100;

/// How far a spider's match rate must drop, as a fraction, to count:
/// 20 points usually means the spider changed its tags.
const MIN_RATE_DROP: f64 = 0.2;

/// One spider in one week's `spider-stats.parquet`.
#[derive(Clone, Debug, Default, PartialEq)]
struct Week {
    /// What that week's AllThePlaces run said about the spider's
    /// features, `None` if the spider has no entry in its
    /// `_results.json`. In the file, a spider that used older data has
    /// the features of that older, healthy run, which is the right
    /// baseline for the week after; for this week, [summarize] replaces
    /// them with those of the latest run.
    features: Option<u64>,

    /// Set if the spider's features came from an older run.
    fallback: Option<Breakage>,

    /// Rows in `conflated.parquet`, and how many of them matched
    /// OpenStreetMap: `same` and `located_in`.
    rows: u64,
    matched: u64,
}

impl Week {
    fn match_rate(&self) -> Option<f64> {
        (self.rows > 0).then(|| self.matched as f64 / self.rows as f64)
    }
}

/// How a spider changed since last week.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Change {
    /// New this week.
    New,
    /// Gone this week.
    Gone,
    /// Healthy last week, and down to 0 features this week.
    ToZero,
    /// Healthy last week, and down to fewer than half its features.
    Halved,
    /// Healthy last week, and killed before it finished this week: a
    /// file in the dump, but no entry in `_results.json`.
    Unfinished,
    /// Broken last week, and still broken: a spider that used older
    /// data last week and still uses it.
    StillBroken,
    /// Broken last week and this week, and no older data any more: its
    /// last healthy run is now more than [fallback::MAX_WEEKS] weeks
    /// old, so its OpenStreetMap features may turn into deletion
    /// candidates.
    Expired,
    /// Broken or empty last week, and back with features this week.
    Back,
}

impl Change {
    fn as_str(self) -> &'static str {
        match self {
            Change::New => "new",
            Change::Gone => "gone",
            Change::ToZero => "to_zero",
            Change::Halved => "halved",
            Change::Unfinished => "unfinished",
            Change::StillBroken => "still_broken",
            Change::Expired => "expired",
            Change::Back => "back",
        }
    }

    /// Whether the spider lost features, so that it gets a WARN line.
    fn is_loss(self) -> bool {
        matches!(
            self,
            Change::Gone | Change::ToZero | Change::Halved | Change::Unfinished | Change::Expired
        )
    }
}

/// How `spider` changed from `previous` to `current`, if it did in a
/// way that the summary reports.
fn change(previous: Option<&Week>, current: Option<&Week>) -> Option<Change> {
    let (previous, current) = match (previous, current) {
        (None, None) => return None,
        (None, Some(_)) => return Some(Change::New),
        (Some(_), None) => return Some(Change::Gone),
        (Some(p), Some(c)) => (p, c),
    };
    let breakage = fallback::breakage(current.features, previous.features);
    let has_features = current.features.is_some_and(|f| f > 0);
    if previous.fallback.is_some() {
        return Some(if breakage.is_none() && has_features {
            Change::Back
        } else if current.fallback.is_some() {
            Change::StillBroken
        } else {
            Change::Expired
        });
    }
    match breakage {
        Some(Breakage::Unfinished) => Some(Change::Unfinished),
        Some(Breakage::Dropped) if current.features == Some(0) => Some(Change::ToZero),
        Some(Breakage::Dropped) => Some(Change::Halved),
        None if previous.features.is_none_or(|f| f == 0) && has_features => Some(Change::Back),
        None => None,
    }
}

/// A spider that lost features.
#[derive(Debug, PartialEq)]
struct Loss<'a> {
    spider: &'a str,
    change: Change,
    previous: Option<u64>,
    current: Option<u64>,
    fallback: Option<Breakage>,
}

impl Loss<'_> {
    fn lost(&self) -> u64 {
        self.previous
            .unwrap_or(0)
            .saturating_sub(self.current.unwrap_or(0))
    }
}

/// A spider whose match rate dropped.
#[derive(Debug, PartialEq)]
struct RateDrop<'a> {
    spider: &'a str,
    previous: f64,
    current: f64,
    previous_rows: u64,
    current_rows: u64,
    fallback: Option<Breakage>,
}

/// What changed between two weeks.
#[derive(Debug, Default, PartialEq)]
struct Churn<'a> {
    spiders: usize,
    counts: BTreeMap<Change, usize>,
    /// Spiders that lost features and had at least
    /// [MIN_LOGGED_FEATURES] last week, most features lost first.
    losses: Vec<Loss<'a>>,
    /// Biggest drop first.
    rate_drops: Vec<RateDrop<'a>>,
    /// Spiders that use older data this week, sorted.
    fallbacks: Vec<&'a str>,
}

fn compare<'a>(
    previous: &'a HashMap<String, Week>,
    current: &'a HashMap<String, Week>,
) -> Churn<'a> {
    let mut churn = Churn {
        spiders: current.len(),
        ..Churn::default()
    };
    let mut spiders: Vec<&str> = previous
        .keys()
        .chain(current.keys().filter(|s| !previous.contains_key(*s)))
        .map(String::as_str)
        .collect();
    spiders.sort_unstable();
    for spider in spiders {
        let (p, c) = (previous.get(spider), current.get(spider));
        if let Some(change) = change(p, c) {
            *churn.counts.entry(change).or_default() += 1;
            let previous_features = p.and_then(|p| p.features);
            if change.is_loss() && previous_features.is_some_and(|f| f >= MIN_LOGGED_FEATURES) {
                churn.losses.push(Loss {
                    spider,
                    change,
                    previous: previous_features,
                    current: c.and_then(|c| c.features),
                    fallback: c.and_then(|c| c.fallback),
                });
            }
        }
        if let (Some(p), Some(c)) = (p, c)
            && p.rows >= MIN_RATE_ROWS
            && let (Some(before), Some(after)) = (p.match_rate(), c.match_rate())
            && before - after >= MIN_RATE_DROP
        {
            churn.rate_drops.push(RateDrop {
                spider,
                previous: before,
                current: after,
                previous_rows: p.rows,
                current_rows: c.rows,
                fallback: c.fallback,
            });
        }
        if c.is_some_and(|c| c.fallback.is_some()) {
            churn.fallbacks.push(spider);
        }
    }
    churn
        .losses
        .sort_by(|a, b| b.lost().cmp(&a.lost()).then(a.spider.cmp(b.spider)));
    churn.rate_drops.sort_by(|a, b| {
        (b.previous - b.current)
            .total_cmp(&(a.previous - a.current))
            .then(a.spider.cmp(b.spider))
    });
    churn
}

/// A fraction rounded to three digits, for the log.
fn rounded(rate: f64) -> f64 {
    (rate * 1000.0).round() / 1000.0
}

fn log_churn(churn: &Churn, previous: &str) {
    let count = |c: Change| churn.counts.get(&c).copied().unwrap_or(0);
    log::info!(
        previous = previous,
        spiders = churn.spiders,
        new = count(Change::New),
        gone = count(Change::Gone),
        to_zero = count(Change::ToZero),
        halved = count(Change::Halved),
        unfinished = count(Change::Unfinished),
        still_broken = count(Change::StillBroken),
        expired = count(Change::Expired),
        back = count(Change::Back),
        match_rate_drops = churn.rate_drops.len(),
        fallbacks = churn.fallbacks.len(),
        fallback_spiders = churn.fallbacks.join(",").as_str();
        "spider_churn: spiders since last week"
    );
    for loss in &churn.losses {
        log::warn!(
            spider = loss.spider,
            change = loss.change.as_str(),
            previous = loss.previous,
            current = loss.current,
            fallback = loss.fallback.map(Breakage::as_str);
            "spider_churn: spider lost features since last week"
        );
    }
    for drop in &churn.rate_drops {
        log::warn!(
            spider = drop.spider,
            previous = rounded(drop.previous),
            current = rounded(drop.current),
            previous_rows = drop.previous_rows,
            current_rows = drop.current_rows,
            fallback = drop.fallback.map(Breakage::as_str);
            "spider_churn: spider's match rate dropped since last week"
        );
    }
}

/// Reads the columns of `spider-stats.parquet` that the summary needs.
fn read(path: &Path) -> Result<HashMap<String, Week>> {
    let file = File::open(path).with_context(|| format!("could not open {path:?}"))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("could not read {path:?}"))?
        .build()?;
    let mut weeks = HashMap::new();
    for batch in reader {
        let batch = batch.with_context(|| format!("could not read {path:?}"))?;
        let spider = strings(&batch, "spider")?;
        let features = numbers(&batch, "atp_features")?;
        let reason = strings(&batch, "fallback_reason")?;
        let (rows, same, located_in) = (
            numbers(&batch, "rows")?,
            numbers(&batch, "same")?,
            numbers(&batch, "located_in")?,
        );
        for i in 0..batch.num_rows() {
            let fallback = match reason.is_valid(i).then(|| reason.value(i)) {
                None => None,
                Some("unfinished") => Some(Breakage::Unfinished),
                Some("dropped") => Some(Breakage::Dropped),
                Some(other) => return Err(anyhow!("unknown fallback_reason {other:?}")),
            };
            weeks.insert(
                spider.value(i).to_string(),
                Week {
                    features: features.is_valid(i).then(|| features.value(i)),
                    fallback,
                    rows: rows.value(i),
                    matched: same.value(i) + located_in.value(i),
                },
            );
        }
    }
    Ok(weeks)
}

fn strings<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .with_context(|| format!("no string column {name:?}"))
}

fn numbers<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
        .with_context(|| format!("no UInt64 column {name:?}"))
}

/// Logs how the spiders in `current`, this week's
/// `spider-stats.parquet`, changed since `previous`, the basename of
/// last week's, which `fetch_inputs.previous` put into `workdir`. Logs
/// a warning instead if there is no previous file or it can't be read.
pub fn summarize(current: &Path, previous: Option<&str>, workdir: &Path) {
    let Some(previous) = previous else {
        log::info!("spider_churn: no previous spider-stats.parquet to compare with");
        return;
    };
    if let Err(err) = try_summarize(current, previous, workdir) {
        log::warn!(
            previous = previous,
            error = format!("{err:#}").as_str();
            "spider_churn: could not compare spiders with last week"
        );
    }
}

fn try_summarize(current: &Path, previous: &str, workdir: &Path) -> Result<()> {
    let old = read(&workdir.join(previous))?;
    let mut new = read(current)?;
    // For spiders that use older data, what the latest run said.
    if let Some(manifest) = fallback::read(workdir)? {
        for f in &manifest.fallbacks {
            if let Some(week) = new.get_mut(&f.spider) {
                week.features = f.latest_features;
            }
        }
    }
    log_churn(&compare(&old, &new), previous);
    Ok(())
}

/// The spiders whose data this week doesn't look healthy, so that the
/// edits step leaves them out: spiders that lost features since last
/// week and don't use older data instead, and spiders whose match rate
/// dropped, which usually means they changed their tags. Empty if
/// there is no previous file or it can't be read; [summarize] logs that.
pub fn unhealthy_spiders(
    current: &Path,
    previous: Option<&str>,
    workdir: &Path,
) -> std::collections::BTreeSet<String> {
    let Some(previous) = previous else {
        return Default::default();
    };
    let weeks = || -> Result<_> { Ok((read(&workdir.join(previous))?, read(current)?)) };
    let Ok((old, new)) = weeks() else {
        return Default::default();
    };
    unhealthy(&old, &new)
}

fn unhealthy(
    previous: &HashMap<String, Week>,
    current: &HashMap<String, Week>,
) -> std::collections::BTreeSet<String> {
    let mut spiders = std::collections::BTreeSet::new();
    for (spider, c) in current {
        let p = previous.get(spider);
        let lost = change(p, Some(c)).is_some_and(Change::is_loss) && c.fallback.is_none();
        let dropped = p.is_some_and(|p| {
            p.rows >= MIN_RATE_ROWS
                && matches!(
                    (p.match_rate(), c.match_rate()),
                    (Some(before), Some(after)) if before - after >= MIN_RATE_DROP
                )
        });
        if lost || dropped {
            spiders.insert(spider.clone());
        }
    }
    spiders
}

#[cfg(test)]
mod tests {
    use super::*;

    fn week(features: Option<u64>) -> Week {
        Week {
            features,
            ..Week::default()
        }
    }

    fn fell_back(features: Option<u64>, breakage: Breakage) -> Week {
        Week {
            fallback: Some(breakage),
            ..week(features)
        }
    }

    #[test]
    fn test_change() {
        let c = |p: Option<Week>, c: Option<Week>| change(p.as_ref(), c.as_ref());
        let (healthy, zero, few, unfinished) =
            (week(Some(100)), week(Some(0)), week(Some(40)), week(None));
        assert_eq!(c(None, Some(healthy.clone())), Some(Change::New));
        assert_eq!(c(Some(healthy.clone()), None), Some(Change::Gone));
        assert_eq!(
            c(Some(healthy.clone()), Some(zero.clone())),
            Some(Change::ToZero)
        );
        assert_eq!(
            c(Some(healthy.clone()), Some(few.clone())),
            Some(Change::Halved)
        );
        assert_eq!(
            c(Some(healthy.clone()), Some(unfinished.clone())),
            Some(Change::Unfinished)
        );
        assert_eq!(c(Some(healthy.clone()), Some(week(Some(50)))), None);
        assert_eq!(c(Some(healthy.clone()), Some(healthy.clone())), None);
        assert_eq!(
            c(Some(zero.clone()), Some(healthy.clone())),
            Some(Change::Back)
        );
        assert_eq!(
            c(Some(unfinished.clone()), Some(healthy.clone())),
            Some(Change::Back)
        );
        assert_eq!(c(Some(zero.clone()), Some(zero.clone())), None);

        // Last week's file has the features of the healthy run that a
        // broken spider fell back to.
        let broken = fell_back(Some(100), Breakage::Dropped);
        assert_eq!(
            c(Some(broken.clone()), Some(healthy.clone())),
            Some(Change::Back)
        );
        assert_eq!(
            c(
                Some(broken.clone()),
                Some(fell_back(Some(0), Breakage::Dropped))
            ),
            Some(Change::StillBroken)
        );
        assert_eq!(
            c(
                Some(broken.clone()),
                Some(fell_back(None, Breakage::Unfinished))
            ),
            Some(Change::StillBroken)
        );
        assert_eq!(
            c(Some(broken.clone()), Some(zero.clone())),
            Some(Change::Expired)
        );
        assert_eq!(
            c(Some(broken.clone()), Some(few.clone())),
            Some(Change::Expired)
        );
        assert_eq!(c(Some(broken), Some(unfinished)), Some(Change::Expired));
    }

    #[test]
    fn test_compare() {
        let rated = |features: u64, rows: u64, matched: u64| Week {
            features: Some(features),
            rows,
            matched,
            ..Week::default()
        };
        let previous: HashMap<String, Week> = [
            ("crashed", rated(500, 500, 400)),
            ("halved", rated(1000, 1000, 900)),
            ("removed", rated(200, 200, 100)),
            ("small", rated(50, 50, 40)),
            ("retagged", rated(300, 300, 270)),
            ("expired", fell_back(Some(150), Breakage::Unfinished)),
            ("steady", rated(300, 300, 270)),
        ]
        .into_iter()
        .map(|(s, w)| (s.to_string(), w))
        .collect();
        let current: HashMap<String, Week> = [
            ("crashed", fell_back(Some(0), Breakage::Dropped)),
            ("halved", rated(400, 400, 360)),
            ("small", rated(0, 0, 0)),
            ("retagged", rated(300, 300, 30)),
            ("expired", week(None)),
            ("steady", rated(310, 310, 270)),
            ("added", rated(10, 10, 5)),
        ]
        .into_iter()
        .map(|(s, w)| (s.to_string(), w))
        .collect();

        let churn = compare(&previous, &current);
        assert_eq!(churn.spiders, 7);
        assert_eq!(
            churn.counts,
            BTreeMap::from([
                (Change::New, 1),
                (Change::Gone, 1),
                (Change::ToZero, 2),
                (Change::Halved, 1),
                (Change::Expired, 1),
            ])
        );
        let losses: Vec<(&str, Change, u64)> = churn
            .losses
            .iter()
            .map(|l| (l.spider, l.change, l.lost()))
            .collect();
        assert_eq!(
            losses,
            [
                ("halved", Change::Halved, 600),
                ("crashed", Change::ToZero, 500),
                ("removed", Change::Gone, 200),
                ("expired", Change::Expired, 150),
            ]
        );
        assert_eq!(churn.losses[1].fallback, Some(Breakage::Dropped));
        // `crashed` has no rows now, so no rate to compare.
        assert_eq!(
            churn.rate_drops,
            [RateDrop {
                spider: "retagged",
                previous: 0.9,
                current: 0.1,
                previous_rows: 300,
                current_rows: 300,
                fallback: None,
            }]
        );
        assert_eq!(churn.fallbacks, ["crashed"]);
    }

    #[test]
    fn test_missing_previous_is_no_error() -> Result<()> {
        let workdir = tempfile::tempdir()?;
        let current = workdir.path().join("spider-stats.parquet");
        summarize(
            &current,
            Some("spider-stats-20261109-a1b2c3d4.parquet"),
            workdir.path(),
        );
        summarize(&current, None, workdir.path());
        assert!(try_summarize(&current, "missing.parquet", workdir.path()).is_err());
        Ok(())
    }

    #[test]
    fn test_unhealthy() {
        let rows = |rows: u64, matched: u64| Week {
            features: Some(1000),
            rows,
            matched,
            ..Week::default()
        };
        let previous: HashMap<String, Week> = [
            ("halved", week(Some(1000))),
            ("halved_fallback", week(Some(1000))),
            ("rate", rows(1000, 900)),
            ("fine", rows(1000, 900)),
        ]
        .into_iter()
        .map(|(s, w)| (s.to_string(), w))
        .collect();
        let current: HashMap<String, Week> = [
            ("halved", week(Some(100))),
            ("halved_fallback", fell_back(Some(1000), Breakage::Dropped)),
            ("rate", rows(1000, 300)),
            ("fine", rows(1000, 880)),
            ("new", week(Some(5))),
        ]
        .into_iter()
        .map(|(s, w)| (s.to_string(), w))
        .collect();
        let got: Vec<String> = unhealthy(&previous, &current).into_iter().collect();
        assert_eq!(got, ["halved", "rate"]);
    }
}
