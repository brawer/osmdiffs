//! Collects the Wikidata IDs of every brand, operator and network that
//! AllThePlaces or OpenStreetMap features refer to, via their
//! `brand:wikidata`, `operator:wikidata` and `network:wikidata` tags,
//! and writes them to a [U64Set] on disk.
//!
//! This set is the reference population
//! for ranking features by the importance of their brand
//! ([brawer/osmdiffs#713](https://github.com/brawer/osmdiffs/issues/713)):
//! a feature's brand gets the quantile of its QRank among the brands
//! in this set, not among all Wikidata items. Brands are, on the whole,
//! far less viewed than cities or countries, so ranking them against
//! all of Wikidata would squeeze every brand into the bottom quantiles.
//! Taking the union of ATP's and OSM's brands gives the same reference
//! population no matter which side a feature comes from.
//!
//! Plain `wikidata` tags are deliberately left out: on OSM, they mostly
//! point at cities, rivers, churches and the like rather than at
//! brands. So are other `*:wikidata` keys such as `species:wikidata` or
//! `trough:material:wikidata`, which are about something else entirely.

use crate::make_progress_bar;
use crate::matchers::parse_wikidata_ids;
use crate::places::PlaceReader;
use crate::tables::{OsmFeatures, U64Set};
use anyhow::Result;
use indicatif::MultiProgress;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// Filename, within `workdir`, of the set [collect_wikidata_ids] writes.
const WIKIDATA_IDS_FILENAME: &str = "wikidata-ids";

/// Tag keys whose values are Wikidata IDs of brands, operators or
/// networks.
const WIKIDATA_KEYS: [&str; 3] = ["brand:wikidata", "network:wikidata", "operator:wikidata"];

/// True for tag keys whose value(s) are the Wikidata IDs of a brand,
/// operator or network.
pub(super) fn is_wikidata_key(key: &str) -> bool {
    WIKIDATA_KEYS.contains(&key)
}

/// Collects brand/operator/network Wikidata IDs from both `atp`
/// (`alltheplaces.parquet`) and `osm`, and writes their union to
/// `workdir/wikidata-ids` as a [U64Set].
pub fn collect_wikidata_ids(
    atp: &Path,
    osm: &OsmFeatures,
    progress: &MultiProgress,
    workdir: &Path,
) -> Result<PathBuf> {
    let out = workdir.join(WIKIDATA_IDS_FILENAME);
    let input_modified = super::last_modified(&[atp])?.max(osm.modified()?);
    if out.exists() && super::last_modified(&[&out])? >= input_modified {
        return Ok(out);
    }

    let mut ids = collect_from_atp(atp)?;
    let num_atp_ids = ids.len();
    ids.extend(collect_from_osm(osm, progress)?);
    let num_osm_ids = ids.len() - num_atp_ids;

    // Sole external sort in this step (see `run_step("collect_wikidata_ids",
    // ...)` in pipeline/mod.rs), so it gets the full chunk-size budget.
    U64Set::create(
        ids.into_iter(),
        workdir,
        &out,
        crate::pipeline::EXTERNAL_SORT_CHUNK_BYTES,
    )?;
    let unique = U64Set::open(&out)?.len();
    log::info!(
        atp_tags = num_atp_ids,
        osm_tags = num_osm_ids,
        unique_ids = unique;
        "collect_wikidata_ids: done"
    );
    Ok(out)
}

/// The Wikidata IDs in `atp`'s brand/operator/network tags, with
/// duplicates.
fn collect_from_atp(atp: &Path) -> Result<Vec<u64>> {
    let reader = PlaceReader::open(atp)?;
    let mut ids = Vec::new();
    for batch in reader.read_all()? {
        for place in batch? {
            for (key, value) in &place.tags {
                if is_wikidata_key(key) {
                    ids.extend(parse_wikidata_ids(value));
                }
            }
        }
    }
    Ok(ids)
}

