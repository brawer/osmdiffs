//! Whether AllThePlaces covers a brand, network or operator *around*
//! an unmatched OpenStreetMap feature, not just somewhere in its
//! country.
//!
//! [super::deletion] makes an unmatched OSM feature a deletion
//! candidate when ATP has features of its brand, of the same kind, in
//! the same country. But a spider often has only part of a brand: the
//! stores of one region, or the post boxes of one city. All other OSM
//! features of the brand in that country then look like closed stores.
//! So we also ask, for each candidate: of the OSM features of its brand
//! nearby, how many did ATP match? A candidate is kept when at least
//! [MIN_LOCAL_SHARE] of them were matched, counting the candidates
//! themselves as unmatched. Both counts are of OSM features of the same
//! brand, so the general density of an area cancels out: a gas station
//! in the Nevada desert has few neighbours, but so do the matched ones.
//!
//! "Nearby" depends on the brand: ATP lists a Burger King every few
//! kilometers, but a car dealer of some brand only every hundred. The
//! radius is [SPACING_FACTOR] times the median distance between a
//! spider's features of the brand and their nearest neighbour, but at
//! least [MIN_RADIUS_METERS] and at most [MAX_RADIUS_METERS]. Since
//! a spider may emit several brands, and a brand may come from several
//! spiders (often one per country), spacing goes by spider and brand,
//! and the radius for a brand in a country is the largest among the
//! spiders that have the brand there.
//!
//! See `docs/notes/2026-11-09-local-deletion-coverage.md`.

use crate::matchers::meters_to_chord_angle;
use crate::utils::countries::Country;
use s2::cellid::CellID;
use s2::latlng::LatLng;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// A candidate is kept when at least this share of the OSM features of
/// its brand within the radius were matched by ATP.
pub(super) const MIN_LOCAL_SHARE: f64 = 0.5;

/// The radius is at least this many meters, so that a dense brand's
/// candidates still see enough neighbours.
const MIN_RADIUS_METERS: f64 = 25_000.0;

/// The radius is at most this many meters, so that ATP features in
/// another region say nothing about the candidate's.
pub(super) const MAX_RADIUS_METERS: f64 = 150_000.0;

/// The radius is this many times the median nearest-neighbour distance
/// of a spider's features of the brand.
const SPACING_FACTOR: f64 = 5.0;

/// S2 level of the cells that [LocalCoverage] counts in. Cells at
/// level 9 are about 18 km wide.
const LOCAL_CELL_LEVEL: u64 = 9;

/// S2 level at which [SpacingBuilder] keeps positions, about 600 m.
/// That is coarse, but only spacings above [MIN_RADIUS_METERS] /
/// [SPACING_FACTOR], or 5 km, change the radius.
const SPACING_CELL_LEVEL: u64 = 14;

/// For the nearest neighbour of a feature, [SpacingBuilder] looks at
/// this many features before and after it in S2 cell order. That
/// misses the true nearest neighbour across a Hilbert curve seam, so
/// the spacing comes out a bit too large rather than too small.
const SPACING_WINDOW: usize = 16;

const EARTH_RADIUS_METERS: f64 = 6_371_000.0;

/// A brand, network or operator, by Wikidata ID or by normalized name,
/// together with the kind of feature: `None` for points of interest,
/// or the infrastructure kind for street furniture such as post boxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct LocalKey {
    /// A Wikidata QID, or a hash of the normalized name with the top
    /// bit set.
    brand: u64,
    kind: Option<&'static str>,
}

impl LocalKey {
    pub(super) fn wikidata(id: u64, kind: Option<&'static str>) -> Self {
        LocalKey {
            brand: id & !(1 << 63),
            kind,
        }
    }

    pub(super) fn name(normalized: &str, kind: Option<&'static str>) -> Self {
        // DefaultHasher::new() always uses the same keys, so this is
        // stable within a process, which is all we need.
        let mut hasher = std::hash::DefaultHasher::new();
        normalized.hash(&mut hasher);
        LocalKey {
            brand: hasher.finish() | (1 << 63),
            kind,
        }
    }
}

