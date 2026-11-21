//! Phone numbers, email addresses and websites as matching evidence.
//!
//! A phone number, email address or website that an AllThePlaces
//! feature and an OpenStreetMap candidate both carry is good evidence that they
//! describe the same *site*, but not necessarily the same *feature*: a
//! flower shop inside a supermarket often lists the supermarket's
//! number, and a hotel bar the hotel's. So a contact match also needs
//! compatible main categories, see [super::category::category_factor]. Values that
//! AllThePlaces lists for places far apart (hotlines, central
//! mailboxes, a brand's home page) are nondescript and never count; see
//! [Nondescript] and `pipeline::atp::nondescript`. For the background,
//! see `docs/notes/2026-10-25-phone-and-email-matching.md` and
//! `docs/notes/2026-11-06-website-matching.md`.

use crate::tables::U64Set;
use crate::utils::countries;
use anyhow::{Context, Result};
use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;

mod emails;
mod phones;
mod urls;

#[cfg(test)]
pub(crate) use emails::email_hash;
pub(crate) use emails::{is_email_key, normalize_email, parse_emails};
pub(crate) use phones::{is_phone_key, parse_phone_number, parse_phone_numbers};
#[cfg(test)]
pub(crate) use urls::url_hash;
pub(crate) use urls::{is_website_key, normalize_url, parse_urls, parse_website_domains};

/// Score factor of a contact match whose main categories are equal.
/// Below 1.0, so a `brand:wikidata` match at the same distance wins.
pub const CONTACT_FACTOR: f64 = 0.98;

/// Phone numbers, email addresses and websites that AllThePlaces lists
/// for places far apart, as written by `import_atp`. A shared value in
/// one of these sets is no evidence for a match.
pub struct Nondescript {
    phones: Option<U64Set>,
    emails: Option<U64Set>,
    websites: Option<U64Set>,
}

impl Nondescript {
    /// Opens the three sets written by `import_atp`.
    pub fn open(phones: &Path, emails: &Path, websites: &Path) -> Result<Nondescript> {
        let open = |path: &Path| {
            U64Set::open(path).with_context(|| format!("could not open {}", path.display()))
        };
        Ok(Nondescript {
            phones: Some(open(phones)?),
            emails: Some(open(emails)?),
            websites: Some(open(websites)?),
        })
    }

    /// No nondescript values at all, for tests, and for telling
    /// whether two contacts agree at all, as `conflate::evidence` does.
    pub fn none() -> Nondescript {
        Nondescript {
            phones: None,
            emails: None,
            websites: None,
        }
    }

    fn is_phone(&self, number: u64) -> bool {
        self.phones.as_ref().is_some_and(|s| s.contains(number))
    }

    fn is_email(&self, hash: u64) -> bool {
        self.emails.as_ref().is_some_and(|s| s.contains(hash))
    }

    fn is_website(&self, hash: u64) -> bool {
        self.websites.as_ref().is_some_and(|s| s.contains(hash))
    }
}

/// The descriptive phone numbers, email addresses and websites of an
/// AllThePlaces feature.
#[derive(Debug, Default)]
pub struct AtpContacts {
    /// Sorted, as encoded by [parse_phone_numbers].
    phones: Vec<u64>,
    /// Sorted, as encoded by [parse_emails].
    emails: Vec<u64>,
    /// Sorted, as encoded by [parse_urls].
    websites: Vec<u64>,
    /// From `addr:country`, for parsing OSM numbers in national format.
    country: Option<phonenumber::country::Id>,
    /// Where the feature lies, as (longitude, latitude) in degrees.
    position: Option<(f64, f64)>,
    /// The country at `position`, looked up on first use; see
    /// [AtpContacts::located_country].
    located: OnceLock<Option<phonenumber::country::Id>>,
}

