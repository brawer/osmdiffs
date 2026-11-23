//! Whether an AllThePlaces value and an OpenStreetMap value of the same
//! tag say the same thing, so that the edits step never proposes an
//! edit that only rewrites a value in another notation.
//!
//! Kept from the old `edit_suggesters::poi` module (see git history)
//! when `suggest_edits` was removed; the new edits step uses it from
//! milestone 3 of `docs/notes/2026-11-23-edits-parquet.md` on. The
//! comparison of `start_date` and `end_date` was dropped along with
//! that module, since the new step doesn't edit those tags.

use crate::matchers::contact::{
    AtpContacts, Nondescript, is_email_key, is_phone_key, is_website_key,
};

/// Whether the ATP and OSM values of `key` say the same, compared as
/// the matchers compare them, so that a different way of writing the
/// same value never becomes an edit. Phone and fax numbers are
/// compared in their international form, so `+41 44 123 45 67` and
/// `044 123 45 67` agree in Switzerland; `country` (ATP's
/// `addr:country`) and `position` give the country for numbers in
/// national format. Emails and websites are compared as
/// [AtpContacts] compares them, websites without scheme, `www.` or
/// tracking parameters. A value with several `;`-separated parts
/// agrees if any part does, as in `match.evidence.agreeing_tags`.
///
/// Unlike `match.evidence.agreeing_tags`, which pools `phone`, `fax` and
/// `mobile` because AllThePlaces mixes them up, this compares one key
/// at a time: a phone number that agrees must not hide a fax number
/// that doesn't.
pub(crate) fn values_agree(
    key: &str,
    atp_value: &str,
    osm_value: &str,
    country: Option<&str>,
    position: Option<(f64, f64)>,
) -> bool {
    if atp_value == osm_value {
        return true;
    }
    if is_phone_key(key) || is_email_key(key) || is_website_key(key) {
        let atp = [(key, atp_value)]
            .into_iter()
            .chain(country.map(|c| ("addr:country", c)));
        let contacts = AtpContacts::from_tags(atp, position, &Nondescript::none());
        return contacts.agrees(std::iter::once((key, osm_value)));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agree(key: &str, atp: &str, osm: &str) -> bool {
        values_agree(key, atp, osm, None, None)
    }

    #[test]
    fn values_differing_only_in_notation_agree() {
        let ch = Some("CH");
        assert!(values_agree(
            "phone",
            "+41 44 123 45 67",
            "044 123 45 67",
            ch,
            None
        ));
        assert!(agree("email", "Info@Example.ch", "info@example.ch"));
        assert!(agree(
            "website",
            "https://www.example.ch/",
            "http://example.ch"
        ));
    }

    #[test]
    fn national_format_phone_uses_position_country() {
        // No `addr:country`: the country comes from the position,
        // which lies in Zürich.
        let zurich = Some((8.54, 47.37));
        assert!(values_agree(
            "phone",
            "+41 44 123 45 67",
            "044 123 45 67",
            None,
            zurich
        ));
    }

    #[test]
    fn fax_is_compared_on_its_own() {
        // `match.evidence.agreeing_tags` pools phone and fax; here a
        // fax only agrees with a fax.
        assert!(!agree("fax", "+1 919-932-7774", "+1-919-932-0000"));
        assert!(agree("fax", "+1 919-932-7774", "+1 (919) 932-7774"));
    }

    #[test]
    fn different_website_disagrees() {
        assert!(!agree(
            "website",
            "https://www.dm.at/store/a070/wien",
            "https://www.dm.at"
        ));
    }

    #[test]
    fn other_keys_agree_only_when_equal() {
        assert!(agree("name", "Coop", "Coop"));
        assert!(!agree("name", "Coop", "Coop City"));
    }
}
