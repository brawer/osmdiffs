//! Matches street infrastructure: post boxes, defibrillators (AEDs)
//! and benches for now, with street lamps and the like to follow.
//!
//! Such features have no brand and rarely a name, and many of them
//! stand close together, so the match rests on position. Each kind has
//! default [Settings]; a spider can override them, since sources differ
//! more than kinds do. Japan Post's positions are within a few meters,
//! Deutsche Post's often tens of meters off. See
//! `docs/notes/2026-11-05-infrastructure-matching.md` for the evals
//! behind the numbers.

use super::{
    Matcher, OsmCandidate, geo_point_distance, geo_point_distance_score, meters_to_chord_angle,
    normalize_name, parse_wikidata_ids,
};
use crate::places::Place;
use geo::Centroid;
use s2::{cell::Cell, cellid::CellID, point::Point, s1::ChordAngle};

/// A candidate with the same ref scores at least this much, so that it
/// wins over any candidate that is merely close.
const SAME_REF_MIN_SCORE: f64 = 0.9;

/// Evidence factor for a candidate of the same kind, before distance
/// and confirming tags. Below [SAME_REF_MIN_SCORE].
const KIND_FACTOR: f64 = 0.8;

/// Added to [KIND_FACTOR] for each confirming tag that agrees, so that
/// the right one of two close candidates wins. Two of them still stay
/// below [SAME_REF_MIN_SCORE].
const CONFIRM_BONUS: f64 = 0.04;

/// What kind of infrastructure a feature is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    PostBox,
    Defibrillator,
    Bench,
}

impl Kind {
    fn from_tags<'a>(mut tags: impl Iterator<Item = (&'a str, &'a str)>) -> Option<Kind> {
        tags.find_map(|kv| match kv {
            ("amenity", "post_box") => Some(Kind::PostBox),
            ("emergency", "defibrillator") => Some(Kind::Defibrillator),
            ("amenity", "bench") => Some(Kind::Bench),
            _ => None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Kind::PostBox => "post_box",
            Kind::Defibrillator => "defibrillator",
            Kind::Bench => "bench",
        }
    }

    /// Tags that confirm a match when equal on both sides. Other tags
    /// say little: bench `material` agrees on only 19% of close pairs
    /// in Seattle, since ATP has `metal;wood` where OSM has `wood`.
    fn confirming_keys(self) -> &'static [&'static str] {
        match self {
            Kind::PostBox => &["collection_times"],
            Kind::Defibrillator => &[],
            // 98% equal on close pairs in Zürich.
            Kind::Bench => &["backrest"],
        }
    }

    /// Whether different `operator:wikidata` on both sides rule out a
    /// match. True for post boxes, where the operator is the postal
    /// service. False for AEDs: ATP's Björn Steiger Stiftung is the
    /// donor, and OSM sometimes names the town or shop that runs it.
    fn operator_must_agree(self) -> bool {
        self == Kind::PostBox
    }
}

/// The kind of street infrastructure that `tags` describe, such as
/// `post_box`, if [InfrastructureMatcher] can match it.
pub(crate) fn kind_name<'a>(
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> Option<&'static str> {
    Kind::from_tags(tags).map(Kind::name)
}

/// How ATP and OSM refs of a spider compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefRule {
    /// Refs say nothing: OSM uses another scheme, such as Swiss Post
    /// boxes (ATP `003BE_00335986`, OSM `3-8716-25024`).
    Ignore,
    /// Equal refs, ignoring leading zeros, such as Japan Post's
    /// `007153` and OSM's `7153`.
    Equal,
    /// The last word of the ATP ref equals the OSM ref, such as
    /// Steiger Stiftung's `AEDR 1786` and OSM's `1786`.
    LastWord,
}

/// How to match the features of one kind, or of one spider.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    /// Search radius; the distance score falls linearly to 0 there.
    max_meters: f64,
    refs: RefRule,
    /// The OSM key that holds the ATP ref, usually `ref`.
    osm_ref_key: &'static str,
}

