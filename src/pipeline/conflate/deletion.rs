//! Which OpenStreetMap features that no AllThePlaces feature matched
//! may be gone from the real world, and should therefore get a row of
//! their own in `conflated.parquet`.
//!
//! AllThePlaces will never cover as much as OpenStreetMap does, so
//! most unmatched OSM features say nothing at all: ATP simply has no
//! source for them. We only emit an OSM-only row when there is a
//! strong signal that ATP would have the feature if it still existed:
//!
//! - [Reason::Wikidata]: its `brand:wikidata`, `network:wikidata` or
//!   `operator:wikidata` names a brand, network or operator for which
//!   ATP has features of the same kind in the same country, such as a
//!   shop of a chain whose shops ATP lists. The kind matters, since
//!   ATP may list the supermarkets of a chain but not its restaurants;
//!   and so does the country, since ATP may list a chain's shops in
//!   one country but not in another.
//! - [Reason::SwissTransitId]: it is a public transport stop with a
//!   Swiss ID, which ATP gets from the authoritative Swiss register,
//!   but ATP knows none of its Swiss IDs. Swiss IDs are IFOPT IDs
//!   (`ref:IFOPT`, `ref:sloid` or `railway:ref:sloid`) starting with
//!   `ch:`, and `uic_ref` codes starting with `85`. A stop whose ID is
//!   known to ATP still exists, even if it wasn't matched: the register
//!   has no bus stop positions, for example, and OSM often maps a stop
//!   with more platforms than the register.
//! - [Reason::SwissTransitStop]: it is a public transport stop in
//!   Switzerland or Liechtenstein without any IFOPT ID or `uic_ref`,
//!   and ATP has no Swiss stop anywhere near it. The Swiss register
//!   covers Liechtenstein too, with the same kinds of IDs. Aerialway stations are left out,
//!   since the register lacks most ski lifts.
//!
//! For stops that one of the Swiss signals is about, the Wikidata
//! signal doesn't apply: the Swiss register lists `network:wikidata`
//! for its stops, and that would flag every unmatched stop of a
//! network. The Swiss signals only apply when ATP actually has the
//! Swiss register, so that a week in which its spider failed doesn't
//! turn every Swiss stop into a deletion candidate.
//!
//! Tag values can hold several `;`-separated values; any one of them
//! counts.

use super::countries::{Country, country};
use crate::matchers::{
    IFOPT_KEYS, MatchMask, is_transit_stop, parse_wikidata_ids, split_values, swiss_uic,
};
use crate::places::Place;
use s2::cellid::CellID;
use std::collections::{HashMap, HashSet};

/// Tag keys whose Wikidata IDs we look up in [AtpCoverage].
const WIKIDATA_KEYS: [&str; 3] = ["brand:wikidata", "network:wikidata", "operator:wikidata"];

/// ATP must have at least this many features with a Swiss IFOPT ID
/// for the Swiss transit signals to apply. The Swiss register has
/// about 75,000.
const MIN_SWISS_TRANSIT_FEATURES: u64 = 10_000;

/// S2 level of the cells in [AtpCoverage::swiss_cells]. Cells at
/// level 15 are about 280 meters wide, so a stop that is neither in
/// the same cell as a Swiss ATP stop nor in a neighbouring one is at
/// least that far from any.
const SWISS_CELL_LEVEL: u64 = 15;

/// Why an unmatched OSM feature is a candidate for deletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Reason {
    Wikidata,
    SwissTransitId,
    SwissTransitStop,
}

/// What AllThePlaces covers, collected from all its features.
#[derive(Debug, Default)]
pub(super) struct AtpCoverage {
    /// For each Wikidata ID in ATP's `brand:wikidata`,
    /// `network:wikidata` and `operator:wikidata` tags, and each
    /// country with ATP features that carry it, the union of the match
    /// masks of those features. A feature in no country, such as on an
    /// oil rig at sea, counts for `None`.
    wikidata: HashMap<(u64, Option<Country>), MatchMask>,

    /// The Swiss IFOPT IDs in ATP, lowercased.
    swiss_ifopt: HashSet<String>,

    /// The Swiss UIC codes in ATP, both stated in `uic_ref` and
    /// derived from Swiss IFOPT IDs.
    swiss_uic: HashSet<String>,

