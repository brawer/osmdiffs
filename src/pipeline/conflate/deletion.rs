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
//!   one country but not in another. For street infrastructure, the
//!   kind is finer than the match mask: ATP's post boxes of Swiss Post
//!   say nothing about its cameras.
//! - [Reason::Name]: it has no such Wikidata IDs, but its `brand`,
//!   `network` or `operator` names one for which ATP has features of
//!   the same kind in the same country. Names are compared without
//!   regard to case and spacing. Since names are weaker evidence than
//!   Wikidata IDs, ATP must have at least [MIN_NAME_FEATURES] features
//!   with the name in the country, and tag values listing more than
//!   [MAX_NAME_VALUES] names are ignored: a bicycle shop tagged
//!   `brand=Trek;Cube;Scott;Specialized` lists what it sells, and is
//!   no Trek store.
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
//!   covers Liechtenstein too, with the same kinds of IDs. Aerialway
//!   stations are left out, since the register lacks most ski lifts.
//!
//! For stops that one of the Swiss signals is about, including ski
//! lift stations in Switzerland and Liechtenstein, the Wikidata and
//! name signals don't apply: the Swiss register lists
//! `network:wikidata` and `operator` for its stops, and that would
//! flag every unmatched stop of a network or operator. The Swiss signals only apply when ATP actually has the
//! Swiss register, so that a week in which its spider failed doesn't
//! turn every Swiss stop into a deletion candidate.
//!
//! Tag values can hold several `;`-separated values; any one of them
//! counts.

use super::countries::{Country, country};
use crate::matchers::{
    IFOPT_KEYS, MatchMask, infrastructure_kind, is_transit_stop, normalize_name,
    parse_wikidata_ids, split_values, swiss_uic,
};
use crate::pipeline::wikidata_ids::WIKIDATA_KEYS;
use crate::places::Place;
use s2::cellid::CellID;
use std::collections::{HashMap, HashSet};

/// Tag keys whose names we look up in [AtpCoverage].
const NAME_KEYS: [&str; 3] = ["brand", "network", "operator"];

/// For [Reason::Name], ATP must have at least this many features
/// with the name in the country. In Switzerland, ATP has a single
/// Trek store, while hundreds of OSM bicycle shops list Trek among
/// the brands they sell.
const MIN_NAME_FEATURES: u32 = 3;

