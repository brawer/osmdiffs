//! Some brands' store data holds positions that someone copied from a
//! Google Maps place URL, `/maps/place/…/@lat,lon,17z`. Those give the
//! camera center of the map, which Google moves west so that the side
//! panel doesn't hide the marker: at zoom 17, about 0.0022° of
//! longitude at any latitude, with no north–south part. Such features
//! sit 120–300 m west of where they are. AllThePlaces' own
//! `url_to_coords` makes the same mistake
//! (alltheplaces/alltheplaces#20570), but for many spiders it is in the
//! brand's data, which no change to AllThePlaces can fix.
//!
//! While `import_atp` reads a spider, [probe] checks each feature with a
//! Wikidata ID (brand, operator or network) against OSM features with
//! the same ID: is one right at the feature, or, if not, is one in the
//! window east of it where the store would be? The same window mirrored
//! to the west serves as a control. The features themselves are passed
//! on unchanged; only counts and the median offset of the hits are
//! kept, in `spider-counts.tsv`. From them, [is_google_camera_coords]
//! decides whether `conflate` also tries a corrected position for the
//! spider's features, and [corrected] makes it.
//!
//! See `docs/notes/2026-11-16-google-camera-coords.md` and
//! brawer/osmdiffs#971.

use super::spider_counts::SpiderCounts;
use crate::matchers::{MatchMask, OsmCandidate, meters_to_chord_angle, parse_wikidata_ids};
use crate::pipeline::wikidata_ids::WIKIDATA_KEYS;
use crate::places::Place;
use crate::tables::OsmFeatures;
use anyhow::Result;
use geo::{Centroid, MapCoords};

/// The window, in degrees of longitude east of an ATP feature, where an
/// OSM feature is if the ATP position is a camera center at zoom 17. It
/// spans the offsets seen in practice, which vary with the window size
/// of whoever copied the URL: most are 0.0020–0.0024°, some 0.0027°.
const WINDOW_LNG: (f64, f64) = (0.0018, 0.0030);

/// The window's half height, in degrees of latitude. The camera only
/// moves sideways.
const WINDOW_LAT: f64 = 0.0003;

/// How close, in meters, an OSM feature with the same ID must be for an
/// ATP feature to count as being right at it.
const EXACT_METERS: f64 = 30.0;

/// Meters per degree of latitude, near enough for sizing a search.
const METERS_PER_DEGREE: f64 = 111_320.0;

/// What [probe] found for one feature.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Probe {
    /// An OSM feature with the same ID is within [EXACT_METERS].
    pub exact: bool,
    /// If none is: the offset, in degrees of longitude (ATP minus OSM,
    /// so negative), of one in the window east of the feature.
    pub hit: Option<f64>,
    /// If none is: whether one is in the mirrored window to the west.
    pub control: bool,
}

/// The camera counts of one spider, gathered while `import_atp` reads
/// it, for [SpiderCounts].
#[derive(Debug, Default)]
pub(super) struct Tally {
    exact: u64,
    control: u64,
    offsets: Vec<f64>,
}

impl Tally {
    pub(super) fn add(&mut self, probe: Probe) {
        self.exact += probe.exact as u64;
        self.control += probe.control as u64;
        self.offsets.extend(probe.hit);
    }