/// Bytes that a `HashMap` with `capacity` holds for its entries of
/// `entry_size` bytes each, including hashbrown's control byte per
/// slot, but not anything the entries point to.
pub(super) fn table_bytes(capacity: usize, entry_size: usize) -> usize {
    capacity * (entry_size + 1)
}

/// Collects the positions of ATP features by spider and [LocalKey], to
/// compute each brand's radius.
#[derive(Debug, Default)]
pub(super) struct SpacingBuilder {
    /// For each spider (by hash) and key, the [SPACING_CELL_LEVEL]
    /// cells of its features, shifted so they fit 32 bits.
    positions: HashMap<(u64, LocalKey), Vec<u32>>,
    /// For each key and country, the spiders that have it there.
    spiders: HashMap<(LocalKey, Option<Country>), Vec<u64>>,
}

impl SpacingBuilder {
    /// Adds an ATP feature of `spider` (hashed) with `key` in
    /// `country`, at `s2_cell_id`.
    pub(super) fn add(
        &mut self,
        spider: u64,
        key: LocalKey,
        country: Option<Country>,
        s2_cell_id: u64,
    ) {
        let cell = CellID(s2_cell_id).parent(SPACING_CELL_LEVEL);
        // A level-14 cell ID has its lowest set bit at 2 * (30 - 14) = 32,
        // so the 31 bits above it say which cell it is.
        self.positions
            .entry((spider, key))
            .or_default()
            .push((cell.0 >> 33) as u32);
        let spiders = self.spiders.entry((key, country)).or_default();
        if !spiders.contains(&spider) {
            spiders.push(spider);
        }
    }

    /// Approximate bytes on the heap.
    pub(super) fn bytes(&self) -> usize {
        table_bytes(
            self.positions.capacity(),
            size_of::<((u64, LocalKey), Vec<u32>)>(),
        ) + self
            .positions
            .values()
            .map(|v| v.capacity() * size_of::<u32>())
            .sum::<usize>()
            + table_bytes(
                self.spiders.capacity(),
                size_of::<((LocalKey, Option<Country>), Vec<u64>)>(),
            )
            + self
                .spiders
                .values()
                .map(|v| v.capacity() * size_of::<u64>())
                .sum::<usize>()
    }

    /// The radius, in meters, for each key and country.
    pub(super) fn build(self) -> Radii {
        let spacing: HashMap<(u64, LocalKey), Option<f64>> = self
            .positions
            .into_iter()
            .map(|(k, mut cells)| {
                // ATP places come sorted by S2 cell, but don't rely on it.
                cells.sort_unstable();
                (k, median_spacing(&cells))
            })
            .collect();
        let radii = self
            .spiders
            .into_iter()
            .map(|((key, country), spiders)| {
                let radius = spiders
                    .iter()
                    .map(|&s| radius_for(spacing.get(&(s, key)).copied().flatten()))
                    .fold(MIN_RADIUS_METERS, f64::max);
                ((key, country), radius as f32)
            })
            .collect();
        Radii(radii)
    }
}

/// The radius for a spider's features of a brand whose median
/// nearest-neighbour distance is `spacing` meters, or `None` if it has
/// fewer than two features at distinct positions.
fn radius_for(spacing: Option<f64>) -> f64 {
    match spacing {
        Some(meters) => (meters * SPACING_FACTOR).clamp(MIN_RADIUS_METERS, MAX_RADIUS_METERS),
        None => MAX_RADIUS_METERS,
    }
}

fn cell_latlng(shifted: u32) -> LatLng {
    LatLng::from(CellID(((shifted as u64) << 33) | (1 << 32)))
}

fn distance_meters(a: &LatLng, b: &LatLng) -> f64 {
    a.distance(b).rad() * EARTH_RADIUS_METERS
}

