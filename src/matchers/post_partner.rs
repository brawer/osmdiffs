//! Postal partners: a postal or parcel counter inside another business,
//! such as a DHL counter in a kiosk or a FedEx counter in a Walgreens.
//! AllThePlaces lists them as features of their own, with
//! `post_office=post_partner` and no other kind than `amenity=yes`;
//! OpenStreetMap puts the tags on the host instead, such as
//! `shop=kiosk` with `post_office=post_partner` and
//! `post_office:brand=DHL`. So an ATP partner matches:
//!
//! - the OSM feature that already is the partner (`same`): a feature
//!   with `post_office=post_partner`, or an `amenity=post_office`
//!   without another `post_office` value, of the same postal brand
//!   ([PostalBrand]), within [SAME_METERS], or within [HOST_METERS] if
//!   it is also the host by name ([PartnerHost]);
//! - otherwise its host (`located_in`), found by the host's name that
//!   ATP gives, within [HOST_METERS]; see `pipeline::conflate::located_in`.
//!
//! A partner without either gets no row: there is no stand-alone place
//! to add to OSM. See `docs/notes/2026-11-22-post-partners.md` and
//! [brawer/osmdiffs#1016](https://github.com/brawer/osmdiffs/issues/1016).

use super::address::{fold_tokens, osm_address_in_atp_text};
use super::{
    MatchMask, Matcher, OsmCandidate, geo_point_distance, meters_to_chord_angle, parse_wikidata_ids,
};
use crate::places::Place;
use geo::Centroid;
use s2::{cellid::CellID, point::Point, s1::ChordAngle};

/// An OSM partner of the same postal brand may be this far away by
/// position alone. In Berlin, Massachusetts and Switzerland, moving the
/// ATP partners 2 km in a random direction brought none this close to
/// an OSM partner of their brand.
pub(crate) const SAME_METERS: f64 = 30.0;

/// The host, or an OSM partner that is also the host by name, may be
/// this far away. Beyond, moving ATP partners 2 km in a random
/// direction finds a store of the same name about as often as the
/// real position does.
pub(crate) const HOST_METERS: f64 = 100.0;

/// A postal brand that a spider gives no tag for; see [SPIDER_BRANDS].
struct SpiderBrand {
    spider: &'static str,
    /// The `post_office:brand` to suggest, and its Wikidata ID.
    name: &'static str,
    wikidata: u64,
    /// Other Wikidata IDs and [brand_words] that OSM uses for the
    /// same network.
    other_ids: &'static [u64],
    words: &'static [&'static str],
}

/// Postal brands that a spider gives no tag for. `deutsche_post_de`
/// lists its DHL Paketshops with just `post_office=post_partner`, and
/// OSM tags them `DHL`, `DHL Paketshop` or `Deutsche Post`.
const SPIDER_BRANDS: &[SpiderBrand] = &[SpiderBrand {
    spider: "deutsche_post_de",
    name: "DHL",
    wikidata: 489815,
    other_ids: &[157645],
    words: &["dhl", "deutsche"],
}];

/// Words that say nothing about which postal network a brand is, for
/// [brand_words].
const GENERIC_BRAND_WORDS: &[&str] = &[
    "ag",
    "agentur",
    "authorized",
    "center",
    "centre",
    "die",
    "filiale",
    "gmbh",
    "inc",
    "office",
    "onsite",
    "paket",
    "pakete",
    "paketshop",
    "parcel",
    "parcels",
    "partner",
    "pickup",
    "point",
    "post",
    "postal",
    "poste",
    "postes",
    "posta",
    "postpartner",
    "punkt",
    "service",
    "shipcenter",
    "shop",
    "store",
    "the",
];

