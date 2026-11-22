//! Edits for the OSM feature that an AllThePlaces postal partner
//! matched: the partner itself (`same`) or its host (`located_in`); see
//! [crate::matchers::post_partner]. Both get the `post_office` tags
//! that OSM lacks, and nothing else: the partner's opening hours or
//! phone may be the counter's, not the store's.
//!
//! Moved unchanged from the old `edit_suggesters` module (see git
//! history) when `suggest_edits` was removed; the new edits step uses
//! it from milestone 3 of `docs/notes/2026-11-23-edits-parquet.md` on.

// Until the edits step uses it.
#![allow(dead_code)]

use crate::matchers::post_partner::{PostalBrand, is_post_partner};

/// Whether an AllThePlaces feature with `atp_tags`, matched with
/// `match.relation` = `relation`, gets postal-partner edits: only a
/// postal partner gets edits for the host it is `located_in`, since
/// OSM puts the partner's tags on its host.
pub(crate) fn has_partner_edits(atp_tags: &[(String, String)], relation: &str) -> bool {
    is_post_partner(atp_tags.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        && matches!(relation, "same" | "located_in")
}

pub(crate) struct PostPartnerEditSuggester {
    brand: PostalBrand,
}

impl PostPartnerEditSuggester {
    pub(crate) fn new(spider: &str, atp_tags: &[(String, String)]) -> Self {
        let tags = atp_tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        PostPartnerEditSuggester {
            brand: PostalBrand::of_atp(spider, tags),
        }
    }

    /// `post_office=post_partner` if OSM has no `post_office` tag. ATP's
    /// postal brand as `post_office:brand` if OSM has none, or added to
    /// OSM's with `;` if OSM's is another network's, as in a kiosk that
    /// is both a Hermes and a DHL partner; for an `amenity=post_office`,
    /// its `brand` counts as OSM's. The brand's Wikidata ID goes along
    /// where OSM's IDs, if any, still line up with its brands.
    pub(crate) fn suggest_edit(
        &self,
        osm_tags: &[(String, String)],
    ) -> Option<Vec<(String, String)>> {
        let tag = |key: &str| {
            osm_tags
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let mut edits = Vec::new();
        if tag("post_office").is_none() {
            edits.push(("post_office".to_string(), "post_partner".to_string()));
        }
        let Some(name) = &self.brand.name else {
            return (!edits.is_empty()).then_some(edits);
        };
        let wikidata = self.brand.wikidata.map(|id| format!("Q{id}"));
        let osm_brand = tag("post_office:brand")
            .or_else(|| tag("brand").filter(|_| tag("amenity") == Some("post_office")));
        match osm_brand {
            None => {
                edits.push(("post_office:brand".to_string(), name.clone()));
                if let Some(id) = wikidata {
                    edits.push(("post_office:brand:wikidata".to_string(), id));
                }
            }
            Some(osm) if self.brand.shares_word(osm) || osm == name => {
                // OSM has the brand; at most its ID is missing.
                if let Some(id) = wikidata
                    && tag("post_office:brand") == Some(osm)
                    && !osm.contains(';')
                    && tag("post_office:brand:wikidata").is_none()
                {
                    edits.push(("post_office:brand:wikidata".to_string(), id));
                }
            }
            Some(osm) => {
                // Another network's partner, unless OSM's brand says
                // nothing, such as `Die Post`.
                if PostalBrand::of_atp("", [("post_office:brand", osm)].into_iter())
                    .agrees(&self.brand)
                {
                    return (!edits.is_empty()).then_some(edits);
                }
                edits.push(("post_office:brand".to_string(), format!("{osm};{name}")));
                let brands = osm.split(';').count();
                let osm_ids = tag("post_office:brand:wikidata");
                if let (Some(id), Some(ids)) = (wikidata, osm_ids)
                    && ids.split(';').count() == brands
                    && tag("post_office:brand") == Some(osm)
                {
                    edits.push((
                        "post_office:brand:wikidata".to_string(),
                        format!("{ids};{id}"),
                    ));
                }
            }
        }
        (!edits.is_empty()).then_some(edits)
    }
}

/// Adds the tags of another edit for the same OSM feature to `tags`.
/// As before postal partners got edits, the other tags come from one
/// edit alone, the first that has any, so that two ATP features never
/// mix their opening hours. The `post_office` tags of
/// this module add up, since several partners may be in
/// one store: `Hermes;DHL` and `Hermes;DPD` give `Hermes;DHL;DPD`. If
/// the brands and their Wikidata IDs no longer line up, the IDs are
/// left out.
pub(crate) fn merge_edits(tags: &mut Vec<(String, String)>, other: Vec<(String, String)>) {
    const LISTS: [&str; 2] = ["post_office:brand", "post_office:brand:wikidata"];
    let is_postal = |key: &str| key == "post_office" || key.starts_with("post_office:");
    let has_other = tags.iter().any(|(k, _)| !is_postal(k));
    for (key, value) in other {
        if !is_postal(&key) {
            if !has_other {
                tags.push((key, value));
            }
            continue;
        }
        match tags.iter_mut().find(|(k, _)| *k == key) {
            None => tags.push((key, value)),
            Some((_, old)) if LISTS.contains(&key.as_str()) => {
                for v in value.split(';') {
                    if !old.split(';').any(|o| o == v) {
                        old.push(';');
                        old.push_str(v);
                    }
                }
            }
            Some(_) => {}
        }
    }
    let count = |key: &str| {
        tags.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.split(';').count())
    };
    if let Some(ids) = count(LISTS[1])
        && count(LISTS[0]).is_some_and(|brands| brands != ids)
    {
        tags.retain(|(k, _)| k != LISTS[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(t: &[(&str, &str)]) -> Vec<(String, String)> {
        t.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn suggest(spider: &str, atp: &[(&str, &str)], osm: &[(&str, &str)]) -> Vec<(String, String)> {
        PostPartnerEditSuggester::new(spider, &tags(atp))
            .suggest_edit(&tags(osm))
            .unwrap_or_default()
    }

    const DHL: &[(&str, &str)] = &[
        ("amenity", "yes"),
        ("post_office", "post_partner"),
        ("name", "Kiosk Waibel"),
        ("opening_hours", "Mo-Fr 08:00-18:00"),
    ];

    #[test]
    fn test_new_partner() {
        assert_eq!(
            suggest("deutsche_post_de", DHL, &[("shop", "kiosk")]),
            tags(&[
                ("post_office", "post_partner"),
                ("post_office:brand", "DHL"),
                ("post_office:brand:wikidata", "Q489815"),
            ])
        );
    }

    #[test]
    fn test_same_brand() {
        let osm = [
            ("shop", "kiosk"),
            ("post_office", "post_partner"),
            ("post_office:brand", "DHL Paketshop"),
        ];
        assert_eq!(
            suggest("deutsche_post_de", DHL, &osm),
            tags(&[("post_office:brand:wikidata", "Q489815")])
        );
        let complete = [
            ("shop", "kiosk"),
            ("post_office", "post_partner"),
            ("post_office:brand", "DHL"),
            ("post_office:brand:wikidata", "Q489815"),
        ];
        assert!(suggest("deutsche_post_de", DHL, &complete).is_empty());
        let office = [("amenity", "post_office"), ("brand", "DHL")];
        assert_eq!(
            suggest("deutsche_post_de", DHL, &office),
            tags(&[("post_office", "post_partner")])
        );
    }

    #[test]
    fn test_other_network() {
        let osm = [
            ("shop", "kiosk"),
            ("post_office", "post_partner"),
            ("post_office:brand", "Hermes"),
        ];
        assert_eq!(
            suggest("deutsche_post_de", DHL, &osm),
            tags(&[("post_office:brand", "Hermes;DHL")])
        );
        let with_ids = [
            ("shop", "kiosk"),
            ("post_office", "post_partner"),
            ("post_office:brand", "Hermes"),
            ("post_office:brand:wikidata", "Q1613213"),
        ];
        assert_eq!(
            suggest("deutsche_post_de", DHL, &with_ids),
            tags(&[
                ("post_office:brand", "Hermes;DHL"),
                ("post_office:brand:wikidata", "Q1613213;Q489815"),
            ])
        );
    }

    #[test]
    fn test_generic_osm_brand() {
        let atp = [
            ("amenity", "yes"),
            ("post_office", "post_partner"),
            ("post_office:brand", "Die Post"),
            ("post_office:brand:wikidata", "Q614803"),
        ];
        let osm = [
            ("shop", "supermarket"),
            ("post_office", "post_partner"),
            ("post_office:brand", "Die Post AG"),
        ];
        assert!(suggest("swiss_post_ch", &atp, &osm).is_empty());
    }

    #[test]
    fn test_operator_only() {
        let atp = [
            ("amenity", "yes"),
            ("post_office", "post_partner"),
            ("operator", "Empresa Brasileira de Correios e Telégrafos"),
        ];
        assert_eq!(
            suggest("correios_br", &atp, &[("amenity", "townhall")]),
            tags(&[("post_office", "post_partner")])
        );
    }

    #[test]
    fn test_merge_edits() {
        let tags = |t: &[(&str, &str)]| -> Vec<(String, String)> {
            t.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let mut a = tags(&[
            ("opening_hours", "Mo-Fr 08:00-18:00"),
            ("post_office:brand", "Hermes;DHL"),
        ]);
        merge_edits(
            &mut a,
            tags(&[
                ("opening_hours", "Mo-Sa 08:00-20:00"),
                ("post_office", "post_partner"),
                ("post_office:brand", "Hermes;DPD"),
            ]),
        );
        assert_eq!(
            a,
            tags(&[
                ("opening_hours", "Mo-Fr 08:00-18:00"),
                ("post_office:brand", "Hermes;DHL;DPD"),
                ("post_office", "post_partner"),
            ])
        );
        let mut b = tags(&[
            ("post_office:brand", "DHL"),
            ("post_office:brand:wikidata", "Q489815"),
        ]);
        merge_edits(
            &mut b,
            tags(&[("post_office:brand", "DPD"), ("phone", "+49 30 1234567")]),
        );
        assert_eq!(
            b,
            tags(&[
                ("post_office:brand", "DHL;DPD"),
                ("phone", "+49 30 1234567")
            ])
        );
    }
}
