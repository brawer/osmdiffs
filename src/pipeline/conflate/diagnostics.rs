//! What conflate finds out about each spider's matching on the side:
//! why features found no match in OpenStreetMap, and how often a
//! feature would match by chance. Written to
//! `conflate.spider-diagnostics.tsv` in the workdir for
//! `compute_spider_stats`, since `conflated.parquet` doesn't keep the
//! candidates that the matchers rejected.

use super::s2_util::MergedCellRanges;
use crate::matchers::{
    OsmCandidate, brand_domains::BrandDomains, contact::Nondescript, create_matcher, match_distance,
};
use crate::pipeline::spider_stats::DISTANCE_BINS_M;
use crate::places::{Place, PlaceReader};
use crate::tables::{LocalFeatureRef, OsmFeatures};
use anyhow::{Context, Result};
use geo::{Destination, Haversine};
use rayon::prelude::*;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Filename, within `workdir`, of the diagnostics that conflate writes.
const FILENAME: &str = "conflate.spider-diagnostics.tsv";

const HEADER: &str = "spider\tno_candidate\trejected\ttaken\trejected_distance_m\
    \tchance_sampled\tchance_matched";

/// How far [chance_pairs] moves features.
const CHANCE_SHIFT_METERS: f64 = 2000.0;

/// How many features per spider [chance_pairs] moves, roughly.
const CHANCE_SAMPLE: u64 = 100;

/// At most this many rejected candidates are kept per ATP feature, to
/// find the nearest among them.
pub(super) const MAX_REJECTED: usize = 64;

/// Path of the diagnostics that conflate writes in `workdir`.
pub(in crate::pipeline) fn path(workdir: &Path) -> PathBuf {
    workdir.join(FILENAME)
}

/// Why an ATP feature ended up `missing_in_osm`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Missing {
    /// No OSM feature of its kind within the matcher's distance.
    NoCandidate,
    /// OSM features of its kind were close enough, but the matcher
    /// found no evidence for any of them. Holds the distance to the
    /// nearest one in meters, if it could be computed.
    Rejected(Option<f64>),
    /// The matcher accepted some OSM features, but other features of
    /// the same spider took them; see `conflate::assign_exclusive`.
    Taken,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::pipeline) struct SpiderDiagnostics {
    pub spider: String,
    /// `missing_in_osm` rows by [Missing].
    pub no_candidate: u64,
    pub rejected: u64,
    pub taken: u64,
    /// Histogram of the distance to the nearest rejected candidate,
    /// with the bins of [DISTANCE_BINS_M].
    pub rejected_distance_m: Vec<u64>,
    /// Features moved by [CHANCE_SHIFT_METERS], and how many of them
    /// still matched something.
    pub chance_sampled: u64,
    pub chance_matched: u64,
}

impl SpiderDiagnostics {
    fn new(spider: &str) -> Self {
        SpiderDiagnostics {
            spider: spider.to_string(),
            rejected_distance_m: vec![0; DISTANCE_BINS_M.len() + 1],
            ..Default::default()
        }
    }
}

/// Diagnostics of all spiders, filled in while conflating.
#[derive(Debug, Default)]
pub(super) struct Diagnostics {
    spiders: HashMap<String, SpiderDiagnostics>,
}

impl Diagnostics {
    fn spider(&mut self, spider: &str) -> &mut SpiderDiagnostics {
        if !self.spiders.contains_key(spider) {
            self.spiders
                .insert(spider.to_string(), SpiderDiagnostics::new(spider));
        }
        self.spiders.get_mut(spider).expect("inserted")
    }

    pub(super) fn add_missing(&mut self, spider: &str, missing: Missing) {
        let s = self.spider(spider);
        match missing {
            Missing::NoCandidate => s.no_candidate += 1,
            Missing::Taken => s.taken += 1,
            Missing::Rejected(distance) => {
                s.rejected += 1;
                if let Some(d) = distance {
                    let bin = DISTANCE_BINS_M.partition_point(|&upper| f64::from(upper) <= d);
                    s.rejected_distance_m[bin] += 1;
                }
            }
        }
    }

    fn add_chance(&mut self, spider: &str, matched: bool) {
        let s = self.spider(spider);
        s.chance_sampled += 1;
        if matched {
            s.chance_matched += 1;
        }
    }

