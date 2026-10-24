//! Phone numbers and email addresses as matching evidence, for
//! experiments (see `PoiMatcher`).
//!
//! A phone number or email address that both an AllThePlaces feature
//! and an OSM candidate carry is good evidence that they describe the
//! same *site*, but not necessarily the same *feature*: a flower shop
//! inside a supermarket often lists the supermarket's number, and a
//! hotel bar the hotel's. So a contact match also needs compatible
//! categories, see [category_factor]. Values that AllThePlaces lists
//! for places far apart (hotlines, central mailboxes) are nondescript
//! and never count; see `pipeline::atp::nondescript`.

use crate::pipeline::nondescript::{
    email_hash, is_email_key, is_phone_key, normalize_email, parse_phone_numbers,
};
use crate::tables::U64Set;
use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::time::Instant;

/// Which contact values count as evidence, from `OSMDIFFS_CONTACT_MATCH`
/// (`off`, `phone`, `email` or `both`; default `off`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactMode {
    pub phone: bool,
    pub email: bool,
}

impl ContactMode {
    pub fn enabled(&self) -> bool {
        self.phone || self.email
    }
}

pub static CONTACT_MODE: LazyLock<ContactMode> =
    LazyLock::new(
        || match std::env::var("OSMDIFFS_CONTACT_MATCH").as_deref() {
            Ok("phone") => ContactMode {
                phone: true,
                email: false,
            },
            Ok("email") => ContactMode {
                phone: false,
                email: true,
            },
            Ok("both") => ContactMode {
                phone: true,
                email: true,
            },
            _ => ContactMode {
                phone: false,
                email: false,
            },
        },
    );

/// How OSM phone values are parsed, from `OSMDIFFS_CONTACT_PHONE_PARSE`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PhoneParse {
    /// Only international `+…` values, with the same fast parser as the
    /// ATP side (default).
    Plus,
    /// `+…` values with the fast parser; anything else with the
    /// `phonenumber` crate, in the ATP feature's `addr:country`.
    Lib,
    /// Every OSM value with the `phonenumber` crate, to measure its cost.
    LibAll,
}

pub static PHONE_PARSE: LazyLock<PhoneParse> =
    LazyLock::new(
        || match std::env::var("OSMDIFFS_CONTACT_PHONE_PARSE").as_deref() {
            Ok("lib") => PhoneParse::Lib,
            Ok("lib-all") => PhoneParse::LibAll,
            _ => PhoneParse::Plus,
        },
    );

/// How categories must agree for a contact match, from
/// `OSMDIFFS_CONTACT_CATEGORY` (`any`, `exact`, or `group`, default).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CategoryRule {
    Any,
    Exact,
    Group,
}

pub static CATEGORY_RULE: LazyLock<CategoryRule> =
    LazyLock::new(
        || match std::env::var("OSMDIFFS_CONTACT_CATEGORY").as_deref() {
            Ok("any") => CategoryRule::Any,
            Ok("exact") => CategoryRule::Exact,
            _ => CategoryRule::Group,
        },
    );

/// Score factor of a contact match whose categories are equal. Below
/// [super::poi_matcher]'s brand-tag factor, so brand evidence at the
/// same distance wins.
pub const CONTACT_FACTOR: f64 = 0.98;

/// Extra factor for a contact match whose categories are only in the
/// same group, see [category_factor].
pub const GROUP_FACTOR: f64 = 0.95;

/// The nondescript sets written by `import_atp`, loaded once per run.
pub struct Nondescript {
    phones: Option<U64Set>,
    emails: Option<U64Set>,
}

static NONDESCRIPT: OnceLock<Nondescript> = OnceLock::new();

/// Opens the nondescript sets in `workdir`, if present, for this run.
pub fn load_nondescript(workdir: &Path) {
    let open = |name: &str| {
        let path = workdir.join(name);
        match U64Set::open(&path) {
            Ok(set) => Some(set),
            Err(err) => {
                log::warn!(path:? = path, err:% = err; "contact: no nondescript set");
                None
            }
        }
    };
    let _ = NONDESCRIPT.set(Nondescript {
        phones: open("alltheplaces.nondescript-phones"),
        emails: open("alltheplaces.nondescript-emails"),
    });
}

fn is_nondescript_phone(n: u64) -> bool {
    NONDESCRIPT
        .get()
        .and_then(|s| s.phones.as_ref())
        .is_some_and(|set| set.contains(n))
}

fn is_nondescript_email(h: u64) -> bool {
    NONDESCRIPT
        .get()
        .and_then(|s| s.emails.as_ref())
        .is_some_and(|set| set.contains(h))
}

/// Descriptive phone numbers and email hashes of an ATP feature.
#[derive(Debug, Default)]
pub struct AtpContacts {
    pub phones: Vec<u64>,
    pub emails: Vec<u64>,
    /// `addr:country`, for parsing national OSM numbers.
    pub country: Option<phonenumber::country::Id>,
}

