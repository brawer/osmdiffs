//! AllThePlaces features that are located in another business, such as
//! a pharmacy inside a supermarket or a Starbucks inside a Target.
//! Such a feature must not match the OpenStreetMap feature of its host,
//! or the edits for it would put the pharmacy's opening hours on the
//! supermarket. When OSM has no feature of its own for it, its row gets
//! `match.relation = located_in` with the host's OSM feature, which
//! tells mappers where to add it.
//!
//! There are two kinds:
//!
//! - A *department*: an ATP feature of a [DEPARTMENT_CATEGORIES]
//!   category within [DEPARTMENT_METERS] of an ATP feature of the same
//!   `brand:wikidata` in a [HOST_CATEGORIES] category, such as the
//!   pharmacy and the liquor store that the `publix_us` spider lists
//!   next to each Publix supermarket.
//!   A department-category ATP feature inside an OSM feature of a
//!   [HOST_CATEGORIES] category and of its own `brand:wikidata` is a
//!   department too, however far the ATP host is; see
//!   [enclosing_host].
//! - An ATP feature with `located_in:wikidata` or `located_in`, which
//!   some spiders set, such as `starbucks_us` for its cafés in Target
//!   stores.
//! - A postal partner (`post_office=post_partner`) that OSM doesn't map
//!   as a partner yet, in the OSM store whose name ATP gives, within
//!   [crate::matchers::post_partner::HOST_METERS]; see [post_partner_host].
//! - A charge point (`man_made=charge_point`) that OSM doesn't map on
//!   its own, in the OSM charging station that contains it or is within
//!   [CHARGING_STATION_METERS]; see [charging_station_host].
//! - Any other ATP feature that nothing matched, inside an OSM shopping
//!   mall (`shop=mall`), or within [MALL_NODE_METERS] of a mall that OSM
//!   maps as a node; see [mall_host]. This is only tried when the
//!   feature has no other host in OSM.
//!
//! See `docs/notes/2026-11-09-departments.md` and
//! `docs/notes/2026-11-28-mall-tenants.md`.

use crate::matchers::category::main_category;
use crate::matchers::post_partner::{self, PartnerHost, PostalBrand};
use crate::matchers::{MatchMask, normalize_name, parse_wikidata_ids};
use crate::places::{Place, PlaceReader};
use anyhow::Result;
use s2::cellid::CellID;
use s2::latlng::LatLng;
use std::collections::HashMap;

/// Main categories of departments: services that a large store often
/// has inside, under its own brand.
const DEPARTMENT_CATEGORIES: &[&str] = &[
    "amenity=pharmacy",
    "shop=alcohol",
    "amenity=cafe",
    "amenity=fast_food",
    "amenity=restaurant",
    "shop=travel_agency",
    "amenity=car_rental",
    "shop=optician",
    "shop=pet",
    "shop=outpost",
    "shop=bakery",
];

/// Department categories that OSM often uses for one another, such as
/// `amenity=restaurant` for what ATP calls the `amenity=fast_food` of
/// IKEA. A department doesn't exclude OSM features of its own group.
const FOOD_CATEGORIES: &[&str] = &[
    "amenity=cafe",
    "amenity=fast_food",
    "amenity=restaurant",
    "shop=bakery",
];

/// Main categories of stores that host departments.
const HOST_CATEGORIES: &[&str] = &[
    "shop=supermarket",
    "shop=hypermarket",
    "shop=department_store",
    "shop=furniture",
    "shop=wholesale",
];

/// Main categories that an OSM feature must have to be the host of an
/// ATP feature whose `located_in` names it, but whose
/// `located_in:wikidata` doesn't: [HOST_CATEGORIES], plus other stores
/// that the `located_in` spiders put businesses in, such as the
/// Walgreens pharmacies that `fedex` puts its counters in. Without
/// this, a Timpson in "Tesco" was `located_in` "Tesco Customer
/// Toilets" or "Waitrose car park"; see
/// [brawer/osmdiffs#929](https://github.com/brawer/osmdiffs/issues/929)
/// and [brawer/osmdiffs#933](https://github.com/brawer/osmdiffs/issues/933).
const NAMED_HOST_CATEGORIES: &[&str] = &[
    "shop=supermarket",
    "shop=hypermarket",
    "shop=department_store",
    "shop=furniture",
    "shop=wholesale",
    "shop=convenience",
    "shop=kiosk",
    "shop=mall",
    "shop=variety_store",
    "shop=general",
    "shop=pet",
    "shop=stationery",
    "shop=doityourself",
    "shop=hardware",
    "shop=chemist",
    "shop=electronics",
    "amenity=fuel",
    "amenity=pharmacy",
];

/// Main categories of OSM features that are never the host of an ATP
/// feature, even when their `brand:wikidata` is its
/// `located_in:wikidata`: the car parks, trolley bays, toilets and
/// charging stations of a store, which OSM tags with the store's brand,
/// and other businesses that hold no store, such as a car rental. A
/// host found by name must be of a [NAMED_HOST_CATEGORIES] category
/// anyway. In the planet run of 2026-11-15, a Pep&Co and a Timpson were
/// `located_in` unnamed trolley bays, and a France Pare-Brise in an ADA
/// car rental; see
/// [brawer/osmdiffs#968](https://github.com/brawer/osmdiffs/issues/968).
const NOT_HOST_CATEGORIES: &[&str] = &[
    "amenity=atm",
    "amenity=bicycle_parking",
    "amenity=bicycle_rental",
    "amenity=car_rental",
    "amenity=car_sharing",
    "amenity=charging_station",
    "amenity=motorcycle_parking",
    "amenity=parcel_locker",
    "amenity=parking",
    "amenity=parking_entrance",
    "amenity=parking_space",
    "amenity=recycling",
    "amenity=toilets",
    "amenity=trolley_bay",
    "amenity=vending_machine",
    "amenity=waste_basket",
    "amenity=waste_disposal",
];

