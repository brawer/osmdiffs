//! Matches public transport stops, stations, platforms and stop positions.
//!
//! Public transport data comes from authorities, not from brands, so
//! there is no `brand:wikidata` to go by. Instead, stops carry
//! identifiers from national registries:
//!
//! - `ref:IFOPT`, the European stop ID of
//!   [EN 28701](https://en.wikipedia.org/wiki/Identification_of_Fixed_Objects_in_Public_Transport),
//!   such as `de:08336:3674:1:2` in Germany. Switzerland uses the
//!   same scheme under the name SLOID, such as `ch:1:sloid:3000:3:4`,
//!   and Swiss OpenStreetMap often tags it as `ref:sloid` or
//!   `railway:ref:sloid`. An ID names a stop, and optionally an area
//!   within it and a quay (platform) within that area.
//! - `uic_ref`, the railway station code of the UIC, such as `8503000`
//!   for Zürich HB. Swiss OpenStreetMap puts it on every part of a
//!   stop: the station, its platforms and its stop positions. So it
//!   tells which stop a feature belongs to, but not which platform.
//!
//! Within a stop, `local_ref` tells platforms and tracks apart, such
//! as `3;4` for the platform between tracks 3 and 4.
//!
//! See [TransitMatcher] for how these are combined.

use super::{MatchMask, Matcher, OsmCandidate, geo_point_distance, geo_point_distance_score};
use crate::places::Place;
use geo::Centroid;
use s2::{cell::Cell, cellid::CellID, point::Point};

/// Keys that hold IFOPT IDs, on either side.
const IFOPT_KEYS: [&str; 3] = ["ref:IFOPT", "ref:sloid", "railway:ref:sloid"];

/// A candidate with the same IFOPT ID scores at least this much, so
/// that it wins over any candidate that merely belongs to the same
/// stop, however close that one is.
const SAME_ID_MIN_SCORE: f64 = 0.9;

/// Evidence factor when both sides name the same stop by ID (shared
/// `uic_ref`, or one IFOPT ID within the other), before `local_ref`.
/// Below [SAME_ID_MIN_SCORE].
const SAME_STOP_FACTOR: f64 = 0.8;

/// Evidence factor when neither side has an ID the other can check,
/// but the names are equal. Weaker than IDs, because names repeat:
/// many towns have a stop called "Bahnhof".
const NAME_FACTOR: f64 = 0.4;

/// Name-only matches must be closer than this.
const NAME_MAX_METERS: f64 = 100.0;

/// What part of a stop a feature is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `public_transport=station`, such as a railway station.
    Station,
    /// `public_transport=stop_area`, the whole of a stop. In OSM
    /// usually a relation.
    StopArea,
    /// Where passengers wait: `public_transport=platform`,
    /// `highway=bus_stop` or `railway=platform`.
    Platform,
    /// Where vehicles halt: `public_transport=stop_position`, such as
    /// one railway track.
    StopPosition,
}

impl Kind {
    fn from_tags<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Option<Kind> {
        let mut fallback = None;
        for (key, value) in tags {
            match (key, value) {
                ("public_transport", "station") => return Some(Kind::Station),
                ("public_transport", "stop_area") => return Some(Kind::StopArea),
                ("public_transport", "platform") => return Some(Kind::Platform),
                ("public_transport", "stop_position") => return Some(Kind::StopPosition),
                ("railway", "station" | "halt") => fallback = fallback.or(Some(Kind::Station)),
                ("railway", "platform") | ("highway", "bus_stop") => {
                    fallback = fallback.or(Some(Kind::Platform))
                }
                ("railway", "stop" | "tram_stop") => {
                    fallback = fallback.or(Some(Kind::StopPosition))
                }
                _ => {}
            }
        }
        fallback
    }

    /// How well an ATP feature of kind `self` fits an OSM feature of
    /// kind `osm`: 1.0 for the same kind, 0.9 for a station against a
    /// stop area (OSM often has both for one railway station), 0.0
    /// otherwise. In particular, a stop never matches one of its own
    /// platforms: for bus stops, OSM rarely has anything but platforms,
    /// and matching the stop to one of them would take it from the
    /// ATP platform it belongs to.
    fn fit(self, osm: Kind) -> f64 {
        use Kind::*;
        match (self, osm) {
            (a, b) if a == b => 1.0,
            (Station, StopArea) | (StopArea, Station) => 0.9,
            _ => 0.0,
        }
    }
}

/// Means of transport, as bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Modes(u16);

