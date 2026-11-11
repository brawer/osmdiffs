use super::atp::nondescript::Signal;
use super::importance::{Quantiles, Signals};
use super::last_modified;
use crate::{
    make_progress_bar,
    matchers::{
        OsmCandidate, brand_domains::BrandDomains, contact::Nondescript, create_matcher,
        match_distance,
    },
    pipeline::EXTERNAL_SORT_CHUNK_BYTES,
    places::{Place, PlaceReader},
    tables::{Feature, LocalFeatureRef, OsmFeatures},
};
use anyhow::{Context, Ok, Result};
use ext_sort::{ExternalSorter, ExternalSorterBuilder, buffer::mem::MemoryLimitedBufferBuilder};
use geo::Centroid;
use indicatif::{MultiProgress, ProgressBar};
use rayon::prelude::*;
use s2::s1::ChordAngle;
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};

/// How often [produce_rows] logs a progress/memory snapshot while
/// running -- a wall-clock interval, not a feature count, since what
/// matters here is watching RSS/page-cache behavior *over time* as this
/// repeatedly queries and decodes from `OsmFeatureIndex`'s mmap'd
/// tables, not RSS at a fixed fraction of progress.
const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(30);

/// Like `log::info!` with key-value pairs, but adds the fields of a
/// `pipeline::memstats` snapshot, so the memory usage between the
/// step's start and end lines shows up in the log: the sub-steps of
/// `conflate` hold large structures (the exclusive-match candidates,
/// the local coverage counts) that are gone again by `conflate: end`.
macro_rules! info_with_memory {
    ($($key:ident = $value:expr),* ; $message:literal) => {{
        let stats = crate::pipeline::memstats::snapshot();
        log::info!(
            $($key = $value,)*
            rss_bytes = stats.rss_bytes,
            rss_peak_bytes = stats.rss_peak_bytes,
            rss_anon_bytes = stats.rss_anon_bytes,
            rss_file_bytes = stats.rss_file_bytes,
            rss_shmem_bytes = stats.rss_shmem_bytes,
            cgroup_current_bytes = stats.cgroup_current_bytes,
            cgroup_max_bytes = stats.cgroup_max_bytes,
            cgroup_peak_bytes = stats.cgroup_peak_bytes;
            $message
        )
    }};
}

mod deletion;
pub(super) mod diagnostics;
pub(super) mod evidence;
mod local_coverage;
mod located_in;
mod s2_util;
mod writer;
use deletion::{AtpCoverage, Candidate, Reason, deletion_reason, local_keys, place_country};
use diagnostics::{Diagnostics, Missing};
use local_coverage::{LocalCoverage, LocalKey, MIN_LOCAL_SHARE};
use located_in::{Host, find_departments, located_in_tag};
use s2_util::MergedCellRanges;
use writer::{MatchInfo, ParquetRow, ParquetWriter};

/// The files that the `importance` column is computed from; see
/// `pipeline::importance`.
pub struct ImportanceInputs<'a> {
    pub osmviews: &'a Path,
    pub osmviews_ranks: &'a Path,
    pub qrank_quantiles: &'a Path,
}

pub fn conflate(
    atp: &Path,
    osm: &OsmFeatures,
    importance: &ImportanceInputs,
    progress: &MultiProgress,
    workdir: &Path,
    pipeline_run_id: &str,
) -> Result<PathBuf> {
    let nondescript_phones = Signal::Phone.path(workdir);
    let nondescript_emails = Signal::Email.path(workdir);
    let nondescript_websites = Signal::Website.path(workdir);
    let brand_domains_path = super::atp::brand_domains_path(workdir);
    let input_modified = last_modified(&[
        atp,
        &nondescript_phones,
        &nondescript_emails,
        &nondescript_websites,
        &brand_domains_path,
        importance.osmviews,
        importance.osmviews_ranks,
        importance.qrank_quantiles,
    ])?
    .max(osm.modified()?);
    let out_path = workdir.join("conflated.parquet");
    let diagnostics_path = diagnostics::path(workdir);
    if out_path.exists()
        && diagnostics_path.exists()
        && last_modified(&[&out_path])? >= input_modified
    {
        return Ok(out_path);
    }

    let nondescript = Nondescript::open(
        &nondescript_phones,
        &nondescript_emails,
        &nondescript_websites,
    )?;
    let brand_domains = BrandDomains::open(&brand_domains_path)?;
    log::info!(count = brand_domains.len(); "conflate: read brand domains");
    let signals = Signals::open(
        importance.osmviews,
        importance.osmviews_ranks,
        importance.qrank_quantiles,
    )?;
    let atp_reader = PlaceReader::open(atp)?;
    let num_atp_features = atp_reader.total_rows() as u64;
    let producer_progress =
        make_progress_bar(progress, "conflate.match", num_atp_features, "ATP features");
    let writer_progress = make_progress_bar(progress, "conflate.write", 0, "parquet rows");

    let mut producer_result = Ok(Diagnostics::default());
    let mut writer_result = Ok(());
    thread::scope(|s| {
        let (tx, rx) = sync_channel::<ParquetRow>(8192);
        s.spawn(|| {
            producer_result = produce_rows(
                &atp_reader,
                osm,
                &nondescript,
                &brand_domains,
                &signals,
                producer_progress,
                tx,
            );
        });
        s.spawn(|| {
            writer_result =
                write_conflated(rx, writer_progress, workdir, &out_path, pipeline_run_id);
        });
    });
    writer_result?;
    producer_result?.write(&diagnostics_path)?;

    Ok(out_path)
}