/// A department and its host must be at most this far apart. Spiders
/// often give a department the position of the store's entrance, and
/// the store its center, so large stores need more than a few meters.
pub(super) const DEPARTMENT_METERS: f64 = 75.0;

/// How far from the ATP feature we look for its host in OSM. Hosts
/// are often large stores whose OSM centroid is far from the spot that
/// ATP gives for the business inside; at 150 m, about a third of the
/// Timpson, Sushi Daily and Starbucks features without a host had one
/// between 150 and 300 m away; see
/// [brawer/osmdiffs#933](https://github.com/brawer/osmdiffs/issues/933).
/// The nearest host wins, so another branch farther away doesn't
/// matter.
pub(super) const HOST_SEARCH_METERS: f64 = 300.0;

/// How far from a charge point we look for its OSM charging station,
/// unless a station's outline contains it. In the Netherlands,
/// Denmark, Luxembourg and Colorado, 9–17% of ChargePoint's devices
/// have an OSM station within 30 m, and another 2–5% within 30–50 m;
/// beyond, the nearest station is more often another one. Electrify
/// America gives each charge point its station's position, and 89% of
/// them in Colorado have an OSM station within 50 m. See
/// `docs/notes/2026-11-21-charge-points.md`.
pub(super) const CHARGING_STATION_METERS: f64 = 50.0;

/// How far from a shopping mall that OSM maps as a node an ATP feature
/// may be to be `located_in` it. A mall mapped as an area must contain
/// the feature instead. The edits step keeps a create's position next
/// to a host node up to 30 m away (`HOST_NODE_RADIUS_M`).
pub(super) const MALL_NODE_METERS: f64 = 30.0;

/// The kinds of ATP features that can be `located_in` a mall; see
/// [mall_host].
const MALL_TENANTS: MatchMask = MatchMask(
    MatchMask::SHOP.0 | MatchMask::RESTAURANT.0 | MatchMask::LODGING.0 | MatchMask::SCHOOL.0,
);

/// How far from an ATP feature we look for the centroid of a shopping
/// mall that may contain it. The OSM index only knows an area by its
/// centroid, and a large mall's centroid can be some way from its
/// shops.
pub(super) const MALL_SEARCH_METERS: f64 = 500.0;

/// S2 level for grouping features in [find_departments], about 1 km.
const GROUP_LEVEL: u64 = 13;

const EARTH_RADIUS_METERS: f64 = 6_371_000.0;

/// What a located-in ATP feature's host looks like in OSM.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Host {
    /// A store of the same brand, of main category `category`, for a
    /// department of main category `own`.
    Department {
        brands: Vec<u64>,
        category: &'static str,
        own: &'static str,
    },
    /// A feature with one of these `brand:wikidata` IDs, or a store
    /// with a `brand` or `name` like this normalized name; see
    /// [same_business_name]. Never a feature of main category `own`,
    /// the category of the ATP feature itself.
    LocatedIn {
        ids: Vec<u64>,
        name: Option<String>,
        own: Option<String>,
    },
    /// The charging station of a charge point, whatever its operator:
    /// a station's operator often differs from the network that lists
    /// the charge point.
    ChargingStation,
    /// The store that a postal partner is in, by the name that ATP
    /// gives for it; see [crate::matchers::post_partner].
    PostPartner(PartnerHost),
    /// The shopping mall (`shop=mall`) around an ATP feature that
    /// nothing matched, unless the mall has one of these
    /// `brand:wikidata` IDs, or a `name` or `brand` like one of these
    /// normalized `names` (see [same_business_name]), those of the ATP
    /// feature: OSM sometimes tags a store as `shop=mall`, such as the
    /// Migros VOI in Kappel SO, and a store isn't `located_in` itself;
    /// see [mall_host].
    Mall {
        brands: Vec<u64>,
        names: Vec<String>,
    },
}