impl Modes {
    const TRAIN: u16 = 1 << 0;
    const TRAM: u16 = 1 << 1;
    const SUBWAY: u16 = 1 << 2;
    const LIGHT_RAIL: u16 = 1 << 3;
    const MONORAIL: u16 = 1 << 4;
    const FUNICULAR: u16 = 1 << 5;
    const BUS: u16 = 1 << 6;
    const TROLLEYBUS: u16 = 1 << 7;
    const FERRY: u16 = 1 << 8;
    const AERIALWAY: u16 = 1 << 9;
    const RAIL: u16 = Self::TRAIN
        | Self::TRAM
        | Self::SUBWAY
        | Self::LIGHT_RAIL
        | Self::MONORAIL
        | Self::FUNICULAR;

    /// Explicit `train=yes`, `bus=yes` etc. If there are none, the
    /// modes implied by the main tag: `highway=bus_stop` means bus,
    /// `railway=tram_stop` tram, and other `railway=*` any rail mode.
    fn from_tags<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Modes {
        let (mut explicit, mut implied) = (0, 0);
        for (key, value) in tags {
            let bit = match key {
                "train" => Self::TRAIN,
                "tram" => Self::TRAM,
                "subway" => Self::SUBWAY,
                // Light rail is between trains and trams, so it fits
                // both, and a trolleybus is a bus.
                "light_rail" => Self::LIGHT_RAIL | Self::TRAIN | Self::TRAM,
                "monorail" => Self::MONORAIL,
                "funicular" => Self::FUNICULAR,
                "bus" => Self::BUS,
                "trolleybus" => Self::TROLLEYBUS | Self::BUS,
                "ferry" => Self::FERRY,
                "aerialway" => Self::AERIALWAY,
                "highway" if value == "bus_stop" => {
                    implied |= Self::BUS;
                    0
                }
                "railway" if value == "tram_stop" => {
                    implied |= Self::TRAM;
                    0
                }
                "railway" if matches!(value, "station" | "halt" | "platform" | "stop") => {
                    implied |= Self::RAIL;
                    0
                }
                _ => 0,
            };
            if bit != 0 && value != "no" {
                explicit |= bit;
            }
        }
        Modes(if explicit != 0 { explicit } else { implied })
    }

    /// False only if both sides know their modes and share none, such
    /// as a train platform against a bus stop.
    fn compatible(self, other: Modes) -> bool {
        self.0 == 0 || other.0 == 0 || self.0 & other.0 != 0
    }
}

/// Splits a `;`-separated tag value into trimmed, non-empty parts.
fn split_values(value: &str) -> impl Iterator<Item = &str> {
    value.split(';').map(str::trim).filter(|s| !s.is_empty())
}

/// The IDs of a feature: its IFOPT IDs, lowercased, and its UIC codes,
/// both stated (`uic_ref`) and derived from Swiss IFOPT IDs.
#[derive(Debug, Default)]
struct Ids {
    ifopt: Vec<String>,
    uic: Vec<String>,
}

impl Ids {
    fn from_tags<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Ids {
        let mut ids = Ids::default();
        for (key, value) in tags {
            if IFOPT_KEYS.contains(&key) {
                for id in split_values(value) {
                    let id = id.to_ascii_lowercase();
                    if let Some(uic) = swiss_uic(&id) {
                        ids.uic.push(uic);
                    }
                    ids.ifopt.push(id);
                }
            } else if key == "uic_ref" {
                ids.uic.extend(split_values(value).map(str::to_string));
            }
        }
        ids.uic.sort();
        ids.uic.dedup();
        ids
    }

    fn is_empty(&self) -> bool {
        self.ifopt.is_empty() && self.uic.is_empty()
    }
}

/// The UIC code of a Swiss stop, from its SLOID: `ch:1:sloid:3000:3:4`
/// belongs to UIC `8503000`. Stops abroad have SLOIDs with longer
/// numbers, such as `ch:1:sloid:1100020` for a bus stop in Germany,
/// which are not UIC codes.
fn swiss_uic(ifopt: &str) -> Option<String> {
    let number = ifopt.strip_prefix("ch:1:sloid:")?.split(':').next()?;
    if number.is_empty() || number.len() > 5 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("85{number:0>5}"))
}

/// The stop an IFOPT ID belongs to, and how many levels follow it
/// (0 for the stop itself, 1 for an area, 2 for a quay). Swiss IDs
/// have one field more than the standard (`ch:1:sloid:3000`), German
/// ones don't (`de:08336:3674`).
fn ifopt_stop(id: &str) -> (&str, usize) {
    let stop_fields = if id.starts_with("ch:1:sloid:") { 4 } else { 3 };
    let mut end = id.len();
    let mut fields = 0;
    for (i, c) in id.char_indices() {
        if c == ':' {
            fields += 1;
            if fields == stop_fields {
                end = i;
                break;
            }
        }
    }
    let rest = &id[end..];
    (&id[..end], rest.matches(':').count())
}

