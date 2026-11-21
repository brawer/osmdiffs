//! Older AllThePlaces data for spiders that broke in the latest run;
//! see brawer/osmdiffs#1010.
//!
//! When a spider crashes, or is killed before it finishes, its file in
//! the dump is empty or partial, and the OpenStreetMap features of its
//! brand turn into deletion candidates. So for a spider that is
//! [broken](breakage) in the latest run, we take its whole file from
//! its most recent [healthy](is_healthy) run within the last
//! [MAX_WEEKS] weeks instead, fetching just that file from the older
//! dump with HTTP range requests (see [super::remote_zip]).
//!
//! Only spiders that still exist get a fallback: those with an entry in
//! the latest `_results.json` or a file in the latest dump. Spiders that
//! AllThePlaces removed stay out.
//!
//! [fetch] does this while the pipeline fetches its inputs, and writes
//! the files to `alltheplaces.fallbacks/` and what it did to
//! `alltheplaces.fallbacks.json` in the workdir. `import_atp` reads the
//! files in place of the spiders' files in the dump, and
//! `spider-stats.parquet` says which run each spider's features come
//! from.

use super::fetch::{self, AtpMetadata};
use super::remote_zip::{self, RemoteZip};
use super::run_stats::{self, SpiderRunStats};
use crate::pipeline::download;
use crate::pipeline::inputs::Input;
use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::SignedDuration;

/// How many weeks back to look for a healthy run of a broken spider.
/// Of the outages in the 60 weekly runs up to 2026-11-14 that ended
/// within 26 weeks, 88% ended within 4 weeks, while feature counts of
/// healthy spiders changed by a median of 0% over that span; see #1010.
pub(in crate::pipeline) const MAX_WEEKS: i64 = 4;

/// The fewest features that a run of a spider needs to count as
/// healthy. Below, half of the features is too few to tell an outage
/// from noise.
pub(in crate::pipeline) const MIN_FEATURES: u64 = 20;

/// Filename, within `workdir`, of the [Manifest] that [fetch] writes.
const MANIFEST_FILENAME: &str = "alltheplaces.fallbacks.json";

/// Directory, within `workdir`, of the files that [fetch] writes.
const DIRNAME: &str = "alltheplaces.fallbacks";

/// How a spider broke; see [breakage].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::pipeline) enum Breakage {
    /// The spider has a file in the dump, but no entry in
    /// `_results.json`: it was killed before it finished, and its file
    /// is partial.
    Unfinished,

    /// The spider finished, but with fewer than half the features of its
    /// most recent healthy run, often none at all.
    Dropped,
}

impl Breakage {
    pub fn as_str(self) -> &'static str {
        match self {
            Breakage::Unfinished => "unfinished",
            Breakage::Dropped => "dropped",
        }
    }
}

/// The test for a broken spider. `current` is what `_results.json` of
/// a run says about the spider's features, or `None` if the spider has
/// a file in the dump but no entry there. `healthy` is the features of
/// the spider's most recent healthy run before, if any; see
/// [last_healthy].
///
/// An unfinished spider is broken even without a healthy run to
/// compare with, though there is then no run to fall back to.
pub(in crate::pipeline) fn breakage(
    current: Option<u64>,
    healthy: Option<u64>,
) -> Option<Breakage> {
    match (current, healthy) {
        (None, _) => Some(Breakage::Unfinished),
        (Some(current), Some(healthy)) if current * 2 < healthy => Some(Breakage::Dropped),
        _ => None,
    }
}

/// Whether a finished run of a spider with `features` is healthy, given
/// the features of its most recent healthy run before, if any: it has
/// at least [MIN_FEATURES], and isn't [broken](breakage) itself.
pub(in crate::pipeline) fn is_healthy(features: u64, healthy: Option<u64>) -> bool {
    features >= MIN_FEATURES && breakage(Some(features), healthy).is_none()
}

/// The most recent healthy run of a spider among `runs`, oldest first,
/// which give the spider's features in each run's `_results.json`, or
/// `None` where it has no entry. Returns the run's index and features.
pub(in crate::pipeline) fn last_healthy(
    runs: impl IntoIterator<Item = Option<u64>>,
) -> Option<(usize, u64)> {
    let mut healthy = None;
    for (i, features) in runs.into_iter().enumerate() {
        if let Some(features) = features
            && is_healthy(features, healthy.map(|(_, f)| f))
        {
            healthy = Some((i, features));
        }
    }
    healthy
}