impl Host {
    /// The `match.evidence.rule` for a row whose ATP feature is in a
    /// host of this kind.
    pub(super) fn rule(&self) -> &'static str {
        match self {
            Host::Department { .. } => "department",
            Host::LocatedIn { .. } => "located_in",
            Host::ChargingStation => "charging_station",
            Host::PostPartner(_) => "post_partner",
            Host::Mall { .. } => "mall",
        }
    }

    /// The mask for querying the OSM index for hosts.
    pub(super) fn mask(&self) -> MatchMask {
        match self {
            Host::Department { category, .. } => {
                let mut mask = MatchMask::default();
                if let Some((k, v)) = category.split_once('=') {
                    mask.add_tag(k, v);
                }
                mask
            }
            // The host may be anything, such as a fuel station.
            Host::LocatedIn { .. } | Host::PostPartner(_) => MatchMask(u16::MAX),
            Host::ChargingStation => {
                let mut mask = MatchMask::default();
                mask.add_tag("amenity", "charging_station");
                mask
            }
            Host::Mall { .. } => {
                let mut mask = MatchMask::default();
                mask.add_tag("shop", "mall");
                mask
            }
        }
    }

    /// How far from the ATP feature [super::find_host] looks for the
    /// host, in meters.
    pub(super) fn search_meters(&self) -> f64 {
        match self {
            Host::ChargingStation => CHARGING_STATION_METERS,
            Host::PostPartner(_) => post_partner::HOST_METERS,
            Host::Mall { .. } => MALL_SEARCH_METERS,
            _ => HOST_SEARCH_METERS,
        }
    }

    /// Whether a host whose outline contains the ATP feature wins over
    /// a nearer centroid. A large car park's charging station may have
    /// its centroid farther away than [CHARGING_STATION_METERS].
    pub(super) fn prefers_enclosing(&self) -> bool {
        matches!(self, Host::ChargingStation)
    }

    /// Whether the host must really be within [Host::search_meters],
    /// not just in the cells that cover that circle.
    pub(super) fn is_strict_radius(&self) -> bool {
        matches!(self, Host::ChargingStation | Host::PostPartner(_))
    }

    /// How far a host mapped as a node may be from the ATP feature, if
    /// a host mapped as an area must contain it rather than just be
    /// near it: [MALL_NODE_METERS] for a mall, `None` for the others.
    pub(super) fn enclosing_or_node_meters(&self) -> Option<f64> {
        matches!(self, Host::Mall { .. }).then_some(MALL_NODE_METERS)
    }

    /// Whether the OSM feature with `tags` is the host. A host is never
    /// of a [NOT_HOST_CATEGORIES] category, such as a trolley bay with
    /// the store's `brand:wikidata`. A host found by name, not by
    /// `located_in:wikidata`, must be a store of a
    /// [NAMED_HOST_CATEGORIES] category. It may have another
    /// `brand:wikidata`: ATP and OSM often disagree on the ID, such as
    /// for a Tesco Express (`Q98456772`) in which ATP has a Timpson
    /// `located_in:wikidata` Tesco (`Q487494`). A feature of the ATP
    /// feature's own category is never its host, so a Tesco pharmacy
    /// `located_in` Tesco may still match the OSM "Tesco Pharmacy". For the ATP
    /// feature itself, this must be false, or it might match its own
    /// host; see [Host::excludes].
    pub(super) fn is_host<'a>(
        &self,
        tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    ) -> bool {
        match self {
            Host::Department {
                brands, category, ..
            } => {
                main_category(tags.clone()).is_some_and(|c| c == *category)
                    && brand_ids(tags).any(|id| brands.contains(&id))
            }
            Host::LocatedIn { ids, name, own } => {
                let category = main_category(tags.clone());
                if own.is_some() && category == *own {
                    return false;
                }
                if category.as_deref().is_some_and(is_not_host_category) {
                    return false;
                }
                if brand_ids(tags.clone()).any(|id| ids.contains(&id)) {
                    return true;
                }
                let Some(name) = name else {
                    return false;
                };
                if !category.as_deref().is_some_and(is_named_host_category) {
                    return false;
                }
                tags.filter(|(k, _)| *k == "brand" || *k == "name")
                    .any(|(_, v)| same_business_name(&normalize_name(v), name))
            }
            Host::ChargingStation => main_category(tags)
                .is_some_and(|c| category_values(&c).any(|c| c == "amenity=charging_station")),
            Host::PostPartner(host) => {
                let category = main_category(tags.clone());
                category.is_some_and(|c| !is_not_host_category(&c)) && host.agrees(tags)
            }
            Host::Mall { brands, names } => {
                main_category(tags.clone())
                    .is_some_and(|c| category_values(&c).any(|c| c == "shop=mall"))
                    && !brand_ids(tags.clone()).any(|id| brands.contains(&id))
                    && !tags
                        .filter(|(k, _)| is_name_key(k))
                        .map(|(_, v)| normalize_name(v))
                        .any(|v| names.iter().any(|n| same_business_name(&v, n)))
            }
        }
    }

    /// Whether the ATP feature must not match the OSM feature with
    /// `tags`: its host, or for a department, a host or another kind of
    /// department, such as the liquor store next to the pharmacy. The
    /// IKEA bistro may still match the IKEA restaurant; see
    /// [FOOD_CATEGORIES].
    pub(super) fn excludes<'a>(
        &self,
        tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    ) -> bool {
        match self {
            Host::Department { own, .. } => main_category(tags).is_some_and(|c| {
                let c = c.as_str();
                let same_kind =
                    c == *own || (FOOD_CATEGORIES.contains(&c) && FOOD_CATEGORIES.contains(own));
                !same_kind && (HOST_CATEGORIES.contains(&c) || DEPARTMENT_CATEGORIES.contains(&c))
            }),
            Host::LocatedIn { .. } => self.is_host(tags),
            // The charge point matcher only takes charge points anyway.
            Host::ChargingStation => false,
            // The partner matcher only takes OSM partners of its brand,
            // and its host may be one already.
            Host::PostPartner(_) => false,
            // A mall is only looked for once nothing matched.
            Host::Mall { .. } => false,
        }
    }
}

/// The host of a charge point (`man_made=charge_point`): the OSM
/// charging station around it. `None` for anything else.
pub(super) fn charging_station_host(place: &Place) -> Option<Host> {
    place
        .tags
        .iter()
        .any(|(k, v)| k == "man_made" && v == "charge_point")
        .then_some(Host::ChargingStation)
}

/// The host of a postal partner, if ATP names one; `None` for anything
/// but a partner; see [crate::matchers::post_partner].
pub(super) fn post_partner_host(place: &Place) -> Option<Host> {
    let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
    if !post_partner::is_post_partner(tags()) {
        return None;
    }
    let brand = PostalBrand::of_atp(&place.spider, tags());
    PartnerHost::of_atp(tags(), &brand).map(Host::PostPartner)
}

/// The host of an ATP feature that nothing matched and that has no
/// other host in OSM: the shopping mall it is in, if OSM has one; see
/// [Host::Mall]. Shops that ATP names after their mall, such as
/// `ａｕショップ イオンモール羽生`, would otherwise become plain
/// creates, or none at all if the mall's name held them back as
/// similar; see
/// [brawer/osmdiffs#1067](https://github.com/brawer/osmdiffs/issues/1067).
/// Only for a shop, restaurant, hotel or school ([MALL_TENANTS]), and
/// `None` for a mall itself and for a postal partner, whose host gets
/// its tags in the edits. Street furniture, such as post boxes, parcel
/// lockers and bike-share docks, often stands in a mall's car park or
/// at its doors, and a transit stop or charging station is no tenant.
pub(super) fn mall_host(place: &Place) -> Option<Host> {
    let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
    if !place.mask.intersects(&MALL_TENANTS)
        || post_partner::is_post_partner(tags())
        || tags().any(|(k, v)| k == "shop" && v == "mall")
    {
        return None;
    }
    let mut names: Vec<String> = tags()
        .filter(|(k, _)| is_name_key(k))
        .map(|(_, v)| normalize_name(v))
        .filter(|n| !n.is_empty())
        .collect();
    names.sort();
    names.dedup();
    Some(Host::Mall {
        brands: brand_ids(tags()).collect(),
        names,
    })
}

