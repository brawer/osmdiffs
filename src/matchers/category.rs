//! The main category of a feature, such as `shop=florist`, and which
//! pairs of main categories one business often has in AllThePlaces and
//! OpenStreetMap, such as `amenity=pharmacy` and `shop=chemist`.
//!
//! The matcher uses this for evidence that doesn't say what a feature
//! is, such as a shared phone number, and the search for deletion
//! candidates uses it to tell which kinds of a brand AllThePlaces
//! lists. The pairs leave out two features that merely share a site,
//! such as a fuel station and the shop at it, or a Western Union
//! counter and the supermarket it is in: those are different features
//! in OpenStreetMap, and both uses must keep them apart.

use crate::matchers::split_values;
use std::collections::HashSet;
use std::sync::LazyLock;

/// Extra factor for a match whose main categories are a known pair,
/// see [category_factor].
pub const CATEGORY_PAIR_FACTOR: f64 = 0.95;

/// Keys whose value is a feature's main category, in order of priority.
const CATEGORY_KEYS: &[&str] = &[
    "shop",
    "amenity",
    "tourism",
    "leisure",
    "healthcare",
    "craft",
    "office",
];

/// A feature's main category as `key=value`, such as `shop=florist`:
/// the value of the first key in [CATEGORY_KEYS] that the feature has.
pub fn main_category<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Option<String> {
    let mut best: Option<(usize, &str, &str)> = None;
    for (k, v) in tags {
        if let Some(i) = CATEGORY_KEYS.iter().position(|c| *c == k)
            && best.is_none_or(|(j, _, _)| i < j)
            && v != "no"
        {
            best = Some((i, k, v));
        }
    }
    best.map(|(_, k, v)| format!("{k}={v}"))
}

/// Pairs of main categories that one business often has in AllThePlaces
/// and OpenStreetMap, such as `tourism=hotel` and `amenity=restaurant`.
/// Inferred by `scripts/infer_contact_category_pairs.py`, from contact
/// and `brand:wikidata` matches, and then reviewed by hand; see the
/// file's header.
static CATEGORY_PAIRS: LazyLock<HashSet<(String, String)>> = LazyLock::new(|| {
    let mut pairs = HashSet::new();
    for line in include_str!("category_pairs.tsv").lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        if let (Some(a), Some(b)) = (cols.next(), cols.next()) {
            pairs.insert((a.to_string(), b.to_string()));
            pairs.insert((b.to_string(), a.to_string()));
        }
    }
    pairs
});

/// How well two main categories agree: 1.0 if
/// they are equal, [CATEGORY_PAIR_FACTOR] if they are a known pair,
/// and 0.0 otherwise, also when either side has no main category. A
/// category with several `;`-separated values, such as
/// `shop=car;car_repair`, agrees as well as its best value does.
pub fn category_factor(atp: Option<&str>, osm: Option<&str>) -> f64 {
    let (Some(atp), Some(osm)) = (atp, osm) else {
        return 0.0;
    };
    let mut best = 0.0_f64;
    for a in split_category(atp) {
        for o in split_category(osm) {
            let factor = if a == o {
                1.0
            } else if CATEGORY_PAIRS.contains(&(a.clone(), o.clone())) {
                CATEGORY_PAIR_FACTOR
            } else {
                0.0
            };
            best = best.max(factor);
        }
    }
    best
}

/// `shop=car;car_repair` as `shop=car` and `shop=car_repair`.
fn split_category(category: &str) -> Vec<String> {
    let Some((key, values)) = category.split_once('=') else {
        return vec![category.to_string()];
    };
    if !values.contains(';') {
        return vec![category.to_string()];
    }
    split_values(values).map(|v| format!("{key}={v}")).collect()
}

/// Whether two main categories are the same, or a known pair: whether
/// [category_factor] is above zero.
pub fn categories_agree(a: &str, b: &str) -> bool {
    category_factor(Some(a), Some(b)) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_main_category() {
        let t = [("name", "X"), ("amenity", "cafe"), ("shop", "bakery")];
        assert_eq!(main_category(t.into_iter()).as_deref(), Some("shop=bakery"));
        let t = [("shop", "no"), ("amenity", "cafe")];
        assert_eq!(
            main_category(t.into_iter()).as_deref(),
            Some("amenity=cafe")
        );
        assert_eq!(main_category([("name", "X")].into_iter()), None);
    }

    #[test]
    fn test_category_factor() {
        let f = category_factor;
        assert_eq!(f(Some("shop=car"), Some("shop=car")), 1.0);
        assert_eq!(
            f(Some("shop=car"), Some("shop=car_repair")),
            CATEGORY_PAIR_FACTOR
        );
        assert_eq!(
            f(Some("shop=car_repair"), Some("shop=car")),
            CATEGORY_PAIR_FACTOR
        );
        // Removed by hand: a Western Union counter inside a supermarket.
        assert_eq!(
            f(Some("amenity=money_transfer"), Some("shop=supermarket")),
            0.0
        );
        assert_eq!(f(Some("shop=florist"), Some("shop=supermarket")), 0.0);
        assert_eq!(f(Some("shop=car_repair"), Some("shop=car;car_repair")), 1.0);
        assert_eq!(
            f(Some("shop=car_repair"), Some("shop=car; tyres")),
            CATEGORY_PAIR_FACTOR
        );
        assert_eq!(f(Some("shop=florist"), Some("shop=car;car_repair")), 0.0);
        assert_eq!(f(None, Some("shop=car")), 0.0);
        assert_eq!(f(None, None), 0.0);
    }

    #[test]
    fn test_categories_agree() {
        assert!(categories_agree("amenity=pharmacy", "amenity=pharmacy"));
        assert!(categories_agree("amenity=pharmacy", "shop=chemist"));
        assert!(categories_agree("shop=chemist", "amenity=pharmacy"));
        // From brand:wikidata matches.
        assert!(categories_agree(
            "shop=mobile_phone",
            "shop=telecommunication"
        ));
        assert!(categories_agree(
            "shop=supermarket",
            "shop=department_store"
        ));
        // The shop at a fuel station is a feature of its own.
        assert!(!categories_agree("amenity=fuel", "shop=convenience"));
        // A pharmacy counter inside a supermarket, too.
        assert!(!categories_agree("shop=supermarket", "amenity=pharmacy"));
        assert!(!categories_agree("shop=supermarket", "shop=garden_centre"));
    }

    #[test]
    fn test_category_pairs_file() {
        for (a, b) in CATEGORY_PAIRS.iter() {
            assert!(a.contains('='), "{a}");
            assert!(b.contains('='), "{b}");
            assert_ne!(a, b);
        }
        assert!(CATEGORY_PAIRS.len() >= 2 * 40);
    }
}