/// True if IFOPT ID `part` names a part of `whole`, such as quay
/// `ch:1:sloid:3000:3:4` of stop `ch:1:sloid:3000`.
fn is_within(part: &str, whole: &str) -> bool {
    part.strip_prefix(whole)
        .is_some_and(|rest| rest.starts_with(':'))
}

/// How two sets of IDs relate.
#[derive(Debug, PartialEq)]
enum IdEvidence {
    /// The same IFOPT ID on both sides.
    Same,
    /// The same stop, but not necessarily the same part of it: a
    /// shared `uic_ref`, or an IFOPT ID of the stop on one side and of
    /// one of its quays on the other.
    SameStop,
    /// The IDs say these are different things.
    Different,
    /// Nothing to compare.
    Unknown,
}

fn compare_ids(atp: &Ids, osm: &Ids) -> IdEvidence {
    if atp.ifopt.iter().any(|a| osm.ifopt.contains(a)) {
        return IdEvidence::Same;
    }
    let mut same_stop = false;
    let mut different = false;
    for a in &atp.ifopt {
        let (a_stop, a_depth) = ifopt_stop(a);
        for o in &osm.ifopt {
            if a.split(':').next() != o.split(':').next() {
                // IDs from different countries can't be compared.
                continue;
            }
            let (o_stop, o_depth) = ifopt_stop(o);
            if a_stop != o_stop {
                different = true;
            } else if a_depth != o_depth && (is_within(a, o) || is_within(o, a)) {
                // One names the stop or area, the other a part of it.
                same_stop = true;
            } else {
                // Two different areas or quays of the same stop.
                different = true;
            }
        }
    }
    if !atp.uic.is_empty() && !osm.uic.is_empty() {
        if atp.uic.iter().any(|u| osm.uic.contains(u)) {
            same_stop = true;
        } else {
            different = true;
        }
    }
    // Any disagreement wins, so that a platform tagged with the ID of
    // quay 2 doesn't take the ATP feature for quay 3, although both
    // share the UIC code of their stop.
    if different {
        IdEvidence::Different
    } else if same_stop {
        IdEvidence::SameStop
    } else {
        IdEvidence::Unknown
    }
}

/// Lowercased `local_ref` values, sorted, such as `["3", "4"]`.
fn local_refs<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<String> {
    let mut refs: Vec<String> = tags
        .filter(|&(k, _)| k == "local_ref")
        .flat_map(|(_, v)| split_values(v).map(str::to_lowercase))
        .collect();
    refs.sort();
    refs.dedup();
    refs
}

/// How well two `local_ref` lists agree: 1.0 if equal, 0.6 if they
/// overlap (such as a platform `3;4` in ATP, mapped as `3` in OSM),
/// 0.0 if disjoint, and 0.8 if one side has none.
fn local_ref_factor(atp: &[String], osm: &[String]) -> f64 {
    if atp.is_empty() || osm.is_empty() {
        0.8
    } else if atp == osm {
        1.0
    } else if atp.iter().any(|r| osm.contains(r)) {
        0.6
    } else {
        0.0
    }
}

/// Lowercased name with whitespace collapsed, so that
/// `Zürich,  Bahnhofplatz/HB` equals `zürich, bahnhofplatz/hb`.
fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Matches public transport features. A candidate must be the same
/// kind of thing ([Kind::fit]) with compatible means of transport
/// ([Modes::compatible]). Then, by evidence:
///
/// - The same IFOPT ID on both sides: [SAME_ID_MIN_SCORE] plus the
///   rest times the distance score.
/// - The same stop by ID (shared `uic_ref`, or an IFOPT ID within the
///   other): [SAME_STOP_FACTOR] times the `local_ref` factor
///   ([local_ref_factor]) times the distance score. That picks the
///   right platform within a stop when `local_ref` is known on both
///   sides, and the nearest one otherwise.
/// - IDs that say these are different stops or quays: no match,
///   whatever else agrees.
/// - No IDs to compare, but equal names within [NAME_MAX_METERS]:
///   [NAME_FACTOR] times the `local_ref` factor times the distance
///   score.
///
/// Scores are then multiplied by the kind fit.
pub struct TransitMatcher {
    center: Point,
    kind: Kind,
    modes: Modes,
    ids: Ids,
    local_refs: Vec<String>,
    name: Option<String>,
    max_meters: f64,
}

