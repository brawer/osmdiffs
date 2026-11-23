//! Modifies of `phone`, `fax`, `email` and `website`; see "Modify:
//! contact tags" in `docs/notes/2026-11-23-edits-parquet.md`.

use super::agreement::values_agree;
use super::{Change, Reason, Tags};
use crate::matchers::contact::{Nondescript, normalize_email, normalize_url, registrable_domain};
use std::collections::{HashMap, HashSet};

/// How many OSM features may carry one value of a family before we
/// take it for a value that says nothing about one place, such as
/// `https://www.europcar.com/en-db` on every Europcar branch.
const MAX_OSM_SHARE: usize = 2;

/// The tag families we edit. Each has a plain key, such as `phone`,
/// and a `contact:` form, such as `contact:phone`.
pub(super) const FAMILIES: [&str; 4] = ["phone", "fax", "email", "website"];

/// What one AllThePlaces source says about a place's contacts.
pub(super) struct ContactSource<'a> {
    pub tags: &'a [(String, String)],
}

impl ContactSource<'_> {
    /// ATP's value for `family`, in its plain or `contact:` form.
    fn value(&self, family: &str) -> Option<&str> {
        let contact = format!("contact:{family}");
        self.tags
            .iter()
            .find(|(k, _)| k == family)
            .or_else(|| self.tags.iter().find(|(k, _)| *k == contact))
            .map(|(_, v)| v.trim())
            .filter(|v| !v.is_empty())
    }

    fn country(&self) -> Option<&str> {
        self.tags
            .iter()
            .find(|(k, _)| k == "addr:country")
            .map(|(_, v)| v.as_str())
    }
}

/// The key that OSM holds a value of `family` in, and that value:
/// `contact:phone` before `phone`, and for websites also the old `url`.
fn osm_value<'a>(osm: &'a Tags, family: &str) -> Option<(&'a str, &'a str)> {
    let contact = format!("contact:{family}");
    let keys: &[&str] = if family == "website" {
        &[contact.as_str(), "website", "url"]
    } else {
        &[contact.as_str(), family]
    };
    keys.iter().find_map(|key| {
        osm.iter()
            .find(|(k, v)| k == key && !v.trim().is_empty())
            .map(|(k, v)| (k.as_str(), v.as_str()))
    })
}

/// Whether OSM writes contacts in the `contact:*` form: it has a
/// `contact:` key of one of our families, or `contact:mobile`, and no
/// plain one. Then new values go there too, so that a feature never
/// gets a second, parallel key.
fn uses_contact_form(osm: &Tags) -> bool {
    let mut contact = false;
    let mut plain = false;
    for (k, _) in osm {
        if let Some(family) = k.strip_prefix("contact:") {
            contact |= FAMILIES.contains(&family) || family == "mobile";
        } else {
            plain |= FAMILIES.contains(&k.as_str()) || k == "mobile" || k == "url";
        }
    }
    contact && !plain
}

/// Whether a website is a home page rather than a page of one store,
/// such as `https://www.migros.ch/` against
/// `https://filialen.migros.ch/de/migros-zuerich`.
fn is_home_page(value: &str) -> bool {
    value
        .split(';')
        .filter_map(normalize_url)
        .all(|url| !url.contains(['/', '?', '#']))
}

/// The OSM values of each family, as `family` and the trimmed,
/// lowercased value joined by a tab, that more than [MAX_OSM_SHARE]
/// of the features `osm` carry.
pub(super) fn shared_osm_values<'a>(osm: impl Iterator<Item = &'a Tags>) -> HashSet<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for tags in osm {
        for family in FAMILIES {
            if let Some((_, value)) = osm_value(tags, family) {
                *counts.entry(shared_key(family, value)).or_default() += 1;
            }
        }
        if let Some((_, value)) = tags.iter().find(|(k, _)| k == HERITAGE_WEBSITE) {
            *counts
                .entry(shared_key(HERITAGE_WEBSITE, value))
                .or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > MAX_OSM_SHARE)
        .map(|(k, _)| k)
        .collect()
}