    /// The cells, at [SWISS_CELL_LEVEL], of the ATP features with a
    /// Swiss IFOPT ID.
    swiss_cells: HashSet<u64>,

    /// How many ATP features have a Swiss IFOPT ID.
    swiss_transit_features: u64,
}

/// The country of `place`, if it matters for [AtpCoverage::add]: that
/// is, if `place` has a Wikidata ID that [AtpCoverage] collects. Else
/// `None`, without the cost of looking up its country.
pub(super) fn place_country(place: &Place) -> Option<Country> {
    if !place
        .tags
        .iter()
        .any(|(key, _)| WIKIDATA_KEYS.contains(&key.as_str()))
    {
        return None;
    }
    let ll = s2::latlng::LatLng::from(CellID(place.s2_cell_id));
    country(ll.lng.deg(), ll.lat.deg())
}

impl AtpCoverage {
    /// Adds `place`, which lies in `country` as [place_country] gives
    /// it.
    pub(super) fn add(&mut self, place: &Place, country: Option<Country>) {
        let mut swiss = false;
        for (key, value) in &place.tags {
            if WIKIDATA_KEYS.contains(&key.as_str()) {
                for id in parse_wikidata_ids(value) {
                    let mask = self.wikidata.entry((id, country)).or_default();
                    *mask = MatchMask(mask.0 | place.mask.0);
                }
            } else if IFOPT_KEYS.contains(&key.as_str()) {
                for id in swiss_ifopts(value) {
                    swiss = true;
                    self.swiss_uic.extend(swiss_uic(&id));
                    self.swiss_ifopt.insert(id);
                }
            } else if key == "uic_ref" {
                self.swiss_uic.extend(swiss_uics(value).map(str::to_string));
            }
        }
        if swiss {
            self.swiss_transit_features += 1;
            let cell = CellID(place.s2_cell_id).parent(SWISS_CELL_LEVEL);
            self.swiss_cells.insert(cell.0);
        }
    }

    /// Whether ATP has the Swiss public transport register.
    pub(super) fn has_swiss_transit(&self) -> bool {
        self.swiss_transit_features >= MIN_SWISS_TRANSIT_FEATURES
    }

    /// How many pairs of Wikidata ID and country ATP has.
    pub(super) fn num_wikidata_countries(&self) -> usize {
        self.wikidata.len()
    }

    /// Whether ATP has features of kind `mask` with Wikidata ID `id`
    /// in `country`.
    fn covers_wikidata(&self, id: u64, country: Option<Country>, mask: MatchMask) -> bool {
        self.wikidata
            .get(&(id, country))
            .is_some_and(|m| m.intersects(&mask))
    }

    /// Whether ATP has a Swiss stop within about [SWISS_CELL_LEVEL]'s
    /// cell size of (`lng`, `lat`).
    fn has_swiss_stop_near(&self, lng: f64, lat: f64) -> bool {
        let ll = s2::latlng::LatLng::from_degrees(lat, lng);
        let cell = CellID::from(ll).parent(SWISS_CELL_LEVEL);
        self.swiss_cells.contains(&cell.0)
            || cell
                .all_neighbors(SWISS_CELL_LEVEL)
                .iter()
                .any(|n| self.swiss_cells.contains(&n.0))
    }
}

/// The Swiss IFOPT IDs in a tag value, lowercased.
fn swiss_ifopts(value: &str) -> impl Iterator<Item = String> + '_ {
    split_values(value)
        .map(str::to_ascii_lowercase)
        .filter(|id| id.starts_with("ch:"))
}

/// The Swiss UIC codes in a `uic_ref` value.
fn swiss_uics(value: &str) -> impl Iterator<Item = &str> {
    split_values(value).filter(|code| code.starts_with("85"))
}