impl Settings {
    /// The settings for `kind`, or for `spider` if it has its own.
    fn for_spider(kind: Kind, spider: &str) -> Settings {
        let by_spider = match (kind, spider) {
            (Kind::PostBox, "swiss_post_ch") => Some((100.0, RefRule::Ignore)),
            (Kind::PostBox, "deutsche_post_de") => Some((100.0, RefRule::Ignore)),
            (Kind::PostBox, "japan_post_jp") => Some((20.0, RefRule::Equal)),
            (Kind::Defibrillator, "steiger_stiftung_aed") => Some((50.0, RefRule::LastWord)),
            (Kind::Defibrillator, "aed_jp") => Some((20.0, RefRule::Ignore)),
            (Kind::Bench, "stadt_zuerich_ch") => Some((10.0, RefRule::Ignore)),
            (Kind::Bench, "seattle_parks_and_recreation_benches_us") => {
                return Settings {
                    max_meters: 15.0,
                    refs: RefRule::Equal,
                    osm_ref_key: "ref:US-WA:DPR:AMWO_ID",
                };
            }
            _ => None,
        };
        let (max_meters, refs) = by_spider.unwrap_or(match kind {
            Kind::PostBox => (50.0, RefRule::Ignore),
            Kind::Defibrillator => (30.0, RefRule::Ignore),
            Kind::Bench => (15.0, RefRule::Ignore),
        });
        Settings {
            max_meters,
            refs,
            osm_ref_key: "ref",
        }
    }
}

/// The ref of a feature as [RefRule] compares it: lowercased, without
/// leading zeros, and for [RefRule::LastWord] on the ATP side, only
/// its last word.
fn normalize_ref(value: &str, last_word: bool) -> Option<String> {
    let value = if last_word {
        value.split_whitespace().last()?
    } else {
        value.trim()
    };
    let value = value.trim_start_matches('0').to_lowercase();
    (!value.is_empty()).then_some(value)
}

