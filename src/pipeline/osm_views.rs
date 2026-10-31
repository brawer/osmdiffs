//! [OSMViews](https://osmviews.brawer.ch/) quantiles for the locations
//! of AllThePlaces features.
//!
//! OSMViews says how much a location is looked at on OpenStreetMap-based
//! maps. Its `rank()` runs from 0 to 1, but inhabited places all sit in
//! a narrow band of it (roughly 0.45 to 0.75), so averaged with an
//! unrelated signal such as a QRank quantile, which spans 0 to 1, it
//! would count for little. Its *quantile* among the places we rank
//! spans the whole range instead.
//!
//! The reference population is the location of every AllThePlaces
//! feature: those are the places this pipeline ranks, and ranking them
//! against all map pixels (most of which are ocean or desert) would put
//! every one of them near the top. [`compute_osmviews_ranks`] writes
//! their sorted `rank()` values to `osmviews-ranks`, and
//! [`OsmViewsQuantiles`] maps any `rank()` value to its quantile among
//! them, so the same scale works for OpenStreetMap features too.
//!
//! Used for the `importance` column of conflated.parquet; see
//! [`super::importance`].

use super::packaged::PackagedResource;
use crate::places::PlaceReader;
use anyhow::{Context, Result, bail};
use osmviews::OsmViews;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// OSMViews' Frictionless Data Package descriptor, which names the
/// current weekly build and its hash. See
/// <https://github.com/brawer/osmviews/blob/main/docs/downloads.md>.
pub(super) const OSMVIEWS_DATAPACKAGE_URL: &str = osmviews::DATAPACKAGE_URL;

/// The OSMViews raster the pipeline fetches: a Cloud-Optimized GeoTIFF
/// of about 560 MB.
pub(super) const OSMVIEWS: PackagedResource = PackagedResource {
    name: "osmviews",
    filename: "osmviews.tiff",
    meta_filename: "osmviews.meta.json",
    label: "osmviews.fetch",
    timeout: Duration::from_secs(3600),
};

/// Filename, within `workdir`, of the file [`compute_osmviews_ranks`]
/// writes.
const RANKS_FILENAME: &str = "osmviews-ranks";

/// File signature of `osmviews-ranks`: 8 bytes of magic, the number of
/// ranks as a little-endian `u64`, then that many little-endian `f32`
/// ranks in ascending order.
const RANKS_SIGNATURE: &[u8; 8] = b"osmvrnk0";

/// Looks up the OSMViews `rank()` at the location of every feature in
/// `atp` (`alltheplaces.parquet`), and writes them, sorted, to
/// `workdir/osmviews-ranks`. See [`OsmViewsQuantiles`] for how they're
/// used.
pub fn compute_osmviews_ranks(atp: &Path, osmviews: &Path, workdir: &Path) -> Result<PathBuf> {
    let out = workdir.join(RANKS_FILENAME);
    let input_modified = super::last_modified(&[atp, osmviews])?;
    if out.exists() && super::last_modified(&[&out])? >= input_modified {
        return Ok(out);
    }

    let views = OsmViews::open(osmviews)
        .with_context(|| format!("Failed to open {}", osmviews.display()))?;
    let reader = PlaceReader::open(atp)?;
    let mut ranks = Vec::with_capacity(reader.total_rows());
    // `alltheplaces.parquet` is sorted by S2 cell, so neighboring places
    // mostly hit the same raster tile, which OsmViews keeps decoded.
    for batch in reader.read_all()? {
        for place in batch? {
            let ll = s2::latlng::LatLng::from(s2::cellid::CellID(place.s2_cell_id));
            ranks.push(views.rank(ll.lng.deg(), ll.lat.deg()) as f32);
        }
    }
    ranks.sort_unstable_by(f32::total_cmp);
    write_ranks(&ranks, &out)?;

    let metrics = views.metrics();
    log::info!(
        places = ranks.len(),
        tile_cache_hit_rate = metrics.tile_cache_hit_rate();
        "compute_osmviews_ranks: done"
    );
    Ok(out)
}

fn write_ranks(ranks: &[f32], out: &Path) -> Result<()> {
    let mut tmp = out.to_path_buf();
    tmp.add_extension("tmp");
    let mut w = BufWriter::new(
        File::create(&tmp).with_context(|| format!("Failed to create {}", tmp.display()))?,
    );
    w.write_all(RANKS_SIGNATURE)?;
    w.write_all(&(ranks.len() as u64).to_le_bytes())?;
    for r in ranks {
        w.write_all(&r.to_le_bytes())?;
    }
    w.into_inner()?.sync_all()?;
    std::fs::rename(&tmp, out)
        .with_context(|| format!("Failed to rename {} to {}", tmp.display(), out.display()))?;
    Ok(())
}