/// Whether [Host::Mall] compares the value of `key` with the other
/// side's names: `name`, `brand`, and their `name:*` and `brand:*`
/// variants in other languages, such as `brand:th` for Big C, but not
/// `brand:wikidata` or `brand:wikipedia`.
fn is_name_key(key: &str) -> bool {
    match key.split_once(':') {
        None => key == "name" || key == "brand",
        Some((base, suffix)) => {
            (base == "name" || base == "brand") && !matches!(suffix, "wikidata" | "wikipedia")
        }
    }
}

/// Whether `category` is in [NAMED_HOST_CATEGORIES]. A category with
/// several `;`-separated values, such as `shop=supermarket;fuel`, is
/// if any of its values is.
fn is_named_host_category(category: &str) -> bool {
    category_values(category).any(|c| NAMED_HOST_CATEGORIES.contains(&c.as_str()))
}

/// Whether `category` is in [NOT_HOST_CATEGORIES]. A category with
/// several `;`-separated values is only if all of its values are, so
/// `amenity=parking;fuel` may still be a host.
fn is_not_host_category(category: &str) -> bool {
    let mut values = category_values(category).peekable();
    values.peek().is_some() && values.all(|c| NOT_HOST_CATEGORIES.contains(&c.as_str()))
}

/// The `key=value` categories of a main category with one or more
/// `;`-separated values, such as `shop=supermarket` and `shop=fuel` for
/// `shop=supermarket;fuel`. None for a category without `=`.
fn category_values(category: &str) -> impl Iterator<Item = String> + '_ {
    let (key, values) = category.split_once('=').unwrap_or(("", ""));
    values
        .split(';')
        .map(str::trim)
        .filter(move |v| !key.is_empty() && !v.is_empty())
        .map(move |v| format!("{key}={v}"))
}