impl TransitMatcher {
    pub fn for_place(place: &Place) -> Option<TransitMatcher> {
        if !place.mask.intersects(&MatchMask::TRANSIT) {
            return None;
        }
        let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        let kind = Kind::from_tags(tags())?;
        let ids = Ids::from_tags(tags());
        let name = tags()
            .find(|&(k, _)| k == "name")
            .map(|(_, v)| normalize_name(v));
        if ids.is_empty() && name.is_none() {
            return None;
        }
        // Platforms can be long, such as 400 m at Zürich HB; same as
        // the search radius in `match_distance`.
        let max_meters = if kind == Kind::Platform {
            1000.0
        } else {
            400.0
        };
        Some(TransitMatcher {
            center: Cell::from(CellID(place.s2_cell_id)).center(),
            kind,
            modes: Modes::from_tags(tags()),
            ids,
            local_refs: local_refs(tags()),
            name,
            max_meters,
        })
    }

    /// For `candidate`, the score at zero distance; the minimum score
    /// at `max_meters`; and `max_meters`, beyond which it gets none.
    fn factor(&self, candidate: &OsmCandidate) -> (f64, f64, f64) {
        let Some(kind) = Kind::from_tags(candidate.tags()) else {
            return (0.0, 0.0, 0.0);
        };
        let mut fit = self.kind.fit(kind);
        // Many bus stops are mapped as a single node on the road,
        // tagged both `public_transport=stop_position` and
        // `highway=bus_stop`. Such a node stands for the platform too,
        // but where OSM has a separate platform, that one is better.
        if self.kind == Kind::Platform
            && kind == Kind::StopPosition
            && candidate.tags().any(|kv| kv == ("highway", "bus_stop"))
        {
            fit = 0.9;
        }
        if fit == 0.0 || !self.modes.compatible(Modes::from_tags(candidate.tags())) {
            return (0.0, 0.0, 0.0);
        }
        let local_refs = || {
            if self.kind == Kind::Platform || self.kind == Kind::StopPosition {
                local_ref_factor(&self.local_refs, &local_refs(candidate.tags()))
            } else {
                1.0
            }
        };
        match compare_ids(&self.ids, &Ids::from_tags(candidate.tags())) {
            IdEvidence::Same => (fit, fit * SAME_ID_MIN_SCORE, self.max_meters),
            IdEvidence::SameStop => (fit * SAME_STOP_FACTOR * local_refs(), 0.0, self.max_meters),
            IdEvidence::Different => (0.0, 0.0, 0.0),
            IdEvidence::Unknown => {
                let Some(name) = &self.name else {
                    return (0.0, 0.0, 0.0);
                };
                let same_name = candidate
                    .tags()
                    .any(|(k, v)| k == "name" && normalize_name(v) == *name);
                if same_name {
                    (fit * NAME_FACTOR * local_refs(), 0.0, NAME_MAX_METERS)
                } else {
                    (0.0, 0.0, 0.0)
                }
            }
        }
    }
}

impl Matcher for TransitMatcher {
    fn score_osm_candidate(&self, candidate: &OsmCandidate) -> f64 {
        let (best, worst, max_meters) = self.factor(candidate);
        if best == 0.0 {
            return 0.0;
        }
        let Result::Ok(geometry) = candidate.geometry() else {
            return 0.0;
        };
        let Some(centroid) = geometry.centroid() else {
            return 0.0;
        };
        if geo_point_distance(&self.center, &centroid) > max_meters {
            return 0.0;
        }
        worst + (best - worst) * geo_point_distance_score(&self.center, &centroid, max_meters)
    }
}

#[cfg(test)]
mod tests {
    //! Most tests are rows in [CASES]: one ATP feature, one OSM
    //! candidate, their distance, and the expected score range. To
    //! cover another country, add rows with real examples from there.
    use super::*;
    use crate::tables::{Feature, StringPool};
    use crate::utils::UtcTimestamp;
    use tempfile::TempDir;

    /// Zürich HB, where most test features are placed.
    const LAT: f64 = 47.3782;
    const LON: f64 = 8.5402;

