//! Matching by official IDs that AllThePlaces (ATP) and OpenStreetMap
//! (OSM) share, such as the national ID of a French school or the
//! reference number of a historic place in the United States.
//!
//! Which keys hold such IDs is a table, [OFFICIAL_IDS], of (ATP key,
//! OSM key) pairs, rather than code for each country. An ID that is
//! the same on both sides says these are the same place, whatever
//! else differs. IDs that differ on both sides for the same pair say
//! these are different places, whatever else agrees, such as a
//! collège and its SEGPA section at the same address.
//!
//! Points of interest, such as schools, compare IDs in
//! [super::poi_matcher::PoiMatcher], before all other evidence. ATP
//! features with an ID that no other matcher handles, such as the
//! historic places of the US National Register, get an
//! [OfficialIdMatcher]. See `docs/notes/2026-11-17-official-ids.md`.
//!
//! Swiss transit stops share IFOPT IDs too, but those have a matcher
//! of their own, which also knows how the IDs of a stop and its
//! platforms relate; see [super::transit_matcher].

use super::{
    MatchMask, Matcher, OsmCandidate, geo_point_distance, geo_point_distance_score, split_values,
};
use crate::places::Place;
use geo::{Closest, ClosestPoint};
use s2::{cell::Cell, cellid::CellID, latlng::LatLng, point::Point, s1::ChordAngle};

/// Keys that hold the same official ID, as (ATP key, OSM key). Values
/// are compared without regard to case or surrounding spaces, and may
/// list several `;`-separated IDs.
///
/// - `ref:FR:UAI`: the national ID of a French school (Unité
///   administrative immatriculée), such as `0351686T`. ATP has it for
///   all French schools (`enseignement_premier_second_degre_fr`); OSM
///   mostly tags it as `ref:UAI`, sometimes as `ref:FR:UAI`.
/// - `ref:nrhp`: the reference number of a place in the US National
///   Register of Historic Places, such as `84003502`
///   (`national_register_of_historic_places`).
pub(crate) const OFFICIAL_IDS: &[(&str, &str)] = &[
    ("ref:FR:UAI", "ref:UAI"),
    ("ref:FR:UAI", "ref:FR:UAI"),
    ("ref:nrhp", "ref:nrhp"),
];

/// Whether `key` holds an official ID on either side, for
/// [MatchMask::OFFICIAL_ID].
pub(crate) fn is_official_id_key(key: &str) -> bool {
    OFFICIAL_IDS
        .iter()
        .any(|&(atp, osm)| key == atp || key == osm)
}

/// The official IDs of an ATP feature, as (ATP key, ID) with the ID
/// trimmed and uppercased.
pub(crate) fn atp_ids<'a>(
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> Vec<(&'static str, String)> {
    let mut ids = Vec::new();
    for (key, value) in tags {
        let Some(&(atp_key, _)) = OFFICIAL_IDS.iter().find(|(atp, _)| *atp == key) else {
            continue;
        };
        for id in split_values(value) {
            let id = id.to_uppercase();
            if !ids.iter().any(|(k, v)| *k == atp_key && *v == id) {
                ids.push((atp_key, id));
            }
        }
    }
    ids
}

/// What the official IDs of two features say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdEvidence {
    /// Some ID is the same on both sides.
    Same,
    /// Both sides have IDs of the same kind, and none is shared.
    Different,
    /// Neither: no IDs on one side, or of different kinds.
    Unknown,
}

