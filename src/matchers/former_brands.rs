//! Brands that a chain had before it was rebranded, by country, so
//! that an OpenStreetMap feature still tagged with the old brand can
//! match the AllThePlaces feature of the new one (brawer/osmdiffs#990).
//!
//! When a chain is rebranded, AllThePlaces has the new brand right
//! away, but OpenStreetMap keeps the old one for years: on the
//! 2026-11-07 run, only 14% of the Japanese apollostation stations
//! matched, as many are still `出光` (Idemitsu) or `シェル` (Showa
//! Shell) in OpenStreetMap.
//!
//! The table is kept by hand, not built from Wikidata ("replaced by",
//! "followed by"): Wikidata also records partial takeovers, where only
//! some branches changed brand, and those would give wrong matches.
//! Each entry holds for one country only, because a replaced brand
//! can live on elsewhere: Shell is still Shell outside Japan.
//! See `docs/notes/2026-11-18-former-brands.md` for the evaluation.

use crate::utils::countries::Country;

/// A rebranding: in `country`, the brand `new` replaced the brands
/// `old`, all given as Wikidata IDs.
struct Rebranding {
    country: Country,
    new: u64,
    old: &'static [u64],
}

/// Before you add an entry, check that the old brand is gone for good
/// in that country, not just at some of its branches.
const REBRANDINGS: &[Rebranding] = &[
    // apollostation: the stations of Idemitsu (Q2216770)
    // and Showa Shell. OpenStreetMap tags Showa Shell both as Showa
    // Shell Sekiyu (Q277115) and, following the name-suggestion-index
    // entry `showashell-fbe2e4`, as Shell (Q110716465).
    Rebranding {
        country: Country::JAPAN,
        new: 114731101,
        old: &[2216770, 277115, 110716465],
    },
    // 三十三銀行 (San ju San Bank): the branches of
    // 三重銀行 (Mie Bank).
    Rebranding {
        country: Country::JAPAN,
        new: 106781309,
        old: &[11358192],
    },
];

/// Whether any of `new` (Wikidata IDs) has a former brand in some
/// country. Cheap, so that callers can skip looking up the country
/// for the many brands without one.
pub fn has_former_brands(new: &[u64]) -> bool {
    REBRANDINGS.iter().any(|r| new.contains(&r.new))
}

/// The Wikidata IDs of the brands that `new` (Wikidata IDs) replaced
/// in `country`; empty if the country isn't known.
pub fn former_brands(new: &[u64], country: Option<Country>) -> Vec<u64> {
    let Some(country) = country else {
        return Vec::new();
    };
    REBRANDINGS
        .iter()
        .filter(|r| r.country == country && new.contains(&r.new))
        .flat_map(|r| r.old.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebrandings_are_valid() {
        for r in REBRANDINGS {
            assert!(
                Country::from_code(r.country.code_bytes()).is_some(),
                "unknown country {}",
                r.country.code()
            );
            assert!(!r.old.is_empty());
            assert!(!r.old.contains(&r.new), "Q{} replaces itself", r.new);
        }
    }

    #[test]
    fn former_brands_by_country() {
        assert!(has_former_brands(&[114731101]));
        assert!(!has_former_brands(&[110716465]));
        assert_eq!(
            former_brands(&[114731101], Some(Country::JAPAN)),
            vec![2216770, 277115, 110716465]
        );
        assert_eq!(
            former_brands(&[106781309], Some(Country::JAPAN)),
            vec![11358192]
        );
        // Shell is still Shell outside Japan.
        assert!(former_brands(&[114731101], Some(Country::GERMANY)).is_empty());
        assert!(former_brands(&[114731101], None).is_empty());
        // Shell did not replace apollostation.
        assert!(former_brands(&[110716465], Some(Country::JAPAN)).is_empty());
    }
}