impl AtpContacts {
    /// The contacts in `tags` of a feature at `position`, given as
    /// (longitude, latitude) in degrees.
    pub fn from_tags<'a>(
        tags: impl Iterator<Item = (&'a str, &'a str)>,
        position: Option<(f64, f64)>,
        nondescript: &Nondescript,
    ) -> AtpContacts {
        let mut c = AtpContacts {
            position,
            ..AtpContacts::default()
        };
        for (k, v) in tags {
            if is_phone_key(k) {
                c.phones
                    .extend(parse_phone_numbers(v).filter(|n| !nondescript.is_phone(*n)));
            } else if is_email_key(k) {
                c.emails
                    .extend(parse_emails(v).filter(|h| !nondescript.is_email(*h)));
            } else if is_website_key(k) {
                c.websites
                    .extend(parse_urls(v).filter(|h| !nondescript.is_website(*h)));
            } else if k == "addr:country" {
                c.country = phonenumber::country::Id::from_str(v.trim()).ok();
            }
        }
        c.phones.sort_unstable();
        c.phones.dedup();
        c.emails.sort_unstable();
        c.emails.dedup();
        c.websites.sort_unstable();
        c.websites.dedup();
        c
    }

    pub fn is_empty(&self) -> bool {
        self.phones.is_empty() && self.emails.is_empty() && self.websites.is_empty()
    }

    /// Whether there is a phone number or email address, not just a
    /// website.
    pub fn has_phone_or_email(&self) -> bool {
        !self.phones.is_empty() || !self.emails.is_empty()
    }

    /// Whether `osm_tags` carry one of our phone numbers, emails or
    /// websites.
    pub fn agrees<'a>(&self, osm_tags: impl Iterator<Item = (&'a str, &'a str)>) -> bool {
        for (k, v) in osm_tags {
            let hit = if is_phone_key(k) {
                !self.phones.is_empty() && v.split(';').any(|part| self.has_osm_phone(part))
            } else if is_email_key(k) {
                !self.emails.is_empty()
                    && parse_emails(v).any(|h| self.emails.binary_search(&h).is_ok())
            } else if is_website_key(k) {
                !self.websites.is_empty()
                    && parse_urls(v).any(|h| self.websites.binary_search(&h).is_ok())
            } else {
                false
            };
            if hit {
                return true;
            }
        }
        false
    }

    fn has_phone(&self, number: u64) -> bool {
        self.phones.binary_search(&number).is_ok()
    }

    /// Whether one OSM phone value is one of our numbers. Most OSM
    /// values are in international format, which [parse_phone_number]
    /// reads quickly. The rest, such as `044 417 77 77`, go through the
    /// `phonenumber` crate, first in the country of our `addr:country`
    /// and then, if that gives none of our numbers, in the country
    /// where the feature lies, if that is another one.
    ///
    /// The second try helps where `addr:country` is wrong, as for
    /// spiders that put one country on all their shops, or where the
    /// phone system differs from `addr:country`, as for France and its
    /// overseas departments. On the 2026-10-10 AllThePlaces run, the
    /// two countries differ for 0.23% of the features with a phone
    /// number. Trying `addr:country` first keeps the places where
    /// phone systems do not follow borders, such as Crimea or the Basel
    /// airport, and trying both cannot match a number that is not ours.
    fn has_osm_phone(&self, part: &str) -> bool {
        let part = part.trim();
        if part.starts_with('+') {
            return parse_phone_number(part).is_some_and(|n| self.has_phone(n));
        }
        if parse_national_phone(part, self.country).is_some_and(|n| self.has_phone(n)) {
            return true;
        }
        match self.located_country() {
            Some(located) if Some(located) != self.country => {
                parse_national_phone(part, Some(located)).is_some_and(|n| self.has_phone(n))
            }
            _ => false,
        }
    }

    /// The country at our position, for [AtpContacts::has_osm_phone].
    /// Looked up only for OSM numbers in national format, which are
    /// rare, so most features never pay for the lookup.
    fn located_country(&self) -> Option<phonenumber::country::Id> {
        *self.located.get_or_init(|| {
            let (lng, lat) = self.position?;
            let country = countries::country(lng, lat)?;
            phonenumber::country::Id::from_str(country.code()).ok()
        })
    }
}

