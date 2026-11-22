//! Matches street infrastructure: post boxes, defibrillators (AEDs),
//! benches, fire hydrants, public bookcases, bicycle parking, waste
//! baskets, fountains, toilets, bike-share stations, the charge
//! points of charging stations and parcel lockers for now, with street
//! lamps and the like to follow.
//!
//! Such features have no brand and rarely a name, and many of them
//! stand close together, so the match rests on position. Each kind has
//! default [Settings]; a spider can override them, since sources differ
//! more than kinds do. Japan Post's positions are within a few meters,
//! Deutsche Post's often tens of meters off. See
//! `docs/notes/2026-11-05-infrastructure-matching.md` for the evals
//! behind the numbers.

use super::{
    Matcher, OsmCandidate, geo_point_distance, geo_point_distance_score, meters_to_chord_angle,
    normalize_name, parse_wikidata_ids,
};
use crate::places::Place;
use geo::Centroid;
use s2::{cell::Cell, cellid::CellID, point::Point, s1::ChordAngle};

/// A candidate with the same ref scores at least this much, so that it
/// wins over any candidate that is merely close.
const SAME_REF_MIN_SCORE: f64 = 0.9;

/// Evidence factor for a candidate of the same kind, before distance
/// and confirming tags. Below [SAME_REF_MIN_SCORE].
const KIND_FACTOR: f64 = 0.8;

/// Added to [KIND_FACTOR] for each confirming tag that agrees, so that
/// the right one of two close candidates wins. Two of them still stay
/// below [SAME_REF_MIN_SCORE].
const CONFIRM_BONUS: f64 = 0.04;

/// A bike-share station with the same GBFS ID ([GbfsId]) or station
/// number may be this far away. Stations get moved, and the IDs are
/// unique within a system: in Japan, 24 of the 16,359 OSM stations
/// whose ID is also in AllThePlaces are more than 100 m from it.
const GBFS_ID_MAX_METERS: f64 = 200.0;

/// A bike-share station whose name shares a word with ours may be this
/// far away. Nextbike's positions in Switzerland are often 20–60 m off
/// OSM's, at stations with the same name on both sides.
const BIKE_SHARE_NAME_MAX_METERS: f64 = 50.0;

/// A parcel locker with the same ref may be this far away. Lockers
/// keep their ref when they move, and it is unique within a network:
/// in Mazowieckie, 90% of the InPost lockers whose ref OSM has are
/// within 19 m of it, and 99% within 55 m.
const PARCEL_LOCKER_REF_MAX_METERS: f64 = 100.0;

/// Spiders whose parcel lockers don't count for deletion candidates
/// ([Matcher::counts_for_coverage]), since they list only part of
/// their network. `allegro_one_box_pl` stops at exactly 10,000
/// lockers, and in Mazowieckie, 26% of the Allegro lockers in OSM have
/// none of its lockers within 100 m
/// ([alltheplaces#20728](https://github.com/alltheplaces/alltheplaces/issues/20728)).
const PARTIAL_LOCKER_SPIDERS: &[&str] = &["allegro_one_box_pl"];

/// The brand of the parcel lockers of a spider that gives them none:
/// Deutsche Post's Packstations only have `operator=Deutsche Post`,
/// where OSM has `brand=DHL Packstation` and `operator=DHL`, and
/// `brand=Paketbox` for the smaller ones. Without it, a Packstation in
/// Berlin took an Amazon Locker 22 m away. Remove once
/// [alltheplaces#20730](https://github.com/alltheplaces/alltheplaces/issues/20730)
/// is fixed.
type BrandTags = &'static [(&'static str, &'static str)];
const LOCKER_BRAND_FALLBACK: &[(&str, BrandTags)] = &[(
    "deutsche_post_de",
    &[
        ("brand", "DHL Packstation"),
        ("brand:wikidata", "Q1766703"),
        ("operator:wikidata", "Q489815"),
    ],
)];

/// Words of parcel locker brands that say nothing about the network,
/// for [LockerBrand]: `DPD Pickup Station` and `DHL BOX 24/7` share
/// no word that counts.
const GENERIC_LOCKER_WORDS: &[&str] = &[
    "box", "hub", "locker", "lockers", "parcel", "pickup", "point", "station",
];

/// What kind of infrastructure a feature is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    PostBox,
    Defibrillator,
    Bench,
    FireHydrant,
    PublicBookcase,
    BicycleParking,
    WasteBasket,
    /// Fountains and drinking water points, which OSM tags either
    /// way: 86% of the OSM features within 2 m of a Zürich fountain
    /// (ATP `amenity=fountain`, mostly with `drinking_water=yes`) are
    /// `amenity=drinking_water`.
    Fountain,
    Toilets,
    /// Bike-share stations (`amenity=bicycle_rental`), as AllThePlaces
    /// takes them from GBFS feeds; not rental shops
    /// (`bicycle_rental=shop`).
    BikeShare,
    /// One charging device (`man_made=charge_point`), which may have
    /// several sockets. A charge point that OSM lacks is `located_in`
    /// the OSM charging station around it; see `pipeline::conflate`.
    ChargePoint,
    /// A parcel locker (`amenity=parcel_locker`), such as an InPost
    /// Paczkomat or a DHL Packstation. Several networks often stand
    /// side by side, so the brands must agree ([LockerBrand]).
    ParcelLocker,
}

impl Kind {
    fn from_tags<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Option<Kind> {
        let mut kind = None;
        let mut rental_shop = false;
        for kv in tags {
            if kv == ("bicycle_rental", "shop") {
                rental_shop = true;
            } else if kind.is_none() {
                kind = Self::from_tag(kv);
            }
        }
        if rental_shop && kind == Some(Kind::BikeShare) {
            return None;
        }
        kind
    }

    fn from_tag(kv: (&str, &str)) -> Option<Kind> {
        match kv {
            ("amenity", "post_box") => Some(Kind::PostBox),
            ("emergency", "defibrillator") => Some(Kind::Defibrillator),
            ("amenity", "bench") => Some(Kind::Bench),
            ("emergency", "fire_hydrant") => Some(Kind::FireHydrant),
            ("amenity", "public_bookcase") => Some(Kind::PublicBookcase),
            ("amenity", "bicycle_parking") => Some(Kind::BicycleParking),
            ("amenity", "waste_basket") => Some(Kind::WasteBasket),
            ("amenity", "fountain" | "drinking_water") => Some(Kind::Fountain),
            ("man_made", "drinking_fountain") => Some(Kind::Fountain),
            ("amenity", "toilets") => Some(Kind::Toilets),
            ("amenity", "bicycle_rental") => Some(Kind::BikeShare),
            ("man_made", "charge_point") => Some(Kind::ChargePoint),
            ("amenity", "parcel_locker") => Some(Kind::ParcelLocker),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::PostBox => "post_box",
            Kind::Defibrillator => "defibrillator",
            Kind::Bench => "bench",
            Kind::FireHydrant => "fire_hydrant",
            Kind::PublicBookcase => "public_bookcase",
            Kind::BicycleParking => "bicycle_parking",
            Kind::WasteBasket => "waste_basket",
            Kind::Fountain => "fountain",
            Kind::Toilets => "toilets",
            Kind::BikeShare => "bicycle_rental",
            Kind::ChargePoint => "charge_point",
            Kind::ParcelLocker => "parcel_locker",
        }
    }

    /// Tags that confirm a match when equal on both sides. Other tags
    /// say little: bench `material` agrees on only 19% of close pairs
    /// in Seattle, since ATP has `metal;wood` where OSM has `wood`, and
    /// `bicycle_parking` almost never, since ATP has `rack` where OSM
    /// has `stands`. Little Free Library's `name` (`Paul Downey #51931
    /// Kalispell MT`) never equals OSM's.
    fn confirming_keys(self) -> &'static [&'static str] {
        match self {
            Kind::PostBox => &["collection_times"],
            Kind::Defibrillator => &[],
            // 98% equal on close pairs in Zürich.
            Kind::Bench => &["backrest"],
            // All 157 close pairs of Barwon Water hydrants agree.
            Kind::FireHydrant => &["fire_hydrant:type"],
            Kind::PublicBookcase => &[],
            // About 60% equal on close pairs in San Francisco and
            // Cambridge (US), against 25–33% for random pairs.
            Kind::BicycleParking => &["capacity"],
            // Copenhagen's are all `waste=trash`.
            Kind::WasteBasket => &[],
            Kind::Fountain => &[],
            Kind::Toilets => &[],
            // `capacity` agrees on only 30% of the pairs within 2 m in
            // Japan: docks get added and removed.
            Kind::BikeShare => &[],
            Kind::ChargePoint => &[],
            // The ref is all that tells lockers apart; see [RefRule].
            Kind::ParcelLocker => &[],
        }
    }

    /// Whether different `operator:wikidata` on both sides rule out a
    /// match. True for post boxes, where the operator is the postal
    /// service. False for AEDs: ATP's Björn Steiger Stiftung is the
    /// donor, and OSM sometimes names the town or shop that runs it.
    fn operator_must_agree(self) -> bool {
        self == Kind::PostBox
    }

    /// Whether different brands on both sides rule out a match, by
    /// `brand:wikidata` or by name ([brand_names]). True for bike-share
    /// stations: a HELLO CYCLING station next to a docomo one is
    /// another station, and so is a Voi or Dott parking zone next to a
    /// PubliBike or nextbike station.
    fn brand_must_agree(self) -> bool {
        self == Kind::BikeShare
    }
}

