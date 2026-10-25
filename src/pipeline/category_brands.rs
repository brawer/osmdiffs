//! The typical brand quantile of each kind of place, as a stand-in for
//! places that carry no brand.
//!
//! For ranking features by importance
//! ([brawer/osmdiffs#713](https://github.com/brawer/osmdiffs/issues/713)),
//! a place's brand signal is its QRank quantile (see
//! [`super::qrank::QRankQuantiles::brand_quantile`]). Many places have
//! no `brand:wikidata` or similar tag, though: an independent bakery, a
//! local hospital, a fire hydrant. Rather than maintaining a table of
//! how important each kind of place is, we derive it from the data: for
//! each category such as `amenity=hospital` or `shop=supermarket`, the
//! median brand quantile of the distinct brands that AllThePlaces
//! places of that category carry. Each brand counts once per category,
//! however many places it has, so one big chain can't dominate.
//! Categories where no place carries a brand at all, such as hydrants
//! or trees, get the lowest brand quantile seen anywhere.
//!
//! That floor is wrong for a few kinds of places that matter a lot but
//! rarely carry a brand, such as defibrillators or hospitals. Those get
//! a fixed value from [`CATEGORY_EXCEPTIONS`] instead.

use super::qrank::QRankQuantiles;
use crate::places::PlaceReader;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Filename, within `workdir`, of the file that
/// [`compute_category_brand_quantiles`] writes.
const FILENAME: &str = "category-brand-quantiles.json";

/// Keys that say what kind of place a feature is, most specific first.
/// A feature's category is the first of these that it carries, so a
/// pharmacy tagged `amenity=pharmacy` + `healthcare=pharmacy` +
/// `shop=chemist` is `amenity=pharmacy` however its tags are ordered.
const CATEGORY_KEYS: [&str; 16] = [
    "amenity",
    "shop",
    "healthcare",
    "tourism",
    "leisure",
    "office",
    "craft",
    "emergency",
    "public_transport",
    "railway",
    "aeroway",
    "man_made",
    "power",
    "historic",
    "natural",
    "building",
];

/// Categories whose brand quantile is fixed rather than computed from
/// brands: places that matter a lot, yet seldom carry a brand or
/// operator, so the median of their brands would be missing or
/// meaningless. Keep this list short; anything with brands of its own
/// is better left to the data.
const CATEGORY_EXCEPTIONS: [(&str, f32); 7] = [
    ("amenity=clinic", 0.8),
    ("amenity=fire_station", 0.8),
    ("amenity=hospital", 0.9),
    ("amenity=police", 0.8),
    ("emergency=ambulance_station", 0.8),
    ("emergency=defibrillator", 0.9),
    ("healthcare=hospital", 0.9),
];

/// The category of a feature with `tags`, such as `amenity=hospital`,
/// or `None` if it has none of the [`CATEGORY_KEYS`]. Of a
/// multi-valued tag (`shop=bakery;cafe`), only the first value counts.
pub fn category<'t>(tags: impl IntoIterator<Item = (&'t str, &'t str)>) -> Option<String> {
    let mut best: Option<(usize, &str, &str)> = None;
    for (key, value) in tags {
        let Some(priority) = CATEGORY_KEYS.iter().position(|k| *k == key) else {
            continue;
        };
        let value = value.split(';').next().unwrap_or("").trim();
        if value.is_empty() || value == "no" {
            continue;
        }
        if best.is_none_or(|(p, _, _)| priority < p) {
            best = Some((priority, key, value));
        }
    }
    best.map(|(_, key, value)| format!("{key}={value}"))
}

/// The contents of `category-brand-quantiles.json`.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct CategoryBrandQuantiles {
    /// For places whose category isn't listed, or that have none.
    pub default: f32,
    /// Median brand quantile by category, e.g. `shop=supermarket`.
    pub categories: BTreeMap<String, f32>,
}