/// The words of email addresses that AllThePlaces gives in more than
/// two places at one domain, in the same position of a two-word local
/// part, such as `migrol` in `migrol.sion@migrolino.com` and
/// `migrol.martigny@migrolino.com`. Such a word names the chain or the
/// store, not a person; see [is_personal_email].
pub(super) fn shared_email_words<'a>(atp: impl Iterator<Item = &'a Tags>) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for tags in atp {
        let source = ContactSource { tags };
        let Some(value) = source.value("email") else {
            continue;
        };
        for part in value.split(';') {
            let Some(email) = normalize_email(part) else {
                continue;
            };
            if !seen.insert(email.clone()) {
                continue;
            }
            for key in email_word_keys(&email) {
                *counts.entry(key).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > MAX_OSM_SHARE)
        .map(|(k, _)| k)
        .collect()
}

/// For a normalized address with a two-word local part, a key per
/// word: `email`, the domain, the word's position and the word.
fn email_word_keys(email: &str) -> Vec<String> {
    let Some((local, domain)) = email.split_once('@') else {
        return Vec::new();
    };
    let words: Vec<&str> = local.split(['.', '_']).collect();
    if words.len() != 2 {
        return Vec::new();
    }
    words
        .iter()
        .enumerate()
        .map(|(i, w)| format!("email\t{domain}\t{i}\t{w}"))
        .collect()
}

/// Spiders of official registries, such as the US National Register
/// of Historic Places. Their page about a place is a record of it, not
/// the place's own website, so it goes into `heritage:website`.
const REGISTRY_SPIDERS: &[&str] = &["national_register_of_historic_places"];

/// Whether `spider` is one of [REGISTRY_SPIDERS].
pub(super) fn is_registry(spider: &str) -> bool {
    REGISTRY_SPIDERS.contains(&spider)
}

/// The key for registry pages; see [REGISTRY_SPIDERS].
const HERITAGE_WEBSITE: &str = "heritage:website";

/// The `heritage:website` change for an OSM feature with tags `osm`,
/// given the website `values` of the registry sources that matched it.
/// Adds the page where OSM has none, and replaces OSM's only if it says
/// nothing about this one place, such as the register's home page.
pub(super) fn heritage_changes(
    osm: &Tags,
    values: &[&str],
    shared: &HashSet<String>,
    warnings: &mut Vec<String>,
) -> Vec<Change> {
    let values: Vec<&str> = values
        .iter()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .collect();
    let Some(&new) = values.first() else {
        return Vec::new();
    };
    if values[1..]
        .iter()
        .any(|v| normalize_url(v) != normalize_url(new))
    {
        warnings.push(format!("sources_disagree:{HERITAGE_WEBSITE}"));
        return Vec::new();
    }
    let old = osm
        .iter()
        .find(|(k, v)| k == HERITAGE_WEBSITE && !v.trim().is_empty())
        .map(|(_, v)| v.as_str());
    let Some(old) = old else {
        return vec![Change::new(
            HERITAGE_WEBSITE,
            None,
            Some(new.to_string()),
            Reason::MissingInOsm,
        )];
    };
    if normalize_url(old) == normalize_url(new) {
        return Vec::new();
    }
    if shared.contains(&shared_key(HERITAGE_WEBSITE, old)) || is_home_page(old) {
        return vec![Change::new(
            HERITAGE_WEBSITE,
            Some(old),
            Some(new.to_string()),
            Reason::GenericValue,
        )];
    }
    Vec::new()
}

fn shared_key(family: &str, value: &str) -> String {
    format!("{family}\t{}", value.trim().to_lowercase())
}

/// Whether OSM's `value` of `family` says nothing about one place,
/// given ATP's value `atp`: a value that AllThePlaces lists for places
/// far apart, one that several OSM features carry, or for websites,
/// the home page of the site that ATP's store page is on, such as
/// `https://www.rossmann.de` for a page under
/// `rossmann.de/de/filialen/`. The home page of another site is most
/// likely the place's own, as for a hotel or a restaurant, and stays.
fn is_generic(
    family: &str,
    value: &str,
    atp: &str,
    nondescript: &Nondescript,
    shared: &HashSet<String>,
) -> bool {
    nondescript.is_nondescript_value(family, value)
        || shared.contains(&shared_key(family, value))
        || (family == "website" && is_home_page(value) && same_site(family, atp, value))
}

/// The site of a website or email value: the registrable domain of
/// its first URL or address, such as `dunkindonuts.com` for both
/// `locations.dunkindonuts.com/en/ma/chelsea` and
/// `www.dunkindonuts.com/locations/us/ma/chelsea`.
fn site(family: &str, value: &str) -> Option<String> {
    let part = value.split(';').next()?;
    let host = match family {
        "website" => normalize_url(part)?,
        "email" => normalize_email(part)?.split_once('@')?.1.to_string(),
        _ => return None,
    };
    registrable_domain(&host)
}

/// Whether two values point to the same site, such as two pages of
/// `marriott.com` or two addresses at `tesla.com`. Then OSM's value is
/// most likely just as right, perhaps a page in another language or
/// another department's address, and replacing it would only churn.
fn same_site(family: &str, a: &str, b: &str) -> bool {
    site(family, a).is_some_and(|s| site(family, b) == Some(s))
}

/// The phone numbers of a value in E.164 form, such as `+97126120860`
/// for `+971 02 6120860`, or `None` if a part isn't a valid number.
fn valid_numbers(value: &str) -> Option<Vec<String>> {
    value
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|part| {
            let n = phonenumber::parse(None, part).ok()?;
            phonenumber::is_valid(&n).then(|| n.format().mode(phonenumber::Mode::E164).to_string())
        })
        .collect()
}