    /// Writes the diagnostics, sorted by spider, to `out` via a
    /// temporary file.
    pub(super) fn write(&self, out: &Path) -> Result<()> {
        let mut spiders: Vec<&SpiderDiagnostics> = self.spiders.values().collect();
        spiders.sort_by(|a, b| a.spider.cmp(&b.spider));
        let mut tmp = out.to_path_buf();
        tmp.add_extension("tmp");
        let mut w = BufWriter::new(
            std::fs::File::create(&tmp).with_context(|| format!("could not create {tmp:?}"))?,
        );
        writeln!(w, "{HEADER}")?;
        for s in spiders {
            let bins: Vec<String> = s.rejected_distance_m.iter().map(u64::to_string).collect();
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                s.spider,
                s.no_candidate,
                s.rejected,
                s.taken,
                bins.join(","),
                s.chance_sampled,
                s.chance_matched
            )?;
        }
        w.into_inner()?.sync_all()?;
        std::fs::rename(&tmp, out)?;
        Ok(())
    }
}

/// Reads diagnostics that [Diagnostics::write] wrote, by spider.
pub(in crate::pipeline) fn read(path: &Path) -> Result<HashMap<String, SpiderDiagnostics>> {
    let file = std::fs::File::open(path).with_context(|| format!("could not open {path:?}"))?;
    let mut lines = BufReader::new(file).lines();
    if lines.next().transpose()?.as_deref() != Some(HEADER) {
        anyhow::bail!("{path:?} doesn't start with the expected header");
    }
    let mut result = HashMap::new();
    for line in lines {
        let line = line?;
        let cols: Vec<&str> = line.split('\t').collect();
        let [
            spider,
            no_candidate,
            rejected,
            taken,
            distances,
            sampled,
            matched,
        ] = cols[..]
        else {
            anyhow::bail!("bad line in {path:?}: {line:?}");
        };
        let num = |s: &str| -> Result<u64> {
            s.parse()
                .with_context(|| format!("bad number in {path:?}: {line:?}"))
        };
        let rejected_distance_m = distances.split(',').map(num).collect::<Result<Vec<_>>>()?;
        anyhow::ensure!(
            rejected_distance_m.len() == DISTANCE_BINS_M.len() + 1,
            "wrong number of distance bins in {path:?}: {line:?}"
        );
        result.insert(
            spider.to_string(),
            SpiderDiagnostics {
                spider: spider.to_string(),
                no_candidate: num(no_candidate)?,
                rejected: num(rejected)?,
                taken: num(taken)?,
                rejected_distance_m,
                chance_sampled: num(sampled)?,
                chance_matched: num(matched)?,
            },
        );
    }
    Ok(result)
}

/// The distance in meters from `atp` to the nearest of `rejected`.
pub(super) fn nearest(
    atp: &Place,
    rejected: &[LocalFeatureRef],
    osm: &OsmFeatures,
) -> Result<Option<f64>> {
    let shape = atp.shape();
    let mut nearest: Option<f64> = None;
    for &r in rejected {
        let feature = osm.index.get_feature(r)?;
        let candidate = OsmCandidate {
            feature: &feature,
            strings: &osm.strings,
        };
        let Ok(geometry) = candidate.geometry() else {
            continue;
        };
        if let Some(d) = super::evidence::distance_meters(&shape, &geometry)
            && nearest.is_none_or(|n| d < n)
        {
            nearest = Some(d);
        }
    }
    Ok(nearest)
}

fn hash(spider: &str, ordinal: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    spider.hash(&mut hasher);
    ordinal.hash(&mut hasher);
    hasher.finish()
}

/// `atp`, moved [CHANCE_SHIFT_METERS] in a direction that `seed`
/// picks.
fn shifted(atp: &Place, seed: u64) -> Place {
    let center = s2::latlng::LatLng::from(s2::cellid::CellID(atp.s2_cell_id));
    let origin = geo::Point::new(center.lng.deg(), center.lat.deg());
    let bearing = (seed >> 11) as f64 / (1u64 << 53) as f64 * 360.0;
    let moved = Haversine.destination(origin, bearing, CHANCE_SHIFT_METERS);
    let lat_lng = s2::latlng::LatLng::from_degrees(moved.y(), moved.x());
    let mut place = atp.deep_clone();
    place.s2_cell_id = s2::cellid::CellID::from(lat_lng).0;
    place.shape_wkb = crate::geometry::encode_wkb(&geo::Geometry::Point(moved));
    place
}