/// The kind of street infrastructure that `tags` describe, such as
/// `post_box`, if [InfrastructureMatcher] can match it.
pub(crate) fn kind_name<'a>(
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> Option<&'static str> {
    Kind::from_tags(tags).map(Kind::name)
}

/// How ATP and OSM refs of a spider compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefRule {
    /// Refs say nothing: OSM uses another scheme, such as Swiss Post
    /// boxes (ATP `003BE_00335986`, OSM `3-8716-25024`).
    Ignore,
    /// Equal refs, ignoring leading zeros, such as Japan Post's
    /// `007153` and OSM's `7153`.
    Equal,
    /// The last word of the ATP ref equals the OSM ref, such as
    /// Steiger Stiftung's `AEDR 1786` and OSM's `1786`.
    LastWord,
    /// The ATP ref without this prefix equals the OSM ref, with or
    /// without the prefix, such as Zürich's fountain `wvz-22` and OSM's
    /// `22`.
    StripPrefix(&'static str),
    /// The last this many characters of the ATP ref equal the OSM ref,
    /// such as Deutsche Post's Packstation `1-14482182` (in postcode
    /// 14482) and OSM's `182`.
    Suffix(usize),
    /// The end of the ATP `name` after ` - ` equals the OSM ref, such as
    /// the nickname of `Amazon Locker - Nilias` and OSM's `nilias`.
    NameSuffix,
}

/// How to match the features of one kind, or of one spider.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    /// Search radius; the distance score falls linearly to 0 there.
    max_meters: f64,
    refs: RefRule,
    /// The OSM key that holds the ATP ref, usually `ref`.
    osm_ref_key: &'static str,
}

impl Settings {
    /// The settings for `kind`, or for `spider` if it has its own.
    fn for_spider(kind: Kind, spider: &str) -> Settings {
        let by_spider = match (kind, spider) {
            (Kind::PostBox, "swiss_post_ch") => Some((100.0, RefRule::Ignore)),
            (Kind::PostBox, "deutsche_post_de") => Some((100.0, RefRule::Ignore)),
            (Kind::PostBox, "japan_post_jp") => Some((20.0, RefRule::Equal)),
            (Kind::Defibrillator, "steiger_stiftung_aed") => Some((50.0, RefRule::LastWord)),
            (Kind::Defibrillator, "aed_jp") => Some((20.0, RefRule::Ignore)),
            (Kind::Bench, "stadt_zuerich_ch") => Some((10.0, RefRule::Ignore)),
            (Kind::FireHydrant, "epcor_fire_hydrants_ca") => Some((10.0, RefRule::Equal)),
            (Kind::PublicBookcase, "little_free_library") => Some((50.0, RefRule::Equal)),
            (Kind::BicycleParking, "stadt_zuerich_ch") => Some((10.0, RefRule::Ignore)),
            (Kind::BicycleParking, "cambridge_city_council_bicycle_parking_us") => {
                Some((10.0, RefRule::Ignore))
            }
            (Kind::BicycleParking, "san_francisco_mta_bicycle_parking_us") => {
                Some((20.0, RefRule::Ignore))
            }
            (Kind::WasteBasket, "copenhagen_municipality_waste_baskets_dk") => {
                Some((10.0, RefRule::Ignore))
            }
            (Kind::WasteBasket, "glasgow_city_council_waste_baskets_gb") => {
                Some((20.0, RefRule::Ignore))
            }
            (Kind::Fountain, "stadt_zuerich_ch") => Some((10.0, RefRule::StripPrefix("wvz-"))),
            (Kind::Toilets, "stadt_zuerich_ch") => Some((20.0, RefRule::Ignore)),
            (Kind::Toilets, "changing_places_gb") => Some((100.0, RefRule::Ignore)),
            // In Mazowieckie, 97% of the InPost lockers and 17% of the
            // Allegro ones have their ref in OSM, and 99% of the
            // nearest same-brand lockers within 30 m have the same
            // ref. In London, 7 of the 8 InPost refs in OSM agree.
            (Kind::ParcelLocker, "paczkomat_inpost_pl") => Some((30.0, RefRule::Equal)),
            (Kind::ParcelLocker, "allegro_one_box_pl") => Some((30.0, RefRule::Equal)),
            (Kind::ParcelLocker, "inpost_gb") => Some((30.0, RefRule::Equal)),
            // In Berlin, 393 of the 420 OSM Packstations within 20 m
            // of a Deutsche Post one have its number as `ref`.
            (Kind::ParcelLocker, "deutsche_post_de") => Some((30.0, RefRule::Suffix(3))),
            // PUDO's `PCJ00177` is OSM's `00177` or `PCJ00177` for 27
            // of the 33 close pairs in Japan.
            // PUDO's positions are rougher: of the 243 OSM PUDO lockers
            // in Japan, 49 have the nearest ATP one 30–75 m away, while
            // moving the 7,356 ATP lockers 2 km brings 3 that close.
            (Kind::ParcelLocker, "pudostation_jp") => Some((75.0, RefRule::StripPrefix("PCJ"))),
            // OSM has the locker's nickname as `ref` for 66 of the 75
            // close pairs with a ref in Berlin, and for all 3 in
            // Massachusetts.
            (Kind::ParcelLocker, s) if s.starts_with("amazon_locker_") => {
                Some((30.0, RefRule::NameSuffix))
            }
            // myflexbox's `DE-30` is OSM's `30` or `DE-30` for 22 of
            // the 27 close pairs with a ref in Berlin.
            (Kind::ParcelLocker, "myflexbox") => Some((30.0, RefRule::StripPrefix("DE-"))),
            // Packeta's `I_23111` is OSM's `ref:packeta=23111` for all
            // 1,584 close pairs in Prague and South Moravia.
            (Kind::ParcelLocker, "packeta") => {
                return Settings {
                    max_meters: 30.0,
                    refs: RefRule::StripPrefix("I_"),
                    osm_ref_key: "ref:packeta",
                };
            }
            (Kind::Bench, "seattle_parks_and_recreation_benches_us") => {
                return Settings {
                    max_meters: 15.0,
                    refs: RefRule::Equal,
                    osm_ref_key: "ref:US-WA:DPR:AMWO_ID",
                };
            }
            _ => None,
        };
        let (max_meters, refs) = by_spider.unwrap_or(match kind {
            Kind::PostBox => (50.0, RefRule::Ignore),
            Kind::Defibrillator => (30.0, RefRule::Ignore),
            Kind::Bench => (15.0, RefRule::Ignore),
            Kind::FireHydrant => (10.0, RefRule::Ignore),
            Kind::PublicBookcase => (30.0, RefRule::Ignore),
            Kind::BicycleParking => (15.0, RefRule::Ignore),
            Kind::WasteBasket => (15.0, RefRule::Ignore),
            Kind::Fountain => (15.0, RefRule::Ignore),
            Kind::Toilets => (30.0, RefRule::Ignore),
            // In Japan, 12,183 of the 13,145 pairs within 20 m are
            // within 2 m; chance pairs catch up beyond 20 m.
            Kind::BikeShare => (20.0, RefRule::Ignore),
            // In Denmark, where OSM maps charge points more than
            // elsewhere, 3,134 of 26,883 ChargePoint devices have an
            // OSM charge point within 15 m.
            Kind::ChargePoint => (15.0, RefRule::Ignore),
            // Of the ATP lockers with an OSM locker of the same brand
            // within 100 m, 90–100% have it within 30 m, against at
            // most 1% when the ATP lockers are moved 2 km away. See
            // `docs/notes/2026-11-22-parcel-lockers.md`.
            Kind::ParcelLocker => (30.0, RefRule::Ignore),
        });
        Settings {
            max_meters,
            refs,
            osm_ref_key: "ref",
        }
    }
}

/// The ref of a feature as [RefRule] compares it: lowercased, without
/// leading zeros, and for [RefRule::LastWord] on the ATP side, only
/// its last word.
fn normalize_ref(value: &str, last_word: bool) -> Option<String> {
    let value = if last_word {
        value.split_whitespace().last()?
    } else {
        value.trim()
    };
    let value = value.trim_start_matches('0').to_lowercase();
    (!value.is_empty()).then_some(value)
}

/// The ID of a bike-share station in its GBFS feed: the system ID,
/// lowercased, and the station ID within it. AllThePlaces writes it as
/// `ref:gbfs=hellocycling:1234`. OpenStreetMap has it in one of three
/// ways: `ref:gbfs` like AllThePlaces (mostly in Canada, Britain and
/// the US), `gbfs:system_id` and `gbfs:station_id` (mostly in Japan),
/// or `ref:gbfs:hellocycling=1234`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GbfsId {
    system: String,
    station: String,
}

impl GbfsId {
    fn new(system: &str, station: &str) -> Option<GbfsId> {
        let (system, station) = (system.trim(), station.trim());
        (!system.is_empty() && !station.is_empty()).then(|| GbfsId {
            system: system.to_lowercase(),
            station: station.to_string(),
        })
    }

    /// Parses `system:station`, as in `ref:gbfs`.
    fn parse(value: &str) -> Option<GbfsId> {
        let (system, station) = value.split_once(':')?;
        GbfsId::new(system, station)
    }

    /// All GBFS IDs that `tags` give, in any of the three ways.
    fn from_tags<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<GbfsId> {
        let mut ids = Vec::new();
        let (mut system, mut station) = (None, None);
        for (key, value) in tags {
            match key {
                "ref:gbfs" => ids.extend(value.split(';').filter_map(GbfsId::parse)),
                "gbfs:system_id" => system = Some(value),
                "gbfs:station_id" => station = Some(value),
                _ => {
                    if let Some(system) = key.strip_prefix("ref:gbfs:") {
                        ids.extend(value.split(';').filter_map(|v| GbfsId::new(system, v)));
                    }
                }
            }
        }
        if let (Some(system), Some(station)) = (system, station) {
            ids.extend(GbfsId::new(system, station));
        }
        ids
    }
}

