//! The OSM features most like an ATP feature that nothing matched,
//! within [SIMILAR_RADIUS_M]: the nearest one with an agreeing category,
//! the same or a known pair (see [crate::matchers::category]), on any
//! category key of either side, so that a fuel station with a shop
//! agrees with one without; and the nearest one of any category
//! whose name shares a distinctive word with the ATP feature's name.
//! Written as `match.evidence.similar_osm` for `missing_in_osm` and
//! `located_in` rows, so that the edits step doesn't propose a new
//! feature where OSM likely has it already: under another name,
//! without a brand, of another kind (a hotel that OSM maps as its
//! restaurant), or some distance away. The edits step decides how close
//! is too close, from the distance.

use super::s2_util::MergedCellRanges;
use crate::matchers::category::{categories, categories_agree};
use crate::matchers::{MatchMask, OsmCandidate};
use crate::pipeline::osm::{decode_feature_id, osm_type_str};
use crate::places::Place;
use crate::tables::{LocalFeatureRef, OsmFeatures};
use anyhow::Result;
use deepsize::DeepSizeOf;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// How far [nearest_similar] looks, in meters.
pub(super) const SIMILAR_RADIUS_M: f64 = 100.0;

/// Why an OSM feature is like the ATP feature.
#[derive(Clone, Copy, Debug, DeepSizeOf, Deserialize, PartialEq, Eq, Serialize)]
pub(super) enum By {
    /// The same main category, or a known pair.
    Category,
    /// A name that shares a distinctive word.
    Name,
}

impl By {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            By::Category => "category",
            By::Name => "name",
        }
    }
}

/// An OSM feature like an unmatched ATP feature, for
/// `match.evidence.similar_osm`.
#[derive(Clone, Debug, DeepSizeOf, Deserialize, PartialEq, Serialize)]
pub(super) struct Similar {
    pub(super) osm_type: String,
    pub(super) osm_id: u64,
    pub(super) distance_m: f32,
    pub(super) by: By,
}

/// Words that say what kind of place something is rather than which
/// one, and so don't make two names alike.
const GENERIC_WORDS: &[&str] = &[
    "hotel",
    "hôtel",
    "hostel",
    "motel",
    "restaurant",
    "ristorante",
    "gasthof",
    "gasthaus",
    "auberge",
    "pizzeria",
    "pizza",
    "cafe",
    "café",
    "coffee",
    "kaffee",
    "bakery",
    "bäckerei",
    "boulangerie",
    "apotheke",
    "pharmacie",
    "farmacia",
    "pharmacy",
    "drogerie",
    "bank",
    "banque",
    "banco",
    "filiale",
    "branch",
    "agence",
    "store",
    "shop",
    "market",
    "markt",
    "marché",
    "mercado",
    "supermarket",
    "supermarkt",
    "center",
    "centre",
    "zentrum",
    "centro",
    "station",
    "bahnhof",
    "gare",
    "garage",
    "auto",
    "autos",
    "service",
    "services",
    "express",
    "city",
    "plaza",
    "mall",
    "street",
    "road",
    "strasse",
    "straße",
    "platz",
    "place",
    "rue",
    "avenue",
    "the",
    "and",
    "und",
    "der",
    "die",
    "das",
    "des",
    "les",
    "del",
    "della",
    "fitness",
    "studio",
    "clinic",
    "klinik",
    "praxis",
    "school",
    "schule",
    "école",
    "post",
    "office",
    "library",
    "bibliothek",
    "kiosk",
    "inn",
    "house",
    "haus",
    "maison",
    "casa",
];

/// The distinctive words of a name: lowercase runs of letters and
/// digits of at least four characters, other than [GENERIC_WORDS].
/// A name in a script without spaces, such as Japanese, is one word.
pub(super) fn name_words(name: &str) -> HashSet<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 4 && !GENERIC_WORDS.contains(&w.as_str()))
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

