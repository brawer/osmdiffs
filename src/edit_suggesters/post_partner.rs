//! Edits for the OSM feature that an AllThePlaces postal partner
//! matched: the partner itself (`same`) or its host (`located_in`); see
//! [crate::matchers::post_partner]. Both get the `post_office` tags
//! that OSM lacks, and nothing else: the partner's opening hours or
//! phone may be the counter's, not the store's.

use super::EditSuggester;
use crate::matchers::post_partner::PostalBrand;

pub struct PostPartnerEditSuggester {
    brand: PostalBrand,
}

impl PostPartnerEditSuggester {
    pub fn new(spider: &str, atp_tags: &[(String, String)]) -> Self {
        let tags = atp_tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        PostPartnerEditSuggester {
            brand: PostalBrand::of_atp(spider, tags),
        }
    }
}

impl EditSuggester for PostPartnerEditSuggester {
    /// `post_office=post_partner` if OSM has no `post_office` tag. ATP's
    /// postal brand as `post_office:brand` if OSM has none, or added to
    /// OSM's with `;` if OSM's is another network's, as in a kiosk that
    /// is both a Hermes and a DHL partner; for an `amenity=post_office`,
    /// its `brand` counts as OSM's. The brand's Wikidata ID goes along
    /// where OSM's IDs, if any, still line up with its brands.
    fn suggest_edit(
        &self,
        _atp_tags: &[(String, String)],
        osm_tags: &[(String, String)],
        _position: Option<(f64, f64)>,
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
            .suggest_edit(&tags(atp), &tags(osm), None)
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
}