/// One spider whose features come from an older run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::pipeline) struct Fallback {
    pub spider: String,
    pub breakage: Breakage,

    /// What the latest run's `_results.json` says about the spider's
    /// features, or `None` if it has no entry there.
    pub latest_features: Option<u64>,

    /// The run that the spider's features come from instead.
    pub run_id: String,

    /// The `output.zip` of that run.
    pub output_url: String,

    /// What that run's `_results.json` says about the spider.
    pub features: u64,
    pub errors: u64,
}

impl Fallback {
    /// Where [fetch] put the spider's file from the older run.
    pub fn path(&self, workdir: &Path) -> PathBuf {
        workdir
            .join(DIRNAME)
            .join(format!("{}.geojson", self.spider))
    }

    /// What the older run's `_results.json` says about the spider, to
    /// stand in for the latest run's.
    pub fn run_stats(&self) -> SpiderRunStats {
        SpiderRunStats {
            spider: self.spider.clone(),
            features: self.features,
            errors: self.errors,
        }
    }
}

/// What [fetch] wrote to `alltheplaces.fallbacks.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::pipeline) struct Manifest {
    /// The latest run, whose broken spiders these are.
    pub run_id: String,

    /// Sorted by spider.
    pub fallbacks: Vec<Fallback>,
}

impl Manifest {
    /// The fallbacks, by spider.
    pub fn by_spider(&self) -> HashMap<&str, &Fallback> {
        self.fallbacks
            .iter()
            .map(|f| (f.spider.as_str(), f))
            .collect()
    }
}

/// Path of the [Manifest] in `workdir`.
pub(in crate::pipeline) fn manifest_path(workdir: &Path) -> PathBuf {
    workdir.join(MANIFEST_FILENAME)
}

/// Reads the [Manifest] that [fetch] wrote in `workdir`, or `None` if
/// there is none.
pub(in crate::pipeline) fn read(workdir: &Path) -> Result<Option<Manifest>> {
    let path = manifest_path(workdir);
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read(&path).with_context(|| format!("could not read {path:?}"))?;
    let manifest =
        serde_json::from_slice(&json).with_context(|| format!("could not parse {path:?}"))?;
    Ok(Some(manifest))
}

fn write(manifest: &Manifest, workdir: &Path) -> Result<()> {
    let path = manifest_path(workdir);
    let mut tmp = path.clone();
    tmp.add_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(manifest)?)
        .with_context(|| format!("could not write {tmp:?}"))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("could not rename {tmp:?}"))?;
    Ok(())
}

/// The latest run's statistics of the spiders of an older run.
#[derive(Debug)]
pub(super) struct OlderRun {
    pub metadata: AtpMetadata,
    pub stats: HashMap<String, SpiderRunStats>,
}

/// A spider to take from an older run, before its file is fetched.
#[derive(Debug)]
struct Planned<'a> {
    spider: &'a str,
    breakage: Breakage,
    latest_features: Option<u64>,
    run: &'a OlderRun,
    stats: &'a SpiderRunStats,
}

/// A broken spider that keeps its latest data, since a spider that is
/// new since its healthy run takes its place, and the old file would
/// duplicate places: `mcdonalds` dropped to 6 features in the run of
/// 2026-11-14, when `mcdonalds_us` appeared.
#[derive(Debug, PartialEq)]
struct Replaced<'a> {
    spider: &'a str,
    by: &'a str,
}

/// Which spiders of the latest run to take from which of the `older`
/// runs, oldest first. `latest` is the latest `_results.json`, and
/// `files` the spiders with a file in the latest dump.
fn plan<'a>(
    latest: &'a HashMap<String, SpiderRunStats>,
    files: &'a HashSet<String>,
    older: &'a [OlderRun],
) -> (Vec<Planned<'a>>, Vec<Replaced<'a>>) {
    let spiders: BTreeSet<&str> = latest
        .keys()
        .chain(files.iter())
        .map(String::as_str)
        .collect();
    let mut planned = Vec::new();
    let mut replaced = Vec::new();
    for &spider in &spiders {
        let latest_features = latest.get(spider).map(|s| s.features);
        let healthy = last_healthy(
            older
                .iter()
                .map(|run| run.stats.get(spider).map(|s| s.features)),
        );
        let Some(breakage) = breakage(latest_features, healthy.map(|(_, f)| f)) else {
            continue;
        };
        let Some((i, _)) = healthy else {
            continue;
        };
        let run = &older[i];
        let prefix = format!("{spider}_");
        if let Some(by) = spiders
            .iter()
            .find(|s| s.starts_with(&prefix) && !run.stats.contains_key(**s))
        {
            replaced.push(Replaced { spider, by });
            continue;
        }
        planned.push(Planned {
            spider,
            breakage,
            latest_features,
            run,
            stats: &run.stats[spider],
        });
    }
    (planned, replaced)
}