/// Words of a business name that don't tell one store from another,
/// for [PartnerHost]: the kind of store, in the languages of the
/// partner networks, and legal forms. Two names that share only such
/// words, such as `Kiosk am Markt` and `Markt-Kiosk`, aren't the same.
const GENERIC_NAME_WORDS: &[&str] = &[
    "ag",
    "and",
    "apotheke",
    "backerei",
    "cafe",
    "center",
    "centre",
    "co",
    "der",
    "die",
    "das",
    "des",
    "du",
    "ek",
    "getranke",
    "getrankemarkt",
    "gmbh",
    "imbiss",
    "inc",
    "kg",
    "kiosk",
    "laden",
    "llc",
    "lotto",
    "ltd",
    "market",
    "markt",
    "mini",
    "minimarkt",
    "nightshop",
    "ohg",
    "pharmacie",
    "pharmacy",
    "presse",
    "restaurant",
    "sa",
    "sarl",
    "service",
    "shop",
    "spati",
    "spatkauf",
    "station",
    "store",
    "supermarkt",
    "tabak",
    "tabakwaren",
    "tankstelle",
    "the",
    "und",
];

/// Legal forms, which [same_name] ignores: `REWE Markt GmbH` is
/// `REWE Markt`.
const LEGAL_FORMS: &[&str] = &[
    "ag", "co", "ek", "gmbh", "inc", "kg", "llc", "ltd", "ohg", "sa", "sarl",
];

/// Whether `place` is a postal partner that this module matches: it has
/// `post_office=post_partner` and nothing else that says what it is,
/// such as `amenity=yes`. A partner that ATP lists as a store, such as
/// `shop=convenience` for a Lawson, is the store, and the POI matcher
/// takes it.
pub(crate) fn is_post_partner<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> bool {
    let mut partner = false;
    let mut mask = MatchMask::default();
    for (k, v) in tags {
        if k == "post_office" {
            partner |= v == "post_partner";
        } else {
            mask.add_tag(k, v);
        }
    }
    partner && mask.is_empty()
}

/// Whether the OSM feature with `tags` is a postal partner, or may be
/// one: `post_office=post_partner`, or an `amenity=post_office` without
/// another `post_office` value. In Berlin, OSM has 35 DHL post offices
/// in kiosks and stores without any `post_office` tag.
fn is_osm_partner<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> bool {
    let (mut partner, mut post_office, mut other) = (false, false, false);
    for (k, v) in tags {
        match (k, v) {
            ("post_office", "post_partner") => partner = true,
            ("post_office", _) => other = true,
            ("amenity", "post_office") => post_office = true,
            _ => {}
        }
    }
    partner || (post_office && !other)
}

/// The postal network of a partner: Wikidata IDs and [brand_words].
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PostalBrand {
    ids: Vec<u64>,
    words: Vec<String>,
    /// The `post_office:brand` to suggest for OSM, and its Wikidata ID.
    pub(crate) name: Option<String>,
    pub(crate) wikidata: Option<u64>,
}

