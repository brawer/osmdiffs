use super::contact::{AtpContacts, CONTACT_FACTOR, CONTACT_MODE, category_factor, main_category};
use super::{
    MatchMask, Matcher, OsmCandidate, address, geo_point_distance, geo_point_distance_score,
    parse_wikidata_ids,
};
use crate::places::Place;
use geo::Centroid;
use s2::{cell::Cell, cellid::CellID, point::Point};
use std::sync::LazyLock;

/// Finds the first `brand:wikidata` tag in `tags` and parses its
/// value, which may list several `;`-separated IDs -- e.g. a car
/// dealer tagged `brand=BMW;Mini`, `brand:wikidata=Q26678;Q116232`.
/// Shared by `for_place` and `score_osm_candidate` -- same lookup, two
/// different tag representations (`Place.tags` and
/// `OsmCandidate::tags()`).
fn find_brand_wikidata<'a>(
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> impl Iterator<Item = u64> + 'a {
    tags.into_iter()
        .find(|&(k, _)| k == "brand:wikidata")
        .into_iter()
        .flat_map(|(_, v)| parse_wikidata_ids(v))
}

/// How [PoiMatcher] compares brands when `brand:wikidata` alone can't
/// decide, i.e. when the ATP feature or the OSM candidate lacks it.
/// Chosen with the `OSMDIFFS_BRAND_MATCH` environment variable, for
/// experiments: `wikidata` (default: only `brand:wikidata` equality),
/// `exact` (also equal `brand` tags after trimming and case folding),
/// or `jw` (also `brand` tags whose Jaro-Winkler similarity after case
/// folding is at least `OSMDIFFS_BRAND_JW_THRESHOLD`, default 0.9).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BrandMode {
    Wikidata,
    Exact,
    /// Like `Exact`, but also splits OSM's `;`-separated multi-value
    /// `brand` tags (e.g. `Toyota;Lexus`) and accepts any part.
    ExactMulti,
    JaroWinkler(f64),
    /// Token sort ratio (as in fuzzywuzzy/RapidFuzz): lowercase, split
    /// into alphanumeric words, sort them, and compute the Indel
    /// similarity of the joined strings; accepts if at least the given
    /// threshold, against any `;`-separated part of OSM's `brand`.
    TokenSort(f64),
}

static BRAND_MODE: LazyLock<BrandMode> =
    LazyLock::new(|| match std::env::var("OSMDIFFS_BRAND_MATCH").as_deref() {
        Ok("exact") => BrandMode::Exact,
        Ok("exact-multi") => BrandMode::ExactMulti,
        Ok("jw") => {
            let t = std::env::var("OSMDIFFS_BRAND_JW_THRESHOLD")
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.9);
            BrandMode::JaroWinkler(t)
        }
        Ok("tsr") => BrandMode::TokenSort(
            std::env::var("OSMDIFFS_BRAND_TSR_THRESHOLD")
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(1.0),
        ),
        _ => BrandMode::Wikidata,
    });

/// With `OSMDIFFS_BRAND_VS_NAME=1`, an OSM candidate that has neither
/// `brand` nor `brand:wikidata` is compared by its `name` instead.
static BRAND_VS_NAME: LazyLock<bool> =
    LazyLock::new(|| std::env::var("OSMDIFFS_BRAND_VS_NAME").as_deref() == Ok("1"));

/// Lowercased alphanumeric words of `s`, sorted and joined by spaces.
fn token_sort_key(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    tokens.sort_unstable();
    tokens.join(" ")
}