/// The Wikidata IDs in `osm`'s brand/operator/network tags, with
/// duplicates. Visits every feature in the index, in parallel; tag
/// keys are compared as string pool indices, so only the values of
/// matching tags get looked up as strings.
fn collect_from_osm(osm: &OsmFeatures, progress: &MultiProgress) -> Result<Vec<u64>> {
    let key_ids: Vec<u32> = WIKIDATA_KEYS
        .iter()
        .filter_map(|key| osm.strings.lookup(key))
        .map(|id| id as u32)
        .collect();
    if key_ids.is_empty() {
        return Ok(Vec::new());
    }

    let progress_bar = make_progress_bar(
        progress,
        "collect_wikidata_ids.osm",
        osm.index.len() as u64,
        "OSM features",
    );
    let ids = osm
        .index
        .all_refs()
        .map(|r| {
            progress_bar.inc(1);
            let feature = osm.index.get_feature(r)?;
            let ids: Vec<u64> = feature
                .tags
                .chunks_exact(2)
                .filter(|kv| key_ids.contains(&kv[0]))
                .flat_map(|kv| parse_wikidata_ids(osm.strings.get(kv[1] as usize)))
                .collect();
            Ok::<_, anyhow::Error>(ids)
        })
        .try_reduce(Vec::new, |mut a, b| {
            a.extend(b);
            Ok(a)
        })?;
    progress_bar.finish();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::{Feature, FeatureToIndex, OsmFeatureIndex, StringPool};
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[test]
    fn test_is_wikidata_key() {
        assert!(is_wikidata_key("brand:wikidata"));
        assert!(is_wikidata_key("network:wikidata"));
        assert!(is_wikidata_key("operator:wikidata"));

        assert!(!is_wikidata_key("wikidata"));
        assert!(!is_wikidata_key("highway"));
        assert!(!is_wikidata_key("species:wikidata"));
        assert!(!is_wikidata_key("trough:material:wikidata"));
    }

    #[test]
    fn test_parse_wikidata_ids() {
        let ids: Vec<u64> = parse_wikidata_ids(" Q123;Q813 ; q21").collect();
        assert_eq!(ids, vec![123, 813, 21]);
    }

    /// Builds an [OsmFeatures] whose features carry `tags`, one feature
    /// per entry.
    fn build_osm(dir: &Path, features: &[&[(&str, &str)]]) -> Result<OsmFeatures<'static>> {
        let mut strings: Vec<String> = features
            .iter()
            .flat_map(|tags| tags.iter())
            .flat_map(|(k, v)| [k.to_string(), v.to_string()])
            .collect();
        strings.sort();
        strings.dedup();
        let strings_path = dir.join("strings");
        let pool = StringPool::create(strings.into_iter(), dir, &strings_path)?;

        let fti: Vec<FeatureToIndex> = features
            .iter()
            .enumerate()
            .map(|(i, tags)| FeatureToIndex {
                centroid_s2_cell_id: i as u64,
                feature: Some(Feature {
                    id: i as u64 * 10 + 1,
                    tags: tags
                        .iter()
                        .flat_map(|(k, v)| {
                            [
                                pool.lookup(k).unwrap() as u32,
                                pool.lookup(v).unwrap() as u32,
                            ]
                        })
                        .collect(),
                    ..Default::default()
                }),
                match_mask: 1,
                coverage_s2_cell_id: vec![i as u64],
            })
            .collect();
        let index_path = dir.join("osm-features.index");
        let index = OsmFeatureIndex::create(fti.into_iter(), dir, &index_path)?;
        Ok(OsmFeatures {
            index,
            strings: pool,
        })
    }

    #[test]
    fn test_collect_from_osm() -> Result<()> {
        let dir = TempDir::new()?;
        let osm = build_osm(
            dir.path(),
            &[
                &[("shop", "supermarket"), ("brand:wikidata", "Q1;Q2")],
                &[("amenity", "fuel"), ("operator:wikidata", "Q3")],
                &[("route", "bus"), ("network:wikidata", "Q4")],
                &[("place", "city"), ("wikidata", "Q72")],
                &[("tourism", "artwork"), ("subject:wikidata", "Q5")],
                &[("shop", "supermarket"), ("brand:wikidata", "Q1")],
            ],
        )?;
        let mut ids = collect_from_osm(&osm, &MultiProgress::new())?;
        ids.sort();
        assert_eq!(ids, vec![1, 1, 2, 3, 4]);
        Ok(())
    }

    #[test]
    fn test_collect_from_osm_without_wikidata_keys() -> Result<()> {
        let dir = TempDir::new()?;
        let osm = build_osm(dir.path(), &[&[("shop", "bakery")]])?;
        assert!(collect_from_osm(&osm, &MultiProgress::new())?.is_empty());
        Ok(())
    }

    #[test]
    fn test_collect_wikidata_ids() -> Result<()> {
        let mut atp = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        atp.push("tests/test_data/alltheplaces.parquet");
        let workdir = TempDir::new()?;
        let linked_atp = workdir.path().join("alltheplaces.parquet");
        symlink(&atp, &linked_atp)?;

        let osm_dir = TempDir::new()?;
        let osm = build_osm(
            osm_dir.path(),
            &[
                // Denner, also in the ATP fixture.
                &[("shop", "supermarket"), ("brand:wikidata", "Q379911")],
                // Migros, only in OSM.
                &[("shop", "supermarket"), ("brand:wikidata", "Q680727")],
                // A plain `wikidata` tag, not a brand.
                &[("place", "city"), ("wikidata", "Q72")],
            ],
        )?;

        let out = collect_wikidata_ids(&linked_atp, &osm, &MultiProgress::new(), workdir.path())?;
        let set = U64Set::open(&out)?;
        // Verified against the ATP fixture directly (via DuckDB) rather
        // than assumed: of the 10 places in this file, 3 carry
        // brand:wikidata=Q116151325 (Misenso), 4 carry
        // operator:wikidata=Q56825906 (Stadtgrün Winterthur), and one
        // each carry brand:wikidata=Q379911 (Denner), Q2381223
        // (MediaMarkt), Q564213 (Tchibo) -- 5 unique IDs once
        // deduplicated. OSM adds Migros.
        assert_eq!(set.len(), 6);
        assert!(set.contains(116151325));
        assert!(set.contains(56825906));
        assert!(set.contains(379911));
        assert!(set.contains(2381223));
        assert!(set.contains(564213));
        assert!(set.contains(680727));
        assert!(!set.contains(72));

        // Calling again should reuse the existing file, not fail or
        // recompute.
        let modified = std::fs::metadata(&out)?.modified()?;
        let out2 = collect_wikidata_ids(&linked_atp, &osm, &MultiProgress::new(), workdir.path())?;
        assert_eq!(out, out2);
        assert_eq!(std::fs::metadata(&out2)?.modified()?, modified);

        Ok(())
    }
}