impl PostalBrand {
    /// The brand of an ATP partner of `spider`: the spider's in
    /// [SPIDER_BRANDS], else `post_office:brand`, else
    /// `post_office:service_provider` (as `kuroneko_jp` gives for Yamato
    /// Transport), else `brand`, else `operator`, each with its Wikidata
    /// ID. The operator isn't
    /// suggested as `post_office:brand`, since it is often the
    /// company's legal name, such as `Empresa Brasileira de Correios e
    /// Telégrafos`.
    pub(crate) fn of_atp<'a>(
        spider: &str,
        tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    ) -> PostalBrand {
        if let Some(s) = SPIDER_BRANDS.iter().find(|s| s.spider == spider) {
            return PostalBrand {
                ids: std::iter::once(s.wikidata)
                    .chain(s.other_ids.iter().copied())
                    .collect(),
                words: s.words.iter().map(|w| w.to_string()).collect(),
                name: Some(s.name.to_string()),
                wikidata: Some(s.wikidata),
            };
        }
        let tag = |key: &str| tags.clone().find(|(k, _)| *k == key).map(|(_, v)| v);
        for key in [
            "post_office:brand",
            "post_office:service_provider",
            "brand",
            "operator",
        ] {
            let Some(value) = tag(key) else {
                continue;
            };
            let ids: Vec<u64> = tag(&format!("{key}:wikidata"))
                .map(|v| parse_wikidata_ids(v).collect())
                .unwrap_or_default();
            let suggest = key != "operator";
            return PostalBrand {
                wikidata: ids.first().copied().filter(|_| suggest),
                ids,
                words: brand_words(value),
                name: suggest.then(|| value.to_string()),
            };
        }
        PostalBrand::default()
    }

    /// The brand of an OSM partner: `post_office:brand` and its
    /// Wikidata ID, and `post_office:service_provider`, or for an `amenity=post_office` without them, its
    /// `brand` and `operator`, or else its `name`.
    fn of_osm<'a>(tags: impl Iterator<Item = (&'a str, &'a str)> + Clone) -> PostalBrand {
        let mut brand = PostalBrand::default();
        let mut post_office = false;
        for (k, v) in tags.clone() {
            match k {
                "post_office:brand" | "post_office:service_provider" => {
                    brand.words.extend(brand_words(v))
                }
                "post_office:brand:wikidata" => brand.ids.extend(parse_wikidata_ids(v)),
                "amenity" => post_office |= v == "post_office",
                _ => {}
            }
        }
        if !post_office || !brand.ids.is_empty() || !brand.words.is_empty() {
            return brand;
        }
        let mut name = Vec::new();
        for (k, v) in tags {
            match k {
                "brand" | "operator" => brand.words.extend(brand_words(v)),
                "brand:wikidata" | "operator:wikidata" => brand.ids.extend(parse_wikidata_ids(v)),
                "name" => name = brand_words(v),
                _ => {}
            }
        }
        // Without a brand, the name often gives it, as in a post office
        // named `Hermes PaketShop` or `DHL`.
        if brand.ids.is_empty() && brand.words.is_empty() {
            brand.words = name;
        }
        brand
    }

    /// Whether nothing is known about the network.
    fn is_unknown(&self) -> bool {
        self.ids.is_empty() && self.words.is_empty()
    }

    /// Whether two partners may be of the same network: they share a
    /// Wikidata ID or a brand word, or have nothing to compare, such as
    /// `Die Post` and `La Poste`, whose words are all generic.
    pub(crate) fn agrees(&self, other: &PostalBrand) -> bool {
        let comparable = (!self.ids.is_empty() && !other.ids.is_empty())
            || (!self.words.is_empty() && !other.words.is_empty());
        !comparable
            || self.ids.iter().any(|id| other.ids.contains(id))
            || self.words.iter().any(|w| other.words.contains(w))
    }

    /// Whether the brand has a word that OSM's `post_office:brand`
    /// value `value` shares.
    pub(crate) fn shares_word(&self, value: &str) -> bool {
        brand_words(value).iter().any(|w| self.words.contains(w))
    }
}

/// The words of a postal brand: folded, of at least three letters, and
/// not in [GENERIC_BRAND_WORDS]. `DHL Paketshop` gives `dhl`; `Die Post`
/// gives none.
fn brand_words(value: &str) -> Vec<String> {
    fold_tokens(value)
        .into_iter()
        .filter(|w| w.chars().count() >= 3 && !GENERIC_BRAND_WORDS.contains(&w.as_str()))
        .collect()
}

/// How to recognize the host of an ATP partner in OSM: by the business
/// name that ATP gives, in `located_in` or as the partner's `name`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PartnerHost {
    /// The folded words of the host's name, without [LEGAL_FORMS].
    name: Vec<String>,
    /// The ATP partner's address tags, for [osm_address_in_atp_text].
    address: Vec<(String, String)>,
}

