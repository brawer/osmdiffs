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
//! - An ATP feature with `located_in:wikidata` or `located_in`, which
//!   some spiders set, such as `starbucks_us` for its cafés in Target
//!   stores.
//!
//! See `docs/notes/2026-11-09-departments.md`.

use crate::matchers::contact::main_category;
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

/// A department and its host must be at most this far apart. Spiders
/// often give a department the position of the store's entrance, and
/// the store its center, so large stores need more than a few meters.
pub(super) const DEPARTMENT_METERS: f64 = 75.0;

/// How far from the ATP feature we look for its host in OSM.
pub(super) const HOST_SEARCH_METERS: f64 = 150.0;

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
    /// A feature with one of these `brand:wikidata` IDs, or without
    /// `brand:wikidata` but with a `brand` or `name` like this
    /// normalized name; see [same_business_name].
    LocatedIn { ids: Vec<u64>, name: Option<String> },
}

impl Host {
    /// The `match.evidence.rule` for a row whose ATP feature is in a
    /// host of this kind.
    pub(super) fn rule(&self) -> &'static str {
        match self {
            Host::Department { .. } => "department",
            Host::LocatedIn { .. } => "located_in",
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
            Host::LocatedIn { .. } => MatchMask(u16::MAX),
        }
    }

    /// Whether the OSM feature with `tags` is the host. For the ATP
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
            Host::LocatedIn { ids, name } => {
                if brand_ids(tags.clone()).any(|id| ids.contains(&id)) {
                    return true;
                }
                if !ids.is_empty() && brand_ids(tags.clone()).next().is_some() {
                    return false;
                }
                let Some(name) = name else {
                    return false;
                };
                tags.filter(|(k, _)| *k == "brand" || *k == "name")
                    .any(|(_, v)| same_business_name(&normalize_name(v), name))
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
        }
    }
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
    (!ids.is_empty() || name.is_some()).then_some(Host::LocatedIn { ids, name })
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
        };
        assert!(
            target.is_host(
                [("shop", "department_store"), ("brand:wikidata", "Q1046951")].into_iter()
            )
        );
        assert!(target.is_host([("shop", "department_store"), ("name", "Target")].into_iter()));
        assert!(
            !target.is_host(
                [
                    ("shop", "department_store"),
                    ("name", "Target"),
                    ("brand:wikidata", "Q2")
                ]
                .into_iter()
            )
        );
        let by_name = Host::LocatedIn {
            ids: vec![],
            name: Some("pingo doce".into()),
        };
        assert!(by_name.is_host([("shop", "supermarket"), ("name", "Pingo  Doce")].into_iter()));
        assert_eq!(by_name.rule(), "located_in");
        let merchant = Host::LocatedIn {
            ids: vec![],
            name: Some("rewe ahmed".into()),
        };
        assert!(merchant.is_host([("shop", "supermarket"), ("name", "REWE")].into_iter()));
        assert!(!merchant.is_host([("shop", "supermarket"), ("name", "REWE City")].into_iter()));
        assert!(!merchant.is_host([("shop", "supermarket"), ("name", "Rewel")].into_iter()));
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
    fn test_known_category() {
        assert_eq!(known_category("amenity=pharmacy"), Some("amenity=pharmacy"));
        assert_eq!(known_category("shop=supermarket"), Some("shop=supermarket"));
        assert_eq!(known_category("shop=clothes"), None);
    }
}