impl CategoryBrandQuantiles {
    #[allow(dead_code)] // Used by conflate in a follow-up change.
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open {}", path.display()))?;
        serde_json::from_reader(std::io::BufReader::new(file))
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// The stand-in brand quantile for a feature with `tags`.
    #[allow(dead_code)] // Used by conflate in a follow-up change.
    pub fn get<'t>(&self, tags: impl IntoIterator<Item = (&'t str, &'t str)>) -> f32 {
        category(tags)
            .and_then(|c| self.categories.get(&c).copied())
            .unwrap_or(self.default)
    }
}

/// Computes the median brand quantile of each category among the
/// places in `atp` (`alltheplaces.parquet`), using the quantiles in
/// `qrank_quantiles`, and writes them to
/// `workdir/category-brand-quantiles.json`.
pub fn compute_category_brand_quantiles(
    atp: &Path,
    qrank_quantiles: &Path,
    workdir: &Path,
) -> Result<PathBuf> {
    let out = workdir.join(FILENAME);
    let input_modified = super::last_modified(&[atp, qrank_quantiles])?;
    if out.exists() && super::last_modified(&[&out])? >= input_modified {
        return Ok(out);
    }

    let qrank = QRankQuantiles::open(qrank_quantiles)?;
    let reader = PlaceReader::open(atp)?;
    // Distinct brand quantiles per category, as f32 bit patterns so
    // they can go in a HashSet. Two brands with the same quantile count
    // once, which is fine: ties are rare except at the bottom.
    let mut brands: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut places = 0_u64;
    for batch in reader.read_all()? {
        for place in batch? {
            places += 1;
            let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
            let Some(category) = category(tags()) else {
                continue;
            };
            let entry = brands.entry(category).or_default();
            if let Some(q) = qrank.brand_quantile(tags()) {
                entry.insert(q.to_bits());
            }
        }
    }

    let result = summarize(brands);
    let tmp = out.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&result)?)?;
    std::fs::rename(&tmp, &out)?;
    log::info!(
        places = places,
        categories = result.categories.len(),
        default = result.default;
        "compute_category_brand_quantiles: done"
    );
    Ok(out)
}

/// Turns the distinct brand quantiles of each category into
/// [`CategoryBrandQuantiles`]. Categories without any brand get the
/// lowest quantile seen in any category, or 0 if there is none; those
/// in [`CATEGORY_EXCEPTIONS`] get their fixed value.
fn summarize(brands: HashMap<String, HashSet<u32>>) -> CategoryBrandQuantiles {
    let default = brands
        .values()
        .flatten()
        .map(|&bits| f32::from_bits(bits))
        .reduce(f32::min)
        .unwrap_or(0.0);
    let categories = brands
        .into_iter()
        .map(|(category, quantiles)| {
            let mut sorted: Vec<f32> = quantiles.into_iter().map(f32::from_bits).collect();
            sorted.sort_by(f32::total_cmp);
            (category, median(&sorted).unwrap_or(default))
        })
        .chain(
            CATEGORY_EXCEPTIONS
                .iter()
                .map(|&(category, q)| (category.to_string(), q)),
        )
        .collect();
    CategoryBrandQuantiles {
        default,
        categories,
    }
}