/// The brand, network and operator names in `tags`, lowercased and
/// with nothing but letters and digits, so that `Deezer nextbike`
/// becomes `deezernextbike`. The system ID of a GBFS feed in `ref:gbfs`
/// counts too: `sharedmobility.ch:velospot:168` gives
/// `sharedmobilitych` and `velospot`, since an aggregator's `brand`
/// names the aggregator, not the operator.
fn brand_names<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<String> {
    let compact = |value: &str| -> String {
        value
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let mut names = Vec::new();
    for (key, value) in tags {
        match key {
            "brand" | "brand:en" | "network" | "network:en" | "network:short:en" | "operator"
            | "operator:en" => names.extend(value.split(';').map(compact)),
            "ref:gbfs" => {
                let mut parts: Vec<&str> = value.split(':').collect();
                parts.pop(); // the station ID
                names.extend(parts.into_iter().map(compact));
            }
            _ => {}
        }
    }
    names.retain(|n| n.chars().count() >= 3);
    names.sort();
    names.dedup();
    names
}

/// A station number, trimmed and lowercased.
fn normalize_code(value: &str) -> String {
    value.trim().to_lowercase()
}

/// The words of a station name, lowercased, of at least four letters
/// or digits, so that `Löwenplatz / Burbaki-Panorama` gives `löwenplatz`,
/// `burbaki` and `panorama`. Japanese names have no spaces, so
/// `M05.塩見縄手` gives `m05` and `塩見縄手`.
fn name_words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_lowercase)
        .collect()
}

/// Whether two lists of [brand_names] share a brand: one name
/// contains the other, so that `nextbike` agrees with
/// `deezernextbike`. Empty lists say nothing, so they agree.
fn brand_names_agree(atp: &[String], osm: &[String]) -> bool {
    atp.is_empty()
        || osm.is_empty()
        || atp.iter().any(|a| {
            osm.iter()
                .any(|o| a.contains(o.as_str()) || o.contains(a.as_str()))
        })
}

