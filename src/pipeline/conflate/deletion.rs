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
//! - [Reason::SwissTransitId]: it is a public transport stop in the
//!   [home area](SWISS_REGISTER) of the authoritative Swiss register
//!   with a Swiss ID, but ATP knows none of its Swiss IDs. Swiss IDs
//!   are IFOPT IDs (`ref:IFOPT`, `ref:sloid` or `railway:ref:sloid`)
//!   starting with `ch:`, and `uic_ref` codes starting with `85`. A
//!   stop whose ID is known to ATP still exists, even if it wasn't
//!   matched: the register has no bus stop positions, for example,
//!   and OSM often maps a stop with more platforms than the register.
//! - [Reason::SwissTransitStop]: it is a public transport stop in the
//!   register's home area without any IFOPT ID or `uic_ref`, and ATP
//!   has no Swiss stop anywhere near it. Aerialway stations are left
//!   out, since the register lacks most ski lifts.
//!
//! The Swiss register lists every stop in its home area: Switzerland,
//! Liechtenstein, and two enclaves served by Swiss public transport,
//! Büsingen am Hochrhein in Germany and Campione d'Italia in Italy.
//! It also lists about 28,000 stops in Germany, Austria, France and
//! Italy, but far from all of them, so outside its home area neither
//! Swiss signal applies. Outside Switzerland and Liechtenstein, its
//! features don't count for the Wikidata and name signals either, not
//! even in the enclaves, since those signals go by country: eight
//! German stops with `operator=DB Netz AG` would otherwise make every
//! Berlin S-Bahn station a candidate. They still match OSM features,
//! though, and their IDs and positions still keep OSM stops near the
//! border from becoming Swiss candidates. See
//! `docs/notes/2026-11-07-swiss-transit-abroad.md`.
//!
//! For stops that one of the Swiss signals is about, including ski
//! lift stations in the home area, the Wikidata and name signals
//! don't apply: the Swiss register lists `network:wikidata` and
//! `operator` for its stops, and that would flag every unmatched stop
//! of a network or operator. The Swiss signals only apply when ATP
//! actually has the Swiss register, so that a week in which its
//! spider failed doesn't turn every Swiss stop into a deletion
//! candidate.
//!
//! Tag values can hold several `;`-separated values; any one of them
//! counts.
//!
//! A candidate by [Reason::Wikidata] or [Reason::Name] must also have
//! enough matched OSM features of its brand, network or operator
//! nearby, since a spider often lists only part of a brand's
//! locations in a country; see [super::local_coverage].

use super::local_coverage::{LocalKey, Radii, SpacingBuilder, table_bytes};
use crate::matchers::{
    IFOPT_KEYS, MatchMask, infrastructure_kind, is_transit_stop, normalize_name,
    parse_wikidata_ids, split_values, swiss_uic,
};
use crate::pipeline::wikidata_ids::WIKIDATA_KEYS;
use crate::places::Place;
use crate::utils::countries::{Country, country};
use s2::cellid::CellID;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

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

/// A national public transport register in ATP, and its home area:
/// where it lists every stop. Only the Swiss register for now; once
/// ATP has others, such as Germany's DELFI, they can go here.
struct TransitRegister {
    /// The countries where the register lists every stop.
    countries: &'static [Country],
    /// Areas outside `countries` where it lists every stop too.
    enclaves: &'static [Enclave],
}

/// A small area that belongs to a [TransitRegister]'s home area
/// although its country doesn't.
struct Enclave {
    /// The country the enclave lies in, by [country].
    country: Country,
    /// The enclave's boundary as (longitude, latitude) pairs: its OSM
    /// boundary relation, widened by about 50 meters so that stops on
    /// the border count, and simplified to about 25 meters.
    ring: &'static [(f64, f64)],
}