/// The median of `sorted`, or `None` if it is empty.
fn median(sorted: &[f32]) -> Option<f32> {
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2]),
        _ => Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[test]
    fn test_category() {
        assert_eq!(
            category([("name", "Coop"), ("shop", "supermarket")]),
            Some("shop=supermarket".to_string())
        );
        // A more specific key wins, whatever the tag order.
        assert_eq!(
            category([
                ("shop", "chemist"),
                ("healthcare", "pharmacy"),
                ("amenity", "pharmacy")
            ]),
            Some("amenity=pharmacy".to_string())
        );
        assert_eq!(
            category([("shop", " bakery ;cafe")]),
            Some("shop=bakery".to_string())
        );
        assert_eq!(
            category([("amenity", "no"), ("shop", "kiosk")]),
            Some("shop=kiosk".to_string())
        );
        assert_eq!(category([("amenity", "")]), None);
        assert_eq!(category([("name", "Foo")]), None);
    }

    #[test]
    fn test_median() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[0.5]), Some(0.5));
        assert_eq!(median(&[0.25, 0.75]), Some(0.5));
        assert_eq!(median(&[0.1, 0.2, 0.9]), Some(0.2));
    }

    #[test]
    fn test_summarize() {
        let set = |qs: &[f32]| qs.iter().map(|q| q.to_bits()).collect::<HashSet<u32>>();
        let brands = HashMap::from([
            ("shop=supermarket".to_string(), set(&[0.25, 0.5, 0.75])),
            ("amenity=fuel".to_string(), set(&[0.125, 0.625])),
            ("emergency=fire_hydrant".to_string(), set(&[])),
            ("amenity=hospital".to_string(), set(&[0.25])),
        ]);
        let result = summarize(brands);
        assert_eq!(result.default, 0.125);
        assert_eq!(result.categories["amenity=fuel"], 0.375);
        assert_eq!(result.categories["emergency=fire_hydrant"], 0.125);
        assert_eq!(result.categories["shop=supermarket"], 0.5);
        // Exceptions win over the data, and apply even when ATP has no
        // such place.
        assert_eq!(result.categories["amenity=hospital"], 0.9);
        assert_eq!(result.categories["emergency=defibrillator"], 0.9);
        assert_eq!(result.categories.len(), 4 + CATEGORY_EXCEPTIONS.len() - 1);
        assert!(
            result.get([("emergency", "defibrillator")]) > result.get([("landuse", "flowerbed")])
        );
        assert_eq!(result.get([("shop", "supermarket")]), 0.5);
        assert_eq!(result.get([("emergency", "fire_hydrant")]), 0.125);
        assert_eq!(result.get([("shop", "unknown")]), 0.125);
        assert_eq!(result.get([]), 0.125);

        let empty = summarize(HashMap::new());
        assert_eq!(empty.default, 0.0);
        assert_eq!(empty.categories.len(), CATEGORY_EXCEPTIONS.len());
    }

    #[test]
    fn test_compute_category_brand_quantiles() -> Result<()> {
        let workdir = TempDir::new()?;
        let mut atp = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        atp.push("tests/test_data/alltheplaces.parquet");
        let linked_atp = workdir.path().join("alltheplaces.parquet");
        symlink(&atp, &linked_atp)?;

        // The brands in the ATP fixture (see wikidata_ids's tests),
        // with made-up quantiles.
        let qrank_path = workdir.path().join("qrank-quantiles");
        crate::tables::BlobTable::create(
            [
                (116151325_u64, 0.1_f32), // Misenso
                (56825906, 0.2),          // Stadtgrün Winterthur
                (379911, 0.6),            // Denner
                (2381223, 0.7),           // MediaMarkt
                (564213, 0.8),            // Tchibo
            ]
            .into_iter()
            .map(|(id, q)| (id, q.to_le_bytes().to_vec())),
            workdir.path(),
            &qrank_path,
            crate::pipeline::EXTERNAL_SORT_CHUNK_BYTES,
        )?;
        let out = compute_category_brand_quantiles(&linked_atp, &qrank_path, workdir.path())?;
        let result = CategoryBrandQuantiles::open(&out)?;
        assert_eq!(result.default, 0.1);
        for (category, q) in &result.categories {
            if !CATEGORY_EXCEPTIONS.iter().any(|(c, _)| c == category) {
                assert!((0.1..=0.8).contains(q), "{category}: {q}");
            }
        }
        assert!(result.categories.len() > CATEGORY_EXCEPTIONS.len());

        // A second call reuses the file.
        let modified = std::fs::metadata(&out)?.modified()?;
        compute_category_brand_quantiles(&linked_atp, &qrank_path, workdir.path())?;
        assert_eq!(std::fs::metadata(&out)?.modified()?, modified);
        Ok(())
    }
}