impl PartnerHost {
    /// The host of an ATP partner with `tags`, if ATP names it: its
    /// `located_in`, or else its `name`, unless the name is the postal
    /// network's own, such as `FedEx OnSite` or `Postpartner Nendeln`.
    pub(crate) fn of_atp<'a>(
        tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
        brand: &PostalBrand,
    ) -> Option<PartnerHost> {
        let tag = |key: &str| tags.clone().find(|(k, _)| *k == key).map(|(_, v)| v);
        let name = match tag("located_in") {
            Some(host) => name_words(host),
            None => {
                let name = name_words(tag("name")?);
                let own = |w: &String| brand.words.contains(w) || w == "postpartner";
                if name.iter().any(own) {
                    return None;
                }
                name
            }
        };
        if name.is_empty() {
            return None;
        }
        let address = tags
            .filter(|(k, _)| k.starts_with("addr:"))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Some(PartnerHost { name, address })
    }

    /// Whether the OSM feature with `tags` is the host: its `name` or
    /// `brand` is the host's name ([same_name]), or the distinctive words
    /// of one name are all in the other while its address is the
    /// partner's. Only the second takes `Getränkemarkt Hust` for `Hust
    /// Getränkemarkt`, or `Bäckerei Harmonie` for `Harmonie`, but not
    /// `Backshop Waterkant` for `Physio an der Waterkant`. A word is
    /// distinctive if it isn't in [GENERIC_NAME_WORDS], a number, or in
    /// the partner's address. The address alone is not enough: in
    /// Berlin, the OSM store at the partner's address was often another
    /// one in the same building.
    pub(crate) fn agrees<'a>(
        &self,
        tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    ) -> bool {
        let names = || {
            tags.clone()
                .filter(|(k, _)| *k == "name" || *k == "brand")
                .map(|(_, v)| name_words(v))
        };
        if names().any(|n| same_name(&self.name, &n)) {
            return true;
        }
        let address_words: Vec<String> = self
            .address
            .iter()
            .flat_map(|(_, v)| fold_tokens(v))
            .collect();
        let distinctive = |w: &&String| {
            w.chars().count() >= 3
                && !w.chars().any(|c| c.is_numeric())
                && !GENERIC_NAME_WORDS.contains(&w.as_str())
                && !address_words.contains(w)
        };
        let own: Vec<&String> = self.name.iter().filter(distinctive).collect();
        let within =
            |a: &[&String], b: &[&String]| !a.is_empty() && a.iter().all(|w| b.contains(w));
        if !names().any(|n| {
            let theirs: Vec<&String> = n.iter().filter(distinctive).collect();
            within(&own, &theirs) || within(&theirs, &own)
        }) {
            return false;
        }
        let atp: Vec<(&str, &str)> = self
            .address
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let osm: Vec<(&str, &str)> = tags.collect();
        osm_address_in_atp_text(&atp, &osm)
    }
}

/// The folded words of a business name, without [LEGAL_FORMS].
fn name_words(name: &str) -> Vec<String> {
    fold_tokens(name)
        .into_iter()
        .filter(|w| !LEGAL_FORMS.contains(&w.as_str()))
        .collect()
}

/// Whether two names, as [name_words], are the same, or one is the
/// other followed by more words, such as `REWE Adrian Powierski` and
/// `REWE`; or they are the same without spaces, such as `M & J
/// Nightshop` and `M&J Nightshop`.
fn same_name(a: &[String], b: &[String]) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    !short.is_empty() && (long.starts_with(short) || a.concat() == b.concat())
}

/// Matches an ATP partner with the OSM feature that already is the
/// partner; see the module documentation.
pub struct PostPartnerMatcher {
    center: Point,
    brand: PostalBrand,
    host: Option<PartnerHost>,
}

impl PostPartnerMatcher {
    pub fn for_place(place: &Place) -> Option<PostPartnerMatcher> {
        let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        if !is_post_partner(tags()) {
            return None;
        }
        let brand = PostalBrand::of_atp(&place.spider, tags());
        let host = PartnerHost::of_atp(tags(), &brand);
        Some(PostPartnerMatcher {
            center: s2::cell::Cell::from(CellID(place.s2_cell_id)).center(),
            brand,
            host,
        })
    }

    fn is_host(&self, candidate: &OsmCandidate) -> bool {
        self.host
            .as_ref()
            .is_some_and(|h| h.agrees(candidate.tags()))
    }
}

impl Matcher for PostPartnerMatcher {
    /// Up to 0.5 for an OSM partner of the same brand within
    /// [SAME_METERS], and from 0.5 to 1.0 for one that is also the host
    /// by name, within [HOST_METERS]; each falling with the distance.
    fn score_osm_candidate(&self, candidate: &OsmCandidate) -> f64 {
        if !is_osm_partner(candidate.tags())
            || !self.brand.agrees(&PostalBrand::of_osm(candidate.tags()))
        {
            return 0.0;
        }
        // A partner of an unknown network may be the OSM partner, but not
        // a post office that OSM doesn't call a partner: in Japan, Yamato
        // partners whose network we didn't know took Japan Post offices.
        if self.brand.is_unknown()
            && !candidate
                .tags()
                .any(|(k, v)| k == "post_office" && v == "post_partner")
        {
            return 0.0;
        }
        let Some(centroid) = candidate.geometry().ok().and_then(|g| g.centroid()) else {
            return 0.0;
        };
        let d = geo_point_distance(&self.center, &centroid);
        if self.is_host(candidate) {
            if d > HOST_METERS {
                return 0.0;
            }
            0.5 + 0.5 * (HOST_METERS - d) / HOST_METERS
        } else if d <= SAME_METERS {
            0.5 * (SAME_METERS - d) / SAME_METERS
        } else {
            0.0
        }
    }