/// The distinctive words of the names that `tags` give a place.
// TODO(#1050): Leave out place names, such as the town in an `official_name`
// like `Orpi Anma Gestion Immo Crosne`, which hold back unrelated creates.
fn tag_words<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> HashSet<String> {
    let mut words = HashSet::new();
    for (k, v) in tags {
        if matches!(
            k,
            "name" | "brand" | "official_name" | "alt_name" | "short_name"
        ) || k.starts_with("name:")
        {
            words.extend(name_words(v));
        }
    }
    words
}

/// The OSM features within [SIMILAR_RADIUS_M] of `atp` that are most
/// like it: the nearest one with a category that agrees with one of
/// `atp`'s,
/// and the nearest one whose name shares a word with `atp`'s, leaving
/// out `host`, the feature that `atp` is located in. Each at most once,
/// nearest first.
pub(super) fn nearest_similar(
    atp: &Place,
    host: Option<LocalFeatureRef>,
    osm: &OsmFeatures,
) -> Result<Vec<Similar>> {
    let atp_tags = || atp.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
    let atp_categories = categories(atp_tags());
    let words = tag_words(atp_tags());
    if atp_categories.is_empty() && words.is_empty() {
        return Ok(Vec::new());
    }
    let coverer = s2::region::RegionCoverer {
        max_cells: 8,
        min_level: 12,
        max_level: s2::cellid::MAX_LEVEL as u8,
        level_mod: 1,
    };
    let center = s2::cell::Cell::from(s2::cellid::CellID(atp.s2_cell_id)).center();
    let radius = crate::matchers::meters_to_chord_angle(SIMILAR_RADIUS_M);
    let cap = s2::cap::Cap::from_center_chordangle(&center, &radius);
    let shape = atp.shape();
    let mut best: [Option<(f64, LocalFeatureRef)>; 2] = [None, None];
    for (lo, hi) in MergedCellRanges::new(coverer.covering(&cap)) {
        // Any kind: a hotel may be mapped as its restaurant.
        for r in osm.index.query(lo..=hi, MatchMask(u16::MAX)) {
            if Some(r) == host {
                continue;
            }
            let feature = osm.index.get_feature(r)?;
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &osm.strings,
            };
            let osm_categories = categories(candidate.tags());
            if osm_categories.is_empty() {
                continue;
            }
            let by_category = atp_categories
                .iter()
                .any(|a| osm_categories.iter().any(|o| categories_agree(a, o)));
            let by_name = !words.is_empty() && !tag_words(candidate.tags()).is_disjoint(&words);
            if !by_category && !by_name {
                continue;
            }
            let Ok(geometry) = candidate.geometry() else {
                continue;
            };
            let Some(d) = super::evidence::distance_meters(&shape, &geometry) else {
                continue;
            };
            // The covering reaches farther than the radius.
            if d > SIMILAR_RADIUS_M {
                continue;
            }
            for (i, hit) in [by_category, by_name].into_iter().enumerate() {
                if hit && best[i].is_none_or(|(b, _)| d < b) {
                    best[i] = Some((d, r));
                }
            }
        }
    }
    let mut found = Vec::new();
    for (slot, by) in best.into_iter().zip([By::Category, By::Name]) {
        let Some((distance, r)) = slot else {
            continue;
        };
        let Some((member_type, osm_id)) = decode_feature_id(osm.index.feature_id(r)) else {
            continue;
        };
        let osm_type = osm_type_str(member_type).to_string();
        if found
            .iter()
            .any(|s: &Similar| s.osm_type == osm_type && s.osm_id == osm_id)
        {
            continue;
        }
        found.push(Similar {
            osm_type,
            osm_id,
            distance_m: distance as f32,
            by,
        });
    }
    found.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_words() {
        let words = |s: &str| {
            let mut w: Vec<String> = name_words(s).into_iter().collect();
            w.sort();
            w
        };
        assert_eq!(
            words("Hotel Jungfrau Wengernalp"),
            ["jungfrau", "wengernalp"]
        );
        assert_eq!(words("Auberge-Restaurant Chez Gaby 1670"), ["chez", "gaby"]);
        assert!(words("Hotel Restaurant zur Post").is_empty());
        assert_eq!(
            words("セブン-イレブン 八重瀬東風平西店"),
            ["イレブン", "八重瀬東風平西店"]
        );
    }
}
