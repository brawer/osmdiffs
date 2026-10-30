use super::atp::nondescript::Signal;
use super::importance::{Quantiles, Signals};
use super::last_modified;
use crate::{
    make_progress_bar,
    matchers::{OsmCandidate, contact::Nondescript, create_matcher, match_distance},
    pipeline::EXTERNAL_SORT_CHUNK_BYTES,
    places::{Place, PlaceReader},
    tables::{Feature, LocalFeatureRef, OsmFeatures},
};
use anyhow::{Context, Ok, Result};
use ext_sort::{ExternalSorter, ExternalSorterBuilder, buffer::mem::MemoryLimitedBufferBuilder};
use indicatif::{MultiProgress, ProgressBar};
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
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

mod s2_util;
mod writer;
use s2_util::MergedCellRanges;
use writer::{ParquetRow, ParquetWriter};

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
    let input_modified = last_modified(&[
        atp,
        &nondescript_phones,
        &nondescript_emails,
        importance.osmviews,
        importance.osmviews_ranks,
        importance.qrank_quantiles,
    ])?
    .max(osm.modified()?);
    let out_path = workdir.join("conflated.parquet");
    if out_path.exists() && last_modified(&[&out_path])? >= input_modified {
        return Ok(out_path);
    }

    let nondescript = Nondescript::open(&nondescript_phones, &nondescript_emails)?;
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

    let mut producer_result = Ok(());
    let mut writer_result = Ok(());
    thread::scope(|s| {
        let (tx, rx) = sync_channel::<ParquetRow>(8192);
        s.spawn(|| {
            producer_result = produce_rows(
                &atp_reader,
                osm,
                &nondescript,
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
    producer_result?;

    Ok(out_path)
}

struct ConflatedFeature {
    atp: Option<Place>,
    osm: Option<Feature>,
    /// See `pipeline::importance::score`.
    score: f32,
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
    /// Score, OSM feature, and whether the matcher allows other ATP
    /// features to match it too ([crate::matchers::Matcher::may_share]).
    candidates: Vec<(f64, LocalFeatureRef, bool)>,
}

/// Matches every ATP feature, in two passes. The first pass matches
/// features whose matcher isn't exclusive and sends their rows right
/// away; for exclusive ones, it keeps the best few candidates. Then
/// [assign_exclusive] gives each OSM feature to at most one ATP feature
/// per spider. The second pass sends the rows of the exclusive ones.
fn produce_rows(
    atp_reader: &PlaceReader,
    osm: &OsmFeatures,
    nondescript: &Nondescript,
    signals: &Signals,
    progress_bar: ProgressBar,
    out: SyncSender<ParquetRow>,
) -> Result<()> {
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

    let mut batch_start: u64 = 0;
    for batch in atp_reader.read_all()? {
        let batch = batch?;
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

            let Some(matcher) = create_matcher(atp, nondescript) else {
                return Ok(());
            };
            let keep = if matcher.is_exclusive() {
                EXCLUSIVE_CANDIDATES
            } else {
                1
            };
            let covering = {
                let s2_cell = s2::cell::Cell::from(s2::cellid::CellID(atp.s2_cell_id));
                let center = s2_cell.center();
                let radius = match_distance(&atp.mask);
                let cap = s2::cap::Cap::from_center_chordangle(&center, &radius);
                coverer.covering(&cap)
            };
            let mut best = Vec::<(f64, LocalFeatureRef)>::new();
            for (lo, hi) in MergedCellRanges::new(covering) {
                for r in osm.index.query(lo..=hi, atp.mask) {
                    let feature = osm.index.get_feature(r)?;
                    osm_candidates_decoded.fetch_add(1, Ordering::Relaxed);
                    let candidate = OsmCandidate {
                        feature: &feature,
                        strings: &osm.strings,
                    };
                    let score = matcher.score_osm_candidate(&candidate);
                    keep_best(&mut best, keep, score, r);
                }
            }

            if matcher.is_exclusive() {
                let mut candidates = Vec::with_capacity(best.len());
                for (score, r) in best {
                    let feature = osm.index.get_feature(r)?;
                    let candidate = OsmCandidate {
                        feature: &feature,
                        strings: &osm.strings,
                    };
                    candidates.push((score, r, matcher.may_share(&candidate)));
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
                    });
                return Ok(());
            }
            let osm_match = best.first().map(|&(_, r)| r);
            send_row(atp, osm_match, osm, signals, &out)
        })?;
        batch_start += batch.len() as u64;
    }

    let exclusive = exclusive.into_inner().expect("exclusive lock");
    let num_exclusive = exclusive.len();
    let assigned = assign_exclusive(exclusive);
    let mut ordinal: u64 = 0;
    for batch in atp_reader.read_all()? {
        for atp in batch? {
            if let Some(&osm_match) = assigned.get(&ordinal) {
                send_row(&atp, osm_match, osm, signals, &out)?;
            }
            ordinal += 1;
        }
    }

    progress_bar.finish();
    log::info!(
        elapsed_seconds = start.elapsed().as_secs_f64(),
        atp_features_processed = atp_features_processed.load(Ordering::Relaxed),
        osm_candidates_decoded = osm_candidates_decoded.load(Ordering::Relaxed),
        exclusive_atp_features = num_exclusive,
        exclusive_matched = assigned.values().filter(|m| m.is_some()).count();
        "conflate.match: done"
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
/// for others that would not share it. Returns the OSM match (or
/// none) of every ATP feature, by its ordinal.
fn assign_exclusive(exclusive: Vec<ExclusiveCandidates>) -> HashMap<u64, Option<LocalFeatureRef>> {
    let mut assigned: HashMap<u64, Option<LocalFeatureRef>> =
        exclusive.iter().map(|e| (e.ordinal, None)).collect();
    let mut pairs: Vec<(f64, u64, u64, LocalFeatureRef, bool)> = exclusive
        .iter()
        .flat_map(|e| {
            e.candidates
                .iter()
                .map(|&(score, r, share)| (score, e.ordinal, e.spider, r, share))
        })
        .collect();
    // Best score first; ties by ordinal and feature, so that runs are
    // reproducible.
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.3.cmp(&b.3)));
    let mut taken: HashSet<(u64, LocalFeatureRef)> = HashSet::new();
    for (_, ordinal, spider, r, share) in pairs {
        let slot = assigned
            .get_mut(&ordinal)
            .expect("every ordinal is in assigned");
        if slot.is_some() {
            continue;
        }
        let free = taken.insert((spider, r));
        if free || share {
            *slot = Some(r);
        }
    }
    assigned
}

fn send_row(
    atp: &Place,
    osm_match: Option<LocalFeatureRef>,
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
    let stats = crate::pipeline::memstats::snapshot();
    log::info!(
        elapsed_seconds = start.elapsed().as_secs_f64(),
        atp_features_processed = atp_features_processed,
        osm_candidates_decoded = osm_candidates_decoded,
        rss_bytes = stats.rss_bytes,
        rss_peak_bytes = stats.rss_peak_bytes,
        rss_anon_bytes = stats.rss_anon_bytes,
        rss_file_bytes = stats.rss_file_bytes,
        rss_shmem_bytes = stats.rss_shmem_bytes,
        cgroup_current_bytes = stats.cgroup_current_bytes,
        cgroup_max_bytes = stats.cgroup_max_bytes,
        cgroup_peak_bytes = stats.cgroup_peak_bytes;
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
    log::info!(
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
            candidates: c.iter().map(|&(s, i)| (s, r(i), false)).collect(),
        }
    }

    #[test]
    fn test_assign_exclusive() {
        let assigned = assign_exclusive(vec![
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
            candidates: vec![(score, r(1), true)],
        };
        let assigned = assign_exclusive(vec![
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
    fn test_assign_exclusive_ties_are_reproducible() {
        let run = |order: &[u64]| {
            let all = [
                candidates(10, 7, &[(0.5, 1), (0.4, 2)]),
                candidates(11, 7, &[(0.5, 1), (0.4, 3)]),
            ];
            let input = order
                .iter()
                .map(|&o| {
                    let e = &all[(o - 10) as usize];
                    ExclusiveCandidates {
                        ordinal: e.ordinal,
                        spider: e.spider,
                        candidates: e.candidates.clone(),
                    }
                })
                .collect();
            assign_exclusive(input)
        };
        let a = run(&[10, 11]);
        let b = run(&[11, 10]);
        assert_eq!(a, b);
        assert_eq!(a[&10], Some(r(1)));
        assert_eq!(a[&11], Some(r(3)));
    }
}