/// Indel similarity: 2 * LCS / (len(a) + len(b)), over chars.
fn indel_ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for ca in &a {
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = if ca == cb {
                prev[j] + 1
            } else {
                cur[j].max(prev[j + 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    2.0 * prev[b.len()] as f64 / (a.len() + b.len()) as f64
}

fn token_sort_ratio(a: &str, b: &str) -> f64 {
    indel_ratio(&token_sort_key(a), &token_sort_key(b))
}

/// With `OSMDIFFS_BRAND_REQUIRE_ATP_WIKIDATA=1`, ATP features without
/// `brand:wikidata` still get no matcher, so the `brand` tag only helps
/// to find OSM features that lack `brand:wikidata`.
static REQUIRE_ATP_WIKIDATA: LazyLock<bool> =
    LazyLock::new(|| std::env::var("OSMDIFFS_BRAND_REQUIRE_ATP_WIKIDATA").as_deref() == Ok("1"));

/// With `OSMDIFFS_ADDR_CHECK_BEYOND_M=<meters>`, candidates farther
/// than that need a matching street address (street and house number).
/// `OSMDIFFS_ADDR_CHECK_SCOPE=all` applies this to `brand:wikidata`
/// matches too; by default only to `brand`/`name` matches.
static ADDR_CHECK_BEYOND_M: LazyLock<Option<f64>> = LazyLock::new(|| {
    std::env::var("OSMDIFFS_ADDR_CHECK_BEYOND_M")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
});
static ADDR_CHECK_ALL: LazyLock<bool> =
    LazyLock::new(|| std::env::var("OSMDIFFS_ADDR_CHECK_SCOPE").as_deref() == Ok("all"));

/// With `OSMDIFFS_POI_MASKS=wide`, restaurants, lodging, schools and
/// fuel stations get a matcher too, not only shops.
static WIDE_MASKS: LazyLock<bool> =
    LazyLock::new(|| std::env::var("OSMDIFFS_POI_MASKS").as_deref() == Ok("wide"));

/// With `OSMDIFFS_CONTACT_ONLY=1`, brand tags are ignored on both
/// sides, so only phone and email can match. For measuring how often
/// contact evidence alone finds the feature `brand:wikidata` finds.
static CONTACT_ONLY: LazyLock<bool> =
    LazyLock::new(|| std::env::var("OSMDIFFS_CONTACT_ONLY").as_deref() == Ok("1"));

/// With `OSMDIFFS_CONTACT_TIEBREAK=1`, a `brand:wikidata` match whose
/// phone or email also agrees gets [TIEBREAK_BONUS] on top, so it wins
/// over a same-brand candidate at about the same distance.
static CONTACT_TIEBREAK: LazyLock<bool> =
    LazyLock::new(|| std::env::var("OSMDIFFS_CONTACT_TIEBREAK").as_deref() == Ok("1"));
const TIEBREAK_BONUS: f64 = 0.05;

/// Score factor for a match found through the `brand` tag only, so a
/// `brand:wikidata` match at the same distance always wins.
const BRAND_TAG_FACTOR: f64 = 0.99;

fn normalize_brand(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn find_tag<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>, key: &str) -> Option<&'a str> {
    tags.into_iter().find(|&(k, _)| k == key).map(|(_, v)| v)
}

pub struct PoiMatcher {
    center: Point,
    /// Usually a single ID; see [find_brand_wikidata].
    brand_wikidata: Vec<u64>,
    brand: Option<String>,
    mode: BrandMode,
    atp_addr_tags: Vec<(String, String)>,
    /// Descriptive phone numbers and emails; empty unless contact
    /// matching is on.
    contacts: AtpContacts,
    /// Main category as `key=value`, for contact matches.
    category: Option<String>,
}

impl PoiMatcher {
    // TODO: We should also look at names, by computing the token sort ratio,
    // but we should do this in a way that works for CJK. So we need
    // a decent segmenter that works for CJK languages. Maybe Lindera?
    pub fn for_place(place: &Place) -> Option<PoiMatcher> {
        Self::for_place_with_mode(place, *BRAND_MODE)
    }

    pub fn for_place_with_mode(place: &Place, mode: BrandMode) -> Option<PoiMatcher> {
        let allowed = if *WIDE_MASKS {
            MatchMask(
                MatchMask::SHOP.0
                    | MatchMask::RESTAURANT.0
                    | MatchMask::LODGING.0
                    | MatchMask::SCHOOL.0
                    | MatchMask::FUEL.0,
            )
        } else {
            MatchMask::SHOP
        };
        if !place.mask.intersects(&allowed) {
            return None;
        }
        let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        let brand_wikidata: Vec<u64> = if *CONTACT_ONLY {
            Vec::new()
        } else {
            find_brand_wikidata(tags()).collect()
        };
        let contacts = if CONTACT_MODE.enabled() {
            AtpContacts::from_tags(tags())
        } else {
            AtpContacts::default()
        };
        let brand = if mode == BrandMode::Wikidata || *CONTACT_ONLY {
            None
        } else {
            find_tag(tags(), "brand")
                .map(normalize_brand)
                .filter(|b| !b.is_empty())
        };
        if brand_wikidata.is_empty()
            && (brand.is_none() || *REQUIRE_ATP_WIKIDATA)
            && contacts.is_empty()
        {
            return None;
        }
        let center = Cell::from(CellID(place.s2_cell_id)).center();
        Some(PoiMatcher {
            center,
            brand_wikidata,
            brand,
            mode,
            atp_addr_tags: tags()
                .filter(|(k, _)| k.starts_with("addr:"))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            category: main_category(tags()),
            contacts,
        })
    }

    fn atp_addr_tags_ref(&self) -> Vec<(&str, &str)> {
        self.atp_addr_tags
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    /// How well `candidate` agrees on brand: 1.0 for equal
    /// `brand:wikidata`, [BRAND_TAG_FACTOR] for a `brand` tag match
    /// when either side lacks `brand:wikidata`, else 0.0.
    fn brand_factor(&self, candidate: &OsmCandidate) -> f64 {
        let osm_wikidata: Vec<u64> = find_brand_wikidata(candidate.tags()).collect();
        if !self.brand_wikidata.is_empty() && !osm_wikidata.is_empty() {
            let shared = osm_wikidata
                .iter()
                .any(|id| self.brand_wikidata.contains(id));
            return if shared { 1.0 } else { 0.0 };
        }
        if self.mode == BrandMode::Wikidata {
            return 0.0;
        }
        if self.brand.is_none() {
            return 0.0;
        }
        let ok = match find_tag(candidate.tags(), "brand") {
            Some(osm_brand) => self.brand_matches(&normalize_brand(osm_brand), true),
            None if *BRAND_VS_NAME && osm_wikidata.is_empty() => {
                match find_tag(candidate.tags(), "name") {
                    Some(name) => self.brand_matches(&normalize_brand(name), false),
                    None => false,
                }
            }
            None => false,
        };
        if ok { BRAND_TAG_FACTOR } else { 0.0 }
    }

    /// Whether both sides have `brand:wikidata` and share no ID.
    fn brand_vetoed(&self, candidate: &OsmCandidate) -> bool {
        if self.brand_wikidata.is_empty() || *CONTACT_ONLY {
            return false;
        }
        let mut osm = find_brand_wikidata(candidate.tags()).peekable();
        osm.peek().is_some() && !osm.any(|id| self.brand_wikidata.contains(&id))
    }

    /// Adds phone and email evidence to `brand_factor`'s result.
    fn with_contacts(&self, factor: f64, candidate: &OsmCandidate) -> f64 {
        if self.contacts.is_empty() {
            return factor;
        }
        if factor >= 1.0 {
            if *CONTACT_TIEBREAK && self.contacts.agrees(candidate.tags()) {
                return factor + TIEBREAK_BONUS;
            }
            return factor;
        }
        if factor > 0.0 || self.brand_vetoed(candidate) {
            return factor;
        }
        if !self.contacts.agrees(candidate.tags()) {
            return 0.0;
        }
        let osm_category = main_category(candidate.tags());
        CONTACT_FACTOR * category_factor(self.category.as_deref(), osm_category.as_deref())
    }

    /// Compares the ATP brand against a normalized OSM string; `multi`
    /// allows `;`-separated values (OSM `brand`, not `name`).
    fn brand_matches(&self, osm: &str, multi: bool) -> bool {
        let Some(atp_brand) = &self.brand else {
            return false;
        };
        let mut parts: Box<dyn Iterator<Item = &str>> = if multi {
            Box::new(osm.split(';').map(str::trim))
        } else {
            Box::new(std::iter::once(osm))
        };
        match self.mode {
            BrandMode::Wikidata => false,
            BrandMode::Exact => osm == atp_brand,
            BrandMode::ExactMulti => parts.any(|b| b == atp_brand),
            BrandMode::JaroWinkler(t) => strsim::jaro_winkler(atp_brand, osm) >= t,
            BrandMode::TokenSort(t) => parts.any(|b| token_sort_ratio(atp_brand, b) >= t),
        }
    }
}

impl Matcher for PoiMatcher {
    fn score_osm_candidate(&self, candidate: &OsmCandidate) -> f64 {
        // Cheap check first: only decode the (potentially large) geometry
        // for candidates that actually match on brand.
        let factor = self.with_contacts(self.brand_factor(candidate), candidate);
        if factor == 0.0 {
            return 0.0;
        }
        let Result::Ok(geometry) = candidate.geometry() else {
            return 0.0;
        };
        let Some(centroid) = geometry.centroid() else {
            return 0.0;
        };
        if let Some(limit) = *ADDR_CHECK_BEYOND_M
            && (factor < 1.0 || *ADDR_CHECK_ALL)
            && geo_point_distance(&self.center, &centroid) > limit
        {
            let osm_tags: Vec<(&str, &str)> = candidate.tags().collect();
            let confirmed = address::osm_address_in_atp_text(&self.atp_addr_tags_ref(), &osm_tags);
            if !confirmed {
                return 0.0;
            }
        }
        factor * geo_point_distance_score(&self.center, &centroid, 400.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_sort_ratio() {
        assert_eq!(token_sort_ratio("COOP Pronto", "pronto coop"), 1.0);
        assert_eq!(token_sort_ratio("k-kiosk", "K Kiosk"), 1.0);
        assert!(token_sort_ratio("Coop Pronto", "Coop") < 0.6);
        assert!((indel_ratio("abcd", "abce") - 0.75).abs() < 1e-9);
    }
    use crate::utils::UtcTimestamp;

    fn test_timestamp() -> UtcTimestamp {
        UtcTimestamp(time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap())
    }

    /// A placeholder point shape for fixtures below that only care about
    /// `s2_cell_id`/`mask`/`tags`, not the real geometry -- reconstructs
    /// a point at the cell's own center, so it's at least consistent
    /// with `s2_cell_id` rather than an arbitrary unrelated coordinate.
    fn test_shape(s2_cell_id: u64) -> Vec<u8> {
        let cell = s2::cellid::CellID(s2_cell_id);
        let lat_lng = s2::latlng::LatLng::from(cell);
        crate::geometry::encode_wkb(&geo::Geometry::from(geo::Point::new(
            lat_lng.lng.deg(),
            lat_lng.lat.deg(),
        )))
    }

    static CH_CLOTHES_ATP: LazyLock<Place> = LazyLock::new(|| Place {
        s2_cell_id: 5159637664633565895,
        spider: String::from("newyorker"),
        mask: MatchMask::SHOP,
        fetched: test_timestamp(),
        shape_wkb: test_shape(5159637664633565895),
        tags: tags(&[
            ("addr:city", "Rapperswil"),
            ("addr:country", "CH"),
            ("addr:full", "Zürcherstrasse 4, 8640 Rapperswil"),
            ("addr:postcode", "8640"),
            ("brand", "New Yorker"),
            ("brand:wikidata", "Q706421"),
            ("name", "NEW YORKER | EKZ Sonnenhof"),
            ("opening_hours", "Mo-Fr 09:00-20:00; Sa 08:00-18:00"),
            ("phone", "+41 55 210 63 10"),
            ("ref", "5b3cb3fac00458481b4375e1"),
            ("shop", "clothes"),
        ]),
    });

    static CH_CLOTHES_OSM: LazyLock<Place> = LazyLock::new(|| Place {
        s2_cell_id: 5159637664662121729,
        spider: String::from("osm"),
        mask: MatchMask(1),
        fetched: test_timestamp(),
        shape_wkb: test_shape(5159637664662121729),
        tags: tags(&[
            ("branch", "Rapperswil Sonnenhof"),
            ("brand", "New Yorker"),
            ("brand:wikidata", "Q706421"),
            ("level", "0"),
            ("name", "New Yorker"),
            ("opening_hours", "Mo-Fr 09:00-20:00; Sa 08:00-18:00"),
            ("shop", "clothes"),
            ("website", "https://www.newyorker.de/ch/"),
        ]),
    });

    static CH_KIOSK_ATP: LazyLock<Place> = LazyLock::new(|| Place {
        s2_cell_id: 5159637400739491865,
        spider: String::from("valora"),
        mask: MatchMask::SHOP,
        fetched: test_timestamp(),
        shape_wkb: test_shape(5159637400739491865),
        tags: tags(&[
            ("addr:city", "Rapperswil"),
            ("addr:country", "CH"),
            ("addr:postcode", "8640"),
            ("addr:street_address", "Unterführung Bahnhof 1"),
            ("brand", "k kiosk"),
            ("brand:wikidata", "Q60381703"),
            ("name", "kkiosk Rapperswil BHF"),
            ("opening_hours", "Mo-Fr 05:30-20:00; Sa-Su 07:00-20:00"),
            ("ref", "730"),
            ("shop", "newsagent"),
        ]),
    });

    static CH_KIOSK_OSM: LazyLock<Place> = LazyLock::new(|| Place {
        s2_cell_id: 5159637400743919515,
        spider: String::from("osm"),
        mask: MatchMask::SHOP,
        fetched: test_timestamp(),
        shape_wkb: test_shape(5159637400743919515),
        tags: tags(&[
            ("brand", "k kiosk"),
            ("brand:wikidata", "Q60381703"),
            ("brand:wikipedia", "it:K Kiosk"),
            ("cash_withdrawal", "yes"),
            ("cash_withdrawal:fee", "no"),
            ("cash_withdrawal:operator", "sonect"),
            ("cash_withdrawal:purchase_required", "no"),
            ("cash_withdrawal:type", "checkout"),
            ("level", "-1"),
            ("name", "k kiosk"),
            (
                "opening_hours",
                "Mo-Fr 05:45-19:00; Sa 08:00-18:00; Su 09:00-18:00",
            ),
            ("shop", "kiosk"),
            ("wheelchair", "yes"),
        ]),
    });

    fn tags(t: &[(&str, &str)]) -> Vec<(String, String)> {
        t.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Exercises `score_osm_candidate` for two ATP shops (New Yorker,
    /// k kiosk) against real-world OSM positions (reused from
    /// `CH_CLOTHES_OSM`/`CH_KIOSK_OSM`'s `s2_cell_id`s, so this exercises
    /// the WKB-decode-then-centroid distance computation against genuine
    /// coordinates, not synthetic ones), with the OSM side as a
    /// `Feature` + `StringPool` instead of a `Place`.
    #[test]
    fn test_poi_matcher_osm_candidate() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        const WKB_OPTS: WriteOptions = WriteOptions {
            endianness: wkb::Endianness::LittleEndian,
        };

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "brand:wikidata",
            "Q706421",
            "shop",
            "clothes",
            "Q60381703",
            "kiosk",
            "Q706421; Q60381703",
            "Q1;Q2",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");

        let make_feature = |tags: &[(&str, &str)], s2_cell_id: u64| -> Feature {
            let mut flat_tags = Vec::with_capacity(tags.len() * 2);
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key in pool") as u32);
                flat_tags.push(pool.lookup(v).expect("value in pool") as u32);
            }
            let ll = LatLng::from(CellID(s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WKB_OPTS,
            )
            .expect("wkb encode");
            Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            }
        };

        let clothes_feature = make_feature(
            &[("brand:wikidata", "Q706421"), ("shop", "clothes")],
            CH_CLOTHES_OSM.s2_cell_id,
        );
        let kiosk_feature = make_feature(
            &[("brand:wikidata", "Q60381703"), ("shop", "kiosk")],
            CH_KIOSK_OSM.s2_cell_id,
        );
        let clothes_candidate = OsmCandidate {
            feature: &clothes_feature,
            strings: &pool,
        };
        let kiosk_candidate = OsmCandidate {
            feature: &kiosk_feature,
            strings: &pool,
        };

        let ch_clothes_matcher =
            PoiMatcher::for_place(&CH_CLOTHES_ATP).expect("should create matcher");
        let ch_kiosk_matcher = PoiMatcher::for_place(&CH_KIOSK_ATP).expect("should create matcher");
        assert!(ch_clothes_matcher.score_osm_candidate(&clothes_candidate) > 0.5);
        assert!(ch_clothes_matcher.score_osm_candidate(&kiosk_candidate) == 0.0);
        assert!(ch_kiosk_matcher.score_osm_candidate(&clothes_candidate) == 0.0);
        assert!(ch_kiosk_matcher.score_osm_candidate(&kiosk_candidate) > 0.5);

        // A multi-brand OSM feature matches each of its brands.
        for (matcher, s2_cell_id) in [
            (&ch_clothes_matcher, CH_CLOTHES_OSM.s2_cell_id),
            (&ch_kiosk_matcher, CH_KIOSK_OSM.s2_cell_id),
        ] {
            let multi_feature = make_feature(
                &[
                    ("brand:wikidata", "Q706421; Q60381703"),
                    ("shop", "clothes"),
                ],
                s2_cell_id,
            );
            let multi_candidate = OsmCandidate {
                feature: &multi_feature,
                strings: &pool,
            };
            assert!(matcher.score_osm_candidate(&multi_candidate) > 0.5);
        }
        let other_feature = make_feature(
            &[("brand:wikidata", "Q1;Q2"), ("shop", "clothes")],
            CH_KIOSK_OSM.s2_cell_id,
        );
        let other_candidate = OsmCandidate {
            feature: &other_feature,
            strings: &pool,
        };
        assert_eq!(ch_kiosk_matcher.score_osm_candidate(&other_candidate), 0.0);
    }

    #[test]
    fn test_poi_matcher_brand_modes() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "brand",
            "k kiosk",
            "k  kiosk",
            "k-kiosk",
            "Migros",
            "Migros; k kiosk",
            "brand:wikidata",
            "Q1",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let make_feature = |tags: &[(&str, &str)]| -> Feature {
            let mut flat_tags = Vec::new();
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key") as u32);
                flat_tags.push(pool.lookup(v).expect("value") as u32);
            }
            let ll = LatLng::from(CellID(CH_KIOSK_OSM.s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WriteOptions {
                    endianness: wkb::Endianness::LittleEndian,
                },
            )
            .expect("wkb encode");
            Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            }
        };
        let spaced = make_feature(&[("brand", "k  kiosk")]);
        let dashed = make_feature(&[("brand", "k-kiosk")]);
        let other = make_feature(&[("brand", "Migros")]);
        let multi = make_feature(&[("brand", "Migros; k kiosk")]);
        let wrong_qid = make_feature(&[("brand", "k kiosk"), ("brand:wikidata", "Q1")]);
        let score = |mode, f: &Feature| {
            PoiMatcher::for_place_with_mode(&CH_KIOSK_ATP, mode)
                .expect("matcher")
                .score_osm_candidate(&OsmCandidate {
                    feature: f,
                    strings: &pool,
                })
        };
        assert_eq!(score(BrandMode::Wikidata, &spaced), 0.0);
        assert!(score(BrandMode::Exact, &spaced) > 0.5);
        assert_eq!(score(BrandMode::Exact, &dashed), 0.0);
        assert!(score(BrandMode::ExactMulti, &multi) > 0.5);
        assert_eq!(score(BrandMode::Exact, &multi), 0.0);
        assert!(score(BrandMode::JaroWinkler(0.9), &dashed) > 0.5);
        assert_eq!(score(BrandMode::JaroWinkler(0.9), &other), 0.0);
        assert!(score(BrandMode::TokenSort(1.0), &dashed) > 0.5);
        assert!(score(BrandMode::TokenSort(1.0), &multi) > 0.5);
        assert_eq!(score(BrandMode::TokenSort(0.9), &other), 0.0);
        // Conflicting brand:wikidata wins over an equal brand tag.
        assert_eq!(score(BrandMode::JaroWinkler(0.9), &wrong_qid), 0.0);
    }
}