impl AtpContacts {
    pub fn from_tags<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> AtpContacts {
        let mode = *CONTACT_MODE;
        let mut c = AtpContacts::default();
        for (k, v) in tags {
            if mode.phone && is_phone_key(k) {
                c.phones
                    .extend(parse_phone_numbers(v).filter(|n| !is_nondescript_phone(*n)));
            } else if mode.email && is_email_key(k) {
                c.emails.extend(
                    v.split([';', ','])
                        .filter_map(normalize_email)
                        .map(|e| email_hash(&e))
                        .filter(|h| !is_nondescript_email(*h)),
                );
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
            if !self.phones.is_empty() && is_phone_key(k) {
                if self
                    .osm_phones(v)
                    .any(|n| self.phones.binary_search(&n).is_ok())
                {
                    return true;
                }
            } else if !self.emails.is_empty() && is_email_key(k) {
                let hit = v
                    .split([';', ','])
                    .filter_map(normalize_email)
                    .any(|e| self.emails.binary_search(&email_hash(&e)).is_ok());
                if hit {
                    return true;
                }
            }
        }
        false
    }

    fn osm_phones<'v>(&self, value: &'v str) -> impl Iterator<Item = u64> + 'v {
        let parse = *PHONE_PARSE;
        let country = self.country;
        value.split(';').filter_map(move |part| {
            let part = part.trim();
            let fast = || parse_phone_numbers(part).next();
            match parse {
                PhoneParse::Plus => fast(),
                PhoneParse::Lib if part.starts_with('+') => fast(),
                PhoneParse::Lib | PhoneParse::LibAll => lib_parse(part, country),
            }
        })
    }
}

/// Calls of the `phonenumber` crate, and the time spent in them.
pub static LIB_CALLS: AtomicU64 = AtomicU64::new(0);
pub static LIB_NANOS: AtomicU64 = AtomicU64::new(0);
pub static LIB_VALID: AtomicU64 = AtomicU64::new(0);

/// Parses `part` with the `phonenumber` crate and encodes a valid
/// number like `parse_phone_numbers` does: the digits of its E.164
/// form as a `u64`.
fn lib_parse(part: &str, country: Option<phonenumber::country::Id>) -> Option<u64> {
    let start = Instant::now();
    let result = phonenumber::parse(country, part)
        .ok()
        .filter(phonenumber::is_valid)
        .and_then(|n| {
            let e164 = n.format().mode(phonenumber::Mode::E164).to_string();
            parse_phone_numbers(&e164).next()
        });
    LIB_CALLS.fetch_add(1, Ordering::Relaxed);
    LIB_NANOS.fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    if result.is_some() {
        LIB_VALID.fetch_add(1, Ordering::Relaxed);
    }
    result
}

pub fn log_stats() {
    let calls = LIB_CALLS.load(Ordering::Relaxed);
    let nanos = LIB_NANOS.load(Ordering::Relaxed);
    log::info!(
        phonenumber_calls = calls,
        phonenumber_valid = LIB_VALID.load(Ordering::Relaxed),
        phonenumber_seconds = nanos as f64 / 1e9,
        phonenumber_micros_per_call = if calls > 0 { nanos as f64 / calls as f64 / 1e3 } else { 0.0 };
        "contact: phonenumber crate"
    );
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

/// A feature's main category as `key=value`, e.g. `shop=florist`.
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

/// Pairs of categories that the same business often gets on the two
/// sides, inferred from contact matches whose names agree; see
/// `scripts/infer_contact_category_pairs.py`.
static CATEGORY_PAIRS: LazyLock<HashSet<(String, String)>> = LazyLock::new(|| {
    let mut pairs = HashSet::new();
    for line in include_str!("contact_category_pairs.tsv").lines() {
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

/// How well the main categories agree, for a contact match: 1.0 if
/// equal, [GROUP_FACTOR] if a known pair, 0.0 otherwise -- depending
/// on [CATEGORY_RULE].
pub fn category_factor(atp: Option<&str>, osm: Option<&str>) -> f64 {
    match *CATEGORY_RULE {
        CategoryRule::Any => 1.0,
        rule => match (atp, osm) {
            (Some(a), Some(o)) if a == o => 1.0,
            (Some(a), Some(o))
                if rule == CategoryRule::Group
                    && CATEGORY_PAIRS.contains(&(a.to_string(), o.to_string())) =>
            {
                GROUP_FACTOR
            }
            _ => 0.0,
        },
    }
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
    fn test_lib_parse() {
        let ch = Some(phonenumber::country::Id::CH);
        assert_eq!(lib_parse("044 417 77 77", ch), Some(41444177777));
        assert_eq!(lib_parse("+41 44 417 77 77", None), Some(41444177777));
        assert_eq!(lib_parse("12", ch), None);
    }
}