/// Whether two phone or fax values share a number once parsed, even
/// with a national trunk prefix after the country code, as OSM's
/// `+971 02 6120860` for `+971 2 612 0860`.
fn same_number(a: &str, b: &str) -> bool {
    let numbers = |v: &str| -> Vec<String> {
        v.split(';')
            .filter_map(|p| valid_numbers(p)?.pop())
            .collect()
    };
    let a = numbers(a);
    numbers(b).iter().any(|n| a.contains(n))
}

/// Whether OSM's `old` value of `family` says the same as ATP's.
fn agrees(
    family: &str,
    atp: &str,
    old: &str,
    country: Option<&str>,
    position: Option<(f64, f64)>,
) -> bool {
    values_agree(family, atp, old, country, position)
        || ((family == "phone" || family == "fax") && same_number(atp, old))
}

/// Whether a phone or fax value looks like a placeholder rather than
/// a number, such as `+254 708 000000`: one whose last six digits are
/// all the same.
fn is_placeholder(family: &str, value: &str) -> bool {
    if family != "phone" && family != "fax" {
        return false;
    }
    value.split(';').any(|part| {
        let digits: Vec<char> = part.chars().filter(char::is_ascii_digit).collect();
        digits.len() >= 6
            && digits[digits.len() - 6..]
                .iter()
                .all(|d| *d == digits[digits.len() - 1])
    })
}

