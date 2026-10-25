//! Phone numbers and email addresses as matching evidence.
//!
//! A phone number or email address that an AllThePlaces feature and an
//! OpenStreetMap candidate both carry is good evidence that they
//! describe the same *site*, but not necessarily the same *feature*: a
//! flower shop inside a supermarket often lists the supermarket's
//! number, and a hotel bar the hotel's. So a contact match also needs
//! compatible main categories, see [category_factor]. Values that
//! AllThePlaces lists for places far apart (hotlines, central
//! mailboxes) are nondescript and never count; see [Nondescript] and
//! `pipeline::atp::nondescript`. For the background, see
//! `docs/notes/2026-10-25-phone-and-email-matching.md`.

use crate::tables::U64Set;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;
use std::sync::LazyLock;

mod emails;
mod phones;

#[cfg(test)]
pub(crate) use emails::email_hash;
pub(crate) use emails::{is_email_key, parse_emails};
pub(crate) use phones::{is_phone_key, parse_phone_numbers};

/// Score factor of a contact match whose main categories are equal.
/// Below 1.0, so a `brand:wikidata` match at the same distance wins.
pub const CONTACT_FACTOR: f64 = 0.98;

/// Extra factor for a contact match whose main categories are a known
/// pair, see [category_factor].
pub const CATEGORY_PAIR_FACTOR: f64 = 0.95;

/// Phone numbers and email addresses that AllThePlaces lists for
/// places far apart, as written by `import_atp`. A shared value in one
/// of these sets is no evidence for a match.
pub struct Nondescript {
    phones: Option<U64Set>,
    emails: Option<U64Set>,
}

impl Nondescript {
    /// Opens the two sets written by `import_atp`.
    pub fn open(phones: &Path, emails: &Path) -> Result<Nondescript> {
        let open = |path: &Path| {
            U64Set::open(path).with_context(|| format!("could not open {}", path.display()))
        };
        Ok(Nondescript {
            phones: Some(open(phones)?),
            emails: Some(open(emails)?),
        })
    }

    /// No nondescript values at all, for tests.
    #[cfg(test)]
    pub fn none() -> Nondescript {
        Nondescript {
            phones: None,
            emails: None,
        }
    }

    fn is_phone(&self, number: u64) -> bool {
        self.phones.as_ref().is_some_and(|s| s.contains(number))
    }

    fn is_email(&self, hash: u64) -> bool {
        self.emails.as_ref().is_some_and(|s| s.contains(hash))
    }
}

/// The descriptive phone numbers and email addresses of an
/// AllThePlaces feature.
#[derive(Debug, Default)]
pub struct AtpContacts {
    /// Sorted, as encoded by [parse_phone_numbers].
    phones: Vec<u64>,
    /// Sorted, as encoded by [parse_emails].
    emails: Vec<u64>,
    /// From `addr:country`, for parsing OSM numbers in national format.
    country: Option<phonenumber::country::Id>,
}

impl AtpContacts {
    pub fn from_tags<'a>(
        tags: impl Iterator<Item = (&'a str, &'a str)>,
        nondescript: &Nondescript,
    ) -> AtpContacts {
        let mut c = AtpContacts::default();
        for (k, v) in tags {
            if is_phone_key(k) {
                c.phones
                    .extend(parse_phone_numbers(v).filter(|n| !nondescript.is_phone(*n)));
            } else if is_email_key(k) {
                c.emails
                    .extend(parse_emails(v).filter(|h| !nondescript.is_email(*h)));
            } else if k == "addr:country" {
                c.country = phonenumber::country::Id::from_str(v.trim()).ok();
            }
        }
        c.phones.sort_unstable();
        c.phones.dedup();
        c.emails.sort_unstable();
        c.emails.dedup();
        c
    }

    pub fn is_empty(&self) -> bool {
        self.phones.is_empty() && self.emails.is_empty()
    }

    /// Whether `osm_tags` carry one of our phone numbers or emails.
    pub fn agrees<'a>(&self, osm_tags: impl Iterator<Item = (&'a str, &'a str)>) -> bool {
        for (k, v) in osm_tags {
            let hit = if is_phone_key(k) {
                !self.phones.is_empty()
                    && v.split(';')
                        .filter_map(|part| parse_osm_phone(part, self.country))
                        .any(|n| self.phones.binary_search(&n).is_ok())
            } else if is_email_key(k) {
                !self.emails.is_empty()
                    && parse_emails(v).any(|h| self.emails.binary_search(&h).is_ok())
            } else {
                false
            };
            if hit {
                return true;
            }
        }
        false
    }
}