/// Moves a sample of about [CHANCE_SAMPLE] features of each spider by
/// [CHANCE_SHIFT_METERS], and counts how many of them still match an
/// OSM feature. Those matches are by chance, so their share estimates
/// how many of the spider's matches may be wrong. It overstates it, as
/// in the real matching a true partner usually takes the feature
/// first; see `docs/notes/2026-11-05-infrastructure-matching.md`.
///
/// `features` is the number of features per spider in `atp_reader`.
/// The sample depends only on the input, so it's the same each run.
pub(super) fn chance_pairs(
    atp_reader: &PlaceReader,
    features: &HashMap<String, u64>,
    osm: &OsmFeatures,
    nondescript: &Nondescript,
    brand_domains: &BrandDomains,
    diagnostics: &mut Diagnostics,
) -> Result<()> {
    let coverer = s2::region::RegionCoverer {
        max_cells: 16,
        min_level: 12,
        max_level: s2::cellid::MAX_LEVEL as u8,
        level_mod: 1,
    };
    let mut ordinal: u64 = 0;
    for batch in atp_reader.read_all()? {
        let batch = batch?;
        let first = ordinal;
        ordinal += batch.len() as u64;
        let results: Vec<Option<bool>> = batch
            .par_iter()
            .enumerate()
            .map(|(i, atp)| -> Result<Option<bool>> {
                let n = features.get(&atp.spider).copied().unwrap_or(1);
                let every = n.div_ceil(CHANCE_SAMPLE).max(1);
                let seed = hash(&atp.spider, first + i as u64);
                if !seed.is_multiple_of(every) {
                    return Ok(None);
                }
                let moved = shifted(atp, seed);
                let Some(matcher) = create_matcher(&moved, nondescript, brand_domains) else {
                    return Ok(None);
                };
                let covering = {
                    let cell = s2::cell::Cell::from(s2::cellid::CellID(moved.s2_cell_id));
                    let radius = matcher
                        .max_distance()
                        .unwrap_or_else(|| match_distance(&moved.mask));
                    let cap = s2::cap::Cap::from_center_chordangle(&cell.center(), &radius);
                    coverer.covering(&cap)
                };
                for (lo, hi) in MergedCellRanges::new(covering) {
                    for r in osm.index.query(lo..=hi, moved.mask) {
                        let feature = osm.index.get_feature(r)?;
                        let candidate = OsmCandidate {
                            feature: &feature,
                            strings: &osm.strings,
                        };
                        if matcher.score_osm_candidate(&candidate) > 0.0 {
                            return Ok(Some(true));
                        }
                    }
                }
                Ok(Some(false))
            })
            .collect::<Result<_>>()?;
        for (atp, matched) in batch.iter().zip(results) {
            if let Some(matched) = matched {
                diagnostics.add_chance(&atp.spider, matched);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{Distance, Geodesic};

    #[test]
    fn test_write_read() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let out = path(dir.path());
        let mut d = Diagnostics::default();
        d.add_missing("b", Missing::NoCandidate);
        d.add_missing("b", Missing::Taken);
        d.add_missing("b", Missing::Rejected(Some(12.0)));
        d.add_missing("b", Missing::Rejected(Some(10.0)));
        d.add_missing("b", Missing::Rejected(None));
        d.add_chance("a", true);
        d.add_chance("a", false);
        d.write(&out)?;

        let read = read(&out)?;
        assert_eq!(read.len(), 2);
        let b = &read["b"];
        assert_eq!((b.no_candidate, b.rejected, b.taken), (1, 3, 1));
        assert_eq!(b.rejected_distance_m, [0, 0, 2, 0, 0, 0, 0, 0]);
        let a = &read["a"];
        assert_eq!((a.chance_sampled, a.chance_matched), (2, 1));
        assert_eq!(a.rejected_distance_m.iter().sum::<u64>(), 0);
        Ok(())
    }

    #[test]
    fn test_shifted() {
        let lat_lng = s2::latlng::LatLng::from_degrees(47.37, 8.54);
        let place = Place {
            s2_cell_id: s2::cellid::CellID::from(lat_lng).0,
            spider: "x".into(),
            mask: crate::matchers::MatchMask(1),
            tags: Vec::new(),
            shape_wkb: Vec::new(),
            fetched: crate::utils::UtcTimestamp(time::UtcDateTime::UNIX_EPOCH),
        };
        for seed in [0, 1 << 62, u64::MAX] {
            let moved = shifted(&place, seed);
            let to = s2::latlng::LatLng::from(s2::cellid::CellID(moved.s2_cell_id));
            let d = Geodesic.distance(
                geo::Point::new(8.54, 47.37),
                geo::Point::new(to.lng.deg(), to.lat.deg()),
            );
            assert!((d - CHANCE_SHIFT_METERS).abs() < 10.0, "{d}");
            assert!(matches!(moved.shape(), geo::Geometry::Point(_)));
        }
    }
}