    /// `post_partner_name` if the OSM partner is also the host by name,
    /// else `post_partner`: an OSM partner of the brand close by.
    fn rule(&self, candidate: &OsmCandidate) -> &'static str {
        if self.is_host(candidate) {
            "post_partner_name"
        } else {
            "post_partner"
        }
    }

    /// Partners say nothing about which OSM stores are gone.
    fn counts_for_coverage(&self) -> bool {
        false
    }

    /// A partner without a host in OSM isn't a place to add.
    fn reports_missing(&self) -> bool {
        false
    }

    fn reports_located_in(&self) -> bool {
        true
    }

    /// Two partners of one network are two counters.
    fn is_exclusive(&self) -> bool {
        true
    }

    fn max_distance(&self) -> Option<ChordAngle> {
        Some(meters_to_chord_angle(if self.host.is_some() {
            HOST_METERS
        } else {
            SAME_METERS
        }))
    }

    /// An OSM partner may be of any kind: mostly a shop, but also a
    /// café or a fuel station.
    fn query_mask(&self) -> Option<MatchMask> {
        Some(MatchMask(u16::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::{Feature, StringPool};
    use crate::utils::UtcTimestamp;
    use tempfile::TempDir;

    const LAT: f64 = 52.52;
    const LON: f64 = 13.405;

    fn place(spider: &str, tags: &[(&str, &str)]) -> Place {
        let mut mask = MatchMask::default();
        for (k, v) in tags {
            mask.add_tag(k, v);
        }
        let coord = geo::Coord { x: LON, y: LAT };
        Place::new(
            &coord,
            String::from(spider),
            mask,
            tags.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            &geo::Geometry::from(geo::Point::from(coord)),
            UtcTimestamp(time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap()),
        )
        .expect("valid place")
    }

    /// The score of an ATP partner of `spider` with `atp` tags against
    /// an OSM feature with `osm` tags `meters` north of it, and the rule.
    fn score(
        spider: &str,
        atp: &[(&str, &str)],
        osm: &[(&str, &str)],
        meters: f64,
    ) -> (f64, Option<&'static str>) {
        let matcher = PostPartnerMatcher::for_place(&place(spider, atp)).expect("a partner");
        let dir = TempDir::new().expect("tempdir");
        let pool = StringPool::create(
            osm.iter().flat_map(|(k, v)| [k.to_string(), v.to_string()]),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let lat = LAT + meters / 6_371_000.0 * 180.0 / std::f64::consts::PI;
        let mut geometry_wkb = Vec::new();
        wkb::writer::write_point(
            &mut geometry_wkb,
            &geo::Point::new(LON, lat),
            &wkb::writer::WriteOptions {
                endianness: wkb::Endianness::LittleEndian,
            },
        )
        .expect("wkb");
        let feature = Feature {
            id: 1,
            tags: osm
                .iter()
                .flat_map(|(k, v)| {
                    [
                        pool.lookup(k).unwrap() as u32,
                        pool.lookup(v).unwrap() as u32,
                    ]
                })
                .collect(),
            geometry_wkb,
            ..Default::default()
        };
        let candidate = OsmCandidate {
            feature: &feature,
            strings: &pool,
        };
        let s = matcher.score_osm_candidate(&candidate);
        (s, (s > 0.0).then(|| matcher.rule(&candidate)))
    }

    const KIOSK: &[(&str, &str)] = &[
        ("amenity", "yes"),
        ("post_office", "post_partner"),
        ("name", "Kiosk Waibel"),
        ("addr:street", "Washingtonallee"),
        ("addr:housenumber", "34"),
    ];

    #[test]
    fn test_is_post_partner() {
        assert!(is_post_partner(KIOSK.iter().copied()));
        assert!(!is_post_partner(
            [("shop", "convenience"), ("post_office", "post_partner")].into_iter()
        ));
        assert!(!is_post_partner(
            [("amenity", "yes"), ("post_office", "bureau")].into_iter()
        ));
        assert!(PostPartnerMatcher::for_place(&place("s", &[("shop", "kiosk")])).is_none());
    }

    #[test]
    fn test_import_mask() {
        // Without a mask, `import_atp` would drop the partner.
        assert!(!place("deutsche_post_de", KIOSK).mask.is_empty());
    }

    #[test]
    fn test_same() {
        let osm = [
            ("shop", "kiosk"),
            ("name", "Kiosk Waibel"),
            ("post_office", "post_partner"),
            ("post_office:brand", "DHL"),
        ];
        let (s, rule) = score("deutsche_post_de", KIOSK, &osm, 50.0);
        assert!(s > 0.5, "{s}");
        assert_eq!(rule, Some("post_partner_name"));
        assert_eq!(score("deutsche_post_de", KIOSK, &osm, 150.0).0, 0.0);

        // Another name: only by distance.
        let other = [
            ("shop", "kiosk"),
            ("name", "Späti"),
            ("post_office", "post_partner"),
            ("post_office:brand", "Deutsche Post"),
        ];
        let (s, rule) = score("deutsche_post_de", KIOSK, &other, 10.0);
        assert!(s > 0.0 && s < 0.5, "{s}");
        assert_eq!(rule, Some("post_partner"));
        assert_eq!(score("deutsche_post_de", KIOSK, &other, 40.0).0, 0.0);

        // Another network.
        let hermes = [
            ("shop", "kiosk"),
            ("name", "Kiosk Waibel"),
            ("post_office", "post_partner"),
            ("post_office:brand", "Hermes PaketShop"),
        ];
        assert_eq!(score("deutsche_post_de", KIOSK, &hermes, 5.0).0, 0.0);

        // A partner of an unknown network and a post office.
        let unknown = [
            ("amenity", "yes"),
            ("post_office", "post_partner"),
            ("name", "段ストアー"),
        ];
        let office = [("amenity", "post_office"), ("name", "中津川郵便局")];
        assert_eq!(score("s", &unknown, &office, 5.0).0, 0.0);
        let partner = [("shop", "convenience"), ("post_office", "post_partner")];
        assert!(score("s", &unknown, &partner, 5.0).0 > 0.0);
        let yamato = [
            ("amenity", "yes"),
            ("post_office", "post_partner"),
            ("post_office:service_provider", "ヤマト運輸"),
        ];
        assert_eq!(score("kuroneko_jp", &yamato, &office, 5.0).0, 0.0);

        // A post office named after another network.
        let named = [("amenity", "post_office"), ("name", "Hermes PaketShop")];
        assert_eq!(score("deutsche_post_de", KIOSK, &named, 5.0).0, 0.0);

        // A DHL post office without a `post_office` tag.
        let office = [("amenity", "post_office"), ("brand", "DHL")];
        assert!(score("deutsche_post_de", KIOSK, &office, 5.0).0 > 0.0);
        let bureau = [
            ("amenity", "post_office"),
            ("brand", "DHL"),
            ("post_office", "bureau"),
        ];
        assert_eq!(score("deutsche_post_de", KIOSK, &bureau, 5.0).0, 0.0);

        // The kiosk itself isn't a partner yet: that's `located_in`.
        let plain = [("shop", "kiosk"), ("name", "Kiosk Waibel")];
        assert_eq!(score("deutsche_post_de", KIOSK, &plain, 5.0).0, 0.0);
    }

    #[test]
    fn test_generic_brand() {
        let atp = [
            ("amenity", "yes"),
            ("post_office", "post_partner"),
            ("post_office:brand", "Die Post"),
            ("post_office:brand:wikidata", "Q614803"),
            ("name", "Volg"),
        ];
        let osm = [
            ("shop", "convenience"),
            ("name", "Volg"),
            ("post_office", "post_partner"),
            ("post_office:brand", "Die Post AG"),
        ];
        assert!(score("swiss_post_ch", &atp, &osm, 20.0).0 > 0.5);
    }

    #[test]
    fn test_postal_brand() {
        let fedex = PostalBrand::of_atp(
            "fedex",
            [("brand", "FedEx"), ("brand:wikidata", "Q376738")].into_iter(),
        );
        assert_eq!(fedex.name.as_deref(), Some("FedEx"));
        assert_eq!(fedex.wikidata, Some(376738));
        assert!(fedex.agrees(&PostalBrand::of_osm(
            [("post_office:brand", "FedEx OnSite")].into_iter()
        )));
        let correios = PostalBrand::of_atp(
            "correios_br",
            [("operator", "Empresa Brasileira de Correios e Telégrafos")].into_iter(),
        );
        assert_eq!(correios.name, None);
        assert!(correios.agrees(&PostalBrand::of_osm(
            [("amenity", "post_office"), ("operator", "Correios")].into_iter()
        )));
        let dhl = PostalBrand::of_atp("deutsche_post_de", std::iter::empty());
        assert_eq!(dhl.name.as_deref(), Some("DHL"));
        assert!(dhl.shares_word("DHL Paketshop"));
        assert!(!dhl.shares_word("Hermes"));
    }

    fn host(tags: &[(&str, &str)]) -> Option<PartnerHost> {
        let brand = PostalBrand::of_atp("s", tags.iter().copied());
        PartnerHost::of_atp(tags.iter().copied(), &brand)
    }

    #[test]
    fn test_host() {
        let h = host(KIOSK).expect("a host");
        assert!(h.agrees([("shop", "kiosk"), ("name", "Kiosk Waibel")].into_iter()));
        assert!(h.agrees([("shop", "kiosk"), ("name", "KIOSK WAIBEL")].into_iter()));
        assert!(!h.agrees([("shop", "kiosk"), ("name", "Späti Waibel")].into_iter()));
        // A distinctive word at the same address.
        assert!(
            h.agrees(
                [
                    ("shop", "kiosk"),
                    ("name", "Waibel"),
                    ("addr:street", "Washingtonallee"),
                    ("addr:housenumber", "34"),
                ]
                .into_iter()
            )
        );
        // Only some distinctive words in common.
        let backshop = host(&[
            ("amenity", "yes"),
            ("post_office", "post_partner"),
            ("name", "Backshop Waterkant"),
            ("addr:street", "Wasserstadtweg"),
            ("addr:housenumber", "7"),
        ])
        .expect("a host");
        let at = |name| {
            [
                ("shop", "bakery"),
                ("name", name),
                ("addr:street", "Wasserstadtweg"),
                ("addr:housenumber", "7"),
            ]
        };
        assert!(!backshop.agrees(at("Physio an der Waterkant").into_iter()));
        assert!(backshop.agrees(at("Waterkant Backshop").into_iter()));
        // The address alone is not enough.
        assert!(
            !h.agrees(
                [
                    ("shop", "hairdresser"),
                    ("name", "Lord Barbershop"),
                    ("addr:street", "Washingtonallee"),
                    ("addr:housenumber", "34"),
                ]
                .into_iter()
            )
        );

        let rewe = host(&[
            ("amenity", "yes"),
            ("post_office", "post_partner"),
            ("name", "REWE Adrian Powierski OHG"),
        ])
        .expect("a host");
        assert!(rewe.agrees([("shop", "supermarket"), ("brand", "REWE")].into_iter()));

        let fedex = host(&[
            ("amenity", "yes"),
            ("post_office", "post_partner"),
            ("brand", "FedEx"),
            ("name", "FedEx OnSite"),
            ("located_in", "Walgreens"),
        ])
        .expect("a host");
        assert!(fedex.agrees([("amenity", "pharmacy"), ("name", "Walgreens")].into_iter()));
        // The network's own name names no host.
        assert_eq!(
            host(&[
                ("amenity", "yes"),
                ("post_office", "post_partner"),
                ("brand", "FedEx"),
                ("name", "FedEx Authorized ShipCenter"),
            ]),
            None
        );
        assert_eq!(
            host(&[
                ("amenity", "yes"),
                ("post_office", "post_partner"),
                ("post_office:brand", "Liechtensteinische Post"),
                ("name", "Postpartner Nendeln"),
            ]),
            None
        );
    }
}
