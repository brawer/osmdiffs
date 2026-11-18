//! The names that a brand, operator or network goes by, for finding it
//! in the `name` of an OpenStreetMap feature that has no `brand` tag,
//! such as `name=セブン-イレブン 八重瀬東風平西店` or `name=Rubis
//! Majaoni` (brawer/osmdiffs#989).
//!
//! The names come from two sources: the AllThePlaces feature's own
//! `brand`, `operator` and `network` tags (and their `:xx` language
//! variants), and the [name-suggestion-index](https://nsi.guide) (NSI),
//! looked up by the feature's `brand:wikidata`, `operator:wikidata` and
//! `network:wikidata`. AllThePlaces copies NSI's canonical tags into its
//! features, but not NSI's `matchNames`: the aliases, abbreviations and
//! former names that mappers often type, such as `Agip` for Eni,
//! `Vijaya Bank` for Bank of Baroda or `7/11` for 7-Eleven. See
//! `docs/notes/2026-11-18-brand-in-name.md` for the evaluation.
//!
//! An NSI item holds only where its `locationSet` says, because one
//! brand can have other names elsewhere: Korean `matchNames` of
//! 7-Eleven include Ministop, which is a brand of its own in Japan. We
//! resolve the `locationSet` with our own country lookup
//! ([countries::regions]) rather than NSI's polygons; see [Region].

use crate::matchers::{parse_wikidata_ids, split_values, street};
use crate::utils::countries;
use anyhow::{Context, Result};
use icu_properties::{CodePointMapData, props::LineBreak};
use std::collections::HashMap;
use std::path::Path;

/// The key under which names are compared: the [street::words] of
/// `name` joined without spaces, so lowercased and without accents or
/// punctuation. `7-Eleven`, `7 ELEVEN` and `7eleven` all give
/// `7eleven`, and `セブン-イレブン` gives `セフンイレフン` (voicing
/// marks are accents too).
pub(crate) fn name_key(name: &str) -> String {
    street::words(name).collect()
}

/// Whether a character belongs to a script that is written without
/// spaces between words, so that a word can end at any character:
/// Chinese, Japanese, Thai, Lao, Khmer, Myanmar and the like. These are
/// the characters whose Unicode line break class is ideographic (`ID`),
/// conditional Japanese starter (`CJ`, such as small kana and `ー`), or
/// complex context (`SA`, scripts that need a dictionary to find word
/// boundaries); see [UAX #14](https://www.unicode.org/reports/tr14/).
fn written_without_spaces(c: char) -> bool {
    let class = CodePointMapData::<LineBreak>::new().get(c);
    class == LineBreak::Ideographic
        || class == LineBreak::ConditionalJapaneseStarter
        || class == LineBreak::ComplexContext
}

/// Whether a name with words `name_words` (by [street::words]) begins
/// with the brand whose [name_key] is `brand`. The brand must end at a
/// word boundary, so that `Spar` is not found in `Sparkasse`, unless
/// the characters on either side of its end are [written without
/// spaces](written_without_spaces): `ローソン` is found in
/// `ローソン能美寺井店`. Words split by punctuation count as one, as in
/// [name_key]: `7-Eleven` is found in `7 Eleven Main Street`.
pub(crate) fn starts_with_brand(name_words: &[String], brand: &str) -> bool {
    if brand.chars().count() < MIN_KEY_CHARS {
        return false;
    }
    let mut rest = brand;
    for word in name_words {
        if rest.is_empty() {
            return true;
        }
        if let Some(r) = rest.strip_prefix(word.as_str()) {
            rest = r;
        } else if let Some(tail) = word.strip_prefix(rest) {
            let last = rest.chars().next_back().is_some_and(written_without_spaces);
            let next = tail.chars().next().is_some_and(written_without_spaces);
            return last || next;
        } else {
            return false;
        }
    }
    rest.is_empty()
}

/// Shorter [name_key]s are never looked for in a name.
const MIN_KEY_CHARS: usize = 2;

/// Keys of OSM and ATP tags whose values name a brand, operator or
/// network, without language suffix.
const NAME_KEYS: [&str; 3] = ["brand", "operator", "network"];

/// Keys of the Wikidata IDs of [NAME_KEYS].
const WIKIDATA_KEYS: [&str; 3] = ["brand:wikidata", "operator:wikidata", "network:wikidata"];

