//! Some AllThePlaces spiders give positions in mainland China in the
//! GCJ-02 or BD-09 coordinate systems, as Chinese map services do,
//! and not in WGS-84 like OpenStreetMap. Both are shifted by a few
//! hundred meters, so these features wouldn't match. Spiders differ:
//! `starbucks_cn` uses GCJ-02, `shell` BD-09, and `apple` WGS-84. We
//! don't know which beforehand, so while importing a spider's features,
//! [detect] tries each system for those in mainland China, and picks
//! the one under which clearly the most of them are next to an OSM
//! feature of the same brand. `import_atp` then [convert]s them, so
//! `alltheplaces.parquet` is in WGS-84 throughout.
//!
//! See `docs/notes/2026-11-09-china-datum.md`.

use crate::matchers::{MatchMask, OsmCandidate, meters_to_chord_angle, parse_wikidata_ids};
use crate::places::Place;
use crate::tables::OsmFeatures;
use crate::utils::countries::{Country, country};
use anyhow::Result;
use geo::MapCoords;
use rayon::prelude::*;
use std::f64::consts::PI;

/// A coordinate system for positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Datum {
    Wgs84,
    Gcj02,
    Bd09,
}

const DATUMS: [Datum; 3] = [Datum::Wgs84, Datum::Gcj02, Datum::Bd09];

impl Datum {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Datum::Wgs84 => "wgs84",
            Datum::Gcj02 => "gcj02",
            Datum::Bd09 => "bd09",
        }
    }

    /// (`lng`, `lat`) of this datum, in WGS-84.
    fn to_wgs84(self, lng: f64, lat: f64) -> (f64, f64) {
        match self {
            Datum::Wgs84 => (lng, lat),
            Datum::Gcj02 => gcj02_to_wgs84(lng, lat),
            Datum::Bd09 => {
                let (lng, lat) = bd09_to_gcj02(lng, lat);
                gcj02_to_wgs84(lng, lat)
            }
        }
    }
}

/// How close, in meters, a converted position must be to an OSM
/// feature of the same brand to count as a hit. The shifts are 300 to
/// 1,000 m, so the right datum stands out clearly.
const HIT_METERS: f64 = 30.0;

/// A spider's datum needs at least this many hits.
const MIN_HITS: usize = 3;

/// A datum other than WGS-84 needs this many times more hits than the
/// next best one.
const MIN_RATIO: usize = 3;

/// The datum of `places`, all from one spider and in mainland China
/// (see [in_mainland_china]), found by voting: for each feature with
/// `brand:wikidata`, which datums put it within [HIT_METERS] of an
/// OSM feature of that brand. Also returns the votes, as hits for
/// WGS-84, GCJ-02 and BD-09.
pub(super) fn detect(places: &[Place], osm: &OsmFeatures) -> Result<(Datum, [usize; 3])> {
    let votes: Vec<[bool; 3]> = places
        .par_iter()
        .map(|place| -> Result<[bool; 3]> {
            let brands: Vec<u64> = brand_ids(place).collect();
            let mut hit = [false; 3];
            if brands.is_empty() {
                return Ok(hit);
            }
            let (lng, lat) = position(place);
            for (i, datum) in DATUMS.iter().enumerate() {
                let (lng, lat) = datum.to_wgs84(lng, lat);
                hit[i] = has_brand_nearby(lng, lat, place.mask, &brands, osm)?;
            }
            Ok(hit)
        })
        .collect::<Result<_>>()?;
    let mut hits = [0; 3];
    for vote in votes {
        for i in 0..3 {
            hits[i] += vote[i] as usize;
        }
    }
    Ok((pick(hits), hits))
}

/// The datum with clearly the most hits, given as WGS-84, GCJ-02 and
/// BD-09; WGS-84 if none stands out.
fn pick(hits: [usize; 3]) -> Datum {
    let best = (0..3).max_by_key(|&i| (hits[i], i == 0)).expect("three");
    let runner_up = (0..3).filter(|&i| i != best).map(|i| hits[i]).max();
    if best == 0 || hits[best] < MIN_HITS || hits[best] < MIN_RATIO * runner_up.unwrap_or(0).max(1)
    {
        return Datum::Wgs84;
    }
    DATUMS[best]
}

