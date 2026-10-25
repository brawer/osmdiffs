//! [QRank](https://qrank.toolforge.org/) quantiles for the brands,
//! operators and networks that AllThePlaces and OpenStreetMap refer to.
//!
//! QRank counts how often the Wikipedia (and other Wikimedia) pages
//! about a Wikidata item were viewed over the past year. Those counts
//! have a unit and a very skewed distribution, so they can't be
//! averaged with other ranking signals such as OSMViews. Their
//! *quantile* can: it says what share of brands are less viewed than
//! this one, a number from 0 to 1 whatever the signal.
//!
//! The reference population is the set from
//! [`super::wikidata_ids::collect_wikidata_ids`], not all of Wikidata:
//! brands are, on the whole, far less viewed than cities or countries,
//! so against all of Wikidata every brand would end up in the bottom
//! quantiles. Each brand counts once, however many stores it has.
//!
//! Not consumed by anything yet; see
//! [brawer/osmdiffs#713](https://github.com/brawer/osmdiffs/issues/713).

use crate::matchers::parse_wikidata_id;
use crate::tables::{BlobTable, U64Set};
use anyhow::{Context, Result, bail};
use flate2::read::MultiGzDecoder;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::packaged::PackagedResource;

/// QRank's Frictionless Data Package descriptor, which names the
/// current build's files and their hashes. See
/// <https://github.com/brawer/wikidata-qrank/blob/main/doc/downloads.md>.
pub(super) const QRANK_DATAPACKAGE_URL: &str =
    "https://qrank.toolforge.org/download/datapackage.json";

/// The QRank file the pipeline fetches: gzipped CSV with columns
/// `Entity,QRank`, one row per Wikidata item with any pageviews.
pub(super) const QRANK: PackagedResource = PackagedResource {
    name: "qrank",
    filename: "qrank.csv.gz",
    meta_filename: "qrank.meta.json",
    label: "qrank.fetch   ",
    timeout: Duration::from_secs(600),
};

/// Filename, within `workdir`, of the [BlobTable] that
/// [`compute_qrank_quantiles`] writes.
const QUANTILES_FILENAME: &str = "qrank-quantiles";

/// Computes the QRank quantile of every Wikidata ID in `wikidata_ids`,
/// among the IDs in that same set, and writes them to
/// `workdir/qrank-quantiles`: a [BlobTable] keyed by numeric Wikidata ID
/// (42 for Q42), each value a little-endian `f32` in `(0, 1)`.
///
/// Items that QRank doesn't list have no pageviews, and share the
/// lowest quantile. Ties get the middle of the range they share (the
/// "mid-rank" quantile), so the result doesn't depend on how ties
/// happen to be ordered: with `less` IDs viewed less than this one, and
/// `same` IDs (including this one) viewed exactly as much, out of `n`,
/// the quantile is `(less + same / 2) / n`.
pub fn compute_qrank_quantiles(
    qrank: &Path,
    wikidata_ids: &Path,
    workdir: &Path,
) -> Result<PathBuf> {
    let out = workdir.join(QUANTILES_FILENAME);
    let input_modified = super::last_modified(&[qrank, wikidata_ids])?;
    if out.exists() && super::last_modified(&[&out])? >= input_modified {
        return Ok(out);
    }

    let ids = U64Set::open(wikidata_ids)?;
    let (pageviews, rows) = read_pageviews(qrank, &ids)?;
    let quantiles = quantiles(ids.iter().map(|id| (id, *pageviews.get(&id).unwrap_or(&0))));

    BlobTable::create(
        quantiles
            .iter()
            .map(|&(id, q)| (id, q.to_le_bytes().to_vec())),
        workdir,
        &out,
        crate::pipeline::EXTERNAL_SORT_CHUNK_BYTES,
    )?;
    log::info!(
        qrank_rows = rows,
        wikidata_ids = ids.len(),
        with_pageviews = pageviews.len();
        "compute_qrank_quantiles: done"
    );
    Ok(out)
}

/// Reads the pageview count of each ID in `ids` from `qrank`, and the
/// total number of data rows. IDs that QRank doesn't list are absent
/// from the returned map.
fn read_pageviews(qrank: &Path, ids: &U64Set) -> Result<(HashMap<u64, u64>, u64)> {
    let file = File::open(qrank).with_context(|| format!("Failed to open {}", qrank.display()))?;
    let reader = BufReader::with_capacity(1 << 20, MultiGzDecoder::new(file));
    let mut pageviews = HashMap::new();
    let mut rows = 0;
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("Failed to read {}", qrank.display()))?;
        if i == 0 && line == "Entity,QRank" {
            continue;
        }
        let parsed = line.split_once(',').and_then(|(entity, views)| {
            Some((
                parse_wikidata_id(entity)?,
                views.trim().parse::<u64>().ok()?,
            ))
        });
        let Some((id, views)) = parsed else {
            bail!("{}:{}: malformed row {line:?}", qrank.display(), i + 1);
        };
        rows += 1;
        if ids.contains(id) {
            pageviews.insert(id, views);
        }
    }
    Ok((pageviews, rows))
}

