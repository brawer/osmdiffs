//! How much a conflated feature matters to map users, from 0.0 to 1.0,
//! so that downstream users can work through conflated.parquet in
//! priority order
//! ([brawer/osmdiffs#713](https://github.com/brawer/osmdiffs/issues/713)).
//!
//! Three signals go in, each a unit-free value in `[0, 1]`:
//!
//! * **location**: the quantile of the place's OSMViews rank among the
//!   locations of all AllThePlaces features (see [`super::osm_views`]);
//! * **brand**: the highest QRank quantile among the place's
//!   `brand:`, `operator:` and `network:wikidata` IDs (see
//!   [`super::qrank`]), from its AllThePlaces tags, else from the
//!   matched OpenStreetMap feature's; and
//! * **category**: [`MatchMask::importance`], a coarse hand-made score
//!   by kind of place, so that an AED ranks above a flower bed.
//!
//! [`score`] takes their weighted mean. A place without any brand drops
//! that term instead of being pushed down for it, since most places
//! that matter (a local hospital, an independent bakery) have no
//! Wikidata brand. The published `importance` is the score's quantile
//! among all conflated rows (see [`Quantiles`]), so `importance >= 0.9`
//! always selects the top tenth.

use super::osm_views::OsmViewsQuantiles;
use super::qrank::QRankQuantiles;
use crate::matchers::MatchMask;
use crate::places::Place;
use anyhow::{Context, Result};
use osmviews::OsmViews;
use std::path::Path;

const LOCATION_WEIGHT: f32 = 0.4;
const BRAND_WEIGHT: f32 = 0.3;
const CATEGORY_WEIGHT: f32 = 0.3;

/// Weighted mean of the signals, each in `[0, 1]`. `brand` is `None`
/// for places without any brand, operator or network; its weight then
/// drops out.
pub fn score(location: f32, brand: Option<f32>, category: f32) -> f32 {
    let (brand_term, brand_weight) = match brand {
        Some(b) => (BRAND_WEIGHT * b, BRAND_WEIGHT),
        None => (0.0, 0.0),
    };
    (LOCATION_WEIGHT * location + brand_term + CATEGORY_WEIGHT * category)
        / (LOCATION_WEIGHT + brand_weight + CATEGORY_WEIGHT)
}

/// The lookup tables [`Signals::score`] needs, opened once per
/// pipeline run and shared by all matching threads.
pub struct Signals<'a> {
    osmviews: OsmViews,
    locations: OsmViewsQuantiles,
    brands: QRankQuantiles<'a>,
}

impl Signals<'_> {
    pub fn open(osmviews: &Path, osmviews_ranks: &Path, qrank_quantiles: &Path) -> Result<Self> {
        Ok(Signals {
            osmviews: OsmViews::open(osmviews)
                .with_context(|| format!("Failed to open {}", osmviews.display()))?,
            locations: OsmViewsQuantiles::open(osmviews_ranks)?,
            brands: QRankQuantiles::open(qrank_quantiles)?,
        })
    }

    /// The [`score`] of `atp`, matched to an OpenStreetMap feature
    /// with `osm_tags` (empty if unmatched).
    pub fn score<'t>(
        &self,
        atp: &Place,
        osm_tags: impl IntoIterator<Item = (&'t str, &'t str)>,
    ) -> f32 {
        let ll = s2::latlng::LatLng::from(s2::cellid::CellID(atp.s2_cell_id));
        let rank = self.osmviews.rank(ll.lng.deg(), ll.lat.deg());
        let location = self.locations.quantile(rank) as f32;
        let atp_tags = atp.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        let brand = self
            .brands
            .brand_quantile(atp_tags)
            .or_else(|| self.brands.brand_quantile(osm_tags));
        score(location, brand, category(atp.mask))
    }
}

fn category(mask: MatchMask) -> f32 {
    mask.importance()
}

/// Turns scores into quantiles among all of them, for the published
/// `importance`. Mid-rank, so ties share a value: with `less` scores
/// below this one and `same` equal to it, out of `n`, the quantile is
/// `(less + same / 2) / n`.
pub struct Quantiles {
    /// Ascending.
    sorted: Vec<f32>,
}

impl Quantiles {
    pub fn new(mut scores: Vec<f32>) -> Self {
        scores.sort_unstable_by(f32::total_cmp);
        Quantiles { sorted: scores }
    }

    /// The quantile of `score`, or 0.5 if there are no scores at all.
    pub fn get(&self, score: f32) -> f32 {
        if self.sorted.is_empty() {
            return 0.5;
        }
        let less = self.sorted.partition_point(|&s| s < score);
        let less_or_same = self.sorted.partition_point(|&s| s <= score);
        let same = (less_or_same - less) as f64;
        ((less as f64 + same / 2.0) / self.sorted.len() as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(tags: &[(&str, &str)]) -> MatchMask {
        let mut mask = MatchMask::default();
        for (key, value) in tags {
            mask.add_tag(key, value);
        }
        mask
    }

    #[test]
    fn test_score() {
        assert_eq!(score(1.0, Some(1.0), 1.0), 1.0);
        assert_eq!(score(0.0, Some(0.0), 0.0), 0.0);
        assert_eq!(score(0.5, Some(1.0), 0.0), 0.5);
        // Without a brand, location and category share the weight.
        assert!((score(0.5, None, 1.0) - 0.714286).abs() < 1e-6);
        assert_eq!(score(0.4, None, 0.4), 0.4);
    }

    /// Pairs at the same location where the first should rank above
    /// the second. Documents the intent, and catches regressions when
    /// weights or category scores change.
    #[test]
    fn test_score_pairs() {
        let at = |location: f32, brand: Option<f32>, tags: &[(&str, &str)]| {
            score(location, brand, category(mask(tags)))
        };
        let pairs: [(&str, f32, f32); 6] = [
            (
                "AED beats flower bed",
                at(0.5, None, &[("emergency", "defibrillator")]),
                at(0.5, None, &[("landuse", "flowerbed")]),
            ),
            (
                "hospital beats vending machine",
                at(0.5, None, &[("amenity", "hospital")]),
                at(0.5, None, &[("amenity", "vending_machine")]),
            ),
            (
                "supermarket beats bench",
                at(0.5, None, &[("shop", "supermarket")]),
                at(0.5, None, &[("amenity", "bench")]),
            ),
            (
                "famous brand beats obscure brand",
                at(0.5, Some(0.99), &[("shop", "supermarket")]),
                at(0.5, Some(0.12), &[("shop", "supermarket")]),
            ),
            (
                "busy street beats quiet one",
                at(0.9, None, &[("shop", "bakery")]),
                at(0.2, None, &[("shop", "bakery")]),
            ),
            (
                "AED on a quiet street beats bench on a busy one",
                at(0.3, None, &[("emergency", "defibrillator")]),
                at(0.7, None, &[("amenity", "bench")]),
            ),
        ];
        for (name, a, b) in pairs {
            assert!(a > b, "{name}: {a} <= {b}");
        }
    }

    #[test]
    fn test_quantiles() {
        let q = Quantiles::new(vec![0.7, 0.0, 0.5, 0.5, 0.5]);
        assert_eq!(q.get(0.0), 0.1);
        assert_eq!(q.get(0.5), 0.5);
        assert_eq!(q.get(0.7), 0.9);
        assert_eq!(Quantiles::new(Vec::new()).get(0.3), 0.5);
    }
}