struct ConflatedFeature {
    atp: Option<Place>,
    osm: Option<Feature>,
    /// See `pipeline::importance::score`.
    score: f32,
    matching: MatchInfo,
}

/// How many candidates we keep for an ATP feature with an exclusive
/// matcher, so it can fall back when a better-scoring feature of the
/// same spider takes its first choice.
const EXCLUSIVE_CANDIDATES: usize = 4;

/// Candidates of an ATP feature whose matcher is exclusive, see
/// [crate::matchers::Matcher::is_exclusive], best first.
struct ExclusiveCandidates {
    /// Position of the ATP feature in [PlaceReader::read_all].
    ordinal: u64,
    /// Hash of the spider name: conflicts only count within a spider,
    /// so two feeds for the same station can both match it.
    spider: u64,
    /// Score, OSM feature, whether the matcher allows other ATP
    /// features to match it too ([crate::matchers::Matcher::may_share]),
    /// and the rule that matched ([crate::matchers::Matcher::rule]).
    candidates: Vec<(f64, LocalFeatureRef, bool, &'static str)>,
    /// The ATP feature's position, for [Self::colocated_radius].
    s2_cell_id: u64,
    /// See [crate::matchers::Matcher::colocated_radius].
    colocated_radius: Option<ChordAngle>,
}

fn count_spider(counts: &mut HashMap<String, u64>, spider: &str) {
    match counts.get_mut(spider) {
        Some(n) => *n += 1,
        None => {
            counts.insert(spider.to_string(), 1);
        }
    }
}

/// The spiders with the most features that no matcher handles, most
/// first, as `spider:count` separated by commas.
fn top_spiders(counts: &HashMap<String, u64>, n: usize) -> String {
    let mut top: Vec<(&String, &u64)> = counts.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    top.iter()
        .take(n)
        .map(|(spider, count)| format!("{spider}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn log_no_matcher(counts: &HashMap<String, u64>) {
    log::info!(
        features = counts.values().sum::<u64>(),
        spiders = counts.len(),
        top_spiders = top_spiders(counts, 20).as_str();
        "conflate.match: ATP features without a matcher"
    );
}

/// Matches every ATP feature, in two passes. The first pass matches
/// features whose matcher isn't exclusive, or that have no candidate,
/// and sends their rows right away; for the others, it keeps the best
/// few candidates. Then
/// [assign_exclusive] gives each OSM feature to at most one ATP feature
/// per spider. The second pass sends the rows of the exclusive ones.
/// Finally, [send_deletion_candidates] sends rows for the OSM features
/// that nothing matched, if there is a strong signal that they are
/// gone.
fn produce_rows(
    atp_reader: &PlaceReader,
    osm: &OsmFeatures,
    nondescript: &Nondescript,
    brand_domains: &BrandDomains,
    signals: &Signals,
    progress_bar: ProgressBar,
    out: SyncSender<ParquetRow>,
) -> Result<Diagnostics> {
    let coverer = s2::region::RegionCoverer {
        max_cells: 16,
        min_level: 12,
        max_level: s2::cellid::MAX_LEVEL as u8,
        level_mod: 1,
    };

    let start = Instant::now();
    let atp_features_processed = AtomicU64::new(0);
    let osm_candidates_decoded = AtomicU64::new(0);
    // Wall-clock timestamp (millis since `start`) of the last progress
    // log line, so concurrent workers can race on a compare_exchange to
    // decide who logs next -- occasionally missing or double-logging a
    // snapshot at the boundary is fine for a diagnostic like this.
    let last_progress_log_ms = AtomicU64::new(0);
    let exclusive = Mutex::new(Vec::new());
    let matched = MatchedSet::new(osm.index.len());
    let mut coverage = AtpCoverage::default();
    // ATP features per spider that no matcher handles, and that get no
    // row in conflated.parquet.
    let mut no_matcher_by_spider: HashMap<String, u64> = HashMap::new();
    let mut features_by_spider: HashMap<String, u64> = HashMap::new();
    let mut diagnostics = Diagnostics::default();
    let departments = find_departments(atp_reader)?;
    log::info!(
        departments = departments.len(),
        elapsed_seconds = start.elapsed().as_secs_f64();
        "conflate.departments: done"
    );
    let host_of = |ordinal: u64, atp: &Place| {
        located_in_tag(atp).or_else(|| departments.get(&ordinal).cloned())
    };
    let located_in_rows = AtomicU64::new(0);
    // Sends the row of an ATP feature without a match, and returns
    // whether it is `missing_in_osm`, rather than `located_in`.
    let send_unmatched = |atp: &Place, host: Option<&Host>| -> Result<bool> {
        if let Some(host) = host
            && let Some(r) = find_host(atp, host, osm)?
        {
            // The host exists, whether or not ATP has it.
            matched.insert(r);
            located_in_rows.fetch_add(1, Ordering::Relaxed);
            send_row(
                atp,
                Some(r),
                MatchInfo::located_in(host.rule()),
                osm,
                signals,
                &out,
            )?;
            return Ok(false);
        }
        send_row(atp, None, MatchInfo::missing_in_osm(), osm, signals, &out)?;
        Ok(true)
    };

    let mut batch_start: u64 = 0;
    for batch in atp_reader.read_all()? {
        let batch = batch?;
        // Which ATP features of this batch have a matcher that
        // [crate::matchers::Matcher::counts_for_coverage]: only these
        // count for `coverage`, since ATP features of a kind that we
        // don't match say nothing about unmatched OSM features.
        let has_matcher: Vec<AtomicBool> = batch.iter().map(|_| AtomicBool::new(false)).collect();
        let no_matcher: Vec<AtomicBool> = batch.iter().map(|_| AtomicBool::new(false)).collect();
        // Why each ATP feature of this batch that is missing in OSM
        // found no match, for the diagnostics.
        let missing: Vec<OnceLock<Missing>> = batch.iter().map(|_| OnceLock::new()).collect();
        // Each batch is processed by the Rayon thread pool in parallel,
        // but the outer loop is sequential — so nearby places (within a
        // batch) always go to nearby workers, preserving spatial locality.
        batch.par_iter().enumerate().try_for_each(|(i, atp)| {
            progress_bar.inc(1);
            atp_features_processed.fetch_add(1, Ordering::Relaxed);
            maybe_log_progress(start, &last_progress_log_ms, || {
                (
                    atp_features_processed.load(Ordering::Relaxed),
                    osm_candidates_decoded.load(Ordering::Relaxed),
                )
            });

            let Some(matcher) = create_matcher(atp, nondescript, brand_domains) else {
                no_matcher[i].store(true, Ordering::Relaxed);
                return Ok(());
            };
            has_matcher[i].store(matcher.counts_for_coverage(), Ordering::Relaxed);
            let host = host_of(batch_start + i as u64, atp);
            let keep = if matcher.is_exclusive() {
                EXCLUSIVE_CANDIDATES
            } else {
                1
            };
            let covering = {
                let s2_cell = s2::cell::Cell::from(s2::cellid::CellID(atp.s2_cell_id));
                let center = s2_cell.center();
                let radius = matcher
                    .max_distance()
                    .unwrap_or_else(|| match_distance(&atp.mask));
                let cap = s2::cap::Cap::from_center_chordangle(&center, &radius);
                coverer.covering(&cap)
            };
            let mut best = Vec::<(f64, LocalFeatureRef)>::new();
            // OSM features of the kind that the matcher found no
            // evidence for, for the diagnostics.
            let mut rejected = Vec::new();
            for (lo, hi) in MergedCellRanges::new(covering) {
                for r in osm.index.query(lo..=hi, atp.mask) {
                    let feature = osm.index.get_feature(r)?;
                    osm_candidates_decoded.fetch_add(1, Ordering::Relaxed);
                    let candidate = OsmCandidate {
                        feature: &feature,
                        strings: &osm.strings,
                    };
                    if host.as_ref().is_some_and(|h| h.excludes(candidate.tags())) {
                        continue;
                    }
                    let score = matcher.score_osm_candidate(&candidate);
                    if score <= 0.0 && rejected.len() < diagnostics::MAX_REJECTED {
                        rejected.push(r);
                    }
                    keep_best(&mut best, keep, score, r);
                }
            }

            // An exclusive matcher without candidates has nothing to
            // compete for, so its row can go out right away; that keeps
            // the many unmatched ATP features out of `exclusive`.
            if matcher.is_exclusive() && !best.is_empty() {
                let mut candidates = Vec::with_capacity(best.len());
                for (score, r) in best {
                    let feature = osm.index.get_feature(r)?;
                    let candidate = OsmCandidate {
                        feature: &feature,
                        strings: &osm.strings,
                    };
                    candidates.push((
                        score,
                        r,
                        matcher.may_share(&candidate),
                        matcher.rule(&candidate),
                    ));
                }
                let mut hasher = std::hash::DefaultHasher::new();
                atp.spider.hash(&mut hasher);
                exclusive
                    .lock()
                    .expect("exclusive lock")
                    .push(ExclusiveCandidates {
                        ordinal: batch_start + i as u64,
                        spider: hasher.finish(),
                        candidates,
                        s2_cell_id: atp.s2_cell_id,
                        colocated_radius: matcher.colocated_radius(),
                    });
                return Ok(());
            }
            let Some(&(score, r)) = best.first() else {
                if send_unmatched(atp, host.as_ref())? {
                    let why = if rejected.is_empty() {
                        Missing::NoCandidate
                    } else {
                        Missing::Rejected(diagnostics::nearest(atp, &rejected, osm)?)
                    };
                    missing[i].set(why).expect("set once");
                }
                return Ok(());
            };
            matched.insert(r);
            let feature = osm.index.get_feature(r)?;
            let rule = matcher.rule(&OsmCandidate {
                feature: &feature,
                strings: &osm.strings,
            });
            send_row(
                atp,
                Some(r),
                MatchInfo::same(score, rule),
                osm,
                signals,
                &out,
            )
        })?;
        let countries: Vec<_> = batch
            .par_iter()
            .zip(&has_matcher)
            .map(|(atp, has_matcher)| {
                has_matcher
                    .load(Ordering::Relaxed)
                    .then(|| place_country(atp))
            })
            .collect();
        for (atp, country) in batch.iter().zip(countries) {
            if let Some(country) = country {
                coverage.add(atp, country);
            }
        }
        for (atp, no_matcher) in batch.iter().zip(&no_matcher) {
            if no_matcher.load(Ordering::Relaxed) {
                count_spider(&mut no_matcher_by_spider, &atp.spider);
            }
            count_spider(&mut features_by_spider, &atp.spider);
        }
        for (atp, why) in batch.iter().zip(missing) {
            if let Some(why) = why.into_inner() {
                diagnostics.add_missing(&atp.spider, why);
            }
        }
        batch_start += batch.len() as u64;
    }

    log_no_matcher(&no_matcher_by_spider);
    let spacing_bytes = coverage.finish();
    info_with_memory!(
        spacing_bytes = spacing_bytes,
        coverage_bytes = coverage.bytes(),
        radii = coverage.num_radii();
        "conflate.coverage: done"
    );

    let exclusive = exclusive.into_inner().expect("exclusive lock");
    let num_exclusive = exclusive.len();
    let exclusive_bytes = exclusive.capacity() * size_of::<ExclusiveCandidates>()
        + exclusive
            .iter()
            .map(|e| {
                e.candidates.capacity() * size_of::<(f64, LocalFeatureRef, bool, &'static str)>()
            })
            .sum::<usize>();
    let assigned = assign_exclusive(&exclusive);
    // The score and rule of each assigned match.
    let assigned: HashMap<u64, Option<(LocalFeatureRef, f64, &'static str)>> = exclusive
        .iter()
        .map(|e| {
            let found = assigned[&e.ordinal].and_then(|r| {
                e.candidates
                    .iter()
                    .find(|c| c.1 == r)
                    .map(|&(score, r, _, rule)| (r, score, rule))
            });
            (e.ordinal, found)
        })
        .collect();
    drop(exclusive);
    let mut ordinal: u64 = 0;
    for batch in atp_reader.read_all()? {
        for atp in batch? {
            if let Some(&osm_match) = assigned.get(&ordinal) {
                match osm_match {
                    Some((r, score, rule)) => {
                        matched.insert(r);
                        send_row(
                            &atp,
                            Some(r),
                            MatchInfo::same(score, rule),
                            osm,
                            signals,
                            &out,
                        )?;
                    }
                    None => {
                        if send_unmatched(&atp, host_of(ordinal, &atp).as_ref())? {
                            diagnostics.add_missing(&atp.spider, Missing::Taken);
                        }
                    }
                }
            }
            ordinal += 1;
        }
    }

    send_deletion_candidates(osm, &coverage, &matched, signals, &out)?;

    let chance_start = Instant::now();
    diagnostics::chance_pairs(
        atp_reader,
        &features_by_spider,
        osm,
        nondescript,
        brand_domains,
        &mut diagnostics,
    )?;
    log::info!(
        elapsed_seconds = chance_start.elapsed().as_secs_f64();
        "conflate.chance_pairs: done"
    );

    progress_bar.finish();
    info_with_memory!(
        elapsed_seconds = start.elapsed().as_secs_f64(),
        atp_features_processed = atp_features_processed.load(Ordering::Relaxed),
        osm_candidates_decoded = osm_candidates_decoded.load(Ordering::Relaxed),
        exclusive_atp_features = num_exclusive,
        exclusive_bytes = exclusive_bytes,
        located_in_rows = located_in_rows.load(Ordering::Relaxed),
        exclusive_matched = assigned.values().filter(|m| m.is_some()).count();
        "conflate.match: done"
    );
    Ok(diagnostics)
}

/// The OSM features that some ATP feature matched, as one bit per
/// feature in the index.
struct MatchedSet(Vec<AtomicU64>);

impl MatchedSet {
    fn new(num_features: usize) -> Self {
        MatchedSet(
            (0..num_features.div_ceil(64))
                .map(|_| AtomicU64::new(0))
                .collect(),
        )
    }

    fn insert(&self, r: LocalFeatureRef) {
        let i = r.ordinal();
        self.0[i / 64].fetch_or(1 << (i % 64), Ordering::Relaxed);
    }

    fn contains(&self, r: LocalFeatureRef) -> bool {
        let i = r.ordinal();
        self.0[i / 64].load(Ordering::Relaxed) & (1 << (i % 64)) != 0
    }
}

/// Sends an OSM-only row for every OSM feature that no ATP feature
/// matched, but that [deletion_reason] considers a candidate for
/// deletion. See the [deletion] module for the signals.
///
/// Candidates by [Reason::Wikidata] and [Reason::Name] must also pass
/// [local_coverage]: enough OSM features of their brand nearby must
/// have been matched. That takes three steps: find the candidates,
/// count the matched OSM features of their brands, and then send the
/// candidates that pass.
fn send_deletion_candidates(
    osm: &OsmFeatures,
    coverage: &AtpCoverage,
    matched: &MatchedSet,
    signals: &Signals,
    out: &SyncSender<ParquetRow>,
) -> Result<()> {
    let start = Instant::now();
    let counts: Mutex<HashMap<Reason, u64>> = Mutex::new(HashMap::new());
    let count_reason = |reason| {
        *counts
            .lock()
            .expect("counts lock")
            .entry(reason)
            .or_default() += 1;
    };
    let send = |feature: Feature, lng: f64, lat: f64, matching: MatchInfo| -> Result<()> {
        let candidate = OsmCandidate {
            feature: &feature,
            strings: &osm.strings,
        };
        let score = signals.score_osm(lng, lat, candidate.tags());
        let conflated = ConflatedFeature {
            atp: None,
            osm: Some(feature),
            score,
            matching,
        };
        out.send(ParquetRow::from_conflated(conflated, &osm.strings)?)?;
        Ok(())
    };
    let centroid = |candidate: &OsmCandidate| {
        let centroid = candidate.geometry().ok()?.centroid()?;
        Some((centroid.x(), centroid.y()))
    };

    // Find the candidates. The Swiss ones go out right away.
    let found: Vec<(LocalFeatureRef, Candidate, f64, f64)> = osm
        .index
        .all_refs()
        .filter(|&r| !matched.contains(r))
        .map(|r| -> Result<Option<_>> {
            let feature = osm.index.get_feature(r)?;
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &osm.strings,
            };
            let location = || centroid(&candidate);
            let country = osm.index.country(r);
            let Some(found) = deletion_reason(candidate.tags(), coverage, country, location) else {
                return Ok(None);
            };
            // A feature without a location can't be put on a map, nor
            // sorted into the output file.
            let Some((lng, lat)) = location() else {
                return Ok(None);
            };
            if found.keys.is_empty() {
                count_reason(found.reason);
                let matching = MatchInfo::deletion_candidate(found.reason.as_str(), None);
                send(feature, lng, lat, matching)?;
                return Ok(None);
            }
            Ok(Some((r, found, lng, lat)))
        })
        .filter_map(Result::transpose)
        .collect::<Result<_>>()?;

    let mut local = LocalCoverage::default();
    for (_, candidate, lng, lat) in &found {
        for &key in &candidate.keys {
            local.add_candidate(key, *lng, *lat);
        }
    }

    // Count the matched OSM features of the candidates' brands.
    let wanted: HashSet<LocalKey> = found
        .iter()
        .flat_map(|(_, c, _, _)| c.keys.iter().copied())
        .collect();
    let matched_keys: Vec<(LocalKey, f64, f64)> = osm
        .index
        .all_refs()
        .filter(|&r| matched.contains(r))
        .map(|r| -> Result<Vec<_>> {
            let feature = osm.index.get_feature(r)?;
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &osm.strings,
            };
            let mut keys = local_keys(candidate.tags());
            keys.retain(|k| wanted.contains(k));
            if keys.is_empty() {
                return Ok(Vec::new());
            }
            let Some((lng, lat)) = centroid(&candidate) else {
                return Ok(Vec::new());
            };
            Ok(keys.into_iter().map(|k| (k, lng, lat)).collect())
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    for (key, lng, lat) in &matched_keys {
        local.add_matched(*key, *lng, *lat);
    }

    // Send the candidates whose brand ATP covers around them.
    let dropped: Mutex<HashMap<Reason, u64>> = Mutex::new(HashMap::new());
    let thin = AtomicU64::new(0);
    found
        .par_iter()
        .try_for_each(|(r, candidate, lng, lat)| -> Result<()> {
            let best = candidate
                .keys
                .iter()
                .map(|&key| {
                    let radius = coverage.radius(key, candidate.country);
                    local.around(key, *lng, *lat, radius)
                })
                .max_by(|a, b| a.share().total_cmp(&b.share()))
                .unwrap_or_default();
            if best.share() < MIN_LOCAL_SHARE {
                *dropped
                    .lock()
                    .expect("dropped lock")
                    .entry(candidate.reason)
                    .or_default() += 1;
                return Ok(());
            }
            // Besides the candidate itself, fewer than three OSM
            // features of its brand nearby: thin evidence.
            if best.matched + best.candidates < 4 {
                thin.fetch_add(1, Ordering::Relaxed);
            }
            count_reason(candidate.reason);
            let matching =
                MatchInfo::deletion_candidate(candidate.reason.as_str(), Some(best.share()));
            send(osm.index.get_feature(*r)?, *lng, *lat, matching)
        })?;

    let counts = counts.into_inner().expect("counts lock");
    let count = |reason| counts.get(&reason).copied().unwrap_or(0);
    let dropped = dropped.into_inner().expect("dropped lock");
    let num_dropped = |reason| dropped.get(&reason).copied().unwrap_or(0);
    info_with_memory!(
        elapsed_seconds = start.elapsed().as_secs_f64(),
        atp_wikidata_countries = coverage.num_wikidata_countries(),
        atp_name_countries = coverage.num_name_countries(),
        atp_has_swiss_transit = coverage.has_swiss_transit(),
        wikidata = count(Reason::Wikidata),
        name = count(Reason::Name),
        swiss_transit_id = count(Reason::SwissTransitId),
        swiss_transit_stop = count(Reason::SwissTransitStop),
        wikidata_not_local = num_dropped(Reason::Wikidata),
        name_not_local = num_dropped(Reason::Name),
        thin_local_evidence = thin.load(Ordering::Relaxed),
        local_coverage_entries = local.len(),
        local_coverage_bytes = local.bytes(),
        local_matched_keys = matched_keys.len();
        "conflate.deletion_candidates: done"
    );
    Ok(())
}

/// Adds the candidate `r` with `score` to `best`, which holds at most
/// `keep` candidates with positive scores, best first. Among equal
/// scores, the one seen first stays ahead; a candidate already in
/// `best` (an OSM feature can come up in several cell ranges) keeps
/// its place.
fn keep_best(best: &mut Vec<(f64, LocalFeatureRef)>, keep: usize, score: f64, r: LocalFeatureRef) {
    if score <= 0.0 || best.iter().any(|&(_, b)| b == r) {
        return;
    }
    let pos = best.partition_point(|&(s, _)| s >= score);
    if pos < keep {
        best.insert(pos, (score, r));
        best.truncate(keep);
    }
}

/// Assigns OSM features to ATP features with exclusive matchers, so
/// that no OSM feature goes to two ATP features of the same spider.
/// Greedy: the best-scoring pairs go first, and an ATP feature whose
/// candidates are all taken stays unmatched. A candidate that the
/// matcher allows to share is never taken away, but it does block it
/// for others that would not share it. An ATP feature also gets a
/// taken candidate if it lies within its
/// [crate::matchers::Matcher::colocated_radius] of the ATP feature
/// that took it first. Returns the OSM match (or none) of every ATP
/// feature, by its ordinal.
fn assign_exclusive(exclusive: &[ExclusiveCandidates]) -> HashMap<u64, Option<LocalFeatureRef>> {
    let mut assigned: HashMap<u64, Option<LocalFeatureRef>> =
        exclusive.iter().map(|e| (e.ordinal, None)).collect();
    let mut pairs: Vec<(f64, usize, LocalFeatureRef, bool)> = exclusive
        .iter()
        .enumerate()
        .flat_map(|(i, e)| {
            e.candidates
                .iter()
                .map(move |&(score, r, share, _)| (score, i, r, share))
        })
        .collect();
    // Best score first; ties by ordinal and feature, so that runs are
    // reproducible.
    pairs.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(exclusive[a.1].ordinal.cmp(&exclusive[b.1].ordinal))
            .then(a.2.cmp(&b.2))
    });
    // For each taken (spider, OSM feature), the position of the ATP
    // feature that took it first.
    let mut taken: HashMap<(u64, LocalFeatureRef), u64> = HashMap::new();
    for (_, i, r, share) in pairs {
        let e = &exclusive[i];
        let slot = assigned
            .get_mut(&e.ordinal)
            .expect("every ordinal is in assigned");
        if slot.is_some() {
            continue;
        }
        let ok = match taken.entry((e.spider, r)) {
            Entry::Vacant(v) => {
                v.insert(e.s2_cell_id);
                true
            }
            Entry::Occupied(o) => share || is_colocated(e, *o.get()),
        };
        if ok {
            *slot = Some(r);
        }
    }
    assigned
}

/// Whether `e` lies within its colocated radius of the ATP feature at
/// `s2_cell_id`.
fn is_colocated(e: &ExclusiveCandidates, s2_cell_id: u64) -> bool {
    let Some(radius) = e.colocated_radius else {
        return false;
    };
    let center = |id| s2::cell::Cell::from(s2::cellid::CellID(id)).center();
    ChordAngle::from(center(e.s2_cell_id).distance(&center(s2_cell_id))) <= radius
}

/// The OSM feature of `host` nearest to `atp`, within
/// [located_in::HOST_SEARCH_METERS].
fn find_host(atp: &Place, host: &Host, osm: &OsmFeatures) -> Result<Option<LocalFeatureRef>> {
    let coverer = s2::region::RegionCoverer {
        max_cells: 16,
        min_level: 12,
        max_level: s2::cellid::MAX_LEVEL as u8,
        level_mod: 1,
    };
    let center = s2::cell::Cell::from(s2::cellid::CellID(atp.s2_cell_id)).center();
    let radius = crate::matchers::meters_to_chord_angle(located_in::HOST_SEARCH_METERS);
    let cap = s2::cap::Cap::from_center_chordangle(&center, &radius);
    let mut best: Option<(f64, LocalFeatureRef)> = None;
    for (lo, hi) in MergedCellRanges::new(coverer.covering(&cap)) {
        for r in osm.index.query(lo..=hi, host.mask()) {
            let feature = osm.index.get_feature(r)?;
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &osm.strings,
            };
            if !host.is_host(candidate.tags()) {
                continue;
            }
            let Some(centroid) = candidate.geometry().ok().and_then(|g| g.centroid()) else {
                continue;
            };
            let ll = s2::latlng::LatLng::from_degrees(centroid.y(), centroid.x());
            let d = center.distance(&s2::point::Point::from(ll)).rad();
            if best.is_none_or(|(b, _)| d < b) {
                best = Some((d, r));
            }
        }
    }
    Ok(best.map(|(_, r)| r))
}

fn send_row(
    atp: &Place,
    osm_match: Option<LocalFeatureRef>,
    matching: MatchInfo,
    osm: &OsmFeatures,
    signals: &Signals,
    out: &SyncSender<ParquetRow>,
) -> Result<()> {
    let feature = osm_match.map(|r| osm.index.get_feature(r)).transpose()?;
    let score = match &feature {
        Some(feature) => {
            let candidate = OsmCandidate {
                feature,
                strings: &osm.strings,
            };
            signals.score(atp, candidate.tags())
        }
        None => signals.score(atp, std::iter::empty()),
    };
    let conflated = ConflatedFeature {
        atp: Some(atp.deep_clone()),
        osm: feature,
        score,
        matching,
    };
    out.send(ParquetRow::from_conflated(conflated, &osm.strings)?)?;
    Ok(())
}

/// Logs a `conflate.match: progress` snapshot (elapsed time, the two
/// counts from `counts`, and a `pipeline::memstats` snapshot -- `rss_bytes`/
/// `rss_file_bytes` in particular, since that's where resident pages of
/// `OsmFeatureIndex`'s mmap'd tables are expected to show up) at most
/// once per [PROGRESS_LOG_INTERVAL], across however many parallel
/// callers race to call this. `counts` is a closure, not plain
/// arguments, so the (relaxed, so individually cheap) atomic loads to
/// build it only happen on the rare call that actually ends up logging.
fn maybe_log_progress(
    start: Instant,
    last_log_ms: &AtomicU64,
    counts: impl FnOnce() -> (u64, u64),
) {
    let now_ms = start.elapsed().as_millis() as u64;
    let last = last_log_ms.load(Ordering::Relaxed);
    let interval_ms = PROGRESS_LOG_INTERVAL.as_millis() as u64;
    if now_ms.saturating_sub(last) < interval_ms {
        return;
    }
    if last_log_ms
        .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return; // another thread already logged this interval
    }

    let (atp_features_processed, osm_candidates_decoded) = counts();
    info_with_memory!(
        elapsed_seconds = start.elapsed().as_secs_f64(),
        atp_features_processed = atp_features_processed,
        osm_candidates_decoded = osm_candidates_decoded;
        "conflate.match: progress"
    );
}

fn write_conflated(
    cf: Receiver<ParquetRow>,
    progress: ProgressBar,
    workdir: &Path,
    out: &Path,
    pipeline_run_id: &str,
) -> Result<()> {
    let start = Instant::now();
    let row_count = AtomicU64::new(0);
    // write_conflated's only caller (conflate()) doesn't run any other
    // external sort concurrently with it, so this gets the full
    // chunk-size budget; see EXTERNAL_SORT_CHUNK_BYTES.
    let sorter: ExternalSorter<ParquetRow, std::io::Error, MemoryLimitedBufferBuilder> =
        ExternalSorterBuilder::new()
            .with_tmp_dir(workdir)
            .with_buffer(MemoryLimitedBufferBuilder::new(
                EXTERNAL_SORT_CHUNK_BYTES as u64,
            ))
            .build()?;
    // Every row's importance score, collected on the way into the sort:
    // `importance` is a score's quantile among all of them, which is
    // only known once every row has been produced.
    let scores = std::sync::Mutex::new(Vec::new());
    let sorted = sorter.sort(cf.iter().map(|row| {
        progress.set_length(row_count.fetch_add(1, Ordering::SeqCst));
        scores.lock().expect("scores lock").push(row.score());
        std::io::Result::Ok(row)
    }))?;
    // `sort` has consumed every row by now. Taken rather than
    // `into_inner`, since `sorted` still borrows `scores`.
    let quantiles = Quantiles::new(std::mem::take(&mut *scores.lock().expect("scores lock")));
    progress.set_length(row_count.load(Ordering::SeqCst));
    let provenance_bom = crate::pipeline::provenance::build_bom_for_conflated_parquet(
        workdir,
        pipeline_run_id,
        None, // the standalone sidecar BOM carries the file's own hashes; this embedded copy can't
    )
    .context("could not assemble provenance BOM")?
    .to_string();
    let mut writer =
        ParquetWriter::create(out, /* max_rows_per_group */ 200_000, &provenance_bom)?;
    for row in sorted {
        let row = row?;
        let importance = quantiles.get(row.score());
        writer.write(row, importance)?;
        progress.inc(1);
    }
    writer.close()?;
    progress.finish();
    info_with_memory!(
        elapsed_seconds = start.elapsed().as_secs_f64(),
        rows_written = row_count.load(Ordering::SeqCst),
        bytes_written = std::fs::metadata(out)?.len();
        "conflate.write: done"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(i: u32) -> LocalFeatureRef {
        LocalFeatureRef::for_test(i)
    }

    #[test]
    fn test_top_spiders() {
        let mut counts = HashMap::new();
        for spider in ["b", "a", "c", "a", "b", "a"] {
            count_spider(&mut counts, spider);
        }
        count_spider(&mut counts, "d");
        assert_eq!(top_spiders(&counts, 3), "a:3,b:2,c:1");
        assert_eq!(top_spiders(&HashMap::new(), 3), "");
    }

    #[test]
    fn test_keep_best() {
        let mut best = Vec::new();
        keep_best(&mut best, 3, 0.5, r(1));
        keep_best(&mut best, 3, 0.0, r(2)); // no evidence
        keep_best(&mut best, 3, 0.9, r(3));
        keep_best(&mut best, 3, 0.5, r(4)); // tie: first seen stays ahead
        keep_best(&mut best, 3, 0.9, r(3)); // seen again in another cell range
        keep_best(&mut best, 3, 0.1, r(5)); // too low to keep
        assert_eq!(best, vec![(0.9, r(3)), (0.5, r(1)), (0.5, r(4))]);

        // With room for one, behaves like keeping the first best.
        let mut one = Vec::new();
        keep_best(&mut one, 1, 0.5, r(1));
        keep_best(&mut one, 1, 0.5, r(2));
        keep_best(&mut one, 1, 0.7, r(3));
        assert_eq!(one, vec![(0.7, r(3))]);
    }

    fn candidates(ordinal: u64, spider: u64, c: &[(f64, u32)]) -> ExclusiveCandidates {
        ExclusiveCandidates {
            ordinal,
            spider,
            candidates: c.iter().map(|&(s, i)| (s, r(i), false, "test")).collect(),
            s2_cell_id: cell_at(0.0),
            colocated_radius: None,
        }
    }

    /// The S2 cell of a point `meters` north of Zürich HB.
    fn cell_at(meters: f64) -> u64 {
        let ll = s2::latlng::LatLng::from_degrees(47.3782 + meters / 111_195.0, 8.5402);
        s2::cellid::CellID::from(ll).0
    }

    #[test]
    fn test_assign_exclusive() {
        let assigned = assign_exclusive(&[
            // Two platforms of one stop both like OSM feature 1 best.
            // The better one keeps it; the other falls back to 2.
            candidates(0, 7, &[(0.6, 1), (0.5, 2)]),
            candidates(1, 7, &[(0.8, 1)]),
            // A third one only likes 1, which is taken: unmatched.
            candidates(2, 7, &[(0.4, 1)]),
            // Another spider's feature may take 1 as well.
            candidates(3, 8, &[(0.3, 1)]),
            // No candidates at all.
            candidates(4, 7, &[]),
        ]);
        assert_eq!(assigned[&0], Some(r(2)));
        assert_eq!(assigned[&1], Some(r(1)));
        assert_eq!(assigned[&2], None);
        assert_eq!(assigned[&3], Some(r(1)));
        assert_eq!(assigned[&4], None);
        assert_eq!(assigned.len(), 5);
    }

    #[test]
    fn test_assign_exclusive_shared() {
        let shared = |ordinal, score| ExclusiveCandidates {
            ordinal,
            spider: 7,
            candidates: vec![(score, r(1), true, "test")],
            s2_cell_id: cell_at(0.0),
            colocated_radius: None,
        };
        let assigned = assign_exclusive(&[
            // Three quays whose IDs are all on OSM feature 1.
            shared(0, 0.95),
            shared(1, 0.93),
            shared(2, 0.91),
            // A platform that is only in the same stop doesn't get it.
            candidates(3, 7, &[(0.7, 1), (0.6, 2)]),
        ]);
        assert_eq!(assigned[&0], Some(r(1)));
        assert_eq!(assigned[&1], Some(r(1)));
        assert_eq!(assigned[&2], Some(r(1)));
        assert_eq!(assigned[&3], Some(r(2)));
    }

    #[test]
    fn test_assign_exclusive_colocated() {
        let radius = Some(crate::matchers::meters_to_chord_angle(25.0));
        let at = |ordinal, spider, meters, c: &[(f64, u32)]| ExclusiveCandidates {
            s2_cell_id: cell_at(meters),
            colocated_radius: radius,
            ..candidates(ordinal, spider, c)
        };
        let assigned = assign_exclusive(&[
            // A car dealer's sales, and its service 10 m away: both
            // get the dealer's one OSM feature.
            at(0, 7, 0.0, &[(0.9, 1)]),
            at(1, 7, 10.0, &[(0.8, 1)]),
            // Another branch 300 m away falls back to its own feature,
            // a third one with nothing else stays unmatched.
            at(2, 7, 300.0, &[(0.5, 1), (0.4, 2)]),
            at(3, 7, 350.0, &[(0.3, 1)]),
            // Close to the second one, but not to the first one, which
            // took the feature: unmatched.
            at(4, 7, 30.0, &[(0.7, 1)]),
            // Without a colocated radius, even the same point can't share.
            candidates(5, 7, &[(0.6, 1)]),
        ]);
        assert_eq!(assigned[&0], Some(r(1)));
        assert_eq!(assigned[&1], Some(r(1)));
        assert_eq!(assigned[&2], Some(r(2)));
        assert_eq!(assigned[&3], None);
        assert_eq!(assigned[&4], None);
        assert_eq!(assigned[&5], None);
    }

    #[test]
    fn test_assign_exclusive_ties_are_reproducible() {
        let run = |order: &[u64]| {
            let all = [
                candidates(10, 7, &[(0.5, 1), (0.4, 2)]),
                candidates(11, 7, &[(0.5, 1), (0.4, 3)]),
            ];
            let input: Vec<_> = order
                .iter()
                .map(|&o| {
                    let e = &all[(o - 10) as usize];
                    ExclusiveCandidates {
                        ordinal: e.ordinal,
                        spider: e.spider,
                        candidates: e.candidates.clone(),
                        s2_cell_id: e.s2_cell_id,
                        colocated_radius: e.colocated_radius,
                    }
                })
                .collect();
            assign_exclusive(&input)
        };
        let a = run(&[10, 11]);
        let b = run(&[11, 10]);
        assert_eq!(a, b);
        assert_eq!(a[&10], Some(r(1)));
        assert_eq!(a[&11], Some(r(3)));
    }
}