/// Mid-rank quantile of each `(id, pageviews)` among all of them; see
/// [`compute_qrank_quantiles`]. Returned in the same order.
fn quantiles(items: impl Iterator<Item = (u64, u64)>) -> Vec<(u64, f32)> {
    let items: Vec<(u64, u64)> = items.collect();
    let mut sorted: Vec<u64> = items.iter().map(|&(_, views)| views).collect();
    sorted.sort_unstable();
    let n = sorted.len() as f64;
    items
        .iter()
        .map(|&(id, views)| {
            let less = sorted.partition_point(|&v| v < views);
            let less_or_same = sorted.partition_point(|&v| v <= views);
            let same = (less_or_same - less) as f64;
            (id, ((less as f64 + same / 2.0) / n) as f32)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    use tempfile::TempDir;

    fn write_qrank(path: &Path, csv: &str) -> Result<()> {
        let mut gz = GzEncoder::new(File::create(path)?, Compression::default());
        gz.write_all(csv.as_bytes())?;
        gz.finish()?;
        Ok(())
    }

    fn quantile(table: &BlobTable, id: u64) -> Option<f32> {
        table
            .lookup(id)
            .map(|b| f32::from_le_bytes(b.try_into().expect("4 bytes")))
    }

    #[test]
    fn test_quantiles() {
        let q = quantiles([(1, 10), (2, 0), (3, 30), (4, 20)].into_iter());
        assert_eq!(q, vec![(1, 0.375), (2, 0.125), (3, 0.875), (4, 0.625)]);

        // Ties share the middle of their range.
        let q = quantiles([(1, 0), (2, 0), (3, 0), (4, 7)].into_iter());
        assert_eq!(q, vec![(1, 0.375), (2, 0.375), (3, 0.375), (4, 0.875)]);

        assert_eq!(quantiles([(5, 99)].into_iter()), vec![(5, 0.5)]);
        assert!(quantiles(std::iter::empty()).is_empty());
    }

    #[test]
    fn test_compute_qrank_quantiles() -> Result<()> {
        let workdir = TempDir::new()?;
        let qrank = workdir.path().join("qrank.csv.gz");
        // Sorted by decreasing QRank, as QRank publishes it. Q42 and
        // Q64 are not in our set of brands, so they must not count.
        write_qrank(
            &qrank,
            "Entity,QRank\nQ64,900000\nQ42,500000\nQ37158,8000\nQ379911,300\nQ680727,300\n",
        )?;
        let ids_path = workdir.path().join("wikidata-ids");
        // Starbucks, Denner, Migros, and an item without pageviews.
        U64Set::create(
            [37158, 379911, 680727, 116151325].into_iter(),
            workdir.path(),
            &ids_path,
            crate::pipeline::EXTERNAL_SORT_CHUNK_BYTES,
        )?;

        let out = compute_qrank_quantiles(&qrank, &ids_path, workdir.path())?;
        let table = BlobTable::open(&out)?;
        assert_eq!(table.len(), 4);
        assert_eq!(quantile(&table, 116151325), Some(0.125));
        assert_eq!(quantile(&table, 379911), Some(0.5));
        assert_eq!(quantile(&table, 680727), Some(0.5));
        assert_eq!(quantile(&table, 37158), Some(0.875));
        assert_eq!(quantile(&table, 42), None);

        // A second call reuses the file.
        let modified = std::fs::metadata(&out)?.modified()?;
        compute_qrank_quantiles(&qrank, &ids_path, workdir.path())?;
        assert_eq!(std::fs::metadata(&out)?.modified()?, modified);
        Ok(())
    }

    #[test]
    fn test_malformed_row() -> Result<()> {
        let workdir = TempDir::new()?;
        let qrank = workdir.path().join("qrank.csv.gz");
        write_qrank(&qrank, "Entity,QRank\nQ1,5\nnot a row\n")?;
        let ids_path = workdir.path().join("wikidata-ids");
        U64Set::create(
            [1].into_iter(),
            workdir.path(),
            &ids_path,
            crate::pipeline::EXTERNAL_SORT_CHUNK_BYTES,
        )?;
        let err = compute_qrank_quantiles(&qrank, &ids_path, workdir.path()).unwrap_err();
        assert!(format!("{err:#}").contains(":3: malformed row"), "{err:#}");
        Ok(())
    }
}