/// Parses one phone number in national format, such as `044 417 77
/// 77`, in `country`. The `phonenumber` crate is slow (about 60 µs a
/// call), but national format is rare: on the planet, it adds about one
/// second in all. In some countries, such as India, Thailand or Japan,
/// a fifth or more of OSM's numbers are in national format.
fn parse_national_phone(part: &str, country: Option<phonenumber::country::Id>) -> Option<u64> {
    let number = phonenumber::parse(country, part).ok()?;
    if !phonenumber::is_valid(&number) {
        return None;
    }
    let e164 = number.format().mode(phonenumber::Mode::E164).to_string();
    parse_phone_number(&e164)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_national_phone() {
        let ch = Some(phonenumber::country::Id::CH);
        assert_eq!(parse_national_phone("044 417 77 77", ch), Some(41444177777));
        assert_eq!(parse_national_phone("12", ch), None);
        assert_eq!(parse_national_phone("044 417 77 77", None), None);
    }

    #[test]
    fn test_agrees_located_country() {
        // Vaduz, Liechtenstein, but `addr:country` says Switzerland.
        let vaduz = Some((9.5209, 47.1410));
        let atp = [("addr:country", "CH"), ("phone", "+423 236 60 00")];
        let c = AtpContacts::from_tags(atp.into_iter(), vaduz, &Nondescript::none());
        assert!(c.agrees([("phone", "236 60 00")].into_iter()));
        assert!(!c.agrees([("phone", "236 60 01")].into_iter()));

        // Without a position, we can only try `addr:country`.
        let c = AtpContacts::from_tags(atp.into_iter(), None, &Nondescript::none());
        assert!(!c.agrees([("phone", "236 60 00")].into_iter()));

        // No `addr:country`: the position alone.
        let atp = [("phone", "+423 236 60 00")];
        let c = AtpContacts::from_tags(atp.into_iter(), vaduz, &Nondescript::none());
        assert!(c.agrees([("phone", "236 60 00")].into_iter()));

        // `addr:country` still comes first: Büsingen lies in Germany
        // but also has Swiss numbers.
        let busingen = Some((8.6900, 47.6960));
        let atp = [("addr:country", "CH"), ("phone", "+41 52 625 00 00")];
        let c = AtpContacts::from_tags(atp.into_iter(), busingen, &Nondescript::none());
        assert!(c.agrees([("phone", "052 625 00 00")].into_iter()));
    }

    #[test]
    fn test_agrees() {
        let atp = [
            ("addr:country", "CH"),
            ("phone", "+41 44 417 77 77"),
            ("email", "Info@Example.ch"),
            ("website", "https://www.example.ch/de/filialen/zuerich/"),
        ];
        let c = AtpContacts::from_tags(atp.into_iter(), None, &Nondescript::none());
        assert!(c.has_phone_or_email());
        let website_only = [("website", "https://www.example.ch/de/filialen/zuerich/")];
        let w = AtpContacts::from_tags(website_only.into_iter(), None, &Nondescript::none());
        assert!(!w.is_empty() && !w.has_phone_or_email());
        assert!(c.agrees([("phone", "+41 44 417 77 77")].into_iter()));
        assert!(c.agrees([("contact:phone", "044 417 77 77")].into_iter()));
        assert!(c.agrees([("fax", "+41 1 234 56 78; +41 44 417 77 77")].into_iter()));
        assert!(c.agrees([("contact:email", "info@example.ch")].into_iter()));
        assert!(!c.agrees([("phone", "+41 44 417 77 78")].into_iter()));
        assert!(!c.agrees([("name", "+41 44 417 77 77")].into_iter()));
        assert!(!c.agrees([("email", "sales@example.ch")].into_iter()));
        assert!(c.agrees([("website", "http://example.ch/de/filialen/zuerich")].into_iter()));
        assert!(
            c.agrees([("contact:website", "https://EXAMPLE.ch/de/Filialen/Zuerich")].into_iter())
        );
        assert!(c.agrees([("url", "example.ch/de/filialen/z%75erich")].into_iter()));
        assert!(!c.agrees([("website", "https://www.example.ch/")].into_iter()));
        assert!(
            !c.agrees([("website:menu", "https://www.example.ch/de/filialen/zuerich")].into_iter())
        );
    }

    #[test]
    fn test_nondescript() -> Result<()> {
        let dir = TempDir::new()?;
        let phones = dir.path().join("phones");
        let emails = dir.path().join("emails");
        let websites = dir.path().join("websites");
        U64Set::create([41444177777].into_iter(), dir.path(), &phones, 1 << 20)?;
        U64Set::create(
            [url_hash("example.ch")].into_iter(),
            dir.path(),
            &websites,
            1 << 20,
        )?;
        U64Set::create(
            [email_hash("info@example.ch")].into_iter(),
            dir.path(),
            &emails,
            1 << 20,
        )?;
        let nondescript = Nondescript::open(&phones, &emails, &websites)?;
        let atp = [
            ("phone", "+41 44 417 77 77"),
            ("email", "info@example.ch"),
            ("website", "https://www.example.ch/"),
        ];
        let c = AtpContacts::from_tags(atp.into_iter(), None, &nondescript);
        assert!(c.is_empty());

        let atp = [("phone", "+41 44 417 77 77; +41 55 210 63 10")];
        let c = AtpContacts::from_tags(atp.into_iter(), None, &nondescript);
        assert_eq!(c.phones, [41552106310]);
        assert!(!c.agrees([("phone", "+41 44 417 77 77")].into_iter()));

        let atp = [("website", "https://example.ch/; https://example.ch/zug")];
        let c = AtpContacts::from_tags(atp.into_iter(), None, &nondescript);
        assert_eq!(c.websites, [url_hash("example.ch/zug")]);
        assert!(!c.agrees([("website", "https://example.ch")].into_iter()));
        Ok(())
    }
}