/// Whether two normalized names are the same, or one is the other
/// followed by more words. Spiders often give the merchant's name in
/// `located_in`, such as "REWE Ahmed" for a store that OSM calls
/// "REWE", and OSM sometimes does the same.
fn same_business_name(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    !short.is_empty()
        && long
            .strip_prefix(short)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

fn brand_ids<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> impl Iterator<Item = u64> {
    tags.filter(|(k, _)| *k == "brand:wikidata")
        .flat_map(|(_, v)| parse_wikidata_ids(v))
}

/// The host from the `located_in:wikidata` or `located_in` tags of
/// `place`, if it has any.
pub(super) fn located_in_tag(place: &Place) -> Option<Host> {
    let tag = |key: &str| {
        place
            .tags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let ids: Vec<u64> = tag("located_in:wikidata")
        .map(|v| parse_wikidata_ids(v).collect())
        .unwrap_or_default();
    let name = tag("located_in")
        .map(normalize_name)
        .filter(|n| !n.is_empty());
    let own = main_category(place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    (!ids.is_empty() || name.is_some()).then_some(Host::LocatedIn { ids, name, own })
}

/// The departments among the ATP features in `reader`, by their
/// position in [PlaceReader::read_all], with their host.
pub(super) fn find_departments(reader: &PlaceReader) -> Result<HashMap<u64, Host>> {
    // For each brand and ~1 km cell: the features of department and
    // host categories, as (S2 cell, category, ordinal).
    type Entry = (u64, &'static str, u64);
    let mut groups: HashMap<(u64, u64), Vec<Entry>> = HashMap::new();
    let mut ordinal: u64 = 0;
    for batch in reader.read_all()? {
        for place in batch? {
            let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
            if let Some(category) = main_category(tags()).and_then(|c| known_category(&c)) {
                let group = CellID(place.s2_cell_id).parent(GROUP_LEVEL).0;
                for id in brand_ids(tags()) {
                    groups.entry((id, group)).or_default().push((
                        place.s2_cell_id,
                        category,
                        ordinal,
                    ));
                }
            }
            ordinal += 1;
        }
    }

    let mut departments = HashMap::new();
    for (&(brand, group), entries) in &groups {
        let neighbours: Vec<&Entry> = std::iter::once(group)
            .chain(CellID(group).all_neighbors(GROUP_LEVEL).iter().map(|c| c.0))
            .filter_map(|g| groups.get(&(brand, g)))
            .flatten()
            .filter(|(_, c, _)| HOST_CATEGORIES.contains(c))
            .collect();
        for &(cell, category, ordinal) in entries {
            if !DEPARTMENT_CATEGORIES.contains(&category) {
                continue;
            }
            let host = neighbours
                .iter()
                .filter(|(c, _, _)| distance_meters(cell, *c) <= DEPARTMENT_METERS)
                .min_by(|a, b| distance_meters(cell, a.0).total_cmp(&distance_meters(cell, b.0)));
            if let Some(&&(_, host_category, _)) = host {
                let entry = departments.entry(ordinal).or_insert(Host::Department {
                    brands: Vec::new(),
                    category: host_category,
                    own: category,
                });
                if let Host::Department { brands, .. } = entry
                    && !brands.contains(&brand)
                {
                    brands.push(brand);
                }
            }
        }
    }
    Ok(departments)
}

/// The main category and `brand:wikidata` IDs of `place`, if it is in
/// a [DEPARTMENT_CATEGORIES] category and has a brand. Such a feature
/// is a department if an OSM host of its brand contains it; see
/// [enclosing_host].
pub(super) fn department_brands(place: &Place) -> Option<(&'static str, Vec<u64>)> {
    let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
    let own = main_category(tags())
        .and_then(|c| DEPARTMENT_CATEGORIES.iter().find(|d| **d == c).copied())?;
    let brands: Vec<u64> = brand_ids(tags()).collect();
    (!brands.is_empty()).then_some((own, brands))
}

/// The host of a department-category ATP feature of main category
/// `own` and `brand:wikidata` `brands`, at `point`, if the OSM feature
/// with `tags` is a [HOST_CATEGORIES] store of one of these brands and
/// its `geometry` contains `point`. `geometry` is only called for such
/// a store. Spiders sometimes put the
/// pharmacy of a large supermarket more than [DEPARTMENT_METERS] from
/// the supermarket, but still inside its building; see
/// [brawer/osmdiffs#930](https://github.com/brawer/osmdiffs/issues/930).
pub(super) fn enclosing_host<'a>(
    own: &'static str,
    brands: &[u64],
    tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    geometry: impl FnOnce() -> Option<geo::Geometry>,
    point: &geo::Point,
) -> Option<Host> {
    use geo::Contains;
    let category = main_category(tags.clone())
        .and_then(|c| HOST_CATEGORIES.iter().find(|h| **h == c).copied())?;
    let ids: Vec<u64> = brand_ids(tags).filter(|id| brands.contains(id)).collect();
    if ids.is_empty() || !geometry().is_some_and(|g| g.contains(point)) {
        return None;
    }
    Some(Host::Department {
        brands: ids,
        category,
        own,
    })
}

/// The category in [DEPARTMENT_CATEGORIES] or [HOST_CATEGORIES] that
/// equals `category`.
fn known_category(category: &str) -> Option<&'static str> {
    DEPARTMENT_CATEGORIES
        .iter()
        .chain(HOST_CATEGORIES)
        .find(|c| **c == category)
        .copied()
}

fn distance_meters(a: u64, b: u64) -> f64 {
    LatLng::from(CellID(a))
        .distance(&LatLng::from(CellID(b)))
        .rad()
        * EARTH_RADIUS_METERS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host() {
        let dept = Host::Department {
            brands: vec![7],
            category: "shop=supermarket",
            own: "amenity=pharmacy",
        };
        let supermarket = [("shop", "supermarket"), ("brand:wikidata", "Q7")];
        let other = [("shop", "supermarket"), ("brand:wikidata", "Q8")];
        let pharmacy = [("amenity", "pharmacy"), ("brand:wikidata", "Q7")];
        assert!(dept.is_host(supermarket.iter().copied()));
        assert!(!dept.is_host(other.iter().copied()));
        assert!(!dept.is_host(pharmacy.iter().copied()));
        assert_eq!(dept.rule(), "department");
        let liquor = [("shop", "alcohol"), ("brand:wikidata", "Q7")];
        let chemist = [("shop", "chemist"), ("brand:wikidata", "Q7")];
        assert!(dept.excludes(supermarket.iter().copied()));
        assert!(dept.excludes(liquor.iter().copied()));
        assert!(!dept.excludes(pharmacy.iter().copied()));
        assert!(!dept.excludes(chemist.iter().copied()));
        assert!(dept.mask().intersects(&MatchMask::SHOP));
        let bistro = Host::Department {
            brands: vec![7],
            category: "shop=furniture",
            own: "amenity=fast_food",
        };
        let restaurant = [("amenity", "restaurant"), ("brand:wikidata", "Q7")];
        let furniture = [("shop", "furniture"), ("brand:wikidata", "Q7")];
        assert!(!bistro.excludes(restaurant.iter().copied()));
        assert!(bistro.excludes(furniture.iter().copied()));
        assert!(bistro.excludes(pharmacy.iter().copied()));

        let target = Host::LocatedIn {
            ids: vec![1046951],
            name: Some("target".into()),
            own: Some("amenity=cafe".into()),
        };
        assert!(
            target.is_host(
                [("shop", "department_store"), ("brand:wikidata", "Q1046951")].into_iter()
            )
        );
        assert!(target.is_host([("shop", "department_store"), ("name", "Target")].into_iter()));
        // A store by name, even with another `brand:wikidata`; see
        // https://github.com/brawer/osmdiffs/issues/933.
        assert!(
            target.is_host(
                [
                    ("shop", "department_store"),
                    ("name", "Target"),
                    ("brand:wikidata", "Q2")
                ]
                .into_iter()
            )
        );
        assert!(
            !target.is_host(
                [
                    ("amenity", "parking"),
                    ("name", "Target"),
                    ("brand:wikidata", "Q2")
                ]
                .into_iter()
            )
        );
        let by_name = Host::LocatedIn {
            ids: vec![],
            name: Some("pingo doce".into()),
            own: Some("amenity=cafe".into()),
        };
        assert!(by_name.is_host([("shop", "supermarket"), ("name", "Pingo  Doce")].into_iter()));
        assert_eq!(by_name.rule(), "located_in");
        let merchant = Host::LocatedIn {
            ids: vec![],
            name: Some("rewe ahmed".into()),
            own: Some("amenity=cafe".into()),
        };
        assert!(merchant.is_host([("shop", "supermarket"), ("name", "REWE")].into_iter()));
        assert!(!merchant.is_host([("shop", "supermarket"), ("name", "REWE City")].into_iter()));
        assert!(!merchant.is_host([("shop", "supermarket"), ("name", "Rewel")].into_iter()));
    }

    #[test]
    fn test_host_by_name_must_be_store() {
        // https://github.com/brawer/osmdiffs/issues/929
        let tesco = Host::LocatedIn {
            ids: vec![],
            name: Some("tesco".into()),
            own: Some("shop=keys".into()),
        };
        let is_host = |tags: &[(&'static str, &'static str)]| tesco.is_host(tags.iter().copied());
        assert!(is_host(&[("shop", "supermarket"), ("name", "Tesco")]));
        assert!(is_host(&[
            ("shop", "convenience"),
            ("name", "Tesco Express")
        ]));
        assert!(is_host(&[("amenity", "fuel"), ("name", "Tesco")]));
        assert!(is_host(&[("shop", "supermarket;fuel"), ("name", "Tesco")]));
        assert!(!is_host(&[
            ("amenity", "toilets"),
            ("name", "Tesco Customer Toilets")
        ]));
        assert!(!is_host(&[("amenity", "parking"), ("name", "Tesco")]));
        assert!(!is_host(&[
            ("amenity", "charging_station"),
            ("name", "Tesco Extra - Gatwick")
        ]));
        assert!(!is_host(&[
            ("amenity", "fast_food"),
            ("name", "Tesco Café")
        ]));
        assert!(!is_host(&[("shop", "butcher"), ("name", "Tesco")]));
        assert!(!is_host(&[("building", "retail"), ("name", "Tesco")]));
        assert!(!is_host(&[
            ("shop", "no"),
            ("amenity", "parking"),
            ("name", "Tesco")
        ]));
        assert!(!tesco.excludes([("amenity", "parking"), ("name", "Tesco")].into_iter()));

        // A `located_in:wikidata` host may be of any category but a
        // [NOT_HOST_CATEGORIES] one.
        let by_id = Host::LocatedIn {
            ids: vec![7],
            name: Some("tesco".into()),
            own: Some("shop=keys".into()),
        };
        assert!(by_id.is_host([("shop", "mall"), ("brand:wikidata", "Q7")].into_iter()));
        assert!(by_id.is_host([("amenity", "hospital"), ("brand:wikidata", "Q7")].into_iter()));
        assert!(!by_id.is_host([("amenity", "parking"), ("brand:wikidata", "Q7")].into_iter()));
        assert!(!by_id.is_host([("amenity", "parking"), ("name", "Tesco")].into_iter()));
        assert!(by_id.is_host([("shop", "supermarket"), ("name", "Tesco")].into_iter()));
    }

    #[test]
    fn test_host_by_id_must_not_be_trolley_bay_or_car_rental() {
        // https://github.com/brawer/osmdiffs/issues/968
        let tesco = Host::LocatedIn {
            ids: vec![487494],
            name: Some("tesco".into()),
            own: Some("shop=keys".into()),
        };
        let is_host = |tags: &[(&'static str, &'static str)]| tesco.is_host(tags.iter().copied());
        assert!(!is_host(&[
            ("amenity", "trolley_bay"),
            ("brand:wikidata", "Q487494")
        ]));
        assert!(!is_host(&[
            ("amenity", "car_rental"),
            ("name", "Tesco"),
            ("brand:wikidata", "Q487494")
        ]));
        assert!(!is_host(&[
            ("amenity", "charging_station"),
            ("brand:wikidata", "Q487494")
        ]));
        assert!(!is_host(&[
            ("amenity", "toilets"),
            ("brand:wikidata", "Q487494")
        ]));
        assert!(is_host(&[
            ("amenity", "parking;fuel"),
            ("brand:wikidata", "Q487494")
        ]));
        assert!(is_host(&[
            ("shop", "supermarket"),
            ("brand:wikidata", "Q487494")
        ]));
        assert!(
            !tesco
                .excludes([("amenity", "trolley_bay"), ("brand:wikidata", "Q487494")].into_iter())
        );
    }

    #[test]
    fn test_host_with_other_brand_id() {
        // https://github.com/brawer/osmdiffs/issues/933
        let tesco = Host::LocatedIn {
            ids: vec![487494],
            name: Some("tesco".into()),
            own: Some("shop=keys".into()),
        };
        let is_host = |tags: &[(&'static str, &'static str)]| tesco.is_host(tags.iter().copied());
        assert!(is_host(&[
            ("shop", "convenience"),
            ("name", "Tesco Express"),
            ("brand:wikidata", "Q98456772")
        ]));
        assert!(!is_host(&[
            ("shop", "convenience"),
            ("name", "Sainsbury's Local"),
            ("brand:wikidata", "Q13218434")
        ]));
        assert!(!is_host(&[
            ("amenity", "parking"),
            ("name", "Tesco"),
            ("brand:wikidata", "Q98456772")
        ]));
    }

    #[test]
    fn test_pharmacy_host() {
        // `fedex` counters in Walgreens, which OSM maps as pharmacies.
        let walgreens = Host::LocatedIn {
            ids: vec![],
            name: Some("walgreens".into()),
            own: Some("shop=copyshop".into()),
        };
        assert!(
            walgreens.is_host(
                [
                    ("amenity", "pharmacy"),
                    ("name", "Walgreens"),
                    ("brand:wikidata", "Q1591889")
                ]
                .into_iter()
            )
        );
    }

    #[test]
    fn test_host_not_of_own_category() {
        // `tesco_gb` sets `located_in` Tesco on its own pharmacies and
        // fuel stations, which must still match their OSM features.
        let tesco = Host::LocatedIn {
            ids: vec![487494],
            name: Some("tesco".into()),
            own: Some("amenity=pharmacy".into()),
        };
        let pharmacy = [
            ("amenity", "pharmacy"),
            ("name", "Tesco Pharmacy"),
            ("brand:wikidata", "Q487494"),
        ];
        assert!(!tesco.is_host(pharmacy.iter().copied()));
        assert!(!tesco.excludes(pharmacy.iter().copied()));
        let store = [("shop", "supermarket"), ("brand:wikidata", "Q487494")];
        assert!(tesco.is_host(store.iter().copied()));
        assert!(tesco.excludes(store.iter().copied()));
        let no_category = Host::LocatedIn {
            ids: vec![487494],
            name: None,
            own: None,
        };
        assert!(no_category.is_host(store.iter().copied()));
    }

    #[test]
    fn test_located_in_tag() {
        let coord = geo::Coord { x: -0.1, y: 51.5 };
        let place = Place::new(
            &coord,
            "timpson_group".to_string(),
            MatchMask::SHOP,
            [
                ("shop", "keys"),
                ("located_in", "Tesco"),
                ("located_in:wikidata", "Q487494"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            &geo::Point::from(coord).into(),
            crate::utils::UtcTimestamp(
                time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap(),
            ),
        )
        .unwrap();
        assert_eq!(
            located_in_tag(&place),
            Some(Host::LocatedIn {
                ids: vec![487494],
                name: Some("tesco".into()),
                own: Some("shop=keys".into()),
            })
        );
    }

    #[test]
    fn test_charging_station_host() {
        let host = Host::ChargingStation;
        let station = [("amenity", "charging_station"), ("operator", "Ionity")];
        assert!(host.is_host(station.iter().copied()));
        assert!(host.is_host([("amenity", "fuel;charging_station")].into_iter()));
        assert!(!host.is_host([("amenity", "fuel")].into_iter()));
        assert!(!host.is_host([("man_made", "charge_point")].into_iter()));
        assert!(!host.excludes(station.iter().copied()));
        assert_eq!(host.rule(), "charging_station");
        assert_eq!(host.search_meters(), CHARGING_STATION_METERS);
        assert!(host.prefers_enclosing());
        assert!(
            !Host::LocatedIn {
                ids: vec![1],
                name: None,
                own: None
            }
            .prefers_enclosing()
        );

        let place = |tags: &[(&str, &str)], mask| {
            let coord = geo::Coord { x: 5.1, y: 52.1 };
            Place::new(
                &coord,
                "electrify_america_us".to_string(),
                mask,
                tags.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                &geo::Point::from(coord).into(),
                crate::utils::UtcTimestamp(
                    time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap(),
                ),
            )
            .unwrap()
        };
        let charge_point = place(
            &[("man_made", "charge_point"), ("ref", "110106-01")],
            MatchMask::STREET_FURNITURE,
        );
        assert_eq!(
            charging_station_host(&charge_point),
            Some(Host::ChargingStation)
        );
        let station = place(&[("amenity", "charging_station")], MatchMask::FUEL);
        assert_eq!(charging_station_host(&station), None);
    }

    #[test]
    fn test_mall_host() {
        // https://github.com/brawer/osmdiffs/issues/1067
        let place = |tags: &[(&str, &str)], mask| {
            let coord = geo::Coord { x: 139.5, y: 36.2 };
            Place::new(
                &coord,
                "au_jp".to_string(),
                mask,
                tags.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                &geo::Point::from(coord).into(),
                crate::utils::UtcTimestamp(
                    time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap(),
                ),
            )
            .unwrap()
        };
        let au = place(
            &[
                ("shop", "mobile_phone"),
                ("brand:wikidata", "Q485814"),
                ("name", "ａｕショップ イオンモール羽生"),
            ],
            MatchMask::SHOP,
        );
        let host = mall_host(&au).expect("a mall host");
        assert_eq!(
            host,
            Host::Mall {
                brands: vec![485814],
                names: vec!["ａｕショップ イオンモール羽生".into()]
            }
        );
        assert_eq!(host.rule(), "mall");
        assert_eq!(host.search_meters(), MALL_SEARCH_METERS);
        assert_eq!(host.enclosing_or_node_meters(), Some(MALL_NODE_METERS));
        assert!(!host.prefers_enclosing() && !host.is_strict_radius());
        assert_eq!(Host::ChargingStation.enclosing_or_node_meters(), None);
        assert!(host.mask().intersects(&MatchMask::SHOP));
        let aeon = [("shop", "mall"), ("name", "イオンモール羽生")];
        assert!(host.is_host(aeon.iter().copied()));
        assert!(!host.excludes(aeon.iter().copied()));
        assert!(!host.is_host([("shop", "supermarket")].into_iter()));
        assert!(
            !host.is_host([("shop", "mobile_phone"), ("brand:wikidata", "Q485814")].into_iter())
        );
        // A mall of the feature's own brand isn't its host.
        assert!(!host.is_host([("shop", "mall"), ("brand:wikidata", "Q485814")].into_iter()));
        assert!(host.is_host([("shop", "mall"), ("brand:wikidata", "Q1")].into_iter()));

        // A store that OSM tags as a mall isn't its own host: the VOI
        // in Kappel SO, node/8996515960, and Big C in Pathum Thani,
        // way/544540116.
        let voi = mall_host(&place(
            &[("shop", "supermarket"), ("name", "VOI"), ("brand", "VOI")],
            MatchMask::SHOP,
        ))
        .expect("a mall host");
        assert!(
            !voi.is_host([("shop", "mall"), ("name", "VOI"), ("operator", "Migros")].into_iter())
        );
        let big_c = mall_host(&place(
            &[
                ("shop", "supermarket"),
                ("name", "BigC ปทุมธานี"),
                ("brand", "Big C"),
                ("brand:th", "บิ๊กซี"),
                ("brand:wikidata", "Q858665"),
            ],
            MatchMask::SHOP,
        ))
        .expect("a mall host");
        let pathum_thani = [("shop", "mall"), ("name", "Big C Pathum Thani -บิ๊กซี ปทุมธานี")];
        assert!(!big_c.is_host(pathum_thani.iter().copied()));
        assert!(!big_c.is_host([("shop", "mall"), ("name", "บิ๊กซี อ่างทอง")].into_iter()));
        // But the Pure pharmacy inside is a tenant.
        let pure = mall_host(&place(
            &[
                ("amenity", "pharmacy"),
                ("name", "Pure ปทุมธานี"),
                ("brand", "Pure"),
            ],
            MatchMask::SHOP,
        ))
        .expect("a mall host");
        assert!(pure.is_host(pathum_thani.iter().copied()));
        assert!(is_name_key("name:ja") && is_name_key("brand:th") && is_name_key("brand"));
        assert!(!is_name_key("brand:wikidata") && !is_name_key("official_name"));

        let mall = place(
            &[("shop", "mall"), ("name", "Emmen Center")],
            MatchMask::SHOP,
        );
        assert_eq!(mall_host(&mall), None);
        let partner = place(&[("post_office", "post_partner")], MatchMask::SHOP);
        assert_eq!(mall_host(&partner), None);
        let charge_point = place(&[("man_made", "charge_point")], MatchMask::STREET_FURNITURE);
        assert_eq!(mall_host(&charge_point), None);
        let station = place(&[("amenity", "charging_station")], MatchMask::FUEL);
        assert_eq!(mall_host(&station), None);
        let post_box = place(&[("amenity", "post_box")], MatchMask::STREET_FURNITURE);
        assert_eq!(mall_host(&post_box), None);
        let stop = place(&[("public_transport", "platform")], MatchMask::TRANSIT);
        assert_eq!(mall_host(&stop), None);
        let cafe = place(&[("amenity", "cafe")], MatchMask::RESTAURANT);
        assert!(mall_host(&cafe).is_some());
    }

    #[test]
    fn test_named_host_categories() {
        for c in HOST_CATEGORIES {
            assert!(NAMED_HOST_CATEGORIES.contains(c), "{c}");
        }
        assert!(is_named_host_category("shop=supermarket"));
        assert!(is_named_host_category("shop=car_repair;convenience"));
        assert!(!is_named_host_category("amenity=parking"));
        assert!(!is_named_host_category("amenity=supermarket"));
        assert!(!is_named_host_category("shop"));
    }

    #[test]
    fn test_not_host_categories() {
        for c in NOT_HOST_CATEGORIES {
            assert!(!NAMED_HOST_CATEGORIES.contains(c), "{c}");
            assert!(is_not_host_category(c), "{c}");
        }
        assert!(is_not_host_category("amenity=parking;toilets"));
        assert!(!is_not_host_category("amenity=parking;fuel"));
        assert!(!is_not_host_category("shop=supermarket"));
        assert!(!is_not_host_category("amenity"));
        assert!(!is_not_host_category("amenity="));
    }

    #[test]
    fn test_same_business_name() {
        assert!(same_business_name("rewe", "rewe"));
        assert!(same_business_name("rewe", "rewe ahmed"));
        assert!(same_business_name("edeka brehm", "edeka"));
        assert!(!same_business_name("hit", "hitachi"));
        assert!(!same_business_name("", "rewe"));
    }

    #[test]
    fn test_enclosing_host() {
        let building: geo::Geometry = geo::Polygon::new(
            geo::LineString::from(vec![(0.0, 0.0), (0.002, 0.0), (0.002, 0.002), (0.0, 0.002)]),
            vec![],
        )
        .into();
        let inside = geo::Point::new(0.0019, 0.0019);
        let outside = geo::Point::new(0.003, 0.001);
        let supermarket = [("shop", "supermarket"), ("brand:wikidata", "Q7;Q9")];
        assert_eq!(
            enclosing_host(
                "amenity=pharmacy",
                &[7],
                supermarket.iter().copied(),
                || Some(building.clone()),
                &inside
            ),
            Some(Host::Department {
                brands: vec![7],
                category: "shop=supermarket",
                own: "amenity=pharmacy",
            })
        );
        let host = |tags: &[(&'static str, &'static str)], brands: &[u64], p: &geo::Point| {
            enclosing_host(
                "amenity=pharmacy",
                brands,
                tags.iter().copied(),
                || Some(building.clone()),
                p,
            )
        };
        assert_eq!(host(&supermarket, &[7], &outside), None);
        assert_eq!(host(&supermarket, &[8], &inside), None);
        let mall = [("shop", "mall"), ("brand:wikidata", "Q7")];
        assert_eq!(host(&mall, &[7], &inside), None);
        let pharmacy = [("amenity", "pharmacy"), ("brand:wikidata", "Q7")];
        assert_eq!(host(&pharmacy, &[7], &inside), None);
        let node: geo::Geometry = geo::Point::new(0.0018, 0.0019).into();
        assert_eq!(
            enclosing_host(
                "amenity=pharmacy",
                &[7],
                supermarket.iter().copied(),
                || Some(node.clone()),
                &inside
            ),
            None
        );
    }

    #[test]
    fn test_department_brands() {
        let place = |tags: &[(&str, &str)]| {
            let coord = geo::Coord { x: 8.5, y: 47.4 };
            Place::new(
                &coord,
                "test_spider".to_string(),
                MatchMask::SHOP,
                tags.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                &geo::Point::from(coord).into(),
                crate::utils::UtcTimestamp(
                    time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap(),
                ),
            )
            .unwrap()
        };
        assert_eq!(
            department_brands(&place(&[("amenity", "pharmacy"), ("brand:wikidata", "Q7")])),
            Some(("amenity=pharmacy", vec![7]))
        );
        assert_eq!(department_brands(&place(&[("amenity", "pharmacy")])), None);
        assert_eq!(
            department_brands(&place(&[("shop", "supermarket"), ("brand:wikidata", "Q7")])),
            None
        );
    }

    #[test]
    fn test_known_category() {
        assert_eq!(known_category("amenity=pharmacy"), Some("amenity=pharmacy"));
        assert_eq!(known_category("shop=supermarket"), Some("shop=supermarket"));
        assert_eq!(known_category("shop=clothes"), None);
    }
}