/// Why the unmatched OSM feature with `tags` is a candidate for
/// deletion, or `None` if nothing suggests so. `location` gives the
/// feature's (longitude, latitude); it is only called when needed,
/// and at most once, since it means decoding the geometry.
pub(super) fn deletion_reason<'a>(
    tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    atp: &AtpCoverage,
    location: impl FnOnce() -> Option<(f64, f64)>,
) -> Option<Reason> {
    let mut location = Some(location);
    let mut located = None;
    let mut locate = || *located.get_or_insert_with(|| location.take().and_then(|f| f()));

    if is_transit_stop(tags.clone()) && atp.has_swiss_transit() {
        let mut has_any_id = false;
        let mut has_swiss_id = false;
        let mut known = false;
        for (key, value) in tags.clone() {
            if IFOPT_KEYS.contains(&key) {
                has_any_id = true;
                for id in swiss_ifopts(value) {
                    has_swiss_id = true;
                    known |= atp.swiss_ifopt.contains(&id)
                        || swiss_uic(&id).is_some_and(|uic| atp.swiss_uic.contains(&uic));
                }
            } else if key == "uic_ref" {
                has_any_id = true;
                for code in swiss_uics(value) {
                    has_swiss_id = true;
                    known |= atp.swiss_uic.contains(code);
                }
            }
        }
        if has_swiss_id {
            return (!known).then_some(Reason::SwissTransitId);
        }
        // Most ski lift stations aren't in the register, and those
        // that are carry its IDs.
        let is_aerialway = tags.clone().any(|(key, _)| key == "aerialway");
        if !has_any_id && !is_aerialway {
            let (lng, lat) = locate()?;
            if matches!(
                country(lng, lat),
                Some(Country::SWITZERLAND | Country::LIECHTENSTEIN)
            ) {
                return (!atp.has_swiss_stop_near(lng, lat)).then_some(Reason::SwissTransitStop);
            }
        }
    }

    let ids: Vec<u64> = tags
        .clone()
        .filter(|(key, _)| WIKIDATA_KEYS.contains(key))
        .flat_map(|(_, value)| parse_wikidata_ids(value))
        .collect();
    if ids.is_empty() {
        return None;
    }
    let mut mask = MatchMask::default();
    for (key, value) in tags {
        mask.add_tag(key, value);
    }
    let country = locate().and_then(|(lng, lat)| country(lng, lat));
    ids.into_iter()
        .any(|id| atp.covers_wikidata(id, country, mask))
        .then_some(Reason::Wikidata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::UtcTimestamp;

    const ZURICH_HB: (f64, f64) = (8.5402, 47.3782);
    /// About 1 km north of Zürich HB.
    const ZURICH_NORTH: (f64, f64) = (8.5402, 47.3872);
    const SINGEN: (f64, f64) = (8.8400, 47.7590);
    const VADUZ: (f64, f64) = (9.5209, 47.1410);

    fn place(tags: &[(&str, &str)], (x, y): (f64, f64)) -> Place {
        let mut mask = MatchMask::default();
        for (k, v) in tags {
            mask.add_tag(k, v);
        }
        let coord = geo::Coord { x, y };
        Place::new(
            &coord,
            String::from("test"),
            mask,
            tags.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            &geo::Geometry::from(geo::Point::from(coord)),
            UtcTimestamp(time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap()),
        )
        .expect("valid place")
    }

    /// ATP with a supermarket chain (Q1), a fuel brand that is also an
    /// operator (Q2), and the Swiss register at Zürich HB and Zürich
    /// Stadelhofen, unless `swiss` is false.
    fn coverage(swiss: bool) -> AtpCoverage {
        let mut atp = AtpCoverage::default();
        add(
            &mut atp,
            &place(&[("shop", "supermarket"), ("brand:wikidata", "Q1")], SINGEN),
        );
        add(
            &mut atp,
            &place(
                &[("amenity", "fuel"), ("operator:wikidata", "Q7;Q2")],
                SINGEN,
            ),
        );
        if swiss {
            add(
                &mut atp,
                &place(
                    &[
                        ("public_transport", "station"),
                        ("ref:IFOPT", "ch:1:sloid:3000"),
                        ("uic_ref", "8503000"),
                        ("network:wikidata", "Q3"),
                    ],
                    ZURICH_HB,
                ),
            );
            add(
                &mut atp,
                &place(
                    &[
                        ("public_transport", "platform"),
                        ("ref:IFOPT", "ch:1:sloid:3003:1:1"),
                        ("network:wikidata", "Q3"),
                    ],
                    (8.5487, 47.3667),
                ),
            );
            atp.swiss_transit_features = MIN_SWISS_TRANSIT_FEATURES;
        }
        atp
    }

    fn add(atp: &mut AtpCoverage, place: &Place) {
        atp.add(place, place_country(place));
    }

    fn reason(atp: &AtpCoverage, tags: &[(&str, &str)], at: (f64, f64)) -> Option<Reason> {
        deletion_reason(tags.iter().copied(), atp, || Some(at))
    }

    #[test]
    fn test_coverage() {
        let atp = coverage(true);
        assert!(atp.swiss_ifopt.contains("ch:1:sloid:3000"));
        assert!(atp.swiss_ifopt.contains("ch:1:sloid:3003:1:1"));
        assert!(atp.swiss_uic.contains("8503000"));
        assert!(atp.swiss_uic.contains("8503003"));
        assert_eq!(atp.swiss_uic.len(), 2);
        assert!(atp.has_swiss_stop_near(ZURICH_HB.0, ZURICH_HB.1));
        assert!(!atp.has_swiss_stop_near(ZURICH_NORTH.0, ZURICH_NORTH.1));
        assert_eq!(atp.num_wikidata_countries(), 4);
    }

    #[test]
    fn test_coverage_counts_swiss_transit() {
        let mut atp = AtpCoverage::default();
        let stop = place(
            &[
                ("public_transport", "platform"),
                ("ref:IFOPT", "de:08336:3674;CH:1:sloid:1100020"),
            ],
            SINGEN,
        );
        let german = place(
            &[
                ("public_transport", "platform"),
                ("ref:IFOPT", "de:08336:3674"),
            ],
            SINGEN,
        );
        for _ in 0..MIN_SWISS_TRANSIT_FEATURES - 1 {
            add(&mut atp, &stop);
        }
        add(&mut atp, &german);
        assert!(!atp.has_swiss_transit());
        add(&mut atp, &stop);
        assert!(atp.has_swiss_transit());
        assert!(atp.swiss_ifopt.contains("ch:1:sloid:1100020"));
        assert!(!atp.swiss_ifopt.contains("de:08336:3674"));
        // A foreign stop in the Swiss register has no UIC code.
        assert!(atp.swiss_uic.is_empty());
    }

    #[test]
    fn test_wikidata() {
        let atp = coverage(true);
        let wikidata = Some(Reason::Wikidata);
        let shop = |key, value| reason(&atp, &[("shop", "supermarket"), (key, value)], SINGEN);
        assert_eq!(shop("brand:wikidata", "Q1"), wikidata);
        assert_eq!(shop("brand:wikidata", "Q9;Q1"), wikidata);
        assert_eq!(shop("operator:wikidata", "Q1"), wikidata);
        assert_eq!(shop("network:wikidata", " Q1 "), wikidata);
        assert_eq!(shop("brand:wikidata", "Q9"), None);
        assert_eq!(shop("wikidata", "Q1"), None);

        // ATP has Q1's supermarkets, but not its restaurants.
        let restaurant = [("amenity", "restaurant"), ("brand:wikidata", "Q1")];
        assert_eq!(reason(&atp, &restaurant, SINGEN), None);

        // ATP has Q1's supermarkets in Germany, but not in Switzerland.
        let swiss = [("shop", "supermarket"), ("brand:wikidata", "Q1")];
        assert_eq!(reason(&atp, &swiss, ZURICH_NORTH), None);

        // Q2 is in the second of two values on the ATP side.
        let fuel = [("amenity", "fuel"), ("brand:wikidata", "Q2")];
        assert_eq!(reason(&atp, &fuel, SINGEN), wikidata);

        // A stop of a network whose stops ATP only has in Switzerland.
        let stop = [("highway", "bus_stop"), ("network:wikidata", "Q3")];
        assert_eq!(reason(&atp, &stop, SINGEN), None);
    }

    #[test]
    fn test_swiss_transit_id() {
        let atp = coverage(true);
        let id = Some(Reason::SwissTransitId);
        let platform = |key, value| {
            let tags = [
                ("public_transport", "platform"),
                ("network:wikidata", "Q3"),
                (key, value),
            ];
            reason(&atp, &tags, ZURICH_HB)
        };

        // IDs that ATP doesn't know.
        assert_eq!(platform("ref:IFOPT", "ch:1:sloid:9999:3:4"), id);
        assert_eq!(platform("ref:sloid", "ch:1:sloid:9999"), id);
        assert_eq!(platform("railway:ref:sloid", "CH:1:sloid:9999"), id);
        assert_eq!(
            platform("ref:IFOPT", "de:08336:3674;ch:1:sloid:1100020"),
            id
        );
        assert_eq!(platform("uic_ref", "8509999"), id);
        assert_eq!(platform("uic_ref", "8000105;8509999"), id);

        // IDs that ATP knows, also within a stop it knows: the stop
        // still exists, even though it wasn't matched. Nor does the
        // network's Wikidata ID count for it.
        assert_eq!(platform("ref:IFOPT", "ch:1:sloid:3000:3:4"), None);
        assert_eq!(platform("ref:sloid", "CH:1:SLOID:3003:1:1"), None);
        assert_eq!(platform("uic_ref", "8503000"), None);
        assert_eq!(platform("uic_ref", "8509999;8503003"), None);
        let tags = [
            ("public_transport", "stop_position"),
            ("bus", "yes"),
            ("uic_ref", "8503000"),
        ];
        assert_eq!(reason(&atp, &tags, ZURICH_HB), None);

        // Foreign IDs: not a Swiss signal. The Wikidata signal
        // applies again, since this isn't a stop the Swiss signals
        // are about.
        let wikidata = Some(Reason::Wikidata);
        assert_eq!(platform("ref:IFOPT", "de:08336:3674"), wikidata);
        assert_eq!(platform("uic_ref", "8000105"), wikidata);
        let tags = [("public_transport", "platform"), ("uic_ref", "8000105")];
        assert_eq!(reason(&atp, &tags, ZURICH_HB), None);

        // Not a stop, so the transit matcher could never match it.
        let building = [("building", "train_station"), ("uic_ref", "8509999")];
        assert_eq!(reason(&atp, &building, ZURICH_HB), None);

        // ATP lacks the Swiss register this week.
        let atp = coverage(false);
        let tags = [("public_transport", "platform"), ("uic_ref", "8509999")];
        assert_eq!(reason(&atp, &tags, ZURICH_HB), None);
    }

    #[test]
    fn test_swiss_transit_stop() {
        let atp = coverage(true);
        let stop = Some(Reason::SwissTransitStop);
        for tags in [
            &[("highway", "bus_stop"), ("name", "Central")][..],
            &[("railway", "platform")][..],
            &[("railway", "tram_stop")][..],
            &[("public_transport", "stop_position"), ("bus", "yes")][..],
            &[("public_transport", "station"), ("railway", "station")][..],
            &[("public_transport", "platform"), ("network:wikidata", "Q3")][..],
        ] {
            // Far from any stop that ATP has.
            assert_eq!(reason(&atp, tags, ZURICH_NORTH), stop, "{tags:?}");
            assert_eq!(reason(&atp, tags, VADUZ), stop, "{tags:?}");
            // Near Zürich HB, which ATP has.
            assert_eq!(reason(&atp, tags, ZURICH_HB), None, "{tags:?}");
            // Outside Switzerland and Liechtenstein.
            assert_ne!(reason(&atp, tags, SINGEN), stop, "{tags:?}");
        }

        // A stop with a foreign ID is not Swiss.
        let german = [("highway", "bus_stop"), ("ref:IFOPT", "de:08336:3674")];
        assert_eq!(reason(&atp, &german, ZURICH_NORTH), None);
        let german = [("highway", "bus_stop"), ("uic_ref", "8000105")];
        assert_eq!(reason(&atp, &german, ZURICH_NORTH), None);

        // Not a stop.
        assert_eq!(reason(&atp, &[("shop", "bakery")], ZURICH_NORTH), None);

        // A ski lift station.
        let lift = [("public_transport", "station"), ("aerialway", "station")];
        assert_eq!(reason(&atp, &lift, ZURICH_NORTH), None);

        // Without a location.
        let tags = [("highway", "bus_stop")];
        assert_eq!(deletion_reason(tags.iter().copied(), &atp, || None), None);

        // ATP lacks the Swiss register this week.
        assert_eq!(reason(&coverage(false), &tags, ZURICH_NORTH), None);
    }
}