    /// Stores the counts, and the median offset of the hits, in
    /// `counts`.
    pub(super) fn finish(mut self, counts: &mut SpiderCounts) {
        counts.exact_coords_hits = self.exact;
        counts.google_camera_coords_hits = self.offsets.len() as u64;
        counts.google_camera_coords_control = self.control;
        counts.google_camera_coords_offset = median(&mut self.offsets);
    }
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

/// Whether the counts of a spider show that a good part of its
/// positions are Google Maps camera centers: at least 8 features in the
/// window, at least four times as many as in the control window (plus
/// one), and at least a tenth of the features that are in OSM either
/// way. See the notes for which spiders this flags.
pub(in crate::pipeline) fn is_google_camera_coords(c: &SpiderCounts) -> bool {
    let hits = c.google_camera_coords_hits;
    hits >= 8
        && hits >= 4 * (c.google_camera_coords_control + 1)
        && hits * 10 >= hits + c.exact_coords_hits
}

/// Probes `place` against `osm`; see the module documentation. A
/// feature without a Wikidata ID gives an empty [Probe].
pub(super) fn probe(place: &Place, osm: &OsmFeatures) -> Result<Probe> {
    let ids = wikidata_ids(place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    if ids.is_empty() {
        return Ok(Probe::default());
    }
    let (lng, lat) = position(place);
    let search = Search {
        atp: (lng, lat),
        mask: place.mask,
        ids: &ids,
        osm,
    };
    let near = |dlng: f64, dlat: f64| {
        let dx = dlng * METERS_PER_DEGREE * lat.to_radians().cos();
        let dy = dlat * METERS_PER_DEGREE;
        dx * dx + dy * dy <= EXACT_METERS * EXACT_METERS
    };
    if search.find(lng, EXACT_METERS, near)?.is_some() {
        return Ok(Probe {
            exact: true,
            ..Default::default()
        });
    }

    // Each window search is centered on the window, and reaches as far
    // as its corners.
    let center = (WINDOW_LNG.0 + WINDOW_LNG.1) / 2.0;
    let half_width = (WINDOW_LNG.1 - WINDOW_LNG.0) / 2.0 * lat.to_radians().cos();
    let radius = METERS_PER_DEGREE * half_width.hypot(WINDOW_LAT);
    let in_window = |dlng: f64, dlat: f64| {
        (-WINDOW_LNG.1..=-WINDOW_LNG.0).contains(&dlng) && dlat.abs() <= WINDOW_LAT
    };
    let hit = search.find(lng + center, radius, in_window)?;
    let control = search.find(lng - center, radius, |x, y| in_window(-x, y))?;
    Ok(Probe {
        exact: false,
        hit,
        control: control.is_some(),
    })
}

/// `place` with its position corrected by the spider's median `offset`
/// (ATP minus OSM, in degrees of longitude, as in
/// [SpiderCounts::google_camera_coords_offset]): where it is if its
/// position is a camera center.
pub(in crate::pipeline) fn corrected(place: &Place, offset: f64) -> Place {
    let shape = place.shape().map_coords(|c| geo::Coord {
        x: c.x - offset,
        y: c.y,
    });
    let (lng, lat) = position(place);
    let cell = s2::cellid::CellID::from(s2::latlng::LatLng::from_degrees(lat, lng - offset));
    Place {
        s2_cell_id: cell.0,
        shape_wkb: crate::geometry::encode_wkb(&shape),
        ..place.deep_clone()
    }
}

/// A search for OSM features that share a Wikidata ID with one ATP
/// feature.
struct Search<'a, 'o> {
    /// The ATP feature's position, longitude and latitude.
    atp: (f64, f64),
    mask: MatchMask,
    ids: &'a [u64],
    osm: &'a OsmFeatures<'o>,
}

impl Search<'_, '_> {
    /// An OSM feature with one of the IDs within `radius_m` of the point
    /// at longitude `lng` and the ATP feature's latitude, whose
    /// centroid lies at an offset that `accept` takes. Gives that
    /// offset in longitude: the ATP feature's position minus the OSM
    /// feature's centroid, in degrees, as `accept` also gets it,
    /// together with the latitude difference.
    fn find(
        &self,
        lng: f64,
        radius_m: f64,
        accept: impl Fn(f64, f64) -> bool,
    ) -> Result<Option<f64>> {
        let coverer = s2::region::RegionCoverer {
            max_cells: 8,
            min_level: 12,
            max_level: s2::cellid::MAX_LEVEL as u8,
            level_mod: 1,
        };
        let (atp_lng, atp_lat) = self.atp;
        let center = s2::point::Point::from(s2::latlng::LatLng::from_degrees(atp_lat, lng));
        let cap = s2::cap::Cap::from_center_chordangle(&center, &meters_to_chord_angle(radius_m));
        for cell in coverer.covering(&cap).0 {
            for r in self
                .osm
                .index
                .query(cell.range_min()..=cell.range_max(), self.mask)
            {
                let feature = self.osm.index.get_feature(r)?;
                let candidate = OsmCandidate {
                    feature: &feature,
                    strings: &self.osm.strings,
                };
                if !wikidata_ids(candidate.tags())
                    .iter()
                    .any(|id| self.ids.contains(id))
                {
                    continue;
                }
                let Some(c) = candidate.geometry().ok().and_then(|g| g.centroid()) else {
                    continue;
                };
                let dlng = lng_difference(atp_lng, c.x());
                if accept(dlng, atp_lat - c.y()) {
                    return Ok(Some(dlng));
                }
            }
        }
        Ok(None)
    }
}

/// Longitude difference `a - b`, in degrees, also across the
/// antimeridian.
fn lng_difference(a: f64, b: f64) -> f64 {
    let d = a - b;
    if d > 180.0 {
        d - 360.0
    } else if d < -180.0 {
        d + 360.0
    } else {
        d
    }
}

/// The brand, operator and network Wikidata IDs in `tags`.
fn wikidata_ids<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<u64> {
    tags.filter(|(k, _)| WIKIDATA_KEYS.contains(k))
        .flat_map(|(_, v)| parse_wikidata_ids(v))
        .collect()
}

fn position(place: &Place) -> (f64, f64) {
    let ll = s2::latlng::LatLng::from(s2::cellid::CellID(place.s2_cell_id));
    (ll.lng.deg(), ll.lat.deg())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(exact: u64, hits: u64, control: u64) -> SpiderCounts {
        SpiderCounts {
            exact_coords_hits: exact,
            google_camera_coords_hits: hits,
            google_camera_coords_control: control,
            ..Default::default()
        }
    }

    #[test]
    fn test_is_google_camera_coords() {
        // From the production run of 2026-11-15.
        assert!(is_google_camera_coords(&counts(1, 84, 0))); // save_mart_us
        assert!(is_google_camera_coords(&counts(379, 85, 0))); // aspen_dental_us
        assert!(!is_google_camera_coords(&counts(1089, 81, 11))); // pepco
        assert!(!is_google_camera_coords(&counts(2342, 60, 9))); // toyota_eu
        // Too few, or not clearly more than in the control window.
        assert!(!is_google_camera_coords(&counts(0, 7, 0)));
        assert!(!is_google_camera_coords(&counts(0, 15, 3)));
        assert!(is_google_camera_coords(&counts(0, 16, 3)));
    }

    #[test]
    fn test_tally() {
        let mut tally = Tally::default();
        let hit = |offset| Probe {
            hit: Some(offset),
            ..Default::default()
        };
        for probe in [
            hit(-0.0021),
            hit(-0.0027),
            hit(-0.0022),
            Probe {
                exact: true,
                ..Default::default()
            },
            Probe {
                control: true,
                ..Default::default()
            },
            Probe::default(),
        ] {
            tally.add(probe);
        }
        let mut c = SpiderCounts::default();
        tally.finish(&mut c);
        assert_eq!(
            (
                c.exact_coords_hits,
                c.google_camera_coords_hits,
                c.google_camera_coords_control
            ),
            (1, 3, 1)
        );
        assert_eq!(c.google_camera_coords_offset, Some(-0.0022));

        let mut c = SpiderCounts::default();
        Tally::default().finish(&mut c);
        assert_eq!(c.google_camera_coords_offset, None);
    }

    #[test]
    fn test_median() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), Some(2.5));
    }

    #[test]
    fn test_lng_difference() {
        assert!((lng_difference(9.7386, 9.7408) - -0.0022).abs() < 1e-9);
        assert!((lng_difference(-179.9999, 179.9999) - 0.0002).abs() < 1e-9);
        assert!((lng_difference(179.9999, -179.9999) - -0.0002).abs() < 1e-9);
    }

    fn shop_mask() -> MatchMask {
        let mut m = MatchMask::default();
        m.add_tag("shop", "clothes");
        m
    }

    fn place(lng: f64, lat: f64, tags: &[(&str, &str)]) -> Place {
        let shape = geo::Geometry::from(geo::Point::new(lng, lat));
        Place::new(
            &geo::Coord { x: lng, y: lat },
            "test".into(),
            shop_mask(),
            tags.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            &shape,
            crate::utils::UtcTimestamp(time::UtcDateTime::from_unix_timestamp(0).unwrap()),
        )
        .unwrap()
    }

    /// One OSM shop of C&A (Q701338), at (`lng`, `lat`).
    fn build_osm(dir: &std::path::Path, lng: f64, lat: f64) -> Result<OsmFeatures<'static>> {
        use crate::tables::{Feature, FeatureToIndex, OsmFeatureIndex, StringPool};
        let strings = ["brand:wikidata", "Q701338", "shop", "clothes"].map(String::from);
        let strings_path = dir.join("strings");
        let pool = StringPool::create(strings.into_iter(), dir, &strings_path)?;
        let tag = |s: &str| pool.lookup(s).unwrap() as u32;
        let cell = s2::cellid::CellID::from(s2::latlng::LatLng::from_degrees(lat, lng)).0;
        let feature = FeatureToIndex {
            centroid_s2_cell_id: cell,
            feature: Some(Feature {
                id: 24354727351,
                tags: vec![
                    tag("brand:wikidata"),
                    tag("Q701338"),
                    tag("shop"),
                    tag("clothes"),
                ],
                geometry_wkb: crate::geometry::encode_wkb(&geo::Geometry::from(geo::Point::new(
                    lng, lat,
                ))),
                ..Default::default()
            }),
            match_mask: shop_mask().0 as u32,
            coverage_s2_cell_id: vec![cell],
        };
        let index_path = dir.join("osm-features.index");
        let index = OsmFeatureIndex::create(std::iter::once(feature), dir, &index_path)?;
        Ok(OsmFeatures {
            index,
            strings: pool,
        })
    }

    #[test]
    fn test_probe() -> Result<()> {
        let dir = tempfile::tempdir()?;
        // C&A Dornbirn, node/2435472735.
        let (lng, lat) = (9.7408157, 47.4132541);
        let osm = build_osm(dir.path(), lng, lat)?;
        let brand = [("brand:wikidata", "Q701338")];

        // Right at the OSM shop, or 10 m off.
        let p = probe(&place(lng, lat, &brand), &osm)?;
        assert_eq!((p.exact, p.hit, p.control), (true, None, false));
        let p = probe(&place(lng + 0.0001, lat, &brand), &osm)?;
        assert!(p.exact);

        // Where ATP has it: the camera center, 0.0022595° west.
        let p = probe(&place(9.7385562, 47.41324, &brand), &osm)?;
        assert!(!p.exact && !p.control);
        let offset = p.hit.expect("a hit");
        assert!((offset - -0.0022595).abs() < 1e-6, "{offset}");

        // The mirror image: east of the OSM shop.
        let p = probe(&place(lng + 0.0022, lat, &brand), &osm)?;
        assert_eq!((p.exact, p.hit, p.control), (false, None, true));

        // West, but also 0.0005° north: outside the window.
        let p = probe(&place(lng - 0.0022, lat + 0.0005, &brand), &osm)?;
        assert_eq!(p, Probe::default());

        // The operator counts too; another ID or none doesn't.
        let operator = [("operator:wikidata", "Q701338")];
        assert!(probe(&place(lng, lat, &operator), &osm)?.exact);
        let other = [("brand:wikidata", "Q1")];
        assert_eq!(probe(&place(lng, lat, &other), &osm)?, Probe::default());
        assert_eq!(probe(&place(lng, lat, &[]), &osm)?, Probe::default());
        Ok(())
    }

    #[test]
    fn test_corrected() {
        // C&A Dornbirn: ATP has the camera center, OSM node/2435472735
        // is at 9.7408157, 47.4132541.
        let shape = geo::Geometry::from(geo::Point::new(9.7385563, 47.41324));
        let place = Place::new(
            &geo::Coord {
                x: 9.7385563,
                y: 47.41324,
            },
            "c_and_a".into(),
            {
                let mut m = MatchMask::default();
                m.add_tag("shop", "clothes");
                m
            },
            vec![("shop".into(), "clothes".into())],
            &shape,
            crate::utils::UtcTimestamp(time::UtcDateTime::from_unix_timestamp(0).unwrap()),
        )
        .unwrap();
        let fixed = corrected(&place, -0.0022);
        let (lng, lat) = position(&fixed);
        assert!((lng - 9.7407563).abs() < 1e-6, "{lng}");
        assert!((lat - 47.41324).abs() < 1e-6, "{lat}");
        let geo::Geometry::Point(p) = fixed.shape() else {
            panic!("not a point");
        };
        assert!((p.x() - 9.7407563).abs() < 1e-9);
        assert_eq!(fixed.tags, place.tags);
    }
}