/// Maps an OSMViews `rank()` to its quantile among the locations of all
/// AllThePlaces features, as stored by [`compute_osmviews_ranks`].
pub struct OsmViewsQuantiles {
    /// Ascending.
    sorted: Vec<f32>,
}

impl OsmViewsQuantiles {
    pub fn open(path: &Path) -> Result<OsmViewsQuantiles> {
        let mut data = Vec::new();
        File::open(path)
            .and_then(|mut f| f.read_to_end(&mut data))
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if data.len() < 16 || &data[0..8] != RANKS_SIGNATURE {
            bail!("{} is not an osmviews-ranks file", path.display());
        }
        let count = u64::from_le_bytes(data[8..16].try_into()?) as usize;
        let body = &data[16..];
        if body.len() != count * 4 {
            bail!("{} is truncated", path.display());
        }
        let sorted = body
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        Ok(OsmViewsQuantiles { sorted })
    }

    /// Mid-rank quantile of `rank` in `(0, 1)`: with `less` places
    /// viewed less and `same` places viewed exactly as much, out of
    /// `n`, it is `(less + same / 2) / n`. Places on one raster pixel
    /// share a value, so ties are common; this way they share a
    /// quantile too. A rank between stored values gets the quantile of
    /// the gap it falls into. Returns 0.5 if there are no places at all.
    pub fn quantile(&self, rank: f64) -> f64 {
        if self.sorted.is_empty() {
            return 0.5;
        }
        let r = rank as f32;
        let less = self.sorted.partition_point(|&v| v < r);
        let less_or_same = self.sorted.partition_point(|&v| v <= r);
        let same = (less_or_same - less) as f64;
        (less as f64 + same / 2.0) / self.sorted.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn fixture(name: &str) -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/test_data");
        p.push(name);
        p
    }

    #[test]
    fn test_quantile() -> Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("osmviews-ranks");
        write_ranks(&[0.0, 0.5, 0.5, 0.5, 0.7], &path)?;
        let q = OsmViewsQuantiles::open(&path)?;
        assert_eq!(q.quantile(0.0), 0.1);
        assert_eq!(q.quantile(0.5), 0.5);
        assert_eq!(q.quantile(0.7), 0.9);
        assert_eq!(q.quantile(0.6), 0.8); // between 0.5 and 0.7
        assert_eq!(q.quantile(0.99), 1.0); // above all of them
        Ok(())
    }

    #[test]
    fn test_quantile_empty() -> Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("osmviews-ranks");
        write_ranks(&[], &path)?;
        assert_eq!(OsmViewsQuantiles::open(&path)?.quantile(0.3), 0.5);
        Ok(())
    }

    #[test]
    fn test_open_rejects_bad_files() -> Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("osmviews-ranks");
        std::fs::write(&path, b"not a ranks file")?;
        assert!(OsmViewsQuantiles::open(&path).is_err());
        write_ranks(&[0.1, 0.2], &path)?;
        let data = std::fs::read(&path)?;
        std::fs::write(&path, &data[..data.len() - 1])?;
        assert!(OsmViewsQuantiles::open(&path).is_err());
        Ok(())
    }

    #[test]
    fn test_compute_osmviews_ranks() -> Result<()> {
        let workdir = TempDir::new()?;
        let atp = workdir.path().join("alltheplaces.parquet");
        symlink(fixture("alltheplaces.parquet"), &atp)?;
        let tiff = workdir.path().join("osmviews.tiff");
        symlink(fixture("osmviews.tiff"), &tiff)?;

        let out = compute_osmviews_ranks(&atp, &tiff, workdir.path())?;
        // The fixture raster has four uniform 256×256 tiles; the
        // north-east one, where all ten fixture places lie, has value 5
        // out of a maximum of 10.
        let q = OsmViewsQuantiles::open(&out)?;
        assert_eq!(q.sorted, vec![0.5; 10]);
        assert_eq!(q.quantile(0.5), 0.5);

        // A second call reuses the file.
        let modified = std::fs::metadata(&out)?.modified()?;
        compute_osmviews_ranks(&atp, &tiff, workdir.path())?;
        assert_eq!(std::fs::metadata(&out)?.modified()?, modified);
        Ok(())
    }
}