/// Whether `key` is one of `keys`, or one of them with a language
/// suffix such as `brand:ja` or `name:zh-Hant`.
fn is_name_key(key: &str, keys: &[&str]) -> bool {
    keys.iter().any(|k| {
        key == *k
            || key
                .strip_prefix(k)
                .and_then(|r| r.strip_prefix(':'))
                .is_some_and(|lang| {
                    let lang = lang.split('-').next().unwrap_or("");
                    (2..=3).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_lowercase())
                })
    })
}

/// The [name_key]s of an AllThePlaces feature's own `brand`,
/// `operator` and `network` tags, and their language variants such as
/// `brand:ja`.
pub(crate) fn atp_name_keys<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<String> {
    let mut keys: Vec<String> = tags
        .filter(|(k, _)| is_name_key(k, &NAME_KEYS))
        .flat_map(|(_, v)| split_values(v).map(name_key))
        .filter(|k| !k.is_empty())
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// The Wikidata IDs of an AllThePlaces feature's brand, operator and
/// network.
pub(crate) fn wikidata_ids<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<u64> {
    let mut ids: Vec<u64> = tags
        .filter(|(k, _)| WIKIDATA_KEYS.contains(k))
        .flat_map(|(_, v)| parse_wikidata_ids(v))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Where an NSI item holds, as one entry of its `locationSet`.
#[derive(Clone, Debug, PartialEq)]
enum Region {
    /// `001`, the whole world.
    World,
    /// A country or subdivision known to [countries::regions], such as
    /// `JP` or `US-TX`. NSI's own polygons named after a subdivision,
    /// such as `us-tx.geojson`, are taken to be that subdivision, or
    /// its country if [countries] doesn't know the subdivision; the
    /// same for ISO subdivisions such as `gb-wls`. M49 Europe (`150`)
    /// is its countries.
    Any(Vec<String>),
    /// A circle given as `[lng, lat, radius in km]`.
    Circle { lng: f64, lat: f64, radius_km: f64 },
    /// Something we can't resolve: other M49 regions, Wikidata IDs and
    /// NSI's polygons not named after a country. Holds nowhere, so
    /// the item is left out rather than used in the wrong place.
    Unknown,
}

/// UN M49 region 150, Europe.
const EUROPE: [&str; 51] = [
    "AD", "AL", "AT", "AX", "BA", "BE", "BG", "BY", "CH", "CZ", "DE", "DK", "EE", "ES", "FI", "FO",
    "FR", "GB", "GG", "GI", "GR", "HR", "HU", "IE", "IM", "IS", "IT", "JE", "LI", "LT", "LU", "LV",
    "MC", "MD", "ME", "MK", "MT", "NL", "NO", "PL", "PT", "RO", "RS", "RU", "SE", "SI", "SJ", "SK",
    "SM", "UA", "VA",
];

impl Region {
    fn parse(value: &serde_json::Value) -> Region {
        if let Some(a) = value.as_array() {
            let num = |i: usize| a.get(i).and_then(|v| v.as_f64());
            return match (num(0), num(1)) {
                (Some(lng), Some(lat)) => Region::Circle {
                    lng,
                    lat,
                    radius_km: num(2).unwrap_or(25.0),
                },
                _ => Region::Unknown,
            };
        }
        let Some(s) = value.as_str() else {
            return Region::Unknown;
        };
        let s = s.to_ascii_uppercase();
        let s = s.strip_suffix(".GEOJSON").unwrap_or(&s);
        match s {
            "001" => return Region::World,
            "150" => return Region::Any(EUROPE.iter().map(|c| c.to_string()).collect()),
            _ => {}
        }
        let country = s.split('-').next().unwrap_or("");
        let is_country = country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase());
        if !is_country {
            return Region::Unknown;
        }
        if countries::is_region(s) {
            Region::Any(vec![s.to_string()])
        } else if countries::is_region(country) {
            Region::Any(vec![country.to_string()])
        } else {
            Region::Unknown
        }
    }

    fn contains(&self, lng: f64, lat: f64, regions: &[&str]) -> bool {
        match self {
            Region::World => true,
            Region::Any(ids) => ids.iter().any(|id| regions.contains(&id.as_str())),
            Region::Circle {
                lng: clng,
                lat: clat,
                radius_km,
            } => {
                let a = s2::latlng::LatLng::from_degrees(lat, lng);
                let b = s2::latlng::LatLng::from_degrees(*clat, *clng);
                a.distance(&b).rad() * super::EARTH_RADIUS_METERS <= radius_km * 1000.0
            }
            Region::Unknown => false,
        }
    }
}

/// One NSI item, reduced to what name matching needs.
#[derive(Clone, Debug)]
struct Item {
    /// The item's category as `key=value`, such as
    /// `shop=convenience`, from its path in NSI.
    category: String,
    include: Vec<Region>,
    exclude: Vec<Region>,
    /// The [name_key]s of its `name`, `brand`, `operator` and
    /// `network` tags (with language variants), and of its
    /// `matchNames`.
    names: Vec<String>,
}

impl Item {
    fn holds_everywhere(&self) -> bool {
        self.exclude.is_empty() && self.include.contains(&Region::World)
    }

    fn holds_at(&self, lng: f64, lat: f64, regions: &[&str]) -> bool {
        self.include.iter().any(|r| r.contains(lng, lat, regions))
            && !self.exclude.iter().any(|r| r.contains(lng, lat, regions))
    }
}

/// The names of brands, operators and networks in the
/// name-suggestion-index, by Wikidata ID.
#[derive(Debug, Default)]
pub struct NsiNames {
    items: HashMap<u64, Vec<Item>>,
    /// NSI's version, such as `8.0.20261101`.
    pub version: String,
}

impl NsiNames {
    /// Reads `dist/json/nsi.json` (or its `.min.json`) of the
    /// name-suggestion-index from `path`.
    pub fn open(path: &Path) -> Result<NsiNames> {
        let bytes =
            std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(NsiNames::from_json(&json))
    }

    /// Builds the table from NSI's `nsi.json`. Items without a
    /// Wikidata ID are left out, and so is the `flags` tree.
    pub fn from_json(json: &serde_json::Value) -> NsiNames {
        let mut items: HashMap<u64, Vec<Item>> = HashMap::new();
        let Some(trees) = json["nsi"].as_object() else {
            return NsiNames::default();
        };
        for (path, tree) in trees {
            let mut parts = path.split('/');
            let (Some(kind), Some(key), Some(value)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if kind == "flags" {
                continue;
            }
            let category = format!("{key}={value}");
            for item in tree["items"].as_array().into_iter().flatten() {
                let tags = item["tags"].as_object();
                let tag_pairs = tags
                    .into_iter()
                    .flatten()
                    .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?)));
                let ids = wikidata_ids(tag_pairs.clone());
                if ids.is_empty() {
                    continue;
                }
                let mut names: Vec<String> = tag_pairs
                    .filter(|(k, _)| is_name_key(k, &["name", "brand", "operator", "network"]))
                    .flat_map(|(_, v)| split_values(v).map(name_key))
                    .chain(
                        item["matchNames"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|n| n.as_str())
                            .map(name_key),
                    )
                    .filter(|k| !k.is_empty())
                    .collect();
                names.sort();
                names.dedup();
                let regions = |side: &str| -> Vec<Region> {
                    item["locationSet"][side]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(Region::parse)
                        .collect()
                };
                let parsed = Item {
                    category: category.clone(),
                    include: regions("include"),
                    exclude: regions("exclude"),
                    names,
                };
                for id in ids {
                    items.entry(id).or_default().push(parsed.clone());
                }
            }
        }
        let version = json["_meta"]["version"].as_str().unwrap_or("").to_string();
        NsiNames { items, version }
    }

    /// How many Wikidata IDs have names.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Adds to `out` the [name_key]s of the NSI items of `ids` that
    /// hold at (`lng`, `lat`). Items of `category` (`key=value`) are
    /// preferred: NSI's `matchNames` are meant for one category, such
    /// as `Circle K` for FamilyMart convenience stores in Japan. Only if
    /// an ID has no item of that category, all its items count.
    pub fn add_names(
        &self,
        ids: &[u64],
        category: Option<&str>,
        lng: f64,
        lat: f64,
        out: &mut Vec<String>,
    ) {
        let mut regions: Option<Vec<&str>> = None;
        for id in ids {
            let Some(items) = self.items.get(id) else {
                continue;
            };
            let same_category = items.iter().any(|i| Some(i.category.as_str()) == category);
            for item in items {
                if same_category && Some(item.category.as_str()) != category {
                    continue;
                }
                let holds = item.holds_everywhere() || {
                    let regions = regions.get_or_insert_with(|| countries::regions(lng, lat));
                    item.holds_at(lng, lat, regions)
                };
                if holds {
                    out.extend(item.names.iter().cloned());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        street::words(s).collect()
    }

    fn found(name: &str, brand: &str) -> bool {
        starts_with_brand(&words(name), &name_key(brand))
    }

    #[test]
    fn test_name_key() {
        assert_eq!(name_key("7-Eleven"), "7eleven");
        assert_eq!(name_key("7 ELEVEN"), "7eleven");
        assert_eq!(name_key("Café Spettacolo"), "cafespettacolo");
        assert_eq!(name_key("セブン-イレブン"), name_key("セブンイレブン"));
        assert_eq!(name_key(" - "), "");
    }

    #[test]
    fn test_starts_with_brand() {
        // Latin script: only at a word boundary.
        assert!(found("Rubis Majaoni", "Rubis"));
        assert!(found("RUBIS Filling Station", "Rubis"));
        assert!(found("Rubis", "Rubis"));
        assert!(found("7 Eleven Main Street", "7-Eleven"));
        assert!(found("Total Kitui Service Station", "Total"));
        assert!(!found("Sparkasse Zürich", "Spar"));
        assert!(!found("Minimalist Atelier", "Mini"));
        assert!(!found("Coop", "Coop Pronto"));
        assert!(!found("Pronto Coop", "Coop"));

        // Japanese and Chinese: anywhere.
        assert!(found("ローソン能美寺井店", "ローソン"));
        assert!(found("セブン-イレブン 八重瀬東風平西店", "セブン-イレブン"));
        assert!(found("山陰合同銀行浦郷支店", "山陰合同銀行"));
        assert!(found("WARHAMMERなんば", "Warhammer"));
        assert!(found("ファミリーマート", "ファミリーマート"));
        assert!(found("麦当劳北京路店", "麦当劳"));

        // Thai, Lao, Khmer, Myanmar: also anywhere.
        assert!(found("บิ๊กซีซุปเปอร์เซ็นเตอร์ กาญจนบุรี", "บิ๊กซี"));
        assert!(found("ທະນາຄານການຄ້າຕ່າງປະເທດລາວ", "ທະນາຄານ"));

        // Korean and Arabic use spaces.
        assert!(found("세븐일레븐 강남점", "세븐일레븐"));
        assert!(!found("세븐일레븐강남점", "세븐일"));
        assert!(found("كارفور مول الإمارات", "كارفور"));
        assert!(!found("كارفورمول", "كارفور"));

        // Too short.
        assert!(!found("A Bar", "A"));
    }

    #[test]
    fn test_atp_name_keys() {
        let tags = [
            ("brand", "FamilyMart"),
            ("brand:ja", "ファミリーマート"),
            ("brand:wikidata", "Q11247682"),
            ("operator", "Foo;Bar"),
            ("network:en", "Baz"),
            ("name", "Not a brand"),
            ("brand:wikidata:note", "nope"),
        ];
        assert_eq!(
            atp_name_keys(tags.into_iter()),
            vec![
                "bar",
                "baz",
                "familymart",
                "foo",
                name_key("ファミリーマート").as_str()
            ]
        );
        assert_eq!(wikidata_ids(tags.into_iter()), vec![11247682]);
    }

    fn nsi() -> NsiNames {
        NsiNames::from_json(&serde_json::json!({
            "_meta": {"version": "8.0.20261101"},
            "nsi": {
                "brands/shop/convenience": {"items": [
                    {
                        "id": "familymart-652b3e",
                        "locationSet": {"include": ["jp"]},
                        "matchNames": ["circle k", "sunkus"],
                        "tags": {
                            "brand": "FamilyMart",
                            "brand:ja": "ファミリーマート",
                            "brand:wikidata": "Q11247682",
                            "name": "ファミリーマート"
                        }
                    },
                    {
                        "id": "7eleven-fab923",
                        "locationSet": {"include": ["kr"]},
                        "matchNames": ["ministop"],
                        "tags": {"brand": "세븐일레븐", "brand:wikidata": "Q259340"}
                    },
                    {
                        "id": "7eleven-e1b4e3",
                        "locationSet": {"include": ["001"], "exclude": ["jp", "kr"]},
                        "matchNames": ["7-11"],
                        "tags": {"brand": "7-Eleven", "brand:wikidata": "Q259340"}
                    },
                    {
                        "id": "nowikidata",
                        "locationSet": {"include": ["001"]},
                        "tags": {"brand": "Nobody"}
                    }
                ]},
                "brands/amenity/fuel": {"items": [
                    {
                        "id": "7eleven-16b3c2",
                        "locationSet": {"include": ["001"]},
                        "matchNames": ["seven eleven fuel"],
                        "tags": {"brand": "7-Eleven", "brand:wikidata": "Q259340"}
                    },
                    {
                        "id": "texas",
                        "locationSet": {"include": ["us-tx.geojson"]},
                        "tags": {"brand": "Texaco", "brand:wikidata": "Q775060"}
                    },
                    {
                        "id": "zurich",
                        "locationSet": {"include": [[8.54, 47.37, 10]]},
                        "tags": {"brand": "Zürich", "brand:wikidata": "Q1"}
                    },
                    {
                        "id": "europe",
                        "locationSet": {"include": ["150"]},
                        "tags": {"brand": "Euro", "brand:wikidata": "Q2"}
                    },
                    {
                        "id": "unknown",
                        "locationSet": {"include": ["Q35"]},
                        "tags": {"brand": "Danish", "brand:wikidata": "Q3"}
                    }
                ]},
                "flags/man_made/flagpole": {"items": [
                    {"id": "flag", "locationSet": {"include": ["001"]},
                     "tags": {"subject": "X", "subject:wikidata": "Q4", "brand:wikidata": "Q4"}}
                ]}
            }
        }))
    }

    fn names(nsi: &NsiNames, id: u64, category: &str, lng: f64, lat: f64) -> Vec<String> {
        let mut out = Vec::new();
        nsi.add_names(&[id], Some(category), lng, lat, &mut out);
        out.sort();
        out
    }

    const TOKYO: (f64, f64) = (139.69, 35.69);
    const SEOUL: (f64, f64) = (126.98, 37.57);
    const ZURICH: (f64, f64) = (8.5402, 47.3782);
    const DALLAS: (f64, f64) = (-96.80, 32.78);
    const DENVER: (f64, f64) = (-104.99, 39.74);

    #[test]
    fn test_nsi_names() {
        let nsi = nsi();
        assert_eq!(nsi.version, "8.0.20261101");
        assert_eq!(nsi.len(), 6); // Q11247682, Q259340, Q775060, Q1, Q2, Q3
        let conv = "shop=convenience";
        let (lng, lat) = TOKYO;
        assert_eq!(
            names(&nsi, 11247682, conv, lng, lat),
            vec![
                "circlek",
                "familymart",
                "sunkus",
                name_key("ファミリーマート").as_str()
            ]
        );
        // FamilyMart's names hold in Japan only.
        assert!(names(&nsi, 11247682, conv, SEOUL.0, SEOUL.1).is_empty());

        // 7-Eleven: Ministop only in Korea; nothing in Japan, where
        // only the world item would apply but excludes it.
        assert!(names(&nsi, 259340, conv, lng, lat).is_empty());
        assert!(names(&nsi, 259340, conv, SEOUL.0, SEOUL.1).contains(&"ministop".to_string()));
        assert_eq!(
            names(&nsi, 259340, conv, ZURICH.0, ZURICH.1),
            vec!["711", "7eleven"]
        );
        // Another category: only that category's items.
        assert_eq!(
            names(&nsi, 259340, "amenity=fuel", lng, lat),
            vec!["7eleven", "sevenelevenfuel"]
        );
        // A category NSI doesn't have for this brand: all its items.
        assert!(
            names(&nsi, 259340, "shop=bakery", SEOUL.0, SEOUL.1).contains(&"ministop".to_string())
        );

        // Regions.
        let fuel = "amenity=fuel";
        assert_eq!(
            names(&nsi, 775060, fuel, DALLAS.0, DALLAS.1),
            vec!["texaco"]
        );
        assert!(names(&nsi, 775060, fuel, DENVER.0, DENVER.1).is_empty());
        assert_eq!(names(&nsi, 1, fuel, ZURICH.0, ZURICH.1), vec!["zurich"]);
        assert!(names(&nsi, 1, fuel, 7.4474, 46.9480).is_empty()); // Bern, 90 km away
        assert_eq!(names(&nsi, 2, fuel, ZURICH.0, ZURICH.1), vec!["euro"]);
        assert!(names(&nsi, 2, fuel, lng, lat).is_empty());
        assert!(names(&nsi, 3, fuel, 12.57, 55.68).is_empty()); // unresolved
    }

    #[test]
    fn test_region_parse() {
        use serde_json::json;
        assert_eq!(Region::parse(&json!("001")), Region::World);
        assert_eq!(Region::parse(&json!("ch")), Region::Any(vec!["CH".into()]));
        assert_eq!(
            Region::parse(&json!("us-tx.geojson")),
            Region::Any(vec!["US-TX".into()])
        );
        // A polygon named after a region that country-boundaries
        // doesn't have: its country.
        assert_eq!(
            Region::parse(&json!("fr-idf.geojson")),
            Region::Any(vec!["FR".into()])
        );
        assert_eq!(Region::parse(&json!("conus")), Region::Unknown);
        assert_eq!(Region::parse(&json!("Q35")), Region::Unknown);
        assert_eq!(Region::parse(&json!("419")), Region::Unknown);
        assert_eq!(Region::parse(&json!("zz")), Region::Unknown);
    }
}