/// A tag value without spaces, lowercased, so that
/// `Mo-Fr 17:30; Sa 08:00` equals `Mo-Fr 17:30;Sa 08:00`.
fn normalize_value(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// Matches street infrastructure. A candidate must be the same
/// [Kind]. Then, by evidence:
///
/// - Different `operator:wikidata` on both sides, for kinds where the
///   operator must agree ([Kind::operator_must_agree]): no match.
/// - The same ref, for spiders whose refs OSM uses ([RefRule]):
///   [SAME_REF_MIN_SCORE] plus the rest times the distance score.
///   A ref never widens the search radius, since short refs repeat:
///   1,201 OSM post boxes in Japan share a ref with a Japan Post box,
///   and 456 of them are more than 200 m away.
/// - Otherwise [KIND_FACTOR], plus [CONFIRM_BONUS] for each confirming
///   tag that agrees, times the distance score. Confirming tags are
///   those of [Kind::confirming_keys], and an OSM `ref` that contains the ATP
///   street, as German OSM describes the box location in `ref`
///   (`Lütticher Str. 76`). Tags that differ never lower a score: half
///   of the right post box matches in Switzerland and Germany have
///   outdated `collection_times` in OSM, which is what an edit should
///   fix.
///
/// Matches are one to one within a spider ([Matcher::is_exclusive]),
/// since several post boxes, AEDs or benches often stand within a few
/// meters: half of the benches in Zürich and Winterthur are less than
/// 5 m from the next one.
pub struct InfrastructureMatcher {
    center: Point,
    kind: Kind,
    settings: Settings,
    /// See [normalize_ref]; `None` if the spider's refs say nothing.
    reference: Option<String>,
    operators: Vec<u64>,
    /// The [Kind::confirming_keys] the ATP feature has, with their
    /// [normalize_value]d values.
    confirming: Vec<(&'static str, String)>,
    /// Lowercased `addr:street`.
    street: Option<String>,
}

impl InfrastructureMatcher {
    pub fn for_place(place: &Place) -> Option<InfrastructureMatcher> {
        let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        let kind = Kind::from_tags(tags())?;
        let settings = Settings::for_spider(kind, &place.spider);
        let tag = |key: &str| tags().find(|&(k, _)| k == key).map(|(_, v)| v);
        let reference = match settings.refs {
            RefRule::Ignore => None,
            RefRule::Equal => tag("ref").and_then(|v| normalize_ref(v, false)),
            RefRule::LastWord => tag("ref").and_then(|v| normalize_ref(v, true)),
        };
        Some(InfrastructureMatcher {
            center: Cell::from(CellID(place.s2_cell_id)).center(),
            kind,
            settings,
            reference,
            operators: tag("operator:wikidata")
                .map(|v| parse_wikidata_ids(v).collect())
                .unwrap_or_default(),
            confirming: kind
                .confirming_keys()
                .iter()
                .filter_map(|&key| tag(key).map(|v| (key, normalize_value(v))))
                .collect(),
            street: tag("addr:street")
                .map(normalize_name)
                .filter(|s| !s.is_empty()),
        })
    }

    /// For `candidate`, the score at zero distance and the minimum
    /// score within the radius; `(0.0, 0.0)` for no match.
    fn factor(&self, candidate: &OsmCandidate) -> (f64, f64) {
        if Kind::from_tags(candidate.tags()) != Some(self.kind) {
            return (0.0, 0.0);
        }
        let tag = |key: &str| candidate.tags().find(|&(k, _)| k == key).map(|(_, v)| v);
        if self.kind.operator_must_agree() && !self.operators.is_empty() {
            let mut osm = tag("operator:wikidata")
                .into_iter()
                .flat_map(parse_wikidata_ids)
                .peekable();
            if osm.peek().is_some() && !osm.any(|id| self.operators.contains(&id)) {
                return (0.0, 0.0);
            }
        }
        if let (Some(atp), Some(osm)) = (
            &self.reference,
            tag(self.settings.osm_ref_key).and_then(|v| normalize_ref(v, false)),
        ) && *atp == osm
        {
            return (1.0, SAME_REF_MIN_SCORE);
        }
        let mut factor = KIND_FACTOR;
        for (key, atp) in &self.confirming {
            if tag(key).is_some_and(|osm| normalize_value(osm) == *atp) {
                factor += CONFIRM_BONUS;
            }
        }
        let osm_ref = tag("ref");
        if let (Some(street), Some(osm)) = (&self.street, osm_ref)
            && normalize_name(osm).contains(street.as_str())
        {
            factor += CONFIRM_BONUS;
        }
        (factor, 0.0)
    }
}

impl Matcher for InfrastructureMatcher {
    fn score_osm_candidate(&self, candidate: &OsmCandidate) -> f64 {
        let (best, worst) = self.factor(candidate);
        if best == 0.0 {
            return 0.0;
        }
        let Result::Ok(geometry) = candidate.geometry() else {
            return 0.0;
        };
        let Some(centroid) = geometry.centroid() else {
            return 0.0;
        };
        let max_meters = self.settings.max_meters;
        if geo_point_distance(&self.center, &centroid) > max_meters {
            return 0.0;
        }
        worst + (best - worst) * geo_point_distance_score(&self.center, &centroid, max_meters)
    }

    /// Two post boxes or benches in a feed are two real things, so they
    /// shouldn't both take the same OSM feature.
    fn is_exclusive(&self) -> bool {
        true
    }

    fn max_distance(&self) -> Option<ChordAngle> {
        Some(meters_to_chord_angle(self.settings.max_meters))
    }
}

#[cfg(test)]
mod tests {
    //! Most tests are rows in [CASES]: one ATP feature, one OSM
    //! candidate, their distance, and the expected score range.
    use super::*;
    use crate::matchers::MatchMask;
    use crate::tables::{Feature, StringPool};
    use crate::utils::UtcTimestamp;
    use tempfile::TempDir;

    const LAT: f64 = 47.3782;
    const LON: f64 = 8.5402;

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

    /// The score for an ATP feature of `spider` with `atp` tags against
    /// an OSM feature with `osm` tags, `meters` north of it; `None` if
    /// the ATP feature gets no [InfrastructureMatcher].
    fn score(spider: &str, atp: &[(&str, &str)], osm: &[(&str, &str)], meters: f64) -> Option<f64> {
        let matcher = InfrastructureMatcher::for_place(&place(spider, atp))?;
        let dir = TempDir::new().expect("tempdir");
        let mut strings: Vec<String> = osm
            .iter()
            .flat_map(|(k, v)| [k.to_string(), v.to_string()])
            .collect();
        strings.sort();
        strings.dedup();
        let pool = StringPool::create(strings.into_iter(), dir.path(), &dir.path().join("strings"))
            .expect("StringPool::create");
        let tags = osm
            .iter()
            .flat_map(|(k, v)| {
                [
                    pool.lookup(k).unwrap() as u32,
                    pool.lookup(v).unwrap() as u32,
                ]
            })
            .collect();
        let point = geo::Point::new(LON, LAT + meters / 111_195.0);
        let feature = Feature {
            id: 1,
            tags,
            geometry_wkb: crate::geometry::encode_wkb(&geo::Geometry::from(point)),
            ..Default::default()
        };
        let candidate = OsmCandidate {
            feature: &feature,
            strings: &pool,
        };
        Some(matcher.score_osm_candidate(&candidate))
    }

    struct Case {
        what: &'static str,
        spider: &'static str,
        atp: &'static [(&'static str, &'static str)],
        osm: &'static [(&'static str, &'static str)],
        meters: f64,
        /// Expected score, inclusive range.
        want: (f64, f64),
    }

    const NO_MATCH: (f64, f64) = (0.0, 0.0);

    // ATP features, as the spiders emit them (shortened).
    const SWISS_POST_BOX: &[(&str, &str)] = &[
        ("amenity", "post_box"),
        ("ref", "003BE_00335986"),
        ("collection_times", "Mo-Fr 17:30; Sa 08:00"),
        ("operator", "Die Post"),
        ("operator:wikidata", "Q614803"),
        ("addr:street_address", "Via alla Roggia 31"),
    ];
    const DEUTSCHE_POST_BOX: &[(&str, &str)] = &[
        ("amenity", "post_box"),
        ("ref", "6-209474"),
        ("collection_times", "Mo-Sa 09:30"),
        ("addr:street", "Lütticher Str."),
        ("brand", "Deutsche Post"),
        ("brand:wikidata", "Q157645"),
    ];
    const JAPAN_POST_BOX: &[(&str, &str)] = &[
        ("amenity", "post_box"),
        ("ref", "007153"),
        ("collection_times", "Mo-Fr 10:00; Sa 09:10; Su,PH 09:10"),
        ("operator", "日本郵便"),
        ("operator:wikidata", "Q11509260"),
    ];
    const STEIGER_AED: &[(&str, &str)] = &[
        ("emergency", "defibrillator"),
        ("ref", "AEDR 1786"),
        ("name", "Oberrhein Gymnasium - Foyer EG"),
        ("operator", "Steiger-Stiftung"),
        ("operator:wikidata", "Q879521"),
    ];

    const ZURICH_BENCH: &[(&str, &str)] = &[
        ("amenity", "bench"),
        ("ref", "B-4711"),
        ("backrest", "yes"),
        ("operator", "Grün Stadt Zürich"),
    ];
    const SEATTLE_BENCH: &[(&str, &str)] = &[("amenity", "bench"), ("ref", "BEN-1234")];

    const CASES: &[Case] = &[
        Case {
            what: "Swiss post box, same spot",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box"), ("operator:wikidata", "Q614803")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "Swiss post box 60 m away still matches",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box")],
            meters: 60.0,
            want: (0.3, 0.33),
        },
        Case {
            what: "Swiss post box beyond 100 m",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box")],
            meters: 110.0,
            want: NO_MATCH,
        },
        Case {
            what: "Swiss refs say nothing",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "003BE_00335986")],
            meters: 10.0,
            want: (0.72, 0.72),
        },
        Case {
            what: "equal collection times raise the score",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[
                ("amenity", "post_box"),
                ("collection_times", "Mo-Fr 17:30;Sa 08:00"),
            ],
            meters: 0.0,
            want: (0.84, 0.84),
        },
        Case {
            what: "different collection times don't lower it",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box"), ("collection_times", "Mo-Fr 10:00")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "another operator",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "post_box"), ("operator:wikidata", "Q157645")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "a waste basket is not a post box",
            spider: "swiss_post_ch",
            atp: SWISS_POST_BOX,
            osm: &[("amenity", "waste_basket")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "German OSM ref describing the location",
            spider: "deutsche_post_de",
            atp: DEUTSCHE_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "Lütticher Str. 76")],
            meters: 0.0,
            want: (0.84, 0.84),
        },
        Case {
            what: "Japan Post ref, ignoring leading zeros",
            spider: "japan_post_jp",
            atp: JAPAN_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "7153")],
            meters: 10.0,
            want: (0.95, 0.95),
        },
        Case {
            what: "Japan Post ref beats a closer box",
            spider: "japan_post_jp",
            atp: JAPAN_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "007154")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "Japan Post box beyond 20 m, even with the same ref",
            spider: "japan_post_jp",
            atp: JAPAN_POST_BOX,
            osm: &[("amenity", "post_box"), ("ref", "007153")],
            meters: 25.0,
            want: NO_MATCH,
        },
        Case {
            what: "Steiger AED with the number as OSM ref",
            spider: "steiger_stiftung_aed",
            atp: STEIGER_AED,
            osm: &[
                ("emergency", "defibrillator"),
                ("ref", "1786"),
                ("operator", "Björn Steiger Stiftung"),
            ],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "AED operated by someone else in OSM",
            spider: "steiger_stiftung_aed",
            atp: STEIGER_AED,
            osm: &[
                ("emergency", "defibrillator"),
                ("operator", "Stadt Heidelberg"),
                ("operator:wikidata", "Q2557"),
            ],
            meters: 10.0,
            want: (0.64, 0.64),
        },
        Case {
            what: "AED tagged on a shop",
            spider: "aed_jp",
            atp: &[("emergency", "defibrillator"), ("ref", "22285")],
            osm: &[
                ("shop", "convenience"),
                ("emergency", "defibrillator"),
                ("ref", "22285"),
            ],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "an AED is not a post box",
            spider: "steiger_stiftung_aed",
            atp: STEIGER_AED,
            osm: &[("amenity", "post_box"), ("ref", "1786")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "Zürich bench, same spot, same backrest",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_BENCH,
            osm: &[("amenity", "bench"), ("backrest", "yes")],
            meters: 0.0,
            want: (0.84, 0.84),
        },
        Case {
            what: "Zürich bench, different backrest doesn't lower the score",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_BENCH,
            osm: &[("amenity", "bench"), ("backrest", "no")],
            meters: 5.0,
            want: (0.4, 0.4),
        },
        Case {
            what: "Zürich bench beyond 10 m",
            spider: "stadt_zuerich_ch",
            atp: ZURICH_BENCH,
            osm: &[("amenity", "bench")],
            meters: 12.0,
            want: NO_MATCH,
        },
        Case {
            what: "Seattle bench with its asset ID in OSM",
            spider: "seattle_parks_and_recreation_benches_us",
            atp: SEATTLE_BENCH,
            osm: &[("amenity", "bench"), ("ref:US-WA:DPR:AMWO_ID", "BEN-1234")],
            meters: 0.0,
            want: (1.0, 1.0),
        },
        Case {
            what: "Seattle asset ID under plain ref doesn't count",
            spider: "seattle_parks_and_recreation_benches_us",
            atp: SEATTLE_BENCH,
            osm: &[("amenity", "bench"), ("ref", "BEN-1234")],
            meters: 0.0,
            want: (0.8, 0.8),
        },
        Case {
            what: "default radius for an unknown bench spider",
            spider: "some_bench_spider",
            atp: &[("amenity", "bench")],
            osm: &[("amenity", "bench")],
            meters: 16.0,
            want: NO_MATCH,
        },
        Case {
            what: "a bench is not a picnic table",
            spider: "some_bench_spider",
            atp: &[("amenity", "bench")],
            osm: &[("leisure", "picnic_table")],
            meters: 0.0,
            want: NO_MATCH,
        },
        Case {
            what: "default radius for an unknown AED spider",
            spider: "some_aed_spider",
            atp: &[("emergency", "defibrillator")],
            osm: &[("emergency", "defibrillator")],
            meters: 35.0,
            want: NO_MATCH,
        },
    ];

    #[test]
    fn test_cases() {
        for case in CASES {
            let got = score(case.spider, case.atp, case.osm, case.meters)
                .unwrap_or_else(|| panic!("{}: no matcher", case.what));
            let (lo, hi) = case.want;
            assert!(
                got >= lo - 1e-3 && got <= hi + 1e-3,
                "{}: got {got:.4}, want {lo}..={hi}",
                case.what
            );
        }
    }

    #[test]
    fn test_no_matcher_for_other_kinds() {
        assert!(
            InfrastructureMatcher::for_place(&place("x", &[("amenity", "waste_basket")])).is_none()
        );
        assert!(InfrastructureMatcher::for_place(&place("x", &[("shop", "bakery")])).is_none());
    }

    #[test]
    fn test_max_distance() {
        let matcher =
            InfrastructureMatcher::for_place(&place("deutsche_post_de", DEUTSCHE_POST_BOX))
                .unwrap();
        assert!(matcher.max_distance() == Some(meters_to_chord_angle(100.0)));
        assert!(matcher.is_exclusive());
    }

    #[test]
    fn test_normalize_ref() {
        assert_eq!(normalize_ref("007153", false).as_deref(), Some("7153"));
        assert_eq!(normalize_ref("AEDR 1786", true).as_deref(), Some("1786"));
        assert_eq!(normalize_ref("000", false), None);
        assert_eq!(normalize_ref("  ", true), None);
    }
}