/// Phone numbers in international format, grouped as is usual in the
/// country, such as `+41 44 123 45 67` for `+41441234567`. Parts that
/// don't parse stay as they are.
pub(super) fn format_phone(value: &str) -> String {
    value
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|part| match phonenumber::parse(None, part) {
            Ok(n) if phonenumber::is_valid(&n) => n
                .format()
                .mode(phonenumber::Mode::International)
                .to_string(),
            _ => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// The contact changes for an OSM feature with tags `osm` at
/// `position`, given what the AllThePlaces `sources` that matched it as
/// `same` say, in their fixed order. Adds to `warnings` the families
/// whose sources disagree.
pub(super) fn contact_changes(
    osm: &Tags,
    sources: &[ContactSource],
    nondescript: &Nondescript,
    shared: &HashSet<String>,
    position: Option<(f64, f64)>,
    warnings: &mut Vec<String>,
) -> Vec<Change> {
    let mut changes = Vec::new();
    let contact_form = uses_contact_form(osm);
    for family in FAMILIES {
        // The values that say something about this one place.
        let values: Vec<(&str, Option<&str>)> = sources
            .iter()
            .filter_map(|s| Some((s.value(family)?, s.country())))
            .filter(|(v, _)| !nondescript.is_nondescript_value(family, v))
            .filter(|(v, _)| !is_placeholder(family, v))
            .filter(|(v, _)| (family != "phone" && family != "fax") || valid_numbers(v).is_some())
            .collect();
        let Some(&(atp, country)) = values.first() else {
            continue;
        };
        if values[1..]
            .iter()
            .any(|(other, _)| !agrees(family, other, atp, country, position))
        {
            warnings.push(format!("sources_disagree:{family}"));
            continue;
        }
        let new_value = if family == "phone" || family == "fax" {
            format_phone(atp)
        } else {
            atp.to_string()
        };
        let Some((key, old)) = osm_value(osm, family) else {
            let key = if contact_form {
                format!("contact:{family}")
            } else {
                family.to_string()
            };
            changes.push(Change::new(
                &key,
                None,
                Some(new_value),
                Reason::MissingInOsm,
            ));
            continue;
        };
        if agrees(family, atp, old, country, position) {
            continue;
        }
        if key == "url" {
            // The old `url` only counts for agreement; moving it to
            // `website` would be an edit of its own.
            continue;
        }
        if old.split(';').filter(|p| !p.trim().is_empty()).count() > 1 {
            // OSM lists several values, such as two phone numbers; add
            // ours rather than replacing the set.
            let new = format!("{};{new_value}", old.trim_end_matches(';'));
            changes.push(Change::new(key, Some(old), Some(new), Reason::AddedValue));
            continue;
        }
        if family == "website" && is_home_page(atp) {
            // A brand home page never replaces anything.
            continue;
        }
        let reason = if is_generic(family, old, atp, nondescript, shared) {
            Reason::GenericValue
        } else if family == "website" && is_home_page(old) {
            // The home page of another site, most likely the place's
            // own: keep it.
            continue;
        } else if same_site(family, atp, old) {
            // Another page of the same site, or another address at the
            // same domain: keep OSM's.
            continue;
        } else {
            Reason::Differs
        };
        changes.push(Change::new(key, Some(old), Some(new_value), reason));
    }
    for change in &changes {
        if change.key.trim_start_matches("contact:") != "email" {
            continue;
        }
        if change.reason == Reason::Differs {
            warnings.push("replaces_email".to_string());
        }
        if change
            .new
            .as_deref()
            .is_some_and(|v| v.split(';').any(|p| is_personal_email(p, osm, shared)))
        {
            warnings.push("personal_email".to_string());
        }
    }
    changes
}

/// Words that name a role or a place rather than a person, in the
/// local part of an email address such as `hotel.zurich@example.com`.
const ROLE_WORDS: &[&str] = &[
    "admin",
    "agendamento",
    "anfrage",
    "apotheke",
    "atendimento",
    "booking",
    "bookings",
    "boutique",
    "branch",
    "centro",
    "contact",
    "contato",
    "customer",
    "events",
    "filiale",
    "front",
    "frontdesk",
    "gerente",
    "geschaeft",
    "guest",
    "hello",
    "hotel",
    "info",
    "kontakt",
    "laden",
    "loja",
    "magasin",
    "mail",
    "manager",
    "office",
    "order",
    "orders",
    "reception",
    "recepcao",
    "reservas",
    "reservation",
    "reservations",
    "restaurant",
    "rooms",
    "sales",
    "service",
    "shop",
    "stay",
    "store",
    "support",
    "team",
    "vendas",
    "welcome",
];

/// Whether an email address looks like one person's, as
/// `jens.eichmann@amag.ch`: a local part of two words of letters,
/// joined by `.` or `_`, neither of which names a role, the chain or
/// the store. A word names the chain or store if it is part of the
/// domain (`omega.geneve@omega.ch`), of the OSM feature's `name` or
/// `brand` (`lacoste.ribershop@gmail.com`), or recurs at the domain
/// (see [shared_email_words]). Such an address may be what the store
/// wants in OSM, but a person should look before it goes in. The test
/// still flags some store addresses, so it only gives a warning.
fn is_personal_email(part: &str, osm: &Tags, shared: &HashSet<String>) -> bool {
    let Some(email) = normalize_email(part) else {
        return false;
    };
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    let words: Vec<&str> = local.split(['.', '_']).collect();
    if words.len() != 2 {
        return false;
    }
    let names: Vec<String> = osm
        .iter()
        .filter(|(k, _)| k == "name" || k == "brand")
        .map(|(_, v)| {
            v.to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect()
        })
        .collect();
    let keys = email_word_keys(&email);
    words.iter().zip(&keys).all(|(w, key)| {
        w.chars().count() >= 2
            && w.chars().all(char::is_alphabetic)
            && !ROLE_WORDS.contains(w)
            && !domain.contains(w)
            && !names.iter().any(|n| n.contains(w))
            && !shared.contains(key)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(t: &[(&str, &str)]) -> Vec<(String, String)> {
        t.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn changes(
        osm: &[(&str, &str)],
        atp: &[&[(&str, &str)]],
    ) -> Vec<(String, Option<String>, Option<String>, &'static str)> {
        let atp: Vec<Vec<(String, String)>> = atp.iter().map(|t| tags(t)).collect();
        let sources: Vec<ContactSource> = atp.iter().map(|t| ContactSource { tags: t }).collect();
        let mut warnings = Vec::new();
        contact_changes(
            &tags(osm),
            &sources,
            &Nondescript::none(),
            &HashSet::new(),
            None,
            &mut warnings,
        )
        .into_iter()
        .map(|c| (c.key, c.old, c.new, c.reason.as_str()))
        .collect()
    }

    fn change(
        key: &str,
        old: Option<&str>,
        new: &str,
        reason: &'static str,
    ) -> (String, Option<String>, Option<String>, &'static str) {
        (
            key.to_string(),
            old.map(str::to_string),
            Some(new.to_string()),
            reason,
        )
    }

    #[test]
    fn adds_missing_phone_in_international_format() {
        assert_eq!(
            changes(&[("shop", "bakery")], &[&[("phone", "+41441234567")]]),
            [change("phone", None, "+41 44 123 45 67", "missing_in_osm")]
        );
    }

    #[test]
    fn notation_only_gives_no_change() {
        let atp: &[(&str, &str)] = &[
            ("addr:country", "CH"),
            ("phone", "+41 44 123 45 67"),
            ("email", "info@example.ch"),
            ("website", "https://example.ch/?utm_source=x"),
        ];
        let osm = [
            ("phone", "044 123 45 67"),
            ("email", "Info@Example.ch"),
            ("website", "http://www.example.ch/"),
        ];
        assert!(changes(&osm, &[atp]).is_empty());
    }

    #[test]
    fn writes_contact_form_where_osm_uses_it() {
        assert_eq!(
            changes(
                &[("contact:phone", "+41 44 123 45 67")],
                &[&[("phone", "+41 44 123 45 67"), ("email", "a@example.ch")]]
            ),
            [change(
                "contact:email",
                None,
                "a@example.ch",
                "missing_in_osm"
            )]
        );
        // A value in one form is replaced where it is.
        assert_eq!(
            changes(
                &[("contact:website", "https://example.com/old")],
                &[&[("website", "https://example.ch/store/1")]]
            ),
            [change(
                "contact:website",
                Some("https://example.com/old"),
                "https://example.ch/store/1",
                "differs"
            )]
        );
    }

    #[test]
    fn replaces_generic_website_but_never_with_home_page() {
        assert_eq!(
            changes(
                &[("website", "https://www.dm.at")],
                &[&[("website", "https://www.dm.at/store/a070/wien")]]
            ),
            [change(
                "website",
                Some("https://www.dm.at"),
                "https://www.dm.at/store/a070/wien",
                "generic_value"
            )]
        );
        assert!(
            changes(
                &[("website", "https://www.dm.at/store/a070/wien")],
                &[&[("website", "https://www.dm.at/")]]
            )
            .is_empty()
        );
        // An old `url` counts as OSM's website, but is never edited.
        assert!(
            changes(
                &[("url", "https://www.denner.ch/")],
                &[&[("website", "https://www.denner.ch/de/filialen/2034-peseux")]]
            )
            .is_empty()
        );
        // An old `url` counts as OSM's website.
        assert!(
            changes(
                &[("url", "https://example.ch/store/1")],
                &[&[("website", "https://example.ch/store/1")]]
            )
            .is_empty()
        );
    }

    #[test]
    fn adds_to_several_values() {
        assert_eq!(
            changes(
                &[("phone", "+41 44 765 43 21;+41 44 234 56 78")],
                &[&[("phone", "+41 44 876 54 32")]]
            ),
            [change(
                "phone",
                Some("+41 44 765 43 21;+41 44 234 56 78"),
                "+41 44 765 43 21;+41 44 234 56 78;+41 44 876 54 32",
                "added_value"
            )]
        );
    }

    #[test]
    fn sources_that_disagree_give_nothing() {
        let a: &[(&str, &str)] = &[("phone", "+41 44 765 43 21"), ("email", "a@example.ch")];
        let b: &[(&str, &str)] = &[("phone", "+41 44 234 56 78"), ("email", "A@example.ch")];
        assert_eq!(
            changes(&[], &[a, b]),
            [change("email", None, "a@example.ch", "missing_in_osm")]
        );
    }

    #[test]
    fn fax_is_its_own_family() {
        assert_eq!(
            changes(
                &[("phone", "+1-919-932-7773")],
                &[&[("phone", "+1 919-932-7773"), ("fax", "+1 919-932-7774")]]
            ),
            [change("fax", None, "+1 919-932-7774", "missing_in_osm")]
        );
    }

    #[test]
    fn skips_placeholder_numbers() {
        assert!(changes(&[], &[&[("phone", "+254 708 000000")]]).is_empty());
        assert!(is_placeholder("phone", "+254 708 000000;+254 708 123456"));
        assert!(!is_placeholder("phone", "+254 708 123456"));
        assert!(!is_placeholder("email", "000000@example.ke"));
    }

    #[test]
    fn trunk_prefix_after_country_code_agrees() {
        assert!(
            changes(
                &[("phone", "+971 02 6120860")],
                &[&[("phone", "+971 2 612 0860")]]
            )
            .is_empty()
        );
        assert!(
            changes(
                &[("phone", "+971 (0) 4 249 7800")],
                &[&[("phone", "+971 4 249 7800")]]
            )
            .is_empty()
        );
    }

    #[test]
    fn skips_invalid_numbers() {
        assert!(changes(&[], &[&[("phone", "+97197126543333")]]).is_empty());
        assert!(changes(&[], &[&[("fax", "1800 PARKING")]]).is_empty());
    }

    #[test]
    fn keeps_other_page_of_same_site() {
        assert!(
            changes(
                &[("website", "https://all.accor.com/hotel/A089/index.en.shtml")],
                &[&[("website", "https://all.accor.com/hotel/A089/index.ar.shtml")]]
            )
            .is_empty()
        );
        assert!(
            changes(
                &[("email", "Dubai_sales@tesla.com")],
                &[&[("email", "Dubai_Service@tesla.com")]]
            )
            .is_empty()
        );
        // Another host of the same domain is the same site.
        assert!(
            changes(
                &[("website", "https://locations.dunkindonuts.com/en/ma/chelsea/83-everett-ave/303479")],
                &[&[("website", "https://www.dunkindonuts.com/locations/us/ma/chelsea/83-everett-ave/store-303479/")]]
            )
            .is_empty()
        );
        // Another site does replace.
        assert_eq!(
            changes(
                &[(
                    "website",
                    "http://www.novotel.com/en/hotel-5261/index.shtml"
                )],
                &[&[("website", "https://all.accor.com/hotel/5261/index.ar.shtml")]]
            ),
            [change(
                "website",
                Some("http://www.novotel.com/en/hotel-5261/index.shtml"),
                "https://all.accor.com/hotel/5261/index.ar.shtml",
                "differs"
            )]
        );
    }

    #[test]
    fn keeps_home_page_of_another_site() {
        // A restaurant's own site against a page of an aggregator.
        assert!(
            changes(
                &[("website", "https://www.restaurantlescale.ch/")],
                &[&[("website", "https://zuerst.proinfirmis.ch/pois/detail/23619")]]
            )
            .is_empty()
        );
    }

    #[test]
    fn value_on_many_osm_features_is_generic() {
        let a = tags(&[("website", "https://www.europcar.com/en-db")]);
        let shared = shared_osm_values([&a, &a, &a].into_iter());
        assert!(
            !shared_osm_values([&a, &a].into_iter())
                .contains("website\thttps://www.europcar.com/en-db")
        );
        let atp = tags(&[(
            "website",
            "https://www.europcar.com/en-us/places/dubai/sharjah-branch",
        )]);
        let mut warnings = Vec::new();
        let got: Vec<_> = contact_changes(
            &a,
            &[ContactSource { tags: &atp }],
            &Nondescript::none(),
            &shared,
            None,
            &mut warnings,
        )
        .into_iter()
        .map(|c| c.reason.as_str())
        .collect();
        assert_eq!(got, ["generic_value"]);
    }

    #[test]
    fn warns_about_replaced_and_personal_emails() {
        let run = |osm: &[(&str, &str)], atp: &[(&str, &str)]| {
            let (osm, atp) = (tags(osm), tags(atp));
            let mut warnings = Vec::new();
            let changes = contact_changes(
                &osm,
                &[ContactSource { tags: &atp }],
                &Nondescript::none(),
                &HashSet::new(),
                None,
                &mut warnings,
            );
            (changes.len(), warnings)
        };
        assert_eq!(
            run(
                &[("email", "rooms@atriahotel.com")],
                &[("email", "reservationblr@radisson.com")]
            ),
            (1, vec!["replaces_email".to_string()])
        );
        assert_eq!(
            run(&[], &[("email", "Jens.Eichmann@amag.ch")]),
            (1, vec!["personal_email".to_string()])
        );
        assert_eq!(
            run(&[], &[("email", "hotel.zurich@example.ch")]),
            (1, vec![])
        );
        assert_eq!(run(&[], &[("email", "h5261@accor.com")]), (1, vec![]));
    }

    #[test]
    fn test_is_personal_email() {
        let none = HashSet::new();
        let personal = |e: &str| is_personal_email(e, &Vec::new(), &none);
        assert!(personal("nicholas.destefano@marriott.com"));
        assert!(personal("matheus_couto@orvelvw.com.br"));
        assert!(!personal("reservations.cpdubaimarina@ihg.com"));
        assert!(!personal("info@example.ch"));
        assert!(!personal("cy.mhtch.gm@marriott.com"));
        assert!(!personal("e409607@minden.edeka.de"));
        assert!(!personal("omega.geneve@omega.ch"));
        // The store's name.
        let osm = tags(&[("name", "Lacoste")]);
        assert!(!is_personal_email(
            "lacoste.ribershop@gmail.com",
            &osm,
            &none
        ));
        // A word that recurs at the domain names stores, not people.
        let atp: Vec<Tags> = ["migrol.sion", "migrol.martigny", "migrol.aigle"]
            .iter()
            .map(|l| tags(&[("email", &format!("{l}@migrolino.com"))]))
            .collect();
        let shared = shared_email_words(atp.iter());
        assert!(!is_personal_email(
            "migrol.bex@migrolino.com",
            &Vec::new(),
            &shared
        ));
        assert!(is_personal_email(
            "anna.muster@migrolino.com",
            &Vec::new(),
            &shared
        ));
    }

    #[test]
    fn registry_pages_go_to_heritage_website() {
        let page = "https://npgallery.nps.gov/AssetDetail/NRIS/76000236";
        let index = "https://www.nps.gov/subjects/nationalregister/index.htm";
        let mut warnings = Vec::new();
        let run = |osm: &[(&str, &str)], shared: &HashSet<String>, w: &mut Vec<String>| {
            heritage_changes(&tags(osm), &[page], shared, w)
                .into_iter()
                .map(|c| (c.key, c.old, c.new, c.reason.as_str()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            run(&[("historic", "building")], &HashSet::new(), &mut warnings),
            [change("heritage:website", None, page, "missing_in_osm")]
        );
        // The register's index page, on many features, gets replaced.
        let osm = tags(&[("heritage:website", index)]);
        let shared = shared_osm_values([&osm, &osm, &osm].into_iter());
        assert_eq!(
            run(&[("heritage:website", index)], &shared, &mut warnings),
            [change(
                "heritage:website",
                Some(index),
                page,
                "generic_value"
            )]
        );
        // Another specific page stays.
        assert!(
            run(
                &[("heritage:website", "https://example.org/quaker-house")],
                &shared,
                &mut warnings
            )
            .is_empty()
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_format_phone() {
        assert_eq!(format_phone("+41441234567"), "+41 44 123 45 67");
        assert_eq!(
            format_phone("+1 919-932-7773; +41441234567"),
            "+1 919-932-7773;+41 44 123 45 67"
        );
        assert_eq!(format_phone("1800 PARKING"), "1800 PARKING");
    }
}