/// For [Reason::Name], OSM tag values with more than this many names
/// are ignored, such as `brand=Trek;Cube;Scott;Specialized`. Car
/// dealers are often tagged with two or three brands, such as
/// `brand=Renault;Dacia;Alpine`.
const MAX_NAME_VALUES: usize = 3;

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
    Name,
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

    /// Like `wikidata`, but for the [normalize_name]d values of ATP's
    /// `brand`, `network` and `operator` tags, with the number of ATP
    /// features that have them.
    names: HashMap<(String, Option<Country>), (MatchMask, u32)>,

    /// Like `wikidata`, but for street infrastructure such as post
    /// boxes, by its kind ([infrastructure_kind]) instead of its match
    /// mask. The mask lumps all street furniture together, so that
    /// ATP's Swiss Post boxes would make every unmatched OSM camera
    /// or vending machine of Swiss Post a candidate.
    infrastructure_wikidata: HashSet<(u64, Option<Country>, &'static str)>,

    /// Like `names`, but for street infrastructure, by its kind.
    infrastructure_names: HashMap<(String, Option<Country>, &'static str), u32>,

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
        .any(|(key, _)| WIKIDATA_KEYS.contains(&key.as_str()) || NAME_KEYS.contains(&key.as_str()))
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
        let infrastructure = infrastructure_of(
            place.mask,
            place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        );
        for (key, value) in &place.tags {
            if WIKIDATA_KEYS.contains(&key.as_str()) {
                for id in parse_wikidata_ids(value) {
                    if let Some(kind) = infrastructure {
                        self.infrastructure_wikidata.insert((id, country, kind));
                    } else {
                        let mask = self.wikidata.entry((id, country)).or_default();
                        *mask = MatchMask(mask.0 | place.mask.0);
                    }
                }
            } else if NAME_KEYS.contains(&key.as_str()) {
                for name in split_values(value).map(normalize_name) {
                    if let Some(kind) = infrastructure {
                        *self
                            .infrastructure_names
                            .entry((name, country, kind))
                            .or_default() += 1;
                    } else {
                        let (mask, count) = self.names.entry((name, country)).or_default();
                        *mask = MatchMask(mask.0 | place.mask.0);
                        *count += 1;
                    }
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

    /// Whether ATP has features of kind `kind` with Wikidata ID `id`
    /// in `country`.
    fn covers_wikidata(&self, id: u64, country: Option<Country>, kind: Kind) -> bool {
        match kind {
            Kind::Mask(mask) => self
                .wikidata
                .get(&(id, country))
                .is_some_and(|m| m.intersects(&mask)),
            Kind::Infrastructure(kind) => {
                self.infrastructure_wikidata.contains(&(id, country, kind))
            }
        }
    }

    /// How many pairs of name and country ATP has.
    pub(super) fn num_name_countries(&self) -> usize {
        self.names.len()
    }

    /// Whether ATP has features of kind `mask` with the
    /// [normalize_name]d brand, network or operator `name` in
    /// `country`, and at least [MIN_NAME_FEATURES] features with that
    /// name in `country`.
    fn covers_name(&self, name: String, country: Option<Country>, kind: Kind) -> bool {
        match kind {
            Kind::Mask(mask) => self
                .names
                .get(&(name, country))
                .is_some_and(|(m, count)| m.intersects(&mask) && *count >= MIN_NAME_FEATURES),
            Kind::Infrastructure(kind) => self
                .infrastructure_names
                .get(&(name, country, kind))
                .is_some_and(|count| *count >= MIN_NAME_FEATURES),
        }
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

/// What kind of thing a feature is, for [AtpCoverage]: its match
/// mask, or for street infrastructure, its finer kind.
#[derive(Clone, Copy)]
enum Kind {
    Mask(MatchMask),
    Infrastructure(&'static str),
}

/// The [infrastructure_kind] of a feature with `mask` and `tags`, if
/// it is street furniture and nothing else. For a shop that also has a
/// defibrillator, `None`: it counts as a shop.
fn infrastructure_of<'a>(
    mask: MatchMask,
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> Option<&'static str> {
    mask.has_only(MatchMask::STREET_FURNITURE)
        .then(|| infrastructure_kind(tags))
        .flatten()
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
        if !has_any_id {
            let (lng, lat) = locate()?;
            if matches!(
                country(lng, lat),
                Some(Country::SWITZERLAND | Country::LIECHTENSTEIN)
            ) {
                // Most ski lift stations aren't in the register, and
                // those that are carry its IDs. Their operators often
                // are, though, so the name signal doesn't apply either.
                if tags.clone().any(|(key, _)| key == "aerialway") {
                    return None;
                }
                return (!atp.has_swiss_stop_near(lng, lat)).then_some(Reason::SwissTransitStop);
            }
        }
    }

    let ids: Vec<u64> = tags
        .clone()
        .filter(|(key, _)| WIKIDATA_KEYS.contains(key))
        .flat_map(|(_, value)| parse_wikidata_ids(value))
        .collect();
    // Names only count for features without Wikidata IDs: a Wikidata
    // ID says better which brand is meant than its name.
    let names: Vec<String> = if ids.is_empty() {
        tags.clone()
            .filter(|(key, value)| {
                NAME_KEYS.contains(key) && split_values(value).count() <= MAX_NAME_VALUES
            })
            .flat_map(|(_, value)| split_values(value).map(normalize_name))
            .collect()
    } else {
        Vec::new()
    };
    if ids.is_empty() && names.is_empty() {
        return None;
    }
    let mut mask = MatchMask::default();
    for (key, value) in tags.clone() {
        mask.add_tag(key, value);
    }
    let kind = if mask.has_only(MatchMask::STREET_FURNITURE) {
        // Street furniture we can't match, such as a camera, says
        // nothing: ATP has none of its kind.
        let kind = infrastructure_kind(tags)?;
        // Nor does a public bookcase. OSM tags many as Little Free
        // Libraries that aren't in its registry, and the registry's
        // positions are often more than 50 m off.
        if kind == "public_bookcase" {
            return None;
        }
        Kind::Infrastructure(kind)
    } else {
        Kind::Mask(mask)
    };
    let country = locate().and_then(|(lng, lat)| country(lng, lat));
    if ids
        .into_iter()
        .any(|id| atp.covers_wikidata(id, country, kind))
    {
        return Some(Reason::Wikidata);
    }
    names
        .into_iter()
        .any(|name| atp.covers_name(name, country, kind))
        .then_some(Reason::Name)
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
    fn test_name() {
        let mut atp = AtpCoverage::default();
        let station = place(&[("amenity", "fuel"), ("brand", "Avia")], SINGEN);
        for _ in 0..MIN_NAME_FEATURES - 1 {
            add(&mut atp, &station);
        }
        // Too few ATP features with the name.
        let avia = [("amenity", "fuel"), ("brand", "Avia")];
        assert_eq!(reason(&atp, &avia, SINGEN), None);

        add(&mut atp, &station);
        let fuel = |key, value| reason(&atp, &[("amenity", "fuel"), (key, value)], SINGEN);
        let name = Some(Reason::Name);
        assert_eq!(fuel("brand", "Avia"), name);
        assert_eq!(fuel("brand", "  AVIA "), name);
        assert_eq!(fuel("operator", "avia"), name);
        assert_eq!(fuel("brand", "Shell;Avia"), name);
        assert_eq!(fuel("brand", "Shell;Avia;BP"), name);
        assert_eq!(fuel("brand", "Shell"), None);
        assert_eq!(fuel("name", "Avia"), None);
        // A list of what the shop sells, not its brand.
        assert_eq!(fuel("brand", "Shell;Avia;BP;Eni"), None);

        // Not of the same kind, or not in the same country.
        let shop = [("shop", "supermarket"), ("brand", "Avia")];
        assert_eq!(reason(&atp, &shop, SINGEN), None);
        let swiss = [("amenity", "fuel"), ("brand", "Avia")];
        assert_eq!(reason(&atp, &swiss, ZURICH_NORTH), None);

        // A Wikidata ID says which brand is meant, so the name does
        // not count.
        let other = [
            ("amenity", "fuel"),
            ("brand", "Avia"),
            ("brand:wikidata", "Q9"),
        ];
        assert_eq!(reason(&atp, &other, SINGEN), None);
    }

    /// The register lists some cable cars, with the names of their
    /// operators, but few ski lifts.
    #[test]
    fn test_name_aerialway() {
        let mut atp = coverage(true);
        let cable_car = place(
            &[
                ("public_transport", "station"),
                ("aerialway", "station"),
                ("operator", "Téléverbier"),
                ("ref:IFOPT", "ch:1:sloid:9000"),
            ],
            ZURICH_HB,
        );
        for _ in 0..MIN_NAME_FEATURES {
            add(&mut atp, &cable_car);
        }
        let lift = [
            ("public_transport", "station"),
            ("aerialway", "station"),
            ("operator", "Téléverbier"),
        ];
        assert_eq!(reason(&atp, &lift, ZURICH_NORTH), None);
        let stop = [("highway", "bus_stop"), ("operator", "Téléverbier")];
        assert_eq!(
            reason(&atp, &stop, ZURICH_NORTH),
            Some(Reason::SwissTransitStop)
        );
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
    fn test_wikidata_infrastructure() {
        let mut atp = AtpCoverage::default();
        let post_box = [
            ("amenity", "post_box"),
            ("operator", "Die Post"),
            ("operator:wikidata", "Q614803"),
        ];
        for _ in 0..MIN_NAME_FEATURES {
            add(&mut atp, &place(&post_box, ZURICH_HB));
        }
        let wikidata = Some(Reason::Wikidata);
        assert_eq!(reason(&atp, &post_box, ZURICH_NORTH), wikidata);

        // Other street furniture of the same operator, which ATP
        // doesn't list and we don't match.
        let camera = [
            ("man_made", "surveillance"),
            ("operator", "Die Post"),
            ("operator:wikidata", "Q614803"),
        ];
        assert_eq!(reason(&atp, &camera, ZURICH_NORTH), None);
        let camera_by_name = [("man_made", "surveillance"), ("operator", "Die Post")];
        assert_eq!(reason(&atp, &camera_by_name, ZURICH_NORTH), None);

        // By name, a post box without `operator:wikidata`.
        let post_box_by_name = [("amenity", "post_box"), ("operator", "Die Post")];
        assert_eq!(
            reason(&atp, &post_box_by_name, ZURICH_NORTH),
            Some(Reason::Name)
        );

        // A post office of the same operator is a shop, not a post box.
        let post_office = [("amenity", "post_office"), ("operator:wikidata", "Q614803")];
        assert_eq!(reason(&atp, &post_office, ZURICH_NORTH), None);
    }

    #[test]
    fn test_public_bookcase_never_flagged() {
        let mut atp = AtpCoverage::default();
        let bookcase = [
            ("amenity", "public_bookcase"),
            ("brand", "Little Free Library"),
            ("brand:wikidata", "Q6650101"),
        ];
        for _ in 0..MIN_NAME_FEATURES {
            add(&mut atp, &place(&bookcase, ZURICH_HB));
        }
        assert_eq!(reason(&atp, &bookcase, ZURICH_NORTH), None);
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