    fn place(tags: &[(&str, &str)]) -> Place {
        let mut mask = MatchMask::default();
        for (k, v) in tags {
            mask.add_tag(k, v);
        }
        let coord = geo::Coord { x: LON, y: LAT };
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

    /// The score for an ATP feature with `atp` tags against an OSM
    /// feature with `osm` tags, `meters` north of it; `None` if the
    /// ATP feature gets no [TransitMatcher].
    fn score(atp: &[(&str, &str)], osm: &[(&str, &str)], meters: f64) -> Option<f64> {
        let matcher = TransitMatcher::for_place(&place(atp))?;
        let dir = TempDir::new().expect("tempdir");
        let mut strings: Vec<String> = osm
            .iter()
            .flat_map(|(k, v)| [k.to_string(), v.to_string()])
            .collect();
        strings.sort();
        strings.dedup();
        let pool = StringPool::create(strings.into_iter(), dir.path(), &dir.path().join("strings"))
            .expect("StringPool::create");
        let tags = osm
            .iter()
            .flat_map(|(k, v)| {
                [
                    pool.lookup(k).unwrap() as u32,
                    pool.lookup(v).unwrap() as u32,
                ]
            })
            .collect();
        let point = geo::Point::new(LON, LAT + meters / 111_195.0);
        let feature = Feature {
            id: 1,
            tags,
            geometry_wkb: crate::geometry::encode_wkb(&geo::Geometry::from(point)),
            ..Default::default()
        };
        let candidate = OsmCandidate {
            feature: &feature,
            strings: &pool,
        };
        Some(matcher.score_osm_candidate(&candidate))
    }

    struct Case {
        what: &'static str,
        atp: &'static [(&'static str, &'static str)],
        osm: &'static [(&'static str, &'static str)],
        meters: f64,
        /// Expected score, inclusive range.
        want: (f64, f64),
    }

    const NO_MATCH: (f64, f64) = (0.0, 0.0);

    // ATP features, as the `opentransportdata_swiss` spider emits them.
    const ZH_HB_STATION: &[(&str, &str)] = &[
        ("public_transport", "station"),
        ("railway", "station"),
        ("train", "yes"),
        ("name", "Zürich HB"),
        ("ref:IFOPT", "ch:1:sloid:3000"),
        ("uic_ref", "8503000"),
    ];
    const ZH_HB_PLATFORM_3_4: &[(&str, &str)] = &[
        ("public_transport", "platform"),
        ("railway", "platform"),
        ("train", "yes"),
        ("name", "Zürich HB"),
        ("ref:IFOPT", "ch:1:sloid:3000:3"),
        ("local_ref", "4;5"),
    ];
    const ZH_HB_TRACK_4: &[(&str, &str)] = &[
        ("public_transport", "stop_position"),
        ("railway", "stop"),
        ("train", "yes"),
        ("name", "Zürich HB"),
        ("ref:IFOPT", "ch:1:sloid:3000:3:4"),
        ("local_ref", "4"),
    ];
    const BUS_STOP_AREA: &[(&str, &str)] = &[
        ("public_transport", "stop_area"),
        ("bus", "yes"),
        ("name", "Zürich, Bahnhofquai/HB"),
        ("ref:IFOPT", "ch:1:sloid:91119"),
        ("uic_ref", "8591119"),
    ];
    const BUS_PLATFORM: &[(&str, &str)] = &[
        ("public_transport", "platform"),
        ("highway", "bus_stop"),
        ("bus", "yes"),
        ("name", "Zürich, Bahnhofquai/HB"),
        ("ref:IFOPT", "ch:1:sloid:91119:0:12345"),
        ("local_ref", "B"),
    ];
    // Swiss trams stop at platforms that OSM tags `railway=platform`.
    const TRAM_PLATFORM: &[(&str, &str)] = &[
        ("public_transport", "platform"),
        ("railway", "platform"),
        ("tram", "yes"),
        ("bus", "yes"),
        ("name", "Zürich, Bahnhofplatz/HB"),
        ("ref:IFOPT", "ch:1:sloid:87348:1:1"),
    ];
    // A stop in Germany, listed by the Swiss feed with both its Swiss
    // and its German ID; the Swiss number is not a UIC code.
    const DE_STOP_AREA: &[(&str, &str)] = &[
        ("public_transport", "stop_area"),
        ("bus", "yes"),
        ("name", "Schönau (Schw), Schönenbuchen"),
        ("ref:IFOPT", "ch:1:sloid:1100020;de:08336:3674"),
    ];
    // A platform from a hypothetical German feed.
    const DE_PLATFORM: &[(&str, &str)] = &[
        ("public_transport", "platform"),
        ("highway", "bus_stop"),
        ("name", "Schönau (Schw), Schönenbuchen"),
        ("ref:IFOPT", "de:08336:3674:1:1"),
    ];
    // A platform from a feed without any IDs.
    const NAME_ONLY_PLATFORM: &[(&str, &str)] = &[
        ("public_transport", "platform"),
        ("highway", "bus_stop"),
        ("name", "Zürich,  Bahnhofquai/HB"),
    ];

    const CASES: &[Case] = &[
        // Exact IDs.
        Case {
            what: "station, same ref:IFOPT and uic_ref",
            atp: ZH_HB_STATION,
            osm: &[
                ("public_transport", "station"),
                ("railway", "station"),
                ("uic_ref", "8503000"),
                ("ref:IFOPT", "ch:1:sloid:3000"),
            ],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "Swiss OSM tags SLOIDs as ref:sloid",
            atp: ZH_HB_TRACK_4,
            osm: &[
                ("public_transport", "stop_position"),
                ("railway", "stop"),
                ("ref:sloid", "ch:1:sloid:3000:3:4"),
            ],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "... or as railway:ref:sloid, in any case and with spaces",
            atp: ZH_HB_TRACK_4,
            osm: &[
                ("public_transport", "stop_position"),
                ("railway:ref:sloid", " CH:1:SLOID:3000:3:4 "),
            ],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "exact ID wins over disagreeing local_ref",
            atp: ZH_HB_TRACK_4,
            osm: &[
                ("public_transport", "stop_position"),
                ("ref:sloid", "ch:1:sloid:3000:3:4"),
                ("local_ref", "44"),
            ],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "same stop, score drops with distance",
            atp: ZH_HB_STATION,
            osm: &[("public_transport", "station"), ("uic_ref", "8503000")],
            meters: 200.0,
            want: (0.4, 0.5),
        },
        Case {
            what: "a second ID of a stop abroad, from another authority",
            atp: DE_STOP_AREA,
            osm: &[
                ("public_transport", "stop_area"),
                ("type", "public_transport"),
                ("ref:IFOPT", "de:08336:3674"),
            ],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "same ID far away beats any same-stop candidate",
            atp: ZH_HB_TRACK_4,
            osm: &[
                ("public_transport", "stop_position"),
                ("ref:sloid", "ch:1:sloid:3000:3:4"),
            ],
            meters: 390.0,
            want: (0.9, 0.91),
        },
        Case {
            what: "OSM puts a platform's SLOID on its stop position",
            atp: BUS_PLATFORM,
            osm: &[
                ("public_transport", "stop_position"),
                ("bus", "yes"),
                ("ref:sloid", "ch:1:sloid:91119:0:12345"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "quay IDs disagree, even though the UIC code is shared",
            atp: BUS_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("highway", "bus_stop"),
                ("uic_ref", "8591119"),
                ("ref:sloid", "ch:1:sloid:91119:0:2"),
                ("local_ref", "B"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        // Same stop, then local_ref decides.
        Case {
            what: "platform, same uic_ref, same local_ref in other order",
            atp: ZH_HB_PLATFORM_3_4,
            osm: &[
                ("public_transport", "platform"),
                ("railway", "platform"),
                ("uic_ref", "8503000"),
                ("local_ref", "5; 4"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "platform mapped per track in OSM: partial local_ref",
            atp: ZH_HB_PLATFORM_3_4,
            osm: &[
                ("public_transport", "platform"),
                ("railway", "platform"),
                ("uic_ref", "8503000"),
                ("local_ref", "4"),
            ],
            meters: 0.0,
            want: (0.48, 0.48),
        },
        Case {
            what: "neighbouring platform of the same station",
            atp: ZH_HB_PLATFORM_3_4,
            osm: &[
                ("public_transport", "platform"),
                ("railway", "platform"),
                ("uic_ref", "8503000"),
                ("local_ref", "6;7"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "OSM platform without local_ref",
            atp: ZH_HB_PLATFORM_3_4,
            osm: &[
                ("public_transport", "platform"),
                ("railway", "platform"),
                ("uic_ref", "8503000"),
            ],
            meters: 0.0,
            want: (0.64, 0.64),
        },
        Case {
            what: "uic_ref with several values",
            atp: ZH_HB_TRACK_4,
            osm: &[
                ("public_transport", "stop_position"),
                ("uic_ref", "8503006;8503000"),
                ("local_ref", "4"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "OSM has the station's SLOID on a platform",
            atp: BUS_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("highway", "bus_stop"),
                ("ref:sloid", "ch:1:sloid:91119"),
                ("local_ref", "B"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "German stop ID on an OSM platform, quay ID in ATP",
            atp: DE_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("ref:IFOPT", "de:08336:3674"),
            ],
            meters: 0.0,
            want: (0.64, 0.64),
        },
        Case {
            what: "station against stop_area relation",
            atp: ZH_HB_STATION,
            osm: &[
                ("public_transport", "stop_area"),
                ("type", "public_transport"),
                ("uic_ref", "8503000"),
            ],
            meters: 0.0,
            want: (0.72, 0.72),
        },
        // IDs that disagree.
        Case {
            what: "different uic_ref, even though very close",
            atp: ZH_HB_STATION,
            osm: &[
                ("public_transport", "station"),
                ("railway", "station"),
                ("name", "Zürich HB"),
                ("uic_ref", "8503016"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            // Known limitation: OSM gives the underground platforms of
            // Zürich HB the UIC codes of the former stations
            // Löwenstrasse and Museumstrasse; the Swiss feed doesn't.
            what: "Zürich HB track 31, mapped with the UIC of Löwenstrasse",
            atp: &[
                ("public_transport", "stop_position"),
                ("railway", "stop"),
                ("train", "yes"),
                ("ref:IFOPT", "ch:1:sloid:3000:500:31"),
                ("local_ref", "31"),
            ],
            osm: &[
                ("public_transport", "stop_position"),
                ("railway", "stop"),
                ("uic_ref", "8516144"),
                ("local_ref", "31"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "different quay of the same German stop",
            atp: DE_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("ref:IFOPT", "de:08336:3674:1:2"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "different German stop",
            atp: DE_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("name", "Schönau (Schw), Schönenbuchen"),
                ("ref:IFOPT", "de:08336:3675:1:1"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "area 3 is not within area 31",
            atp: &[
                ("public_transport", "platform"),
                ("ref:IFOPT", "de:08336:3674:3"),
            ],
            osm: &[
                ("public_transport", "platform"),
                ("ref:IFOPT", "de:08336:3674:31:1"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        // Kinds and modes.
        Case {
            what: "a stop never takes one of its platforms",
            atp: BUS_STOP_AREA,
            osm: &[
                ("public_transport", "platform"),
                ("highway", "bus_stop"),
                ("uic_ref", "8591119"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "a track is not a platform",
            atp: ZH_HB_TRACK_4,
            osm: &[
                ("public_transport", "platform"),
                ("railway", "platform"),
                ("uic_ref", "8503000"),
                ("local_ref", "4"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "train platform against bus stop of the same station",
            atp: ZH_HB_PLATFORM_3_4,
            osm: &[
                ("public_transport", "platform"),
                ("highway", "bus_stop"),
                ("bus", "yes"),
                ("uic_ref", "8503000"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "tram platform mapped as highway=bus_stop with tram=yes",
            atp: TRAM_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("highway", "bus_stop"),
                ("tram", "yes"),
                ("uic_ref", "8587348"),
            ],
            meters: 0.0,
            want: (0.64, 0.64),
        },
        Case {
            what: "bus platform as a plain highway=bus_stop node",
            atp: BUS_PLATFORM,
            osm: &[("highway", "bus_stop"), ("uic_ref", "8591119")],
            meters: 0.0,
            want: (0.64, 0.64),
        },
        Case {
            what: "rural bus stop, mapped as one node on the road",
            atp: BUS_PLATFORM,
            osm: &[
                ("public_transport", "stop_position"),
                ("highway", "bus_stop"),
                ("bus", "yes"),
                ("uic_ref", "8591119"),
            ],
            meters: 0.0,
            want: (0.576, 0.576),
        },
        Case {
            what: "a station that OSM also tags highway=bus_stop",
            atp: BUS_PLATFORM,
            osm: &[
                ("public_transport", "station"),
                ("highway", "bus_stop"),
                ("uic_ref", "8591119"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "railway=halt without public_transport",
            atp: ZH_HB_STATION,
            osm: &[("railway", "halt"), ("uic_ref", "8503000")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        // Names, when there are no IDs to compare.
        Case {
            what: "OSM without IDs, same name",
            atp: BUS_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("highway", "bus_stop"),
                ("name", "Zürich, Bahnhofquai/HB"),
            ],
            meters: 0.0,
            want: (0.32, 0.32),
        },
        Case {
            what: "ATP without IDs, same name except case and spaces",
            atp: NAME_ONLY_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("highway", "bus_stop"),
                ("name", "zürich, bahnhofquai/HB"),
                ("uic_ref", "8591119"),
            ],
            meters: 50.0,
            want: (0.15, 0.17),
        },
        Case {
            what: "same name, but too far for a name-only match",
            atp: NAME_ONLY_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("name", "Zürich, Bahnhofquai/HB"),
            ],
            meters: 150.0,
            want: NO_MATCH,
        },
        Case {
            what: "no IDs, different name",
            atp: NAME_ONLY_PLATFORM,
            osm: &[
                ("public_transport", "platform"),
                ("name", "Zürich, Central"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "no IDs, no name",
            atp: BUS_PLATFORM,
            osm: &[("public_transport", "platform")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "not public transport",
            atp: BUS_PLATFORM,
            osm: &[("amenity", "bench"), ("uic_ref", "8591119")],
            meters: 0.0,
            want: NO_MATCH,
        },
    ];

    #[test]
    fn test_cases() {
        let mut failures = Vec::new();
        for case in CASES {
            let got = score(case.atp, case.osm, case.meters).expect("matcher");
            let (lo, hi) = case.want;
            if got < lo - 1e-3 || got > hi + 1e-3 {
                failures.push(format!("{}: got {got:.3}, want {lo}..={hi}", case.what));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Within a station, the right platform and track must score
    /// higher than their neighbours.
    #[test]
    fn test_ranking_within_station() {
        let platform = |local_ref: &'static str| {
            vec![
                ("public_transport", "platform"),
                ("railway", "platform"),
                ("uic_ref", "8503000"),
                ("local_ref", local_ref),
            ]
        };
        let right = score(ZH_HB_PLATFORM_3_4, &platform("4;5"), 30.0).unwrap();
        let near_wrong = score(ZH_HB_PLATFORM_3_4, &platform("6;7"), 5.0).unwrap();
        let partial = score(ZH_HB_PLATFORM_3_4, &platform("5"), 5.0).unwrap();
        assert!(right > partial, "{right} vs {partial}");
        assert!(partial > near_wrong, "{partial} vs {near_wrong}");
    }

    /// Where OSM has a platform and a combined stop position with the
    /// same ID, the platform must win, even if a bit farther away.
    #[test]
    fn test_platform_beats_combined_node() {
        let combined = [
            ("public_transport", "stop_position"),
            ("highway", "bus_stop"),
            ("ref:sloid", "ch:1:sloid:91119:0:12345"),
        ];
        let platform = [
            ("public_transport", "platform"),
            ("highway", "platform"),
            ("ref:sloid", "ch:1:sloid:91119:0:12345"),
        ];
        let node = score(BUS_PLATFORM, &combined, 0.0).unwrap();
        let way = score(BUS_PLATFORM, &platform, 15.0).unwrap();
        assert!(way > node, "{way} vs {node}");
    }

    #[test]
    fn test_no_matcher() {
        // Not public transport.
        assert!(score(&[("amenity", "taxi"), ("name", "Taxi")], &[], 0.0).is_none());
        // Public transport, but nothing to match by.
        assert!(score(&[("public_transport", "platform")], &[], 0.0).is_none());
        // A shop is for PoiMatcher.
        assert!(score(&[("shop", "kiosk"), ("name", "k kiosk")], &[], 0.0).is_none());
    }

    #[test]
    fn test_swiss_uic() {
        assert_eq!(swiss_uic("ch:1:sloid:3000"), Some("8503000".into()));
        assert_eq!(swiss_uic("ch:1:sloid:3000:3:4"), Some("8503000".into()));
        assert_eq!(
            swiss_uic("ch:1:sloid:91119:0:12345"),
            Some("8591119".into())
        );
        // Empty area field, as the feed writes when there is no area.
        assert_eq!(swiss_uic("ch:1:sloid:274::2741"), Some("8500274".into()));
        // Stops abroad have longer numbers, which aren't UIC codes.
        assert_eq!(swiss_uic("ch:1:sloid:1100020"), None);
        assert_eq!(swiss_uic("ch:1:sloid:"), None);
        assert_eq!(swiss_uic("ch:1:sloid:12a"), None);
        assert_eq!(swiss_uic("de:08336:3674"), None);
    }

    #[test]
    fn test_ifopt_stop() {
        assert_eq!(ifopt_stop("ch:1:sloid:3000"), ("ch:1:sloid:3000", 0));
        assert_eq!(ifopt_stop("ch:1:sloid:3000:3"), ("ch:1:sloid:3000", 1));
        assert_eq!(ifopt_stop("ch:1:sloid:274::2741"), ("ch:1:sloid:274", 2));
        assert_eq!(ifopt_stop("de:08336:3674"), ("de:08336:3674", 0));
        assert_eq!(ifopt_stop("de:08336:3674:1:2"), ("de:08336:3674", 2));
        assert_eq!(ifopt_stop("garbage"), ("garbage", 0));
    }

    #[test]
    fn test_modes() {
        let modes = |tags: &[(&str, &str)]| Modes::from_tags(tags.iter().copied());
        let train = modes(&[("railway", "platform"), ("train", "yes")]);
        let bus_stop = modes(&[("highway", "bus_stop")]);
        let any_rail = modes(&[("railway", "platform")]);
        let unknown = modes(&[("public_transport", "platform")]);
        assert!(!train.compatible(bus_stop));
        assert!(train.compatible(any_rail));
        assert!(train.compatible(unknown));
        assert!(!any_rail.compatible(bus_stop));
        assert_eq!(modes(&[("bus", "no"), ("highway", "bus_stop")]), bus_stop);
        let bus = modes(&[("bus", "yes")]);
        let trolleybus = modes(&[("trolleybus", "yes")]);
        let tram = modes(&[("tram", "yes")]);
        let light_rail = modes(&[("light_rail", "yes")]);
        assert!(bus.compatible(trolleybus));
        assert!(train.compatible(light_rail));
        assert!(tram.compatible(light_rail));
        assert!(!tram.compatible(train));
        assert!(!bus.compatible(tram));
    }
}