/// The median distance, in meters, from each of the sorted `cells` to
/// its nearest neighbour, looking [SPACING_WINDOW] cells either way.
/// Features in the same cell count as one.
fn median_spacing(cells: &[u32]) -> Option<f64> {
    let mut distinct = cells.to_vec();
    distinct.dedup();
    if distinct.len() < 2 {
        return None;
    }
    let latlngs: Vec<LatLng> = distinct.iter().map(|&c| cell_latlng(c)).collect();
    let mut nearest: Vec<f64> = (0..latlngs.len())
        .map(|i| {
            let lo = i.saturating_sub(SPACING_WINDOW);
            let hi = (i + SPACING_WINDOW + 1).min(latlngs.len());
            (lo..hi)
                .filter(|&j| j != i)
                .map(|j| distance_meters(&latlngs[i], &latlngs[j]))
                .fold(f64::INFINITY, f64::min)
        })
        .collect();
    let mid = nearest.len() / 2;
    let (_, median, _) = nearest.select_nth_unstable_by(mid, f64::total_cmp);
    Some(*median)
}

/// The radius, in meters, for each [LocalKey] and country, from
/// [SpacingBuilder::build].
#[derive(Debug, Default)]
pub(super) struct Radii(HashMap<(LocalKey, Option<Country>), f32>);

impl Radii {
    /// The radius for `key` in `country`; [MAX_RADIUS_METERS] if ATP
    /// has no feature of `key` in `country`.
    pub(super) fn get(&self, key: LocalKey, country: Option<Country>) -> f64 {
        self.0
            .get(&(key, country))
            .map_or(MAX_RADIUS_METERS, |&r| r as f64)
    }

    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn bytes(&self) -> usize {
        table_bytes(
            self.0.capacity(),
            size_of::<((LocalKey, Option<Country>), f32)>(),
        )
    }
}

/// How many OSM features of each [LocalKey] ATP matched, and how many
/// are deletion candidates, by [LOCAL_CELL_LEVEL] cell.
#[derive(Debug, Default)]
pub(super) struct LocalCoverage(HashMap<(LocalKey, u64), Counts>);

/// Counts of OSM features in one cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Counts {
    pub(super) matched: u32,
    pub(super) candidates: u32,
}

impl Counts {
    /// The share of matched features, or 0 if there are none at all.
    pub(super) fn share(&self) -> f64 {
        let total = self.matched + self.candidates;
        if total == 0 {
            0.0
        } else {
            self.matched as f64 / total as f64
        }
    }
}

fn local_cell(lng: f64, lat: f64) -> u64 {
    CellID::from(LatLng::from_degrees(lat, lng))
        .parent(LOCAL_CELL_LEVEL)
        .0
}

impl LocalCoverage {
    /// Counts a matched OSM feature with `key` at (`lng`, `lat`).
    pub(super) fn add_matched(&mut self, key: LocalKey, lng: f64, lat: f64) {
        self.0
            .entry((key, local_cell(lng, lat)))
            .or_default()
            .matched += 1;
    }

    /// Counts a deletion candidate with `key` at (`lng`, `lat`).
    pub(super) fn add_candidate(&mut self, key: LocalKey, lng: f64, lat: f64) {
        self.0
            .entry((key, local_cell(lng, lat)))
            .or_default()
            .candidates += 1;
    }

