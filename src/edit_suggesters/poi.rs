use super::EditSuggester;
use crate::matchers::contact::{
    AtpContacts, Nondescript, is_email_key, is_phone_key, is_website_key,
};
use std::collections::HashMap;

/// Suggests tag edits for POI-like OSM features (shops today; other
/// categories once matching supports them -- see the module doc for why
/// that doesn't imply an edit suggester per matcher). Ported from the
/// old `PoiMatcher::suggest_edit` (see git history), unchanged in logic:
/// only *where* it lives changed, not *what* it does.
pub struct PoiEditSuggester;

impl PoiEditSuggester {
    /// Tells whether we trust an AllThePlaces tag enough to suggest an
    /// OpenStreetMap edit. AllThePlaces mostly propagates whatever comes
    /// from spidered websites, so we use an allowlist to prevent
    /// spamming human OpenStreetMap editors.
    fn is_atp_tag_trustworthy(key: &str) -> bool {
        // Before you add entries to this list, please make sure that the quality
        // is good. To evaluate, look at the diff of workdir/shops.jsonl
        // from before and after your change to this code.
        matches!(
            key,
            "email" | "end_date" | "fax" | "opening_hours" | "phone" | "start_date" | "website"
        )
    }
}

impl EditSuggester for PoiEditSuggester {
    fn suggest_edit(
        &self,
        atp_tags: &[(String, String)],
        osm_tags: &[(String, String)],
        position: Option<(f64, f64)>,
    ) -> Option<Vec<(String, String)>> {
        let osm_tags: HashMap<&str, &str> = osm_tags
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let country = atp_tags
            .iter()
            .find(|(k, _)| k == "addr:country")
            .map(|(_, v)| v.as_str());
        let tag_edits: Vec<(String, String)> = atp_tags
            .iter()
            .filter(|(key, _atp_value)| Self::is_atp_tag_trustworthy(key))
            .filter(|(key, atp_value)| match osm_tags.get(key.as_str()) {
                Some(osm_value) => !values_agree(key, atp_value, osm_value, country, position),
                None => true, // OSM feature has no value yet for this key
            })
            .cloned()
            .collect();
        if tag_edits.is_empty() {
            None
        } else {
            Some(tag_edits)
        }
    }
}

