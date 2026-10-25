use super::contact::{AtpContacts, CONTACT_FACTOR, Nondescript, category_factor, main_category};
use super::{
    MatchMask, Matcher, OsmCandidate, address, geo_point_distance, geo_point_distance_score,
    parse_wikidata_ids,
};
use crate::places::Place;
use geo::Centroid;
use s2::{cell::Cell, cellid::CellID, point::Point};

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

/// AllThePlaces features of these kinds get a [PoiMatcher].
const POI_MASK: MatchMask = MatchMask(
    MatchMask::SHOP.0
        | MatchMask::RESTAURANT.0
        | MatchMask::LODGING.0
        | MatchMask::SCHOOL.0
        | MatchMask::FUEL.0,
);

/// Beyond this distance, a contact match needs the OSM street address
/// to appear in the ATP address, see [address::osm_address_in_atp_text].
/// On the planet, only a third of the far contact matches through a
/// category pair were right without this check.
const CONTACT_ADDRESS_CHECK_BEYOND_METERS: f64 = 150.0;

/// Matches points of interest, such as shops, restaurants, hotels and
/// schools, by evidence:
///
/// - Equal `brand:wikidata`: the distance score.
/// - Both sides have `brand:wikidata`, but share no ID: no match,
///   whatever else agrees.
/// - Otherwise, a shared descriptive phone number or email address:
///   [CONTACT_FACTOR] times [category_factor] times the distance score.
///   Beyond [CONTACT_ADDRESS_CHECK_BEYOND_METERS], the street address
///   must agree too.
///
/// Phones or emails that differ never lower a score: about one in six
/// right `brand:wikidata` matches has different numbers on the two
/// sides.
pub struct PoiMatcher {
    center: Point,
    /// Usually a single ID; see [find_brand_wikidata].
    brand_wikidata: Vec<u64>,
    contacts: AtpContacts,
    /// Main category as `key=value`, for contact matches.
    category: Option<String>,
    /// The ATP feature's `addr:*` tags, for far contact matches. Empty
    /// if `contacts` is empty.
    address: Vec<(String, String)>,
}

impl PoiMatcher {
    // TODO: We should also look at names, by computing the token sort ratio,
    // but we should do this in a way that works for CJK. So we need
    // a decent segmenter that works for CJK languages. Maybe Lindera?
    pub fn for_place(place: &Place, nondescript: &Nondescript) -> Option<PoiMatcher> {
        if !place.mask.intersects(&POI_MASK) {
            return None;
        }
        let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        let brand_wikidata: Vec<u64> = find_brand_wikidata(tags()).collect();
        let contacts = AtpContacts::from_tags(tags(), nondescript);
        if brand_wikidata.is_empty() && contacts.is_empty() {
            return None;
        }
        let (category, address) = if contacts.is_empty() {
            (None, Vec::new())
        } else {
            let address = tags()
                .filter(|(k, _)| k.starts_with("addr:"))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            (main_category(tags()), address)
        };
        Some(PoiMatcher {
            center: Cell::from(CellID(place.s2_cell_id)).center(),
            brand_wikidata,
            contacts,
            category,
            address,
        })
    }

    /// The evidence factor for `candidate`, before distance: 1.0 for
    /// equal `brand:wikidata`, below 1.0 for a contact match, 0.0 for
    /// no match.
    fn factor(&self, candidate: &OsmCandidate) -> f64 {
        if !self.brand_wikidata.is_empty() {
            let mut osm = find_brand_wikidata(candidate.tags()).peekable();
            if osm.peek().is_some() {
                let shared = osm.any(|id| self.brand_wikidata.contains(&id));
                return if shared { 1.0 } else { 0.0 };
            }
        }
        if self.contacts.is_empty() || !self.contacts.agrees(candidate.tags()) {
            return 0.0;
        }
        let osm_category = main_category(candidate.tags());
        CONTACT_FACTOR * category_factor(self.category.as_deref(), osm_category.as_deref())
    }

    fn address_agrees(&self, candidate: &OsmCandidate) -> bool {
        let atp: Vec<(&str, &str)> = self
            .address
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let osm: Vec<(&str, &str)> = candidate.tags().collect();
        address::osm_address_in_atp_text(&atp, &osm)
    }
}