/// Whether `spider` is safe to use in a file name. AllThePlaces names
/// its spiders like Python modules.
fn is_plain_name(spider: &str) -> bool {
    !spider.is_empty()
        && spider
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Finds the spiders that are broken in the latest run, `latest`, and
/// fetches their files from their most recent healthy run, as described
/// in the module documentation. Does nothing if `workdir` already has
/// the [Manifest] for this run, or if the dump is gone (`import_atp`
/// then reuses `alltheplaces.parquet`, so there would be nothing to put
/// the files into).
///
/// Falling back is a repair, not an input the pipeline can't do
/// without: if it fails, the error is logged, no manifest is written,
/// and the pipeline goes on with the latest data, as before #1010. A
/// spider whose file can't be fetched keeps its latest data too.
pub(super) async fn fetch(latest: &Input<AtpMetadata>, client: &Client, workdir: &Path) {
    if let Err(err) = try_fetch(latest, client, workdir).await {
        log::warn!(
            error = format!("{err:#}").as_str();
            "fetch_atp: could not look for older data for broken spiders, using the latest"
        );
    }
}

async fn try_fetch(latest: &Input<AtpMetadata>, client: &Client, workdir: &Path) -> Result<()> {
    let run_id = &latest.metadata.run_id;
    match read(workdir) {
        Ok(Some(manifest)) if manifest.run_id == *run_id => return Ok(()),
        Ok(_) => {}
        Err(err) => log::warn!(
            error = format!("{err:#}").as_str();
            "fetch_atp: could not read {MANIFEST_FILENAME}, looking again"
        ),
    }
    if !latest.path.exists() {
        log::info!(
            "fetch_atp: no dump to look for broken spiders in, reusing alltheplaces.parquet"
        );
        return Ok(());
    }
    let files = super::dump_spiders(&latest.path)?;
    let stats = run_stats::read(workdir)?
        .ok_or_else(|| anyhow!("no AllThePlaces stats for run {run_id}"))?;
    let older = fetch_older_runs(&latest.metadata, client).await?;

    let (planned, replaced) = plan(&stats, &files, &older);
    for r in &replaced {
        log::info!(
            spider = r.spider,
            replaced_by = r.by;
            "fetch_atp: not using older data for broken spider, since a new spider took its place"
        );
    }

    let dir = workdir.join(DIRNAME);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {dir:?}"))?;
    let mut zips: HashMap<&str, Option<RemoteZip>> = HashMap::new();
    let mut fallbacks = Vec::new();
    for p in planned {
        let fallback = Fallback {
            spider: p.spider.to_string(),
            breakage: p.breakage,
            latest_features: p.latest_features,
            run_id: p.run.metadata.run_id.clone(),
            output_url: p.run.metadata.output_url.clone(),
            features: p.stats.features,
            errors: p.stats.errors,
        };
        if !is_plain_name(p.spider) {
            log::warn!(spider = p.spider; "fetch_atp: spider name unfit for a file name");
            continue;
        }
        let run_id = p.run.metadata.run_id.as_str();
        if !zips.contains_key(run_id) {
            let opened = RemoteZip::open(client, &fallback.output_url)
                .await
                .inspect_err(|err| {
                    log::warn!(
                        run_id = run_id,
                        error = format!("{err:#}").as_str();
                        "fetch_atp: could not open older AllThePlaces dump"
                    )
                })
                .ok();
            zips.insert(run_id, opened);
        }
        let Some(zip) = &zips[run_id] else { continue };
        let Some(name) = zip
            .names()
            .find(|n| super::spider_of_entry(n) == Some(p.spider))
            .map(String::from)
        else {
            log::warn!(
                spider = p.spider,
                run_id = fallback.run_id.as_str();
                "fetch_atp: older AllThePlaces dump has no file for broken spider"
            );
            continue;
        };
        match remote_zip::extract_to(zip, client, &name, &fallback.path(workdir)).await {
            Ok(bytes) => {
                log::info!(
                    spider = p.spider,
                    breakage = p.breakage.as_str(),
                    latest_features = p.latest_features,
                    run_id = fallback.run_id.as_str(),
                    features = fallback.features,
                    bytes = bytes;
                    "fetch_atp: using older data for broken spider"
                );
                fallbacks.push(fallback);
            }
            Err(err) => log::warn!(
                spider = p.spider,
                run_id = fallback.run_id.as_str(),
                error = format!("{err:#}").as_str();
                "fetch_atp: could not fetch older data for broken spider"
            ),
        }
    }

    let manifest = Manifest {
        run_id: run_id.clone(),
        fallbacks,
    };
    write(&manifest, workdir)?;
    log_summary(&manifest);
    Ok(())
}

/// Logs one line listing the spiders that use older data: a warning,
/// since their places may be out of date by up to [MAX_WEEKS] weeks.
fn log_summary(manifest: &Manifest) {
    let spiders = |b: Breakage| -> String {
        manifest
            .fallbacks
            .iter()
            .filter(|f| f.breakage == b)
            .map(|f| f.spider.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    let features: u64 = manifest.fallbacks.iter().map(|f| f.features).sum();
    if manifest.fallbacks.is_empty() {
        log::info!(run_id = manifest.run_id.as_str(); "fetch_atp: no broken spiders to use older data for");
        return;
    }
    log::warn!(
        run_id = manifest.run_id.as_str(),
        count = manifest.fallbacks.len(),
        features = features,
        unfinished = spiders(Breakage::Unfinished).as_str(),
        dropped = spiders(Breakage::Dropped).as_str();
        "fetch_atp: using older AllThePlaces data for broken spiders"
    );
}

/// The runs in `history.json` that started within [MAX_WEEKS] weeks
/// before `latest`, oldest first, with their `_results.json`. A run
/// whose statistics can't be fetched is left out.
async fn fetch_older_runs(latest: &AtpMetadata, client: &Client) -> Result<Vec<OlderRun>> {
    // A day of slack, since runs don't start at exactly the same time
    // each week.
    let earliest = latest.start_time - SignedDuration::weeks(MAX_WEEKS) - SignedDuration::days(1);
    let mut runs: Vec<AtpMetadata> = fetch::fetch_runs(client, &latest.history_url)
        .await?
        .into_iter()
        .filter(|r| r.start_time < latest.start_time && r.start_time >= earliest)
        .collect();
    runs.sort_by_key(|r| r.start_time);
    let mut older = Vec::new();
    for metadata in runs {
        let Some(url) = metadata.stats_url.as_deref() else {
            continue;
        };
        let stats = download::get_bytes(client, url, Duration::from_secs(120))
            .await
            .and_then(|body| run_stats::parse(&body));
        match stats {
            Ok(stats) => older.push(OlderRun {
                stats: stats.into_iter().map(|s| (s.spider.clone(), s)).collect(),
                metadata,
            }),
            Err(err) => log::warn!(
                run_id = metadata.run_id.as_str(),
                error = format!("{err:#}").as_str();
                "fetch_atp: could not fetch the stats of an older AllThePlaces run"
            ),
        }
    }
    Ok(older)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use time::UtcDateTime;

    #[test]
    fn test_breakage() {
        assert_eq!(breakage(None, None), Some(Breakage::Unfinished));
        assert_eq!(breakage(None, Some(100)), Some(Breakage::Unfinished));
        assert_eq!(breakage(Some(0), Some(100)), Some(Breakage::Dropped));
        assert_eq!(breakage(Some(49), Some(100)), Some(Breakage::Dropped));
        assert_eq!(breakage(Some(50), Some(100)), None);
        assert_eq!(breakage(Some(0), None), None);
        assert_eq!(breakage(Some(500), Some(100)), None);
    }

    #[test]
    fn test_last_healthy() {
        assert_eq!(last_healthy([]), None);
        assert_eq!(last_healthy([None, Some(19)]), None);
        assert_eq!(last_healthy([Some(20), None]), Some((0, 20)));
        // A run that dropped by more than half isn't healthy itself.
        assert_eq!(
            last_healthy([Some(1000), Some(400), Some(0)]),
            Some((0, 1000))
        );
        // Until the spider is back above half.
        assert_eq!(
            last_healthy([Some(1000), Some(400), Some(600)]),
            Some((2, 600))
        );
        // Without a healthy run before, any run with enough features is.
        assert_eq!(last_healthy([None, Some(400)]), Some((1, 400)));
    }

    fn stats(spiders: &[(&str, u64)]) -> HashMap<String, SpiderRunStats> {
        spiders
            .iter()
            .map(|(s, f)| {
                (
                    s.to_string(),
                    SpiderRunStats {
                        spider: s.to_string(),
                        features: *f,
                        errors: 1,
                    },
                )
            })
            .collect()
    }

    fn run(run_id: &str, spiders: &[(&str, u64)]) -> OlderRun {
        OlderRun {
            metadata: AtpMetadata {
                run_id: run_id.to_string(),
                output_url: format!("https://example.org/{run_id}/output.zip"),
                history_url: String::new(),
                start_time: UtcDateTime::UNIX_EPOCH,
                end_time: UtcDateTime::UNIX_EPOCH,
                spiders: 0,
                total_lines: 0,
                size_bytes: 0,
                stats_url: None,
                sha256: None,
            },
            stats: stats(spiders),
        }
    }

    #[test]
    fn test_plan() {
        let older = [
            run(
                "w1",
                &[
                    ("crashed", 100),
                    ("killed", 900),
                    ("mcdonalds", 4556),
                    ("removed", 50),
                    ("shrunk", 100),
                ],
            ),
            run(
                "w2",
                &[
                    ("crashed", 120),
                    ("mcdonalds", 4556),
                    ("removed", 50),
                    ("shrunk", 60),
                    ("small", 10),
                ],
            ),
        ];
        let latest = stats(&[
            ("crashed", 0),
            ("mcdonalds", 6),
            ("mcdonalds_us", 13000),
            ("shrunk", 55),
            ("small", 0),
            ("new", 0),
        ]);
        let files: HashSet<String> = ["crashed", "killed", "mcdonalds", "mcdonalds_us", "shrunk"]
            .map(String::from)
            .into();
        let (planned, replaced) = plan(&latest, &files, &older);
        let got: Vec<(&str, Breakage, Option<u64>, &str, u64)> = planned
            .iter()
            .map(|p| {
                (
                    p.spider,
                    p.breakage,
                    p.latest_features,
                    p.run.metadata.run_id.as_str(),
                    p.stats.features,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("crashed", Breakage::Dropped, Some(0), "w2", 120),
                ("killed", Breakage::Unfinished, None, "w1", 900),
            ]
        );
        assert_eq!(
            replaced,
            [Replaced {
                spider: "mcdonalds",
                by: "mcdonalds_us"
            }]
        );
    }

    /// A spider's file in the format of the dump, with `n` features.
    pub(in crate::pipeline::atp) fn geojson(spider: &str, n: usize) -> Vec<u8> {
        let mut out = format!(
            "{{\"type\":\"FeatureCollection\",\"dataset_attributes\":{{\"@spider\":\"{spider}\",\
             \"spider:collection_time\":\"2026-11-01T00:00:00\",\"spider:lineage\":\"S_ATP_BRANDS\"}},\
             \"features\":[\n"
        );
        for i in 0..n {
            out.push_str(&format!(
                "{{\"type\":\"Feature\",\"geometry\":{{\"type\":\"Point\",\"coordinates\":[8.5,47.{i}]}},\
                 \"properties\":{{\"@spider\":\"{spider}\",\"shop\":\"bakery\"}}}}{}\n",
                if i + 1 < n { "," } else { "" }
            ));
        }
        out.push_str("]}\n");
        out.into_bytes()
    }

    fn history_entry(run_id: &str, base: &str, weeks_ago: i64) -> serde_json::Value {
        let start =
            UtcDateTime::now() - SignedDuration::weeks(weeks_ago) - SignedDuration::hours(5);
        let end = start + SignedDuration::hours(3);
        let format = |t: UtcDateTime| {
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap()
        };
        serde_json::json!({
            "run_id": run_id,
            "output_url": format!("{base}/{run_id}/output.zip"),
            "stats_url": format!("{base}/{run_id}/stats/_results.json"),
            "start_time": format(start),
            "end_time": format(end),
            "spiders": 5000,
            "total_lines": 5_000_000,
            "size_bytes": 3_000_000_000u64,
        })
    }

    fn results(spiders: &[(&str, u64)]) -> Vec<u8> {
        let results: Vec<serde_json::Value> = spiders
            .iter()
            .map(|(s, f)| serde_json::json!({"spider": s, "errors": 0, "features": f}))
            .collect();
        serde_json::to_vec(&serde_json::json!({"count": results.len(), "results": results}))
            .unwrap()
    }

    #[tokio::test]
    async fn test_fetch() -> Result<()> {
        use remote_zip::tests::{build_zip, serve, test_client};
        let old = build_zip(
            &[
                ("output/crashed.geojson", &geojson("crashed", 30)),
                ("output/killed.geojson", &geojson("killed", 25)),
                ("output/healthy.geojson", &geojson("healthy", 40)),
            ],
            true,
        );
        // Runs 5 weeks ago are too old to fall back to.
        let ancient = build_zip(&[("output/gone.geojson", &geojson("gone", 99))], false);
        let placeholder = "http://placeholder";
        let (base, seen) = serve(vec![
            ("/w5/output.zip".into(), ancient),
            (
                "/w5/stats/_results.json".into(),
                results(&[("gone", 99), ("ancient", 99)]),
            ),
            ("/w2/output.zip".into(), old),
            (
                "/w2/stats/_results.json".into(),
                results(&[
                    ("crashed", 30),
                    ("killed", 25),
                    ("healthy", 40),
                    ("missing", 30),
                ]),
            ),
            // This run's stats are gone; it is left out.
            ("/w1/output.zip".into(), Vec::new()),
        ])
        .await;
        let history = serde_json::json!([
            history_entry("w5", &base, 5),
            history_entry("w2", &base, 2),
            history_entry("w1", &base, 1),
            history_entry("w0", placeholder, 0),
        ]);
        let (history_base, _) = serve(vec![(
            "/history.json".into(),
            serde_json::to_vec(&history)?,
        )])
        .await;
        let history_url = format!("{history_base}/history.json");
        let latest_metadata = fetch::fetch_runs(&test_client(), &history_url)
            .await?
            .into_iter()
            .find(|r| r.run_id == "w0")
            .unwrap();

        let workdir = tempfile::tempdir()?;
        let zip_path = workdir.path().join("alltheplaces.zip");
        std::fs::write(
            &zip_path,
            build_zip(
                &[
                    ("output/crashed.geojson", b""),
                    ("output/killed.geojson", &geojson("killed", 3)),
                    ("output/healthy.geojson", &geojson("healthy", 41)),
                    ("output/missing.geojson", &geojson("missing", 1)),
                ],
                false,
            ),
        )?;
        std::fs::write(
            workdir.path().join(fetch::STATS_JSON_FILENAME),
            results(&[
                ("crashed", 0),
                ("healthy", 41),
                ("missing", 1),
                ("ancient", 0),
            ]),
        )?;
        let input = Input {
            path: zip_path,
            metadata: latest_metadata,
        };
        fetch(&input, &test_client(), workdir.path()).await;

        let manifest = read(workdir.path())?.expect("manifest");
        assert_eq!(manifest.run_id, "w0");
        let got: Vec<(&str, Breakage, Option<u64>, &str, u64)> = manifest
            .fallbacks
            .iter()
            .map(|f| {
                (
                    f.spider.as_str(),
                    f.breakage,
                    f.latest_features,
                    f.run_id.as_str(),
                    f.features,
                )
            })
            .collect();
        // `missing` has no file in the older dump, so it keeps its
        // latest data.
        assert_eq!(
            got,
            [
                ("crashed", Breakage::Dropped, Some(0), "w2", 30),
                ("killed", Breakage::Unfinished, None, "w2", 25),
            ]
        );
        assert_eq!(
            std::fs::read(manifest.fallbacks[0].path(workdir.path()))?,
            geojson("crashed", 30)
        );
        let ranges = seen.lock().unwrap().clone();
        assert!(ranges.iter().all(|r| r.starts_with("/w2/")), "{ranges:?}");

        // With the manifest for this run, nothing is fetched again.
        let requests = seen.lock().unwrap().len();
        fetch(&input, &test_client(), workdir.path()).await;
        assert_eq!(seen.lock().unwrap().len(), requests);
        Ok(())
    }

    #[test]
    fn test_is_plain_name() {
        assert!(is_plain_name(
            "new_york_city_department_of_parks_and_recreation_trees_us"
        ));
        assert!(is_plain_name("7-eleven"));
        assert!(!is_plain_name(""));
        assert!(!is_plain_name("../x"));
        assert!(!is_plain_name("a/b"));
    }

    #[test]
    fn test_manifest_roundtrip() -> Result<()> {
        let dir = tempfile::tempdir()?;
        assert_eq!(read(dir.path())?, None);
        let manifest = Manifest {
            run_id: "2026-11-14-13-32-33".into(),
            fallbacks: vec![Fallback {
                spider: "sdek".into(),
                breakage: Breakage::Dropped,
                latest_features: Some(0),
                run_id: "2026-11-07-13-32-33".into(),
                output_url: "https://example.org/output.zip".into(),
                features: 10532,
                errors: 0,
            }],
        };
        write(&manifest, dir.path())?;
        assert_eq!(read(dir.path())?, Some(manifest.clone()));
        let json = std::fs::read_to_string(manifest_path(dir.path()))?;
        assert!(json.contains("\"breakage\": \"dropped\""), "{json}");
        assert_eq!(
            manifest.fallbacks[0].path(dir.path()),
            dir.path().join("alltheplaces.fallbacks/sdek.geojson")
        );
        Ok(())
    }
}

/// Runs the detection against the real AllThePlaces runs, with the
/// latest dump's directory read remotely, and fetches one fallback file:
/// `cargo test --lib real_atp -- --ignored --nocapture`.
#[cfg(test)]
mod real_atp {
    use super::*;

    #[tokio::test]
    #[ignore = "fetches from data.alltheplaces.xyz"]
    async fn test_real_atp() -> Result<()> {
        let client = Client::new();
        let mut runs = fetch::fetch_runs(&client, fetch::ATP_RUN_HISTORY_URL).await?;
        runs.sort_by_key(|r| r.start_time);
        let latest = runs.pop().expect("runs");
        let body = download::get_bytes(
            &client,
            latest.stats_url.as_deref().unwrap(),
            Duration::from_secs(120),
        )
        .await?;
        let stats: HashMap<String, SpiderRunStats> = run_stats::parse(&body)?
            .into_iter()
            .map(|s| (s.spider.clone(), s))
            .collect();
        let zip = RemoteZip::open(&client, &latest.output_url).await?;
        let files: HashSet<String> = zip
            .names()
            .filter_map(super::super::spider_of_entry)
            .map(String::from)
            .collect();
        let older = fetch_older_runs(&latest, &client).await?;
        let (planned, replaced) = plan(&stats, &files, &older);
        println!(
            "latest {} ({} spiders, {} files), older runs {:?}",
            latest.run_id,
            stats.len(),
            files.len(),
            older.iter().map(|r| &r.metadata.run_id).collect::<Vec<_>>()
        );
        for p in &planned {
            println!(
                "{}\t{}\t{:?}\t{}\t{}",
                p.spider,
                p.breakage.as_str(),
                p.latest_features,
                p.run.metadata.run_id,
                p.stats.features
            );
        }
        println!("replaced: {replaced:?}");
        let unfinished = planned
            .iter()
            .filter(|p| p.breakage == Breakage::Unfinished)
            .count();
        println!("{} fallbacks, {unfinished} unfinished", planned.len());

        // Fetch one of a few megabytes, in several chunks.
        if let Some(p) = planned
            .iter()
            .find(|p| p.spider == "sdek")
            .or_else(|| planned.iter().min_by_key(|p| p.stats.features))
        {
            let old = RemoteZip::open(&client, &p.run.metadata.output_url).await?;
            let name = old
                .names()
                .find(|n| super::super::spider_of_entry(n) == Some(p.spider))
                .unwrap()
                .to_string();
            let mut out = Vec::new();
            old.extract(&client, &name, &mut out).await?;
            let lines = String::from_utf8(out)?;
            let features = lines.lines().skip(1).filter(|l| l.starts_with('{')).count();
            println!(
                "{name} from {}: {} bytes, {features} feature lines, {} in stats",
                p.run.metadata.run_id,
                lines.len(),
                p.stats.features
            );
        }
        Ok(())
    }
}