/// Parses one OSM phone value. Most are in international format, which
/// [parse_phone_numbers] reads quickly. The rest, such as `044 417 77
/// 77`, go through the `phonenumber` crate in the AllThePlaces
/// feature's country. That is slow (about 60 µs a call) but rare: on
/// the planet, it adds about one second in all. In some countries,
/// such as India, Thailand or Japan, a fifth or more of OSM's numbers
/// are in national format.
fn parse_osm_phone(part: &str, country: Option<phonenumber::country::Id>) -> Option<u64> {
    let part = part.trim();
    if part.starts_with('+') {
        return parse_phone_numbers(part).next();
    }
    let number = phonenumber::parse(country, part).ok()?;
    if !phonenumber::is_valid(&number) {
        return None;
    }
    let e164 = number.format().mode(phonenumber::Mode::E164).to_string();
    parse_phone_numbers(&e164).next()
}

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
/// Inferred from a planet run by `scripts/infer_contact_category_pairs.py`
/// and then reviewed by hand; see the file's header.
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

/// How well two main categories agree, for a contact match: 1.0 if
/// they are equal, [CATEGORY_PAIR_FACTOR] if they are a known pair,
/// and 0.0 otherwise, also when either side has no main category.
pub fn category_factor(atp: Option<&str>, osm: Option<&str>) -> f64 {
    match (atp, osm) {
        (Some(a), Some(o)) if a == o => 1.0,
        (Some(a), Some(o)) if CATEGORY_PAIRS.contains(&(a.to_string(), o.to_string())) => {
            CATEGORY_PAIR_FACTOR
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
        assert_eq!(f(None, Some("shop=car")), 0.0);
        assert_eq!(f(None, None), 0.0);
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

    #[test]
    fn test_parse_osm_phone() {
        let ch = Some(phonenumber::country::Id::CH);
        assert_eq!(parse_osm_phone("044 417 77 77", ch), Some(41444177777));
        assert_eq!(
            parse_osm_phone(" +41 44 417 77 77", None),
            Some(41444177777)
        );
        assert_eq!(parse_osm_phone("12", ch), None);
        assert_eq!(parse_osm_phone("044 417 77 77", None), None);
    }

    #[test]
    fn test_agrees() {
        let atp = [
            ("addr:country", "CH"),
            ("phone", "+41 44 417 77 77"),
            ("email", "Info@Example.ch"),
        ];
        let c = AtpContacts::from_tags(atp.into_iter(), &Nondescript::none());
        assert!(c.agrees([("phone", "+41 44 417 77 77")].into_iter()));
        assert!(c.agrees([("contact:phone", "044 417 77 77")].into_iter()));
        assert!(c.agrees([("fax", "+41 1 234 56 78; +41 44 417 77 77")].into_iter()));
        assert!(c.agrees([("contact:email", "info@example.ch")].into_iter()));
        assert!(!c.agrees([("phone", "+41 44 417 77 78")].into_iter()));
        assert!(!c.agrees([("name", "+41 44 417 77 77")].into_iter()));
        assert!(!c.agrees([("email", "sales@example.ch")].into_iter()));
    }

    #[test]
    fn test_nondescript() -> Result<()> {
        let dir = TempDir::new()?;
        let phones = dir.path().join("phones");
        let emails = dir.path().join("emails");
        U64Set::create([41444177777].into_iter(), dir.path(), &phones, 1 << 20)?;
        U64Set::create(
            [email_hash("info@example.ch")].into_iter(),
            dir.path(),
            &emails,
            1 << 20,
        )?;
        let nondescript = Nondescript::open(&phones, &emails)?;
        let atp = [("phone", "+41 44 417 77 77"), ("email", "info@example.ch")];
        let c = AtpContacts::from_tags(atp.into_iter(), &nondescript);
        assert!(c.is_empty());

        let atp = [("phone", "+41 44 417 77 77; +41 55 210 63 10")];
        let c = AtpContacts::from_tags(atp.into_iter(), &nondescript);
        assert_eq!(c.phones, [41552106310]);
        assert!(!c.agrees([("phone", "+41 44 417 77 77")].into_iter()));
        Ok(())
    }
}