    /// The counts for `key` in the cells within about `radius_meters`
    /// of (`lng`, `lat`). Cells at the edge count in full, so this
    /// reaches up to one cell width further.
    pub(super) fn around(&self, key: LocalKey, lng: f64, lat: f64, radius_meters: f64) -> Counts {
        let coverer = s2::region::RegionCoverer {
            max_cells: 1000,
            min_level: LOCAL_CELL_LEVEL as u8,
            max_level: LOCAL_CELL_LEVEL as u8,
            level_mod: 1,
        };
        let center = s2::point::Point::from(LatLng::from_degrees(lat, lng));
        let cap =
            s2::cap::Cap::from_center_chordangle(&center, &meters_to_chord_angle(radius_meters));
        let mut total = Counts::default();
        for cell in coverer.covering(&cap).0 {
            if let Some(c) = self.0.get(&(key, cell.0)) {
                total.matched += c.matched;
                total.candidates += c.candidates;
            }
        }
        total
    }

    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    /// Approximate bytes on the heap.
    pub(super) fn bytes(&self) -> usize {
        table_bytes(self.0.capacity(), size_of::<((LocalKey, u64), Counts)>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(lng: f64, lat: f64) -> u64 {
        CellID::from(LatLng::from_degrees(lat, lng)).0
    }

    /// `n` points spaced `meters` apart along the equator, from 10°E.
    fn row(n: usize, meters: f64) -> Vec<u64> {
        let step = meters / EARTH_RADIUS_METERS * 180.0 / std::f64::consts::PI;
        (0..n).map(|i| cell(10.0 + i as f64 * step, 0.0)).collect()
    }

    #[test]
    fn test_median_spacing() {
        let mut cells: Vec<u32> = row(20, 10_000.0)
            .into_iter()
            .map(|c| (CellID(c).parent(SPACING_CELL_LEVEL).0 >> 33) as u32)
            .collect();
        cells.sort_unstable();
        let spacing = median_spacing(&cells).expect("spacing");
        assert!((spacing - 10_000.0).abs() < 1_000.0, "{spacing}");
        assert_eq!(median_spacing(&cells[..1]), None);
        assert_eq!(median_spacing(&[cells[0], cells[0]]), None);
    }

    #[test]
    fn test_radii() {
        let q1 = LocalKey::wikidata(1, None);
        let q2 = LocalKey::wikidata(2, None);
        let mut b = SpacingBuilder::default();
        // Spider 1 has Q1 every 2 km: radius at the 25 km floor.
        for c in row(20, 2_000.0) {
            b.add(1, q1, None, c);
        }
        // Spider 2 has Q2 every 10 km: 50 km.
        for c in row(20, 10_000.0) {
            b.add(2, q2, None, c);
        }
        // Spider 3 has Q2 once in Switzerland: the cap.
        b.add(3, q2, Some(Country::SWITZERLAND), cell(8.5, 47.4));
        assert!(b.bytes() > 0);
        let radii = b.build();
        assert_eq!(radii.get(q1, None), MIN_RADIUS_METERS);
        let r = radii.get(q2, None);
        assert!((r - 50_000.0).abs() < 5_000.0, "{r}");
        assert_eq!(radii.get(q2, Some(Country::SWITZERLAND)), MAX_RADIUS_METERS);
        assert_eq!(radii.get(q1, Some(Country::GERMANY)), MAX_RADIUS_METERS);
    }

    #[test]
    fn test_keys() {
        assert_ne!(LocalKey::name("coop", None), LocalKey::name("migros", None));
        assert_eq!(LocalKey::name("coop", None), LocalKey::name("coop", None));
        assert_ne!(
            LocalKey::wikidata(1, None),
            LocalKey::wikidata(1, Some("post_box"))
        );
    }

    #[test]
    fn test_local_coverage() {
        let q1 = LocalKey::wikidata(1, None);
        let mut lc = LocalCoverage::default();
        // Zürich: 3 matched, 1 candidate. Bern, 95 km away: 2 candidates.
        for _ in 0..3 {
            lc.add_matched(q1, 8.54, 47.38);
        }
        lc.add_candidate(q1, 8.55, 47.37);
        lc.add_candidate(q1, 7.44, 46.95);
        lc.add_candidate(q1, 7.45, 46.94);
        let zurich = lc.around(q1, 8.54, 47.38, 25_000.0);
        assert_eq!(
            zurich,
            Counts {
                matched: 3,
                candidates: 1
            }
        );
        assert_eq!(zurich.share(), 0.75);
        let bern = lc.around(q1, 7.44, 46.95, 25_000.0);
        assert_eq!(bern.share(), 0.0);
        let wide = lc.around(q1, 7.44, 46.95, 150_000.0);
        assert_eq!(wide.share(), 0.5);
        assert_eq!(
            lc.around(LocalKey::wikidata(2, None), 8.54, 47.38, 25_000.0),
            Counts::default()
        );
        assert!(lc.bytes() > 0);
        assert!((2..=4).contains(&lc.len()));
    }
}