/// Compares the ATP feature's IDs, as [atp_ids] gives them, with those
/// of an OSM feature.
pub(crate) fn compare<'a>(
    atp: &[(&'static str, String)],
    osm_tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> IdEvidence {
    if atp.is_empty() {
        return IdEvidence::Unknown;
    }
    let mut evidence = IdEvidence::Unknown;
    for (key, value) in osm_tags {
        for &(atp_key, osm_key) in OFFICIAL_IDS {
            if key != osm_key {
                continue;
            }
            for (_, id) in atp.iter().filter(|(k, _)| *k == atp_key) {
                if split_values(value).any(|v| v.eq_ignore_ascii_case(id)) {
                    return IdEvidence::Same;
                }
                evidence = IdEvidence::Different;
            }
        }
    }
    evidence
}

/// Matches with an equal official ID only up to this distance from
/// the nearest point of the OSM shape. A historic district can be
/// large, but its ATP point should still lie within it or near it.
const MAX_METERS: f64 = 1000.0;

/// A candidate with the same official ID scores at least this much,
/// however far it is within [MAX_METERS].
pub(crate) const SAME_ID_MIN_SCORE: f64 = 0.9;

/// The score for an OSM feature with the same official ID, `geometry`
/// being its shape and `center` the ATP position: [SAME_ID_MIN_SCORE]
/// plus the rest by distance to the nearest point of the shape, or
/// 0.0 beyond [MAX_METERS].
pub(crate) fn same_id_score(center: &Point, geometry: &geo::Geometry<f64>) -> f64 {
    let ll = LatLng::from(*center);
    let atp = geo::Point::new(ll.lng.deg(), ll.lat.deg());
    let nearest = match geometry.closest_point(&atp) {
        Closest::Intersection(_) => atp,
        Closest::SinglePoint(p) => p,
        Closest::Indeterminate => return 0.0,
    };
    if geo_point_distance(center, &nearest) > MAX_METERS {
        return 0.0;
    }
    SAME_ID_MIN_SCORE
        + (1.0 - SAME_ID_MIN_SCORE) * geo_point_distance_score(center, &nearest, MAX_METERS)
}

/// Matches ATP features that have an official ID, but no other
/// matcher, such as historic places: only to an OSM feature with the
/// same ID, within [MAX_METERS] of its shape.
///
/// A place that isn't matched isn't reported as missing in OSM, nor
/// does it count for deletion coverage: OSM often has the place, just
/// without the ID.
pub struct OfficialIdMatcher {
    center: Point,
    ids: Vec<(&'static str, String)>,
}

impl OfficialIdMatcher {
    pub fn for_place(place: &Place) -> Option<OfficialIdMatcher> {
        if !place.mask.intersects(&MatchMask::OFFICIAL_ID) {
            return None;
        }
        let ids = atp_ids(place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        if ids.is_empty() {
            return None;
        }
        Some(OfficialIdMatcher {
            center: Cell::from(CellID(place.s2_cell_id)).center(),
            ids,
        })
    }
}

impl Matcher for OfficialIdMatcher {
    fn rule(&self, _candidate: &OsmCandidate) -> &'static str {
        "official_id"
    }

    fn counts_for_coverage(&self) -> bool {
        false
    }

    fn reports_missing(&self) -> bool {
        false
    }

    fn max_distance(&self) -> Option<ChordAngle> {
        Some(super::meters_to_chord_angle(MAX_METERS))
    }

    fn score_osm_candidate(&self, candidate: &OsmCandidate) -> f64 {
        if compare(&self.ids, candidate.tags()) != IdEvidence::Same {
            return 0.0;
        }
        let Ok(geometry) = candidate.geometry() else {
            return 0.0;
        };
        same_id_score(&self.center, &geometry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::{Feature, StringPool};
    use crate::utils::UtcTimestamp;
    use tempfile::TempDir;

    fn ids(tags: &[(&'static str, &'static str)]) -> Vec<(&'static str, String)> {
        atp_ids(tags.iter().copied())
    }

    #[test]
    fn test_atp_ids() {
        assert_eq!(
            ids(&[
                ("ref:FR:UAI", " 0351686t "),
                ("ref", "0351686T"),
                ("name", "École")
            ]),
            vec![("ref:FR:UAI", "0351686T".to_string())]
        );
        assert_eq!(
            ids(&[("ref:nrhp", "84003502;84003503;84003502")]),
            vec![
                ("ref:nrhp", "84003502".to_string()),
                ("ref:nrhp", "84003503".to_string())
            ]
        );
        assert!(ids(&[("ref:UAI", "0351686T")]).is_empty());
    }

    #[test]
    fn test_compare() {
        let uai = ids(&[("ref:FR:UAI", "0351686T")]);
        let cmp = |osm: &[(&str, &str)]| compare(&uai, osm.iter().copied());
        assert_eq!(cmp(&[("ref:UAI", "0351686T")]), IdEvidence::Same);
        assert_eq!(cmp(&[("ref:UAI", "0351686t")]), IdEvidence::Same);
        assert_eq!(cmp(&[("ref:FR:UAI", "0351686T")]), IdEvidence::Same);
        assert_eq!(cmp(&[("ref:UAI", "0350001A; 0351686T")]), IdEvidence::Same);
        // A collège and its SEGPA section have IDs of their own.
        assert_eq!(cmp(&[("ref:UAI", "0350001A")]), IdEvidence::Different);
        assert_eq!(cmp(&[("amenity", "school")]), IdEvidence::Unknown);
        // Only IDs of the same kind count.
        assert_eq!(cmp(&[("ref:nrhp", "0351686T")]), IdEvidence::Unknown);
        assert_eq!(cmp(&[("ref", "0351686T")]), IdEvidence::Unknown);
        assert_eq!(
            compare(&[], [("ref:UAI", "0351686T")].into_iter()),
            IdEvidence::Unknown
        );
    }

    #[test]
    fn test_is_official_id_key() {
        assert!(is_official_id_key("ref:FR:UAI"));
        assert!(is_official_id_key("ref:UAI"));
        assert!(is_official_id_key("ref:nrhp"));
        assert!(!is_official_id_key("ref"));
        assert!(!is_official_id_key("ref:IFOPT"));
    }

    /// The Old State House in Boston.
    const LAT: f64 = 42.35872;
    const LON: f64 = -71.05774;

    fn place(tags: &[(&str, &str)]) -> Place {
        let mut mask = MatchMask::default();
        for (k, v) in tags {
            mask.add_tag(k, v);
        }
        let point = geo::Geometry::from(geo::Point::new(LON, LAT));
        Place::new(
            &geo::Coord { x: LON, y: LAT },
            "national_register_of_historic_places".to_string(),
            mask,
            tags.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            &point,
            UtcTimestamp(time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap()),
        )
        .expect("valid place")
    }

    /// Scores an OSM point `meters` north of the ATP feature.
    fn score(atp: &Place, osm: &[(&str, &str)], meters: f64) -> f64 {
        let matcher = OfficialIdMatcher::for_place(atp).expect("matcher");
        let dir = TempDir::new().unwrap();
        let mut strings: Vec<String> = osm
            .iter()
            .flat_map(|(k, v)| [k.to_string(), v.to_string()])
            .collect();
        strings.sort();
        strings.dedup();
        let strings =
            StringPool::create(strings.into_iter(), dir.path(), &dir.path().join("strings"))
                .unwrap();
        let lat = LAT + meters / 111_195.0;
        let feature = Feature {
            id: 1,
            geometry_wkb: crate::geometry::encode_wkb(&geo::Geometry::from(geo::Point::new(
                LON, lat,
            ))),
            tags: osm
                .iter()
                .flat_map(|(k, v)| {
                    [
                        strings.lookup(k).unwrap() as u32,
                        strings.lookup(v).unwrap() as u32,
                    ]
                })
                .collect(),
            ..Default::default()
        };
        matcher.score_osm_candidate(&OsmCandidate {
            feature: &feature,
            strings: &strings,
        })
    }

    #[test]
    fn test_official_id_matcher() {
        let atp = place(&[
            ("historic", "monument"),
            ("heritage", "2"),
            ("heritage:operator", "nrhp"),
            ("ref:nrhp", "66000779"),
            ("name", "Old State House"),
        ]);
        assert_eq!(atp.mask, MatchMask::OFFICIAL_ID);
        let same = [("building", "yes"), ("ref:nrhp", "66000779")];
        assert!((score(&atp, &same, 0.0) - 1.0).abs() < 1e-6);
        let far = score(&atp, &same, 900.0);
        assert!((SAME_ID_MIN_SCORE..0.92).contains(&far), "{far}");
        assert_eq!(score(&atp, &same, 1100.0), 0.0);
        assert_eq!(score(&atp, &[("ref:nrhp", "66000780")], 0.0), 0.0);
        assert_eq!(score(&atp, &[("name", "Old State House")], 0.0), 0.0);

        let m = OfficialIdMatcher::for_place(&atp).unwrap();
        assert!(!m.reports_missing());
        assert!(!m.counts_for_coverage());

        // A shop has no official ID, and so no such matcher.
        let shop = place(&[("shop", "books"), ("name", "Old State House Books")]);
        assert!(OfficialIdMatcher::for_place(&shop).is_none());
    }
}