/// A tag value without spaces, lowercased, so that
/// `Mo-Fr 17:30; Sa 08:00` equals `Mo-Fr 17:30;Sa 08:00`.
fn normalize_value(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// Matches street infrastructure. A candidate must be the same
/// [Kind]. Then, by evidence:
///
/// - Different `operator:wikidata` on both sides, for kinds where the
///   operator must agree ([Kind::operator_must_agree]): no match.
/// - The same ref, for spiders whose refs OSM uses ([RefRule]):
///   [SAME_REF_MIN_SCORE] plus the rest times the distance score.
///   A ref never widens the search radius, since short refs repeat:
///   1,201 OSM post boxes in Japan share a ref with a Japan Post box,
///   and 456 of them are more than 200 m away.
/// - For bike-share stations, the same GBFS ID ([GbfsId]) scores like
///   the same ref, up to [GBFS_ID_MAX_METERS], since these IDs are
///   unique within their system. Another ID of the same system, or
///   different brands on both sides ([Kind::brand_must_agree]), rules
///   out a match. The same station number (ATP `short_name`, OSM
///   `ref`) counts like the same GBFS ID: Bluebikes stations in OSM are
///   often 20–150 m from where the feed now puts them, but keep their
///   number. A station name that shares a word confirms a match, and
///   widens the radius to [BIKE_SHARE_NAME_MAX_METERS].
/// - Otherwise [KIND_FACTOR], plus [CONFIRM_BONUS] for each confirming
///   tag that agrees, times the distance score. Confirming tags are
///   those of [Kind::confirming_keys], and an OSM `ref` that contains the ATP
///   street, as German OSM describes the box location in `ref`
///   (`Lütticher Str. 76`). Tags that differ never lower a score: half
///   of the right post box matches in Switzerland and Germany have
///   outdated `collection_times` in OSM, which is what an edit should
///   fix.
///
/// Matches are one to one within a spider ([Matcher::is_exclusive]),
/// since several post boxes, AEDs or benches often stand within a few
/// meters: half of the benches in Zürich and Winterthur are less than
/// 5 m from the next one. Bicycle parking is the exception
/// ([Matcher::may_share]).
pub struct InfrastructureMatcher {
    center: Point,
    kind: Kind,
    settings: Settings,
    /// See [normalize_ref]; `None` if the spider's refs say nothing.
    reference: Option<String>,
    operators: Vec<u64>,
    /// The [Kind::confirming_keys] the ATP feature has, with their
    /// [normalize_value]d values.
    confirming: Vec<(&'static str, String)>,
    /// Lowercased `addr:street`.
    street: Option<String>,
    /// For bicycle parking, the ATP `capacity`; see [Matcher::may_share].
    capacity: Option<u32>,
    /// For bike-share stations, the ATP `brand:wikidata`.
    brands: Vec<u64>,
    /// For bike-share stations, the ATP [brand_names].
    brand_names: Vec<String>,
    /// The station's ID in its GBFS feed, from `ref:gbfs`.
    gbfs_id: Option<GbfsId>,
    /// For bike-share stations, the [name_words] of the ATP `name`.
    name_words: Vec<String>,
    /// For bike-share stations, the station number that riders see,
    /// lowercased: GBFS `short_name`, which OSM has as `ref`, such as
    /// Bluebikes' `D32022` or nextbike's `80285`.
    station_code: Option<String>,
    /// For parcel lockers, the ATP brand.
    locker_brand: Option<LockerBrand>,
    /// Whether the feature counts for deletion candidates; see
    /// [PARTIAL_LOCKER_SPIDERS].
    counts_for_coverage: bool,
}

impl InfrastructureMatcher {
    /// Whether `candidate` has the same reference as ours, such as the
    /// number of a post box.
    fn same_ref(&self, candidate: &OsmCandidate) -> bool {
        let osm = candidate
            .tags()
            .find(|&(k, _)| k == self.settings.osm_ref_key)
            .map(|(_, v)| match self.settings.refs {
                // OSM sometimes has the prefix too: PUDO's `PCJ02291`.
                RefRule::StripPrefix(prefix) => v.strip_prefix(prefix).unwrap_or(v),
                _ => v,
            })
            .and_then(|v| normalize_ref(v, false));
        matches!((&self.reference, osm), (Some(atp), Some(osm)) if *atp == osm)
            || self.gbfs_match(candidate) == Some(true)
            || self.same_code(candidate)
    }

    /// For a bike-share station, `Some(true)` if `candidate` has our
    /// GBFS ID, `Some(false)` if it has another ID of the same system,
    /// and `None` otherwise. An ID of another system only counts as a
    /// name ([brand_names]): a system may be renamed, such as
    /// `docomo-cycle` and `docomo-cycle-tokyo`, and OSM may still have
    /// the old one.
    fn gbfs_match(&self, candidate: &OsmCandidate) -> Option<bool> {
        let atp = self.gbfs_id.as_ref()?;
        let osm = GbfsId::from_tags(candidate.tags());
        if osm.contains(atp) {
            Some(true)
        } else if osm.iter().any(|id| id.system == atp.system) {
            Some(false)
        } else {
            None
        }
    }

    pub fn for_place(place: &Place) -> Option<InfrastructureMatcher> {
        let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        let kind = Kind::from_tags(tags())?;
        let settings = Settings::for_spider(kind, &place.spider);
        let tag = |key: &str| tags().find(|&(k, _)| k == key).map(|(_, v)| v);
        let reference = match settings.refs {
            RefRule::Ignore => None,
            RefRule::Equal => tag("ref").and_then(|v| normalize_ref(v, false)),
            RefRule::LastWord => tag("ref").and_then(|v| normalize_ref(v, true)),
            RefRule::StripPrefix(prefix) => tag("ref")
                .and_then(|v| v.strip_prefix(prefix))
                .and_then(|v| normalize_ref(v, false)),
            RefRule::NameSuffix => tag("name")
                .and_then(|v| v.rsplit_once(" - "))
                .and_then(|(_, nick)| normalize_ref(nick, false)),
            RefRule::Suffix(n) => tag("ref")
                .map(str::trim)
                .and_then(|v| v.char_indices().rev().nth(n - 1).map(|(i, _)| &v[i..]))
                .and_then(|v| normalize_ref(v, false)),
        };
        // A locker without brand or operator could be of any network.
        // `czech_post_cz` lists the Z-BOX, AlzaBox and other lockers
        // where its parcels can be picked up that way, and Packeta
        // lists the Z-BOXes again with their brand
        // (alltheplaces/alltheplaces#20729).
        let has_brand = ["brand", "brand:wikidata", "operator", "operator:wikidata"]
            .iter()
            .any(|key| tag(key).is_some());
        if kind == Kind::ParcelLocker && !has_brand {
            return None;
        }
        Some(InfrastructureMatcher {
            center: Cell::from(CellID(place.s2_cell_id)).center(),
            kind,
            settings,
            reference,
            operators: tag("operator:wikidata")
                .map(|v| parse_wikidata_ids(v).collect())
                .unwrap_or_default(),
            confirming: kind
                .confirming_keys()
                .iter()
                .filter_map(|&key| tag(key).map(|v| (key, normalize_value(v))))
                .collect(),
            street: tag("addr:street")
                .map(normalize_name)
                .filter(|s| !s.is_empty()),
            capacity: (kind == Kind::BicycleParking)
                .then(|| tag("capacity").and_then(parse_capacity))
                .flatten(),
            brands: if kind.brand_must_agree() {
                tag("brand:wikidata")
                    .map(|v| parse_wikidata_ids(v).collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            brand_names: if kind.brand_must_agree() {
                brand_names(tags())
            } else {
                Vec::new()
            },
            gbfs_id: (kind == Kind::BikeShare)
                .then(|| tag("ref:gbfs").and_then(GbfsId::parse))
                .flatten(),
            station_code: (kind == Kind::BikeShare)
                .then(|| tag("short_name").map(normalize_code))
                .flatten()
                .filter(|code| code.chars().count() >= 3),
            name_words: if kind == Kind::BikeShare {
                tag("name").map(name_words).unwrap_or_default()
            } else {
                Vec::new()
            },
            locker_brand: (kind == Kind::ParcelLocker).then(|| {
                let fallback = LOCKER_BRAND_FALLBACK
                    .iter()
                    .find(|(s, _)| *s == place.spider);
                match fallback {
                    Some((_, brand)) if tag("brand").is_none() => {
                        LockerBrand::from_tags(brand.iter().copied())
                    }
                    _ => LockerBrand::from_tags(tags()),
                }
            }),
            counts_for_coverage: !(kind == Kind::ParcelLocker
                && PARTIAL_LOCKER_SPIDERS.contains(&place.spider.as_str())),
        })
    }

    /// Whether `candidate`'s `ref` is our station number, for
    /// bike-share stations.
    fn same_code(&self, candidate: &OsmCandidate) -> bool {
        self.station_code.as_ref().is_some_and(|code| {
            candidate
                .tags()
                .find(|&(k, _)| k == "ref")
                .is_some_and(|(_, v)| normalize_code(v) == *code)
        })
    }

    /// Whether `candidate`'s name shares a word with ours, for
    /// bike-share stations.
    fn same_name(&self, candidate: &OsmCandidate) -> bool {
        !self.name_words.is_empty()
            && candidate
                .tags()
                .find(|&(k, _)| k == "name")
                .is_some_and(|(_, v)| name_words(v).iter().any(|w| self.name_words.contains(w)))
    }

    /// For `candidate`, the score at zero distance and the minimum
    /// score within the radius; `(0.0, 0.0)` for no match.
    fn factor(&self, candidate: &OsmCandidate) -> (f64, f64) {
        if Kind::from_tags(candidate.tags()) != Some(self.kind) {
            return (0.0, 0.0);
        }
        let tag = |key: &str| candidate.tags().find(|&(k, _)| k == key).map(|(_, v)| v);
        if self.kind.operator_must_agree() && !self.operators.is_empty() {
            let mut osm = tag("operator:wikidata")
                .into_iter()
                .flat_map(parse_wikidata_ids)
                .peekable();
            if osm.peek().is_some() && !osm.any(|id| self.operators.contains(&id)) {
                return (0.0, 0.0);
            }
        }
        // The same GBFS ID outweighs a different brand, since OSM may
        // still have a station's former brand.
        match self.gbfs_match(candidate) {
            Some(true) => return (1.0, SAME_REF_MIN_SCORE),
            Some(false) => return (0.0, 0.0),
            None => {}
        }
        if !self.brands.is_empty() {
            let mut osm = tag("brand:wikidata")
                .into_iter()
                .flat_map(parse_wikidata_ids)
                .peekable();
            if osm.peek().is_some() && !osm.any(|id| self.brands.contains(&id)) {
                return (0.0, 0.0);
            }
        }
        if !self.brand_names.is_empty()
            && !brand_names_agree(&self.brand_names, &brand_names(candidate.tags()))
        {
            return (0.0, 0.0);
        }
        if let Some(atp) = &self.locker_brand
            && !atp.agrees(&LockerBrand::from_tags(candidate.tags()))
        {
            return (0.0, 0.0);
        }
        if self.same_ref(candidate) {
            return (1.0, SAME_REF_MIN_SCORE);
        }
        let mut factor = KIND_FACTOR;
        for (key, atp) in &self.confirming {
            if tag(key).is_some_and(|osm| normalize_value(osm) == *atp) {
                factor += CONFIRM_BONUS;
            }
        }
        if self.same_name(candidate) {
            factor += CONFIRM_BONUS;
        }
        let osm_ref = tag("ref");
        if let (Some(street), Some(osm)) = (&self.street, osm_ref)
            && normalize_name(osm).contains(street.as_str())
        {
            factor += CONFIRM_BONUS;
        }
        (factor, 0.0)
    }
}

impl Matcher for InfrastructureMatcher {
    /// `ref` when the references agree, else `kind`: the same kind of
    /// thing close by.
    fn rule(&self, candidate: &OsmCandidate) -> &'static str {
        if self.same_ref(candidate) {
            "ref"
        } else {
            "kind"
        }
    }

    fn score_osm_candidate(&self, candidate: &OsmCandidate) -> f64 {
        let (best, worst) = self.factor(candidate);
        if best == 0.0 {
            return 0.0;
        }
        let Result::Ok(geometry) = candidate.geometry() else {
            return 0.0;
        };
        let Some(centroid) = geometry.centroid() else {
            return 0.0;
        };
        let max_meters = if self.gbfs_match(candidate) == Some(true) || self.same_code(candidate) {
            GBFS_ID_MAX_METERS
        } else if self.same_name(candidate) {
            BIKE_SHARE_NAME_MAX_METERS
        } else if self.kind == Kind::ParcelLocker && self.same_ref(candidate) {
            PARCEL_LOCKER_REF_MAX_METERS
        } else {
            self.settings.max_meters
        };
        if geo_point_distance(&self.center, &centroid) > max_meters {
            return 0.0;
        }
        worst + (best - worst) * geo_point_distance_score(&self.center, &centroid, max_meters)
    }

    /// Two post boxes or benches in a feed are two real things, so they
    /// shouldn't both take the same OSM feature.
    fn is_exclusive(&self) -> bool {
        true
    }

    /// Bicycle parking that OSM maps as one feature with a larger
    /// `capacity` may take several ATP features. Cambridge (US) lists
    /// each rack of two on its own, with 711 of its 2,684 racks sharing
    /// a position, where OSM has one node with `capacity=6`.
    fn may_share(&self, candidate: &OsmCandidate) -> bool {
        let Some(atp) = self.capacity else {
            return false;
        };
        candidate
            .tags()
            .find(|&(k, _)| k == "capacity")
            .and_then(|(_, v)| parse_capacity(v))
            .is_some_and(|osm| osm > atp)
    }

    /// False for the parcel lockers of [PARTIAL_LOCKER_SPIDERS].
    fn counts_for_coverage(&self) -> bool {
        self.counts_for_coverage
    }

    fn max_distance(&self) -> Option<ChordAngle> {
        let meters = if self.gbfs_id.is_some() || self.station_code.is_some() {
            self.settings.max_meters.max(GBFS_ID_MAX_METERS)
        } else if !self.name_words.is_empty() {
            self.settings.max_meters.max(BIKE_SHARE_NAME_MAX_METERS)
        } else if self.kind == Kind::ParcelLocker && self.reference.is_some() {
            self.settings.max_meters.max(PARCEL_LOCKER_REF_MAX_METERS)
        } else {
            self.settings.max_meters
        };
        Some(meters_to_chord_angle(meters))
    }
}

/// The brand of a parcel locker: its `brand:wikidata` and
/// `operator:wikidata`, and the [locker_brand_words] of its `brand`. The operator only counts for a
/// locker without brand, since it says little otherwise: in
/// Mazowieckie, OSM gives 283 Allegro lockers `operator=UPS`.
#[derive(Clone, Debug, Default, PartialEq)]
struct LockerBrand {
    ids: Vec<u64>,
    words: Vec<String>,
}

impl LockerBrand {
    fn from_tags<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> LockerBrand {
        let mut brand = LockerBrand::default();
        let mut operator = Vec::new();
        for (key, value) in tags {
            match key {
                "brand:wikidata" | "operator:wikidata" => {
                    brand.ids.extend(parse_wikidata_ids(value))
                }
                "brand" => brand.words.extend(locker_brand_words(value)),
                "operator" => operator.extend(locker_brand_words(value)),
                _ => {}
            }
        }
        // Without a brand, the operator stands in, so that an OSM
        // locker with just `operator=Amazon` is no PUDO station.
        if brand.ids.is_empty() && brand.words.is_empty() {
            brand.words = operator;
        }
        brand
    }

    /// Whether two lockers may be of the same network: they share a
    /// Wikidata ID or a brand word, or have nothing to compare. Brand
    /// and operator IDs count alike, since ATP gives PUDO lockers the
    /// operator's ID as `brand:wikidata`, while OSM has the brand's.
    /// A word is enough, since OSM has both `Amazon Locker` and
    /// `Amazon Hub`, each with a Wikidata ID of its own.
    fn agrees(&self, other: &LockerBrand) -> bool {
        let shares = |a: &[u64], b: &[u64]| a.iter().any(|x| b.contains(x));
        let comparable = (!self.ids.is_empty() && !other.ids.is_empty())
            || (!self.words.is_empty() && !other.words.is_empty());
        !comparable
            || shares(&self.ids, &other.ids)
            || self.words.iter().any(|w| other.words.contains(w))
    }
}

/// The words of a locker brand for [LockerBrand]: lowercase, of at
/// least three letters, and not in [GENERIC_LOCKER_WORDS]; or if none
/// is left, the whole brand, so that `Z-Box` gives `zbox`.
fn locker_brand_words(value: &str) -> Vec<String> {
    let words: Vec<String> = value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|w| !GENERIC_LOCKER_WORDS.contains(&w.as_str()))
        .collect();
    if !words.is_empty() {
        return words;
    }
    let whole: String = value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    if whole.is_empty() {
        Vec::new()
    } else {
        vec![whole]
    }
}

fn parse_capacity(value: &str) -> Option<u32> {
    value.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    //! Most tests are rows in [CASES]: one ATP feature, one OSM
    //! candidate, their distance, and the expected score range.
    use super::*;
    use crate::matchers::MatchMask;
    use crate::tables::{Feature, StringPool};
    use crate::utils::UtcTimestamp;
    use tempfile::TempDir;

    const LAT: f64 = 47.3782;
    const LON: f64 = 8.5402;

    fn place(spider: &str, tags: &[(&str, &str)]) -> Place {
        let mut mask = MatchMask::default();
        for (k, v) in tags {
            mask.add_tag(k, v);
        }
        let coord = geo::Coord { x: LON, y: LAT };
        Place::new(
            &coord,
            String::from(spider),
            mask,
            tags.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            &geo::Geometry::from(geo::Point::from(coord)),
            UtcTimestamp(time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap()),
        )
        .expect("valid place")
    }

    /// The score for an ATP feature of `spider` with `atp` tags against
    /// an OSM feature with `osm` tags, `meters` north of it; `None` if
    /// the ATP feature gets no [InfrastructureMatcher].
    fn score(spider: &str, atp: &[(&str, &str)], osm: &[(&str, &str)], meters: f64) -> Option<f64> {
        let matcher = InfrastructureMatcher::for_place(&place(spider, atp))?;
        Some(with_candidate_at(osm, meters, |c| {
            matcher.score_osm_candidate(c)
        }))
    }

    /// Calls `f` with an OSM candidate with `osm` tags, at the ATP
    /// feature's position.
    fn with_candidate<T>(osm: &[(&str, &str)], f: impl FnOnce(&OsmCandidate) -> T) -> T {
        with_candidate_at(osm, 0.0, f)
    }

    /// Calls `f` with an OSM candidate with `osm` tags, `meters` north
    /// of the ATP feature.
    fn with_candidate_at<T>(
        osm: &[(&str, &str)],
        meters: f64,
        f: impl FnOnce(&OsmCandidate) -> T,
    ) -> T {
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
        f(&OsmCandidate {
            feature: &feature,
            strings: &pool,
        })
    }

    struct Case {
        what: &'static str,
        spider: &'static str,
        atp: &'static [(&'static str, &'static str)],
        osm: &'static [(&'static str, &'static str)],
        meters: f64,
        /// Expected score, inclusive range.
        want: (f64, f64),
    }

    const NO_MATCH: (f64, f64) = (0.0, 0.0);

    const HELLO_CYCLING: &[(&str, &str)] = &[
        ("amenity", "bicycle_rental"),
        ("ref", "hellocycling:1234"),
        ("ref:gbfs", "hellocycling:1234"),
        ("ref:gbfs:hellocycling", "1234"),
        ("brand", "HELLO CYCLING"),
        ("brand:wikidata", "Q91231927"),
        ("capacity", "7"),
    ];

    // ATP features, as the spiders emit them (shortened).
    const SWISS_POST_BOX: &[(&str, &str)] = &[
        ("amenity", "post_box"),
        ("ref", "003BE_00335986"),
        ("collection_times", "Mo-Fr 17:30; Sa 08:00"),
        ("operator", "Die Post"),
        ("operator:wikidata", "Q614803"),
        ("addr:street_address", "Via alla Roggia 31"),
    ];
    const DEUTSCHE_POST_BOX: &[(&str, &str)] = &[
        ("amenity", "post_box"),
        ("ref", "6-209474"),
        ("collection_times", "Mo-Sa 09:30"),
        ("addr:street", "Lütticher Str."),
        ("brand", "Deutsche Post"),
        ("brand:wikidata", "Q157645"),
    ];
    const JAPAN_POST_BOX: &[(&str, &str)] = &[
        ("amenity", "post_box"),
        ("ref", "007153"),
        ("collection_times", "Mo-Fr 10:00; Sa 09:10; Su,PH 09:10"),
        ("operator", "日本郵便"),
        ("operator:wikidata", "Q11509260"),
    ];
    const STEIGER_AED: &[(&str, &str)] = &[
        ("emergency", "defibrillator"),
        ("ref", "AEDR 1786"),
        ("name", "Oberrhein Gymnasium - Foyer EG"),
        ("operator", "Steiger-Stiftung"),
        ("operator:wikidata", "Q879521"),
    ];

    const ZURICH_BENCH: &[(&str, &str)] = &[
        ("amenity", "bench"),
        ("ref", "B-4711"),
        ("backrest", "yes"),
        ("operator", "Grün Stadt Zürich"),
    ];
    const SEATTLE_BENCH: &[(&str, &str)] = &[("amenity", "bench"), ("ref", "BEN-1234")];

    const BARWON_HYDRANT: &[(&str, &str)] = &[
        ("emergency", "fire_hydrant"),
        ("ref", "1098992"),
        ("fire_hydrant:type", "underground"),
        ("operator", "Barwon Water"),
    ];
    const LITTLE_FREE_LIBRARY: &[(&str, &str)] = &[
        ("amenity", "public_bookcase"),
        ("ref", "51931"),
        ("name", "Paul Downey #51931 Kalispell MT"),
        ("brand", "Little Free Library"),
        ("brand:wikidata", "Q6650101"),
    ];
    const SF_BICYCLE_PARKING: &[(&str, &str)] = &[
        ("amenity", "bicycle_parking"),
        ("ref", "8505"),
        ("capacity", "2"),
        ("bicycle_parking", "rack"),
    ];

    const ZURICH_FOUNTAIN: &[(&str, &str)] = &[
        ("amenity", "fountain"),
        ("ref", "wvz-22"),
        ("drinking_water", "yes"),
        ("trough:material", "sandstone"),
    ];

    const CHARGE_POINT: &[(&str, &str)] = &[
        ("man_made", "charge_point"),
        ("ref", "15997299"),
        ("capacity", "1"),
        ("brand", "ChargePoint"),
        ("brand:wikidata", "Q5176149"),
        ("operator:wikidata", "Q5176149"),
    ];

    const PACZKOMAT: &[(&str, &str)] = &[
        ("amenity", "parcel_locker"),
        ("ref", "WAW211AP"),
        ("brand", "Paczkomat InPost"),
        ("brand:wikidata", "Q110970254"),
        ("operator", "InPost"),
        ("operator:wikidata", "Q3182097"),
    ];
    const PACKSTATION: &[(&str, &str)] = &[
        ("amenity", "parcel_locker"),
        ("ref", "1-14482182"),
        ("operator", "Deutsche Post"),
        ("operator:wikidata", "Q157645"),
    ];
    const Z_BOX: &[(&str, &str)] = &[
        ("amenity", "parcel_locker"),
        ("ref", "I_23111"),
        ("brand", "Z-Box"),
        ("brand:wikidata", "Q121537464"),
        ("operator", "Zásilkovna"),
    ];
    const AMAZON_LOCKER: &[(&str, &str)] = &[
        ("amenity", "parcel_locker"),
        ("ref", "54243621"),
        ("brand", "Amazon Locker"),
        ("brand:wikidata", "Q16974764"),
    ];

    const CASES: &[Case] = &[
        Case {
            what: "Swiss post box, same spot",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box"), ("operator:wikidata", "Q614803")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "Swiss post box 60 m away still matches",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box")],
            meters: 60.0,
            want: (0.3, 0.33),
        },
        Case {
            what: "Swiss post box beyond 100 m",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box")],
            meters: 110.0,
            want: NO_MATCH,
        },
        Case {
            what: "Swiss refs say nothing",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "003BE_00335986")],
            meters: 10.0,
            want: (0.72, 0.72),
        },
        Case {
            what: "equal collection times raise the score",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[
                ("amenity", "post_box"),
                ("collection_times", "Mo-Fr 17:30;Sa 08:00"),
            ],
            meters: 0.0,
            want: (0.84, 0.84),
        },
        Case {
            what: "different collection times don't lower it",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box"), ("collection_times", "Mo-Fr 10:00")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "another operator",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box"), ("operator:wikidata", "Q157645")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "a waste basket is not a post box",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "waste_basket")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "German OSM ref describing the location",
            spider: "deutsche_post_de",
            atp: DEUTSCHE_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "Lütticher Str. 76")],
            meters: 0.0,
            want: (0.84, 0.84),
        },
        Case {
            what: "Japan Post ref, ignoring leading zeros",
            spider: "japan_post_jp",
            atp: JAPAN_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "7153")],
            meters: 10.0,
            want: (0.95, 0.95),
        },
        Case {
            what: "Japan Post ref beats a closer box",
            spider: "japan_post_jp",
            atp: JAPAN_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "007154")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "Japan Post box beyond 20 m, even with the same ref",
            spider: "japan_post_jp",
            atp: JAPAN_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "007153")],
            meters: 25.0,
            want: NO_MATCH,
        },
        Case {
            what: "Steiger AED with the number as OSM ref",
            spider: "steiger_stiftung_aed",
            atp: STEIGER_AED,
            osm: &[
                ("emergency", "defibrillator"),
                ("ref", "1786"),
                ("operator", "Björn Steiger Stiftung"),
            ],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "AED operated by someone else in OSM",
            spider: "steiger_stiftung_aed",
            atp: STEIGER_AED,
            osm: &[
                ("emergency", "defibrillator"),
                ("operator", "Stadt Heidelberg"),
                ("operator:wikidata", "Q2557"),
            ],
            meters: 10.0,
            want: (0.64, 0.64),
        },
        Case {
            what: "AED tagged on a shop",
            spider: "aed_jp",
            atp: &[("emergency", "defibrillator"), ("ref", "22285")],
            osm: &[
                ("shop", "convenience"),
                ("emergency", "defibrillator"),
                ("ref", "22285"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "an AED is not a post box",
            spider: "steiger_stiftung_aed",
            atp: STEIGER_AED,
            osm: &[("amenity", "post_box"), ("ref", "1786")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "Zürich bench, same spot, same backrest",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_BENCH,
            osm: &[("amenity", "bench"), ("backrest", "yes")],
            meters: 0.0,
            want: (0.84, 0.84),
        },
        Case {
            what: "Zürich bench, different backrest doesn't lower the score",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_BENCH,
            osm: &[("amenity", "bench"), ("backrest", "no")],
            meters: 5.0,
            want: (0.4, 0.4),
        },
        Case {
            what: "Zürich bench beyond 10 m",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_BENCH,
            osm: &[("amenity", "bench")],
            meters: 12.0,
            want: NO_MATCH,
        },
        Case {
            what: "Seattle bench with its asset ID in OSM",
            spider: "seattle_parks_and_recreation_benches_us",
            atp: SEATTLE_BENCH,
            osm: &[("amenity", "bench"), ("ref:US-WA:DPR:AMWO_ID", "BEN-1234")],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "Seattle asset ID under plain ref doesn't count",
            spider: "seattle_parks_and_recreation_benches_us",
            atp: SEATTLE_BENCH,
            osm: &[("amenity", "bench"), ("ref", "BEN-1234")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "default radius for an unknown bench spider",
            spider: "some_bench_spider",
            atp: &[("amenity", "bench")],
            osm: &[("amenity", "bench")],
            meters: 16.0,
            want: NO_MATCH,
        },
        Case {
            what: "a bench is not a picnic table",
            spider: "some_bench_spider",
            atp: &[("amenity", "bench")],
            osm: &[("leisure", "picnic_table")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "hydrant of the same type",
            spider: "barwon_water_potable_fire_hydrants_au",
            atp: BARWON_HYDRANT,
            osm: &[
                ("emergency", "fire_hydrant"),
                ("fire_hydrant:type", "underground"),
            ],
            meters: 0.0,
            want: (0.84, 0.84),
        },
        Case {
            what: "hydrant beyond 10 m",
            spider: "barwon_water_potable_fire_hydrants_au",
            atp: BARWON_HYDRANT,
            osm: &[("emergency", "fire_hydrant")],
            meters: 12.0,
            want: NO_MATCH,
        },
        Case {
            what: "EPCOR hydrant with the same ref",
            spider: "epcor_fire_hydrants_ca",
            atp: &[("emergency", "fire_hydrant"), ("ref", "27772")],
            osm: &[("emergency", "fire_hydrant"), ("ref", "27772")],
            meters: 5.0,
            want: (0.95, 0.95),
        },
        Case {
            what: "Little Free Library 40 m away, same charter number",
            spider: "little_free_library",
            atp: LITTLE_FREE_LIBRARY,
            osm: &[("amenity", "public_bookcase"), ("ref", "51931")],
            meters: 40.0,
            want: (0.92, 0.92),
        },
        Case {
            what: "Little Free Library 40 m away, no ref",
            spider: "little_free_library",
            atp: LITTLE_FREE_LIBRARY,
            osm: &[("amenity", "public_bookcase")],
            meters: 40.0,
            want: (0.16, 0.16),
        },
        Case {
            what: "default radius for an unknown bookcase spider",
            spider: "some_bookcase_spider",
            atp: &[("amenity", "public_bookcase")],
            osm: &[("amenity", "public_bookcase")],
            meters: 35.0,
            want: NO_MATCH,
        },
        Case {
            what: "bicycle parking with the same capacity",
            spider: "san_francisco_mta_bicycle_parking_us",
            atp: SF_BICYCLE_PARKING,
            osm: &[("amenity", "bicycle_parking"), ("capacity", "2")],
            meters: 10.0,
            want: (0.42, 0.42),
        },
        Case {
            what: "bicycle parking with another type still matches",
            spider: "san_francisco_mta_bicycle_parking_us",
            atp: SF_BICYCLE_PARKING,
            osm: &[
                ("amenity", "bicycle_parking"),
                ("bicycle_parking", "stands"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "Zürich fountain tagged as drinking water in OSM",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_FOUNTAIN,
            osm: &[("amenity", "drinking_water")],
            meters: 5.0,
            want: (0.4, 0.4),
        },
        Case {
            what: "Zürich fountain with its number as OSM ref",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_FOUNTAIN,
            osm: &[("amenity", "drinking_water"), ("ref", "22")],
            meters: 5.0,
            want: (0.95, 0.95),
        },
        Case {
            what: "Zürich fountain beyond 10 m, even with the same ref",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_FOUNTAIN,
            osm: &[("amenity", "fountain"), ("ref", "22")],
            meters: 12.0,
            want: NO_MATCH,
        },
        Case {
            what: "a fountain is not a waste basket",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_FOUNTAIN,
            osm: &[("amenity", "waste_basket")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "Copenhagen waste basket beyond 10 m",
            spider: "copenhagen_municipality_waste_baskets_dk",
            atp: &[("amenity", "waste_basket"), ("waste", "trash")],
            osm: &[("amenity", "waste_basket")],
            meters: 11.0,
            want: NO_MATCH,
        },
        Case {
            what: "waste basket, default radius",
            spider: "some_waste_basket_spider",
            atp: &[("amenity", "waste_basket")],
            osm: &[("amenity", "waste_basket")],
            meters: 7.5,
            want: (0.4, 0.4),
        },
        Case {
            what: "Changing Places toilet inside a leisure centre",
            spider: "changing_places_gb",
            atp: &[
                ("amenity", "toilets"),
                ("brand", "Changing Places"),
                ("brand:wikidata", "Q104870811"),
            ],
            osm: &[("amenity", "toilets"), ("wheelchair", "yes")],
            meters: 75.0,
            want: (0.2, 0.2),
        },
        Case {
            what: "toilets, default radius",
            spider: "some_toilets_spider",
            atp: &[("amenity", "toilets")],
            osm: &[("amenity", "toilets")],
            meters: 35.0,
            want: NO_MATCH,
        },
        Case {
            what: "bike share, same GBFS ID as gbfs:system_id and gbfs:station_id",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[
                ("amenity", "bicycle_rental"),
                ("gbfs:system_id", "hellocycling"),
                ("gbfs:station_id", "1234"),
            ],
            meters: 150.0,
            want: (0.9, 0.93),
        },
        Case {
            what: "bike share, same GBFS ID as ref:gbfs",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "HelloCycling:1234"),
            ],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "bike share, same GBFS ID as ref:gbfs:<system>",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs:hellocycling", "1234"),
            ],
            meters: 10.0,
            want: (0.99, 1.0),
        },
        Case {
            what: "bike share, same GBFS ID beats a different brand",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[
                ("amenity", "bicycle_rental"),
                ("brand:wikidata", "Q55533296"),
                ("ref:gbfs", "hellocycling:1234"),
            ],
            meters: 10.0,
            want: (0.99, 1.0),
        },
        Case {
            what: "bike share, same GBFS ID too far away",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "hellocycling:1234"),
            ],
            meters: 250.0,
            want: NO_MATCH,
        },
        Case {
            what: "bike share, another station of the same system",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "hellocycling:999"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "bike share, ID of another system names another brand",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "docomo-cycle:1234"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "bike share, ID of a renamed system",
            spider: "gbfs",
            atp: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "docomo-cycle-tokyo:1131"),
                ("brand", "Docomo Bike Share"),
            ],
            osm: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "docomo-cycle:1131"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "bike share without IDs, close by",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[
                ("amenity", "bicycle_rental"),
                ("brand:wikidata", "Q91231927"),
            ],
            meters: 10.0,
            want: (0.4, 0.4),
        },
        Case {
            what: "bike share without IDs, same name word at 35 m",
            spider: "gbfs",
            atp: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "nextbike_ch:348292"),
                ("brand", "Nextbike"),
                ("name", "Mariazell"),
            ],
            osm: &[
                ("amenity", "bicycle_rental"),
                ("operator", "nextbike"),
                ("name", "Mariazell"),
            ],
            meters: 35.0,
            want: (0.25, 0.26),
        },
        Case {
            what: "bike share, same station number at 90 m",
            spider: "gbfs",
            atp: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "bluebikes:f8348719-0de8-11e7-991c-3863bb43a7d0"),
                ("brand", "Bluebikes"),
                ("short_name", "B32000"),
            ],
            osm: &[
                ("amenity", "bicycle_rental"),
                ("brand", "Bluebikes"),
                ("ref", "B32000"),
            ],
            meters: 90.0,
            want: (0.94, 0.96),
        },
        Case {
            what: "bike share, other station number at 90 m",
            spider: "gbfs",
            atp: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "bluebikes:f8348719-0de8-11e7-991c-3863bb43a7d0"),
                ("short_name", "B32000"),
            ],
            osm: &[("amenity", "bicycle_rental"), ("ref", "B32005")],
            meters: 90.0,
            want: NO_MATCH,
        },
        Case {
            what: "bike share without IDs, other name at 35 m",
            spider: "gbfs",
            atp: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "nextbike_ch:348292"),
                ("name", "Mariazell"),
            ],
            osm: &[("amenity", "bicycle_rental"), ("name", "Bahnhof")],
            meters: 35.0,
            want: NO_MATCH,
        },
        Case {
            what: "bike share without IDs, beyond 20 m",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[("amenity", "bicycle_rental")],
            meters: 25.0,
            want: NO_MATCH,
        },
        Case {
            what: "bike share of another brand",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[
                ("amenity", "bicycle_rental"),
                ("brand:wikidata", "Q55533296"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "bike share, brand names differ",
            spider: "gbfs",
            atp: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "voi_ch:VOI:Station:af32f6ac"),
                ("brand", "Voi Switzerland"),
                ("network", "Voi Switzerland"),
            ],
            osm: &[
                ("amenity", "bicycle_rental"),
                ("brand", "PubliBike"),
                ("operator", "PubliBike"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "bike share, brand name within the OSM network",
            spider: "gbfs",
            atp: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "nextbike_bn:3167232"),
                ("brand", "Nextbike"),
            ],
            osm: &[
                ("amenity", "bicycle_rental"),
                ("network", "Deezer nextbike"),
                ("operator", "nextbike GmbH"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "bike share, aggregator feed names the operator in its system ID",
            spider: "gbfs",
            atp: &[
                ("amenity", "bicycle_rental"),
                ("ref:gbfs", "sharedmobility.ch:velospot:1186"),
                ("brand", "sharedmobility.ch"),
                ("operator", "Swiss Federal Office of Energy"),
            ],
            osm: &[
                ("amenity", "bicycle_rental"),
                ("brand", "Velospot"),
                ("operator", "PubliBike"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "bike share, OSM without brand names",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[("amenity", "bicycle_rental"), ("name", "Jelbi-Station")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "bike share station against a rental shop",
            spider: "gbfs",
            atp: HELLO_CYCLING,
            osm: &[("amenity", "bicycle_rental"), ("bicycle_rental", "shop")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "charge point, same spot",
            spider: "chargepoint",
            atp: CHARGE_POINT,
            osm: &[("man_made", "charge_point")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "charge point, another operator",
            spider: "chargepoint",
            atp: CHARGE_POINT,
            osm: &[
                ("man_made", "charge_point"),
                ("operator:wikidata", "Q42717773"),
            ],
            meters: 5.0,
            want: (0.5, 0.6),
        },
        Case {
            what: "charge point beyond 15 m",
            spider: "chargepoint",
            atp: CHARGE_POINT,
            osm: &[("man_made", "charge_point")],
            meters: 20.0,
            want: NO_MATCH,
        },
        Case {
            what: "charge point against its charging station",
            spider: "chargepoint",
            atp: CHARGE_POINT,
            osm: &[("amenity", "charging_station")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "parcel locker, same ref",
            spider: "paczkomat_inpost_pl",
            atp: PACZKOMAT,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "Paczkomat InPost"),
                ("brand:wikidata", "Q110970254"),
                ("ref", "WAW211AP"),
            ],
            meters: 50.0,
            want: (0.94, 0.96),
        },
        Case {
            what: "parcel locker, same ref beyond 100 m",
            spider: "paczkomat_inpost_pl",
            atp: PACZKOMAT,
            osm: &[("amenity", "parcel_locker"), ("ref", "WAW211AP")],
            meters: 110.0,
            want: NO_MATCH,
        },
        Case {
            what: "parcel locker, another ref",
            spider: "paczkomat_inpost_pl",
            atp: PACZKOMAT,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "Paczkomat InPost"),
                ("ref", "WAW211M"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "parcel locker without ref beyond 30 m",
            spider: "paczkomat_inpost_pl",
            atp: PACZKOMAT,
            osm: &[("amenity", "parcel_locker"), ("brand", "Paczkomat InPost")],
            meters: 35.0,
            want: NO_MATCH,
        },
        Case {
            what: "parcel locker of another network next to it",
            spider: "paczkomat_inpost_pl",
            atp: PACZKOMAT,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "Orlen Paczka"),
                ("brand:wikidata", "Q110457879"),
                ("operator", "Orlen"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "Appkomat shares the InPost brand word",
            spider: "paczkomat_inpost_pl",
            atp: PACZKOMAT,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "Appkomat InPost"),
                ("brand:wikidata", "Q110970263"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "parcel locker without brand in OSM",
            spider: "paczkomat_inpost_pl",
            atp: PACZKOMAT,
            osm: &[("amenity", "parcel_locker"), ("ref", "WAW211M")],
            meters: 15.0,
            want: (0.4, 0.4),
        },
        Case {
            what: "OSM locker of another operator, without brand",
            spider: "paczkomat_inpost_pl",
            atp: PACZKOMAT,
            osm: &[("amenity", "parcel_locker"), ("operator", "UPS")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "Packstation number at the end of Deutsche Post's ref",
            spider: "deutsche_post_de",
            atp: PACKSTATION,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "DHL Packstation"),
                ("brand:wikidata", "Q1766703"),
                ("operator", "DHL"),
                ("ref", "182"),
            ],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "Packstation next to an Amazon Locker",
            spider: "deutsche_post_de",
            atp: PACKSTATION,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "Amazon Locker"),
                ("ref", "filomena"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "Packstation that OSM tags with its operator only",
            spider: "deutsche_post_de",
            atp: PACKSTATION,
            osm: &[("amenity", "parcel_locker"), ("operator", "DHL")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "a DHL Paketbox is a Packstation too",
            spider: "deutsche_post_de",
            atp: PACKSTATION,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "Paketbox"),
                ("brand:wikidata", "Q2046604"),
                ("operator:wikidata", "Q489815"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "PUDO Station of the operator that ATP names as brand",
            spider: "pudostation_jp",
            atp: &[
                ("amenity", "parcel_locker"),
                ("ref", "PCJ00528"),
                ("brand", "プドー"),
                ("brand:wikidata", "Q86738066"),
            ],
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "PUDOステーション"),
                ("operator:wikidata", "Q86738066"),
                ("ref", "00528"),
            ],
            meters: 15.0,
            want: (0.98, 0.99),
        },
        Case {
            what: "Packstation that OSM numbers otherwise",
            spider: "deutsche_post_de",
            atp: PACKSTATION,
            osm: &[("amenity", "parcel_locker"), ("ref", "1-14482182")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "Z-Box by ref:packeta",
            spider: "packeta",
            atp: Z_BOX,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "Z-Box"),
                ("brand:wikidata", "Q121537464"),
                ("ref:packeta", "23111"),
            ],
            meters: 60.0,
            want: (0.93, 0.95),
        },
        Case {
            what: "Z-Box next to an AlzaBox",
            spider: "packeta",
            atp: Z_BOX,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "AlzaBox"),
                ("brand:wikidata", "Q115254158"),
            ],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "Amazon Locker and Amazon Hub share a word",
            spider: "amazon_locker_us",
            atp: AMAZON_LOCKER,
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "Amazon Hub"),
                ("brand:wikidata", "Q100274430"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "Amazon Locker nickname as OSM ref",
            spider: "amazon_locker_at_cz_de",
            atp: &[
                ("amenity", "parcel_locker"),
                ("ref", "43651462"),
                ("name", "Amazon Locker - Nilias"),
                ("brand", "Amazon Locker"),
            ],
            osm: &[
                ("amenity", "parcel_locker"),
                ("brand", "Amazon Hub"),
                ("ref", "nilias"),
            ],
            meters: 40.0,
            want: (0.95, 0.96),
        },
        Case {
            what: "PUDO ref with or without its prefix",
            spider: "pudostation_jp",
            atp: &[
                ("amenity", "parcel_locker"),
                ("ref", "PCJ00177"),
                ("brand", "プドー"),
            ],
            osm: &[("amenity", "parcel_locker"), ("ref", "PCJ177")],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "a parcel locker is not a vending machine",
            spider: "amazon_locker_us",
            atp: AMAZON_LOCKER,
            osm: &[("amenity", "vending_machine"), ("brand", "Amazon Locker")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "default radius for an unknown AED spider",
            spider: "some_aed_spider",
            atp: &[("emergency", "defibrillator")],
            osm: &[("emergency", "defibrillator")],
            meters: 35.0,
            want: NO_MATCH,
        },
    ];

    #[test]
    fn test_cases() {
        for case in CASES {
            let got = score(case.spider, case.atp, case.osm, case.meters)
                .unwrap_or_else(|| panic!("{}: no matcher", case.what));
            let (lo, hi) = case.want;
            assert!(
                got >= lo - 1e-3 && got <= hi + 1e-3,
                "{}: got {got:.4}, want {lo}..={hi}",
                case.what
            );
        }
    }

    #[test]
    fn test_no_matcher_for_other_kinds() {
        assert!(
            InfrastructureMatcher::for_place(&place("x", &[("amenity", "recycling")])).is_none()
        );
        assert!(InfrastructureMatcher::for_place(&place("x", &[("shop", "bakery")])).is_none());
    }

    #[test]
    fn test_max_distance() {
        let matcher =
            InfrastructureMatcher::for_place(&place("deutsche_post_de", DEUTSCHE_POST_BOX))
                .unwrap();
        assert!(matcher.max_distance() == Some(meters_to_chord_angle(100.0)));
        assert!(matcher.is_exclusive());
    }

    #[test]
    fn test_parcel_locker_matcher() {
        let matcher = |spider, atp| InfrastructureMatcher::for_place(&place(spider, atp));
        let paczkomat = matcher("paczkomat_inpost_pl", PACZKOMAT).unwrap();
        assert!(paczkomat.max_distance() == Some(meters_to_chord_angle(100.0)));
        assert!(paczkomat.counts_for_coverage());
        let dhl = &[
            ("amenity", "parcel_locker"),
            ("ref", "234972"),
            ("brand", "DHL BOX 24/7"),
        ];
        assert!(
            matcher("dhl_pl", dhl).unwrap().max_distance() == Some(meters_to_chord_angle(30.0))
        );
        let allegro = &[
            ("amenity", "parcel_locker"),
            ("ref", "AL010ZST"),
            ("brand", "Allegro One Box"),
        ];
        assert!(
            !matcher("allegro_one_box_pl", allegro)
                .unwrap()
                .counts_for_coverage()
        );
        // `czech_post_cz` doesn't say whose locker it is.
        let unbranded = &[
            ("amenity", "parcel_locker"),
            ("ref", "6"),
            ("name", "Praha 10 AlzaBox Šrobárova"),
        ];
        assert!(matcher("czech_post_cz", unbranded).is_none());
    }

    #[test]
    fn test_locker_brand() {
        let brand = |tags: &[(&str, &str)]| LockerBrand::from_tags(tags.iter().copied());
        let dhl = brand(&[("brand", "DHL BOX 24/7")]);
        assert_eq!(dhl.words, vec!["dhl"]);
        assert!(dhl.agrees(&brand(&[("brand", "DHL Packstation")])));
        assert!(!dhl.agrees(&brand(&[("brand", "DPD Pickup Station")])));
        assert!(!dhl.agrees(&brand(&[("operator", "DPD")])));
        assert!(dhl.agrees(&brand(&[("operator", "DHL")])));
        assert!(dhl.agrees(&brand(&[("brand", "DHL Packstation"), ("operator", "UPS")])));
        assert!(dhl.agrees(&LockerBrand::default()));
        let z_box = brand(&[("brand", "Z-Box"), ("brand:wikidata", "Q121537464")]);
        assert_eq!(z_box.words, vec!["zbox"]);
        assert!(z_box.agrees(&brand(&[("brand", "Z-BOX")])));
        assert!(z_box.agrees(&brand(&[("brand:wikidata", "Q121537464")])));
        assert!(!z_box.agrees(&brand(&[("brand", "AlzaBox")])));
        assert!(!z_box.agrees(&brand(&[
            ("brand", "AlzaBox"),
            ("brand:wikidata", "Q115254158")
        ])));
        let amazon = brand(&[("brand", "Amazon Locker"), ("brand:wikidata", "Q16974764")]);
        assert!(amazon.agrees(&brand(&[
            ("brand", "Amazon Hub"),
            ("brand:wikidata", "Q100274430")
        ])));
    }

    #[test]
    fn test_may_share() {
        let may_share = |atp: &[(&str, &str)], osm: &[(&str, &str)]| {
            with_candidate(osm, |candidate| {
                InfrastructureMatcher::for_place(&place("x", atp))
                    .unwrap()
                    .may_share(candidate)
            })
        };
        let rack = &[("amenity", "bicycle_parking"), ("capacity", "2")];
        let six = &[("amenity", "bicycle_parking"), ("capacity", "6")];
        assert!(may_share(rack, six));
        assert!(!may_share(rack, rack));
        assert!(!may_share(six, rack));
        assert!(!may_share(&[("amenity", "bicycle_parking")], six));
        let bench = &[("amenity", "bench"), ("capacity", "2")];
        assert!(!may_share(
            bench,
            &[("amenity", "bench"), ("capacity", "6")]
        ));
    }

    #[test]
    fn test_kind_name() {
        let kind = |tags: &[(&'static str, &'static str)]| kind_name(tags.iter().copied());
        assert_eq!(kind(&[("amenity", "fountain")]), Some("fountain"));
        assert_eq!(kind(&[("amenity", "drinking_water")]), Some("fountain"));
        assert_eq!(kind(&[("man_made", "drinking_fountain")]), Some("fountain"));
        assert_eq!(kind(&[("amenity", "toilets")]), Some("toilets"));
        assert_eq!(kind(&[("man_made", "charge_point")]), Some("charge_point"));
        assert_eq!(kind(&[("amenity", "parcel_locker")]), Some("parcel_locker"));
        assert_eq!(kind(&[("amenity", "charging_station")]), None);
        assert_eq!(kind(&[("amenity", "waste_basket")]), Some("waste_basket"));
        assert_eq!(
            kind(&[("amenity", "bicycle_rental")]),
            Some("bicycle_rental")
        );
        assert_eq!(
            kind(&[("bicycle_rental", "shop"), ("amenity", "bicycle_rental")]),
            None
        );
        assert_eq!(
            kind(&[("amenity", "bicycle_rental"), ("bicycle_rental", "shop")]),
            None
        );
    }

    #[test]
    fn test_no_matcher_for_rental_shops() {
        let shop = &[("amenity", "bicycle_rental"), ("bicycle_rental", "shop")];
        assert!(InfrastructureMatcher::for_place(&place("x", shop)).is_none());
    }

    #[test]
    fn test_gbfs_max_distance() {
        let station = InfrastructureMatcher::for_place(&place("gbfs", HELLO_CYCLING)).unwrap();
        assert!(station.max_distance() == Some(meters_to_chord_angle(GBFS_ID_MAX_METERS)));
        assert!(station.is_exclusive());
        let without_id = &[("amenity", "bicycle_rental")];
        let other = InfrastructureMatcher::for_place(&place("x", without_id)).unwrap();
        assert!(other.max_distance() == Some(meters_to_chord_angle(20.0)));
    }

    #[test]
    fn test_gbfs_rule() {
        let station = InfrastructureMatcher::for_place(&place("gbfs", HELLO_CYCLING)).unwrap();
        let rule = |osm: &[(&str, &str)]| with_candidate(osm, |c| station.rule(c));
        let same = &[
            ("amenity", "bicycle_rental"),
            ("ref:gbfs", "hellocycling:1234"),
        ];
        assert_eq!(rule(same), "ref");
        assert_eq!(rule(&[("amenity", "bicycle_rental")]), "kind");
    }

    #[test]
    fn test_name_words() {
        assert_eq!(
            name_words("Löwenplatz / Burbaki-Panorama"),
            vec!["löwenplatz", "burbaki", "panorama"]
        );
        assert_eq!(name_words("M05.塩見縄手"), vec!["塩見縄手"]);
        assert!(name_words("U 7").is_empty());
    }

    #[test]
    fn test_brand_names() {
        let names = |tags: &[(&'static str, &'static str)]| brand_names(tags.iter().copied());
        assert_eq!(
            names(&[
                ("brand", "Deezer nextbike"),
                ("operator", "nextbike GmbH"),
                ("name", "Fraenkelufer"),
            ]),
            vec!["deezernextbike", "nextbikegmbh"]
        );
        assert_eq!(
            names(&[("ref:gbfs", "sharedmobility.ch:velospot:168")]),
            vec!["sharedmobilitych", "velospot"]
        );
        assert_eq!(
            names(&[("brand", "ドコモ・バイクシェア")]),
            vec!["ドコモバイクシェア"]
        );
        assert!(names(&[("brand", "VB")]).is_empty());
        let agree = |a: &[&str], o: &[&str]| {
            let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            brand_names_agree(&v(a), &v(o))
        };
        assert!(agree(&["nextbike"], &["deezernextbike"]));
        assert!(agree(&["docomobikeshareservice"], &["docomobikeshare"]));
        assert!(!agree(&["voiswitzerland", "voi"], &["publibike"]));
        assert!(agree(&[], &["publibike"]));
        assert!(agree(&["voi"], &[]));
    }

    #[test]
    fn test_gbfs_id_from_tags() {
        let ids = |tags: &[(&'static str, &'static str)]| GbfsId::from_tags(tags.iter().copied());
        let id = |system: &str, station: &str| GbfsId::new(system, station).unwrap();
        assert_eq!(
            ids(&[("ref:gbfs", "Bixi_MTL:3;Bixi_MTL:4")]),
            vec![id("bixi_mtl", "3"), id("bixi_mtl", "4")]
        );
        assert_eq!(
            ids(&[
                ("gbfs:station_id", "77"),
                ("gbfs:system_id", "docomo-cycle")
            ]),
            vec![id("docomo-cycle", "77")]
        );
        assert_eq!(
            ids(&[("ref:gbfs:hellocycling", "1234")]),
            vec![id("hellocycling", "1234")]
        );
        // Station IDs may contain colons; only the first one separates.
        assert_eq!(
            ids(&[("ref:gbfs", "lyft_nyc:a:b")]),
            vec![id("lyft_nyc", "a:b")]
        );
        assert!(ids(&[("gbfs:station_id", "77")]).is_empty());
        assert!(ids(&[("ref:gbfs", "77"), ("ref", "x:1")]).is_empty());
        assert!(ids(&[("ref:gbfs", ":77")]).is_empty());
    }

    #[test]
    fn test_normalize_ref() {
        assert_eq!(normalize_ref("007153", false).as_deref(), Some("7153"));
        assert_eq!(normalize_ref("AEDR 1786", true).as_deref(), Some("1786"));
        assert_eq!(normalize_ref("000", false), None);
        assert_eq!(normalize_ref("  ", true), None);
    }
}