/// Whether the ATP and OSM values of `key` say the same, compared as
/// the matchers compare them, so that a different way of writing the
/// same value never becomes an edit. Phone and fax numbers are
/// compared in their international form, so `+41 44 123 45 67` and
/// `044 123 45 67` agree in Switzerland; `country` (ATP's
/// `addr:country`) and `position` give the country for numbers in
/// national format. Emails and websites are compared as
/// [AtpContacts] compares them, websites without scheme, `www.` or
/// tracking parameters. A value with several `;`-separated parts
/// agrees if any part does, as in `match.evidence.agreeing`.
///
/// Unlike `match.evidence.agreeing`, which pools `phone`, `fax` and
/// `mobile` because AllThePlaces mixes them up, this compares one key
/// at a time: a phone number that agrees must not hide a fax number
/// that doesn't.
fn values_agree(
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
    if key == "opening_hours" {
        let squash = |v: &str| v.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        return squash(atp_value) == squash(osm_value);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(t: &[(&str, &str)]) -> Vec<(String, String)> {
        t.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn suggests_edit_for_trustworthy_changed_tag() {
        let atp = tags(&[
            ("opening_hours", "Mo-Fr 09:00-20:00"),
            ("name", "New Yorker"),
        ]);
        let osm = tags(&[
            ("opening_hours", "Mo-Fr 08:00-18:00"),
            ("name", "New Yorker"),
        ]);
        let edit = PoiEditSuggester
            .suggest_edit(&atp, &osm, None)
            .expect("should suggest an edit");
        assert_eq!(
            edit,
            vec![("opening_hours".to_string(), "Mo-Fr 09:00-20:00".to_string())]
        );
    }

    #[test]
    fn no_edit_when_tags_already_match() {
        let atp = tags(&[("opening_hours", "Mo-Fr 09:00-20:00")]);
        let osm = tags(&[("opening_hours", "Mo-Fr 09:00-20:00")]);
        assert!(PoiEditSuggester.suggest_edit(&atp, &osm, None).is_none());
    }

    #[test]
    fn ignores_untrustworthy_atp_tags() {
        // `name` is not on the trustworthy allowlist -- even though ATP
        // and OSM disagree, we don't want to propose spamming a human
        // editor with an unreviewed name change.
        let atp = tags(&[("name", "Something Else")]);
        let osm = tags(&[("name", "Original Name")]);
        assert!(PoiEditSuggester.suggest_edit(&atp, &osm, None).is_none());
    }

    #[test]
    fn suggests_edit_for_tag_missing_on_osm_side() {
        let atp = tags(&[("website", "https://example.com")]);
        let osm = tags(&[]);
        let edit = PoiEditSuggester
            .suggest_edit(&atp, &osm, None)
            .expect("should suggest an edit");
        assert_eq!(
            edit,
            vec![("website".to_string(), "https://example.com".to_string())]
        );
    }

    #[test]
    fn no_edit_when_no_trustworthy_tags_present() {
        let atp = tags(&[("brand", "New Yorker")]);
        let osm = tags(&[]);
        assert!(PoiEditSuggester.suggest_edit(&atp, &osm, None).is_none());
    }

    #[test]
    fn no_edit_when_values_differ_only_in_notation() {
        let atp = tags(&[
            ("addr:country", "CH"),
            ("phone", "+41 44 123 45 67"),
            ("email", "Info@Example.ch"),
            ("website", "https://www.example.ch/"),
            ("opening_hours", "Mo-Fr 08:00-18:00; Sa 09:00-12:00"),
        ]);
        let osm = tags(&[
            ("phone", "044 123 45 67"),
            ("email", "info@example.ch"),
            ("website", "http://example.ch"),
            ("opening_hours", "Mo-Fr 08:00-18:00;Sa 09:00-12:00"),
        ]);
        assert!(PoiEditSuggester.suggest_edit(&atp, &osm, None).is_none());
    }

    #[test]
    fn national_format_phone_uses_position_country() {
        // No `addr:country`: the country comes from the position,
        // which lies in Zürich.
        let atp = tags(&[("phone", "+41 44 123 45 67")]);
        let osm = tags(&[("phone", "044 123 45 67")]);
        let zurich = Some((8.54, 47.37));
        assert!(PoiEditSuggester.suggest_edit(&atp, &osm, zurich).is_none());
    }

    #[test]
    fn fax_is_compared_on_its_own() {
        // The phone agrees, but the fax doesn't, so the fax is still
        // suggested; `match.evidence.agreeing` pools the two.
        let atp = tags(&[("phone", "+1 919-932-7773"), ("fax", "+1 919-932-7774")]);
        let osm = tags(&[("phone", "+1-919-932-7773"), ("fax", "+1-919-932-0000")]);
        let edit = PoiEditSuggester
            .suggest_edit(&atp, &osm, None)
            .expect("should suggest an edit");
        assert_eq!(
            edit,
            vec![("fax".to_string(), "+1 919-932-7774".to_string())]
        );
        let osm = tags(&[("phone", "+1-919-932-7773"), ("fax", "+1 (919) 932-7774")]);
        assert!(PoiEditSuggester.suggest_edit(&atp, &osm, None).is_none());
    }

    #[test]
    fn suggests_edit_for_different_website() {
        let atp = tags(&[("website", "https://www.dm.at/store/a070/wien")]);
        let osm = tags(&[("website", "https://www.dm.at")]);
        assert!(PoiEditSuggester.suggest_edit(&atp, &osm, None).is_some());
    }
}