/// The Swiss public transport register, as ATP's
/// `opentransportdata_swiss` spider emits it.
#[rustfmt::skip]
const SWISS_REGISTER: TransitRegister = TransitRegister {
    countries: &[Country::SWITZERLAND, Country::LIECHTENSTEIN],
    enclaves: &[
        // Büsingen am Hochrhein, relation 2785126, on 2026-10-20.
        Enclave {
            country: Country::GERMANY,
            ring: &[
                (8.65822, 47.69171), (8.65797, 47.69046), (8.65917, 47.69013),
                (8.65920, 47.68918), (8.66217, 47.68786), (8.66345, 47.68774),
                (8.66295, 47.68546), (8.66962, 47.68439), (8.67198, 47.68470),
                (8.67885, 47.68714), (8.68537, 47.69106), (8.68856, 47.69428),
                (8.69018, 47.69493), (8.69164, 47.69482), (8.69942, 47.69251),
                (8.70454, 47.68992), (8.70753, 47.68912), (8.71492, 47.68919),
                (8.71854, 47.69070), (8.71772, 47.69229), (8.71826, 47.69273),
                (8.71823, 47.69513), (8.71216, 47.69662), (8.71244, 47.70190),
                (8.70954, 47.70436), (8.70707, 47.71122), (8.70524, 47.71363),
                (8.70340, 47.71498), (8.70132, 47.71564), (8.69827, 47.71542),
                (8.69697, 47.71411), (8.68933, 47.71210), (8.68902, 47.71165),
                (8.68966, 47.70979), (8.68618, 47.70903), (8.68422, 47.70915),
                (8.68155, 47.71019), (8.68070, 47.71076), (8.68202, 47.71161),
                (8.68188, 47.71268), (8.67804, 47.71360), (8.67392, 47.71309),
                (8.67131, 47.71147), (8.66598, 47.71347), (8.66425, 47.71372),
                (8.66543, 47.70766), (8.67301, 47.70459), (8.67571, 47.69810),
                (8.67364, 47.69818), (8.67303, 47.69537), (8.67216, 47.69537),
                (8.66764, 47.69356), (8.66764, 47.69251), (8.65822, 47.69171),
            ],
        },
        // Campione d'Italia, relation 46664, on 2026-10-20.
        Enclave {
            country: Country::ITALY,
            ring: &[
                (8.95792, 45.96493), (8.95832, 45.96439), (8.96797, 45.96262),
                (8.96787, 45.96183), (8.96830, 45.96149), (8.97103, 45.96118),
                (8.97277, 45.96156), (8.97616, 45.96137), (8.97756, 45.96365),
                (8.97797, 45.96783), (8.97884, 45.96825), (8.98001, 45.97101),
                (8.97800, 45.97197), (8.97698, 45.97593), (8.97708, 45.97954),
                (8.97821, 45.98256), (8.97765, 45.98340), (8.96718, 45.98460),
                (8.96656, 45.98442), (8.96425, 45.97993), (8.96352, 45.97629),
                (8.96087, 45.97252), (8.95792, 45.96493),
            ],
        },
    ],
};

/// The enclaves of [SWISS_REGISTER] as polygons.
static SWISS_ENCLAVES: LazyLock<Vec<(Country, geo::Polygon)>> = LazyLock::new(|| {
    SWISS_REGISTER
        .enclaves
        .iter()
        .map(|e| {
            let ring: Vec<geo::Coord> = e.ring.iter().map(|&(x, y)| geo::Coord { x, y }).collect();
            (e.country, geo::Polygon::new(ring.into(), Vec::new()))
        })
        .collect()
});

/// Whether (`lng`, `lat`), which lies in `country` by [country], is
/// in the home area of [SWISS_REGISTER].
fn in_swiss_home(country: Option<Country>, lng: f64, lat: f64) -> bool {
    use geo::Contains;
    let Some(country) = country else {
        return false;
    };
    SWISS_REGISTER.countries.contains(&country)
        || SWISS_ENCLAVES
            .iter()
            .any(|(c, polygon)| *c == country && polygon.contains(&geo::Point::new(lng, lat)))
}