fn has_brand_nearby(
    lng: f64,
    lat: f64,
    mask: MatchMask,
    brands: &[u64],
    osm: &OsmFeatures,
) -> Result<bool> {
    let coverer = s2::region::RegionCoverer {
        max_cells: 8,
        min_level: 12,
        max_level: s2::cellid::MAX_LEVEL as u8,
        level_mod: 1,
    };
    let center = s2::point::Point::from(s2::latlng::LatLng::from_degrees(lat, lng));
    let cap = s2::cap::Cap::from_center_chordangle(&center, &meters_to_chord_angle(HIT_METERS));
    for cell in coverer.covering(&cap).0 {
        for r in osm.index.query(cell.range_min()..=cell.range_max(), mask) {
            let feature = osm.index.get_feature(r)?;
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &osm.strings,
            };
            let mut ids = candidate
                .tags()
                .filter(|(k, _)| *k == "brand:wikidata")
                .flat_map(|(_, v)| parse_wikidata_ids(v));
            if ids.any(|id| brands.contains(&id)) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn brand_ids(place: &Place) -> impl Iterator<Item = u64> + '_ {
    place
        .tags
        .iter()
        .filter(|(k, _)| k == "brand:wikidata")
        .flat_map(|(_, v)| parse_wikidata_ids(v))
}

fn position(place: &Place) -> (f64, f64) {
    let ll = s2::latlng::LatLng::from(s2::cellid::CellID(place.s2_cell_id));
    (ll.lng.deg(), ll.lat.deg())
}

/// Whether `place` lies in mainland China, where Chinese map services
/// use GCJ-02 or BD-09.
pub(super) fn place_in_mainland_china(place: &Place) -> bool {
    let (lng, lat) = position(place);
    in_mainland_china(lng, lat)
}

fn in_mainland_china(lng: f64, lat: f64) -> bool {
    // A cheap box first: the country lookup is slower.
    (73.0..136.0).contains(&lng)
        && (18.0..54.0).contains(&lat)
        && country(lng, lat) == Some(Country::CHINA)
}

/// `place`, given in `datum`, converted to WGS-84.
pub(super) fn convert(place: Place, datum: Datum) -> Place {
    let shape = place.shape().map_coords(|c| {
        let (x, y) = datum.to_wgs84(c.x, c.y);
        geo::Coord { x, y }
    });
    let (lng, lat) = position(&place);
    let (lng, lat) = datum.to_wgs84(lng, lat);
    let cell = s2::cellid::CellID::from(s2::latlng::LatLng::from_degrees(lat, lng));
    Place {
        s2_cell_id: cell.0,
        shape_wkb: crate::geometry::encode_wkb(&shape),
        ..place
    }
}

// The usual conversion formulas for GCJ-02 and BD-09, as in many open
// source libraries. They are accurate to a meter or two.

const A: f64 = 6_378_245.0;
const EE: f64 = 0.006_693_421_622_965_943;

fn transform_lat(x: f64, y: f64) -> f64 {
    let mut r = -100.0 + 2.0 * x + 3.0 * y + 0.2 * y * y + 0.1 * x * y + 0.2 * x.abs().sqrt();
    r += (20.0 * (6.0 * x * PI).sin() + 20.0 * (2.0 * x * PI).sin()) * 2.0 / 3.0;
    r += (20.0 * (y * PI).sin() + 40.0 * (y / 3.0 * PI).sin()) * 2.0 / 3.0;
    r += (160.0 * (y / 12.0 * PI).sin() + 320.0 * (y * PI / 30.0).sin()) * 2.0 / 3.0;
    r
}

fn transform_lng(x: f64, y: f64) -> f64 {
    let mut r = 300.0 + x + 2.0 * y + 0.1 * x * x + 0.1 * x * y + 0.1 * x.abs().sqrt();
    r += (20.0 * (6.0 * x * PI).sin() + 20.0 * (2.0 * x * PI).sin()) * 2.0 / 3.0;
    r += (20.0 * (x * PI).sin() + 40.0 * (x / 3.0 * PI).sin()) * 2.0 / 3.0;
    r += (150.0 * (x / 12.0 * PI).sin() + 300.0 * (x / 30.0 * PI).sin()) * 2.0 / 3.0;
    r
}

fn wgs84_to_gcj02(lng: f64, lat: f64) -> (f64, f64) {
    let dlat = transform_lat(lng - 105.0, lat - 35.0);
    let dlng = transform_lng(lng - 105.0, lat - 35.0);
    let rad = lat / 180.0 * PI;
    let magic = 1.0 - EE * rad.sin() * rad.sin();
    let sqrt_magic = magic.sqrt();
    let dlat = (dlat * 180.0) / ((A * (1.0 - EE)) / (magic * sqrt_magic) * PI);
    let dlng = (dlng * 180.0) / (A / sqrt_magic * rad.cos() * PI);
    (lng + dlng, lat + dlat)
}

/// The inverse of [wgs84_to_gcj02], by fixed-point iteration, which
/// gets within a millimeter.
fn gcj02_to_wgs84(lng: f64, lat: f64) -> (f64, f64) {
    let (mut x, mut y) = (lng, lat);
    for _ in 0..5 {
        let (gx, gy) = wgs84_to_gcj02(x, y);
        x -= gx - lng;
        y -= gy - lat;
    }
    (x, y)
}

fn bd09_to_gcj02(lng: f64, lat: f64) -> (f64, f64) {
    let (x, y) = (lng - 0.0065, lat - 0.006);
    let z = (x * x + y * y).sqrt() - 0.00002 * (y * PI * 3000.0 / 180.0).sin();
    let theta = y.atan2(x) - 0.000003 * (x * PI * 3000.0 / 180.0).cos();
    (z * theta.cos(), z * theta.sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meters(a: (f64, f64), b: (f64, f64)) -> f64 {
        let dx = (a.0 - b.0).to_radians() * a.1.to_radians().cos();
        let dy = (a.1 - b.1).to_radians();
        (dx * dx + dy * dy).sqrt() * 6_371_000.0
    }

    #[test]
    fn test_gcj02_round_trip() {
        // People's Square, Shanghai.
        let wgs = (121.4737, 31.2304);
        let gcj = wgs84_to_gcj02(wgs.0, wgs.1);
        let shift = meters(wgs, gcj);
        assert!((400.0..700.0).contains(&shift), "{shift}");
        assert!(meters(gcj02_to_wgs84(gcj.0, gcj.1), wgs) < 0.01);
    }

    #[test]
    fn test_bd09() {
        // BD-09 is GCJ-02 plus another few hundred meters.
        let wgs = (116.3975, 39.9087); // Tian'anmen, Beijing
        let bd = Datum::Bd09.to_wgs84(wgs.0, wgs.1);
        assert!(meters(wgs, bd) > 500.0);
        assert_eq!(Datum::Wgs84.to_wgs84(wgs.0, wgs.1), wgs);
    }

    #[test]
    fn test_pick() {
        // starbucks_cn in Shanghai, Beijing and Guangdong.
        assert_eq!(pick([2, 472, 4]), Datum::Gcj02);
        assert_eq!(pick([1, 0, 25]), Datum::Bd09);
        assert_eq!(pick([10, 0, 0]), Datum::Wgs84);
        // Too few hits, or no clear winner.
        assert_eq!(pick([0, 2, 0]), Datum::Wgs84);
        assert_eq!(pick([0, 4, 2]), Datum::Wgs84);
        assert_eq!(pick([3, 9, 0]), Datum::Gcj02);
        assert_eq!(pick([0, 0, 0]), Datum::Wgs84);
    }

    #[test]
    fn test_in_mainland_china() {
        assert!(in_mainland_china(121.4737, 31.2304)); // Shanghai
        assert!(!in_mainland_china(114.1694, 22.3193)); // Hong Kong
        assert!(!in_mainland_china(121.5654, 25.0330)); // Taipei
        assert!(!in_mainland_china(139.7671, 35.6812)); // Tokyo
    }
}