impl Matcher for PoiMatcher {
    fn score_osm_candidate(&self, candidate: &OsmCandidate) -> f64 {
        // Cheap check first: only decode the (potentially large) geometry
        // for candidates that have some evidence.
        let factor = self.factor(candidate);
        if factor == 0.0 {
            return 0.0;
        }
        let Result::Ok(geometry) = candidate.geometry() else {
            return 0.0;
        };
        let Some(centroid) = geometry.centroid() else {
            return 0.0;
        };
        if factor < 1.0
            && geo_point_distance(&self.center, &centroid) > CONTACT_ADDRESS_CHECK_BEYOND_METERS
            && !self.address_agrees(candidate)
        {
            return 0.0;
        }
        factor * geo_point_distance_score(&self.center, &centroid, 400.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::UtcTimestamp;
    use std::sync::LazyLock;

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

        let ch_clothes_matcher = PoiMatcher::for_place(&CH_CLOTHES_ATP, &Nondescript::none())
            .expect("should create matcher");
        let ch_kiosk_matcher = PoiMatcher::for_place(&CH_KIOSK_ATP, &Nondescript::none())
            .expect("should create matcher");
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

    /// Phone evidence for an ATP feature without `brand:wikidata`: a
    /// shared number matches when the categories agree, never against a
    /// `brand:wikidata` conflict, and never with a different category.
    #[test]
    fn test_poi_matcher_contact() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "amenity",
            "brand:wikidata",
            "car",
            "car_repair",
            "contact:phone",
            "florist",
            "Q1",
            "Q2",
            "shop",
            "supermarket",
            "055 210 63 10",
            "+41 55 210 63 10",
            "+41 55 210 63 11",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let s2_cell_id = CH_CLOTHES_ATP.s2_cell_id;
        let make_feature = |tags: &[(&str, &str)]| -> Feature {
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
        let score = |matcher: &PoiMatcher, tags: &[(&str, &str)]| {
            let feature = make_feature(tags);
            matcher.score_osm_candidate(&OsmCandidate {
                feature: &feature,
                strings: &pool,
            })
        };
        let atp = |tags: &[(&str, &str)]| Place {
            s2_cell_id,
            spider: String::from("test"),
            mask: MatchMask::SHOP,
            fetched: test_timestamp(),
            shape_wkb: test_shape(s2_cell_id),
            tags: self::tags(tags),
        };

        let dealer = atp(&[
            ("addr:country", "CH"),
            ("phone", "+41 55 210 63 10"),
            ("shop", "car"),
        ]);
        let m = PoiMatcher::for_place(&dealer, &Nondescript::none()).expect("matcher");
        let same = score(
            &m,
            &[("shop", "car"), ("contact:phone", "+41 55 210 63 10")],
        );
        assert!(same > 0.9 && same < 1.0, "{same}");
        let national = score(&m, &[("shop", "car"), ("contact:phone", "055 210 63 10")]);
        assert_eq!(national, same);
        let pair = score(
            &m,
            &[
                ("shop", "car_repair"),
                ("contact:phone", "+41 55 210 63 10"),
            ],
        );
        assert!(pair > 0.9 && pair < same, "{pair}");
        let other_category = score(
            &m,
            &[
                ("shop", "supermarket"),
                ("contact:phone", "+41 55 210 63 10"),
            ],
        );
        assert_eq!(other_category, 0.0);
        let other_phone = score(
            &m,
            &[("shop", "car"), ("contact:phone", "+41 55 210 63 11")],
        );
        assert_eq!(other_phone, 0.0);
        // OSM has brand:wikidata, ATP does not: no conflict.
        let osm_brand = score(
            &m,
            &[
                ("brand:wikidata", "Q1"),
                ("shop", "car"),
                ("contact:phone", "+41 55 210 63 10"),
            ],
        );
        assert_eq!(osm_brand, same);

        // Both sides have brand:wikidata, and they differ: no match.
        let branded = atp(&[
            ("brand:wikidata", "Q2"),
            ("phone", "+41 55 210 63 10"),
            ("shop", "car"),
        ]);
        let m = PoiMatcher::for_place(&branded, &Nondescript::none()).expect("matcher");
        let vetoed = score(
            &m,
            &[
                ("brand:wikidata", "Q1"),
                ("shop", "car"),
                ("contact:phone", "+41 55 210 63 10"),
            ],
        );
        assert_eq!(vetoed, 0.0);
        let brand = score(&m, &[("brand:wikidata", "Q2"), ("shop", "florist")]);
        assert!(brand > same, "{brand}");

        // No brand:wikidata and no contacts: no matcher at all.
        let bare = atp(&[("shop", "florist")]);
        assert!(PoiMatcher::for_place(&bare, &Nondescript::none()).is_none());
    }
}