/// Why an unmatched OSM feature is a candidate for deletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Reason {
    Wikidata,
    Name,
    SwissTransitId,
    SwissTransitStop,
}

impl Reason {
    /// The name in logs and in `conflated.parquet`.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Reason::Wikidata => "wikidata",
            Reason::Name => "name",
            Reason::SwissTransitId => "swiss_transit_id",
            Reason::SwissTransitStop => "swiss_transit_stop",
        }
    }
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

    /// The positions of ATP features by spider and brand, until
    /// [AtpCoverage::finish] turns them into `radii`.
    spacing: SpacingBuilder,

    /// The radius of [super::local_coverage] for each brand and
    /// country.
    radii: Radii,
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
        // Outside its home countries, the Swiss register is far from
        // complete, so its features there say nothing about which
        // brands, networks and operators ATP covers. That holds in
        // its enclaves too, since coverage is by country: the
        // register's stops in Büsingen would make their network look
        // covered in all of Germany.
        let abroad = place.tags.iter().any(|(key, value)| {
            IFOPT_KEYS.contains(&key.as_str()) && swiss_ifopts(value).next().is_some()
        }) && !country.is_some_and(|c| SWISS_REGISTER.countries.contains(&c));
        let infrastructure = infrastructure_of(
            place.mask,
            place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        );
        let spider = {
            let mut hasher = std::hash::DefaultHasher::new();
            place.spider.hash(&mut hasher);
            hasher.finish()
        };
        for (key, value) in &place.tags {
            if abroad
                && (WIKIDATA_KEYS.contains(&key.as_str()) || NAME_KEYS.contains(&key.as_str()))
            {
                continue;
            }
            if WIKIDATA_KEYS.contains(&key.as_str()) {
                for id in parse_wikidata_ids(value) {
                    self.spacing.add(
                        spider,
                        LocalKey::wikidata(id, infrastructure),
                        country,
                        place.s2_cell_id,
                    );
                    if let Some(kind) = infrastructure {
                        self.infrastructure_wikidata.insert((id, country, kind));
                    } else {
                        let mask = self.wikidata.entry((id, country)).or_default();
                        *mask = MatchMask(mask.0 | place.mask.0);
                    }
                }
            } else if NAME_KEYS.contains(&key.as_str()) {
                for name in split_values(value).map(normalize_name) {
                    self.spacing.add(
                        spider,
                        LocalKey::name(&name, infrastructure),
                        country,
                        place.s2_cell_id,
                    );
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

    /// Computes the radius of [super::local_coverage] for each brand
    /// and country, once all ATP features have been added. Returns the
    /// approximate bytes that the positions took until then.
    pub(super) fn finish(&mut self) -> usize {
        let spacing = std::mem::take(&mut self.spacing);
        let bytes = spacing.bytes();
        self.radii = spacing.build();
        bytes
    }

    /// The radius, in meters, of [super::local_coverage] for `key` in
    /// `country`.
    pub(super) fn radius(&self, key: LocalKey, country: Option<Country>) -> f64 {
        self.radii.get(key, country)
    }

    /// How many pairs of brand and country have a radius.
    pub(super) fn num_radii(&self) -> usize {
        self.radii.len()
    }

    /// Approximate bytes on the heap, not counting the positions that
    /// [AtpCoverage::finish] has dropped.
    pub(super) fn bytes(&self) -> usize {
        let strings = |names: &mut dyn Iterator<Item = &String>| -> usize {
            names.map(|s| s.capacity()).sum()
        };
        table_bytes(
            self.wikidata.capacity(),
            size_of::<((u64, Option<Country>), MatchMask)>(),
        ) + table_bytes(
            self.names.capacity(),
            size_of::<((String, Option<Country>), (MatchMask, u32))>(),
        ) + strings(&mut self.names.keys().map(|(s, _)| s))
            + table_bytes(
                self.infrastructure_wikidata.capacity(),
                size_of::<(u64, Option<Country>, &'static str)>(),
            )
            + table_bytes(
                self.infrastructure_names.capacity(),
                size_of::<((String, Option<Country>, &'static str), u32)>(),
            )
            + strings(&mut self.infrastructure_names.keys().map(|(s, _, _)| s))
            + table_bytes(self.swiss_ifopt.capacity(), size_of::<String>())
            + strings(&mut self.swiss_ifopt.iter())
            + table_bytes(self.swiss_uic.capacity(), size_of::<String>())
            + strings(&mut self.swiss_uic.iter())
            + table_bytes(self.swiss_cells.capacity(), size_of::<u64>())
            + self.spacing.bytes()
            + self.radii.bytes()
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
) -> Option<Candidate> {
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
        // A stop whose ID ATP knows still exists, wherever it is.
        if known {
            return None;
        }
        if has_swiss_id || !has_any_id {
            let (lng, lat) = locate()?;
            // Outside the home area, OSM still has old Swiss codes on
            // stops that the register has since given their own
            // country's codes, such as `uic_ref=8581987` in Nauders,
            // Austria. Neither Swiss signal is about such a stop.
            if in_swiss_home(country(lng, lat), lng, lat) {
                if has_swiss_id {
                    return Some(Candidate::swiss(Reason::SwissTransitId));
                }
                // Most ski lift stations aren't in the register, and
                // those that are carry its IDs. Their operators often
                // are, though, so the name signal doesn't apply either.
                if tags.clone().any(|(key, _)| key == "aerialway") {
                    return None;
                }
                return (!atp.has_swiss_stop_near(lng, lat))
                    .then(|| Candidate::swiss(Reason::SwissTransitStop));
            }
        }
    }

    let brand = brand_tags(tags)?;
    let local_kind = brand.local_kind();
    let country = locate().and_then(|(lng, lat)| country(lng, lat));
    let keys: Vec<LocalKey> = brand
        .ids
        .iter()
        .filter(|&&id| atp.covers_wikidata(id, country, brand.kind))
        .map(|&id| LocalKey::wikidata(id, local_kind))
        .collect();
    if !keys.is_empty() {
        return Some(Candidate {
            reason: Reason::Wikidata,
            keys,
            country,
        });
    }
    let keys: Vec<LocalKey> = brand
        .names
        .into_iter()
        .filter(|name| atp.covers_name(name.clone(), country, brand.kind))
        .map(|name| LocalKey::name(&name, local_kind))
        .collect();
    (!keys.is_empty()).then_some(Candidate {
        reason: Reason::Name,
        keys,
        country,
    })
}

/// A deletion candidate, as [deletion_reason] finds it.
#[derive(Debug, PartialEq)]
pub(super) struct Candidate {
    pub(super) reason: Reason,
    /// For [Reason::Wikidata] and [Reason::Name], the brands, networks
    /// or operators by which ATP covers the feature in its country;
    /// [super::local_coverage] then decides whether ATP covers them
    /// around the feature too. Empty for the Swiss signals.
    pub(super) keys: Vec<LocalKey>,
    /// The country of the feature, for [AtpCoverage::radius].
    pub(super) country: Option<Country>,
}

impl Candidate {
    fn swiss(reason: Reason) -> Self {
        Candidate {
            reason,
            keys: Vec::new(),
            country: None,
        }
    }
}

/// The brands, networks and operators of an OSM feature, and its kind.
struct BrandTags {
    /// Wikidata IDs of `brand:wikidata`, `network:wikidata` and
    /// `operator:wikidata`.
    ids: Vec<u64>,
    /// The [normalize_name]d `brand`, `network` and `operator` values,
    /// only for features without Wikidata IDs: a Wikidata ID says
    /// better which brand is meant than its name.
    names: Vec<String>,
    kind: Kind,
}

impl BrandTags {
    /// The kind for [LocalKey].
    fn local_kind(&self) -> Option<&'static str> {
        match self.kind {
            Kind::Mask(_) => None,
            Kind::Infrastructure(kind) => Some(kind),
        }
    }
}

/// The [BrandTags] of the OSM feature with `tags`, or `None` if it has
/// no brand, network or operator that could make it a candidate.
fn brand_tags<'a>(tags: impl Iterator<Item = (&'a str, &'a str)> + Clone) -> Option<BrandTags> {
    let ids: Vec<u64> = tags
        .clone()
        .filter(|(key, _)| WIKIDATA_KEYS.contains(key))
        .flat_map(|(_, value)| parse_wikidata_ids(value))
        .collect();
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
    Some(BrandTags { ids, names, kind })
}

/// The [LocalKey]s of a matched OSM feature with `tags`, for counting
/// it in [super::local_coverage::LocalCoverage]. These are the keys
/// that [deletion_reason] would consider for an unmatched one.
pub(super) fn local_keys<'a>(
    tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
) -> Vec<LocalKey> {
    let Some(brand) = brand_tags(tags) else {
        return Vec::new();
    };
    let kind = brand.local_kind();
    if brand.ids.is_empty() {
        brand
            .names
            .iter()
            .map(|n| LocalKey::name(n, kind))
            .collect()
    } else {
        brand
            .ids
            .iter()
            .map(|&id| LocalKey::wikidata(id, kind))
            .collect()
    }
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
    const BUSINGEN: (f64, f64) = (8.6900, 47.6960);
    const CAMPIONE: (f64, f64) = (8.9710, 45.9690);
    const NAUDERS: (f64, f64) = (10.5017, 46.8906);
    const BERLIN_HBF: (f64, f64) = (13.3695, 52.5251);

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
        deletion_reason(tags.iter().copied(), atp, || Some(at)).map(|c| c.reason)
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

    #[test]
    fn test_in_swiss_home() {
        let home = |(lng, lat)| in_swiss_home(country(lng, lat), lng, lat);
        assert!(home(ZURICH_HB));
        assert!(home(VADUZ));
        assert!(home(BUSINGEN));
        assert!(home(CAMPIONE));
        assert!(!home(SINGEN));
        assert!(!home(NAUDERS));
        assert!(!home(BERLIN_HBF));
        // Near Büsingen, but in Germany proper.
        assert!(!home((8.7550, 47.6970))); // Gailingen am Hochrhein
        assert!(!in_swiss_home(None, BUSINGEN.0, BUSINGEN.1));
    }

    #[test]
    fn test_coverage_ignores_swiss_register_abroad() {
        let mut atp = AtpCoverage::default();
        // The Swiss register lists eight stops in Germany with
        // `operator=DB Netz AG`, but far from all of Germany's.
        let db = place(
            &[
                ("public_transport", "stop_area"),
                ("ref:IFOPT", "ch:1:sloid:1100008"),
                ("uic_ref", "1100008"),
                ("operator", "DB Netz AG"),
                ("operator:wikidata", "Q896765"),
            ],
            BERLIN_HBF,
        );
        for _ in 0..MIN_NAME_FEATURES {
            add(&mut atp, &db);
        }
        atp.swiss_transit_features = MIN_SWISS_TRANSIT_FEATURES;
        assert_eq!(atp.num_wikidata_countries(), 0);
        assert_eq!(atp.num_name_countries(), 0);
        // Its IDs and positions still count.
        assert!(atp.swiss_ifopt.contains("ch:1:sloid:1100008"));
        assert!(atp.has_swiss_stop_near(BERLIN_HBF.0, BERLIN_HBF.1));
        let s_bahn = [
            ("public_transport", "stop_area"),
            ("operator", "DB Netz AG;DB Station&Service AG"),
        ];
        assert_eq!(reason(&atp, &s_bahn, BERLIN_HBF), None);
        let s_bahn = [
            ("public_transport", "station"),
            ("operator:wikidata", "Q896765"),
        ];
        assert_eq!(reason(&atp, &s_bahn, BERLIN_HBF), None);

        // Other spiders' features abroad count as before, and so do
        // the register's features in Switzerland. In the enclaves,
        // they would make a network look covered in all of Germany
        // or Italy, so they don't count.
        add(
            &mut atp,
            &place(
                &[("public_transport", "station"), ("operator", "Bahn")],
                SINGEN,
            ),
        );
        for at in [ZURICH_HB, BUSINGEN, CAMPIONE] {
            add(
                &mut atp,
                &place(
                    &[
                        ("public_transport", "platform"),
                        ("ref:IFOPT", "ch:1:sloid:3000:1:1"),
                        ("operator:wikidata", "Q83835"),
                    ],
                    at,
                ),
            );
        }
        assert_eq!(atp.num_name_countries(), 1);
        assert_eq!(atp.num_wikidata_countries(), 1);
    }

    #[test]
    fn test_swiss_signals_in_enclaves_only() {
        let atp = coverage(true);
        let bus_stop = [("highway", "bus_stop")];
        let old_code = [("public_transport", "platform"), ("uic_ref", "8581987")];
        for at in [BUSINGEN, CAMPIONE] {
            assert_eq!(reason(&atp, &bus_stop, at), Some(Reason::SwissTransitStop));
            assert_eq!(reason(&atp, &old_code, at), Some(Reason::SwissTransitId));
        }
        // Old Swiss codes abroad, such as in Nauders, Austria, where
        // the register now has its own Austrian codes.
        for at in [NAUDERS, SINGEN] {
            assert_eq!(reason(&atp, &bus_stop, at), None);
            assert_eq!(reason(&atp, &old_code, at), None);
        }
        // Abroad, the general signals apply to such stops again.
        let tags = [
            ("public_transport", "platform"),
            ("uic_ref", "8581987"),
            ("operator:wikidata", "Q7"),
            ("amenity", "fuel"),
        ];
        assert_eq!(reason(&atp, &tags, SINGEN), Some(Reason::Wikidata));
        // Without a location, nothing says where the stop is.
        assert_eq!(
            deletion_reason(old_code.iter().copied(), &atp, || None),
            None
        );
    }

    #[test]
    fn test_candidate_keys() {
        let atp = coverage(false);
        let tags = [("shop", "supermarket"), ("brand:wikidata", "Q9;Q1")];
        let c = deletion_reason(tags.iter().copied(), &atp, || Some(SINGEN)).expect("candidate");
        assert_eq!(c.reason, Reason::Wikidata);
        // Only Q1 is covered by ATP, so only Q1 counts locally.
        assert_eq!(c.keys, vec![LocalKey::wikidata(1, None)]);
        assert_eq!(c.country, Some(Country::GERMANY));
        assert_eq!(
            local_keys(tags.iter().copied()),
            vec![LocalKey::wikidata(9, None), LocalKey::wikidata(1, None)]
        );
        assert_eq!(
            local_keys([("shop", "bakery"), ("brand", "Beck  Bäck")].into_iter()),
            vec![LocalKey::name("beck bäck", None)]
        );
        assert_eq!(
            local_keys([("amenity", "post_box"), ("operator:wikidata", "Q5")].into_iter()),
            vec![LocalKey::wikidata(5, Some("post_box"))]
        );
        assert!(local_keys([("shop", "bakery")].into_iter()).is_empty());
    }

    #[test]
    fn test_coverage_radii() {
        let mut atp = coverage(false);
        assert!(atp.bytes() > 0);
        atp.finish();
        // ATP has a single feature of Q1 in Germany: the widest radius.
        assert_eq!(
            atp.radius(LocalKey::wikidata(1, None), Some(Country::GERMANY)),
            super::super::local_coverage::MAX_RADIUS_METERS
        );
        assert!(atp.num_radii() >= 2);
    }
}
