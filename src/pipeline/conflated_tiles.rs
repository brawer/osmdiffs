//! Turns `conflated.parquet` (produced by `crate::pipeline::conflate`)
//! into PMTiles-ready GeoJSON Lines tile layers, for visualizing the
//! *matching* step itself -- every AllThePlaces feature, matched or
//! not -- independent of whatever `crate::pipeline::edits` separately
//! decides to propose as an edit (see
//! [#709](https://github.com/brawer/osmdiffs/issues/709)).
//!
//! Unlike `edits`, this doesn't need an external sort: every output
//! feature comes from exactly one input row (a row contributes one
//! overview feature and one-to-three detail features, never anything
//! merged across rows), so a straight sequential-batch,
//! rayon-parallel-per-batch scan is enough.
//!
//! [`extract_conflated_layers`] produces every layer
//! `conflated.pmtiles`' two-pass build needs (see
//! `pipeline::tiles::ZoomRange` and `pipeline::tiles::join_tiles`) in
//! one scan:
//!
//! - the coarse **overview** layers (`matched` / `unmatched`, one
//!   deliberately minimal feature per row -- `spider`, `relation`, and
//!   `osm:type`/`osm:id` when the row has an OSM feature, but *no tags
//!   and no `fid`*): at
//!   z0 a single tile holds the entire planet, and every per-feature
//!   byte counts. Tags push the tile so far past tippecanoe's size
//!   limit that `--drop-densest-as-needed` guts it to a few hundred
//!   features worldwide; even `fid` alone (unique per feature, so it
//!   defeats the vector tile's columnar value dedup) costs ~20x the
//!   low-zoom density. Measured on a full planet: full tags -> ~370
//!   features visible at z0; `+fid` -> ~7k; overview as it stands ->
//!   ~40k, a workable world map.
//! - the high-zoom **detail** layers (`matched` / `unmatched`, z13+,
//!   where a tile covers a few km and per-feature bytes cost nothing):
//!   matched rows get up to three features each (see
//!   [`ConflatedTileRow::to_detail_geojson_lines`]), unmatched rows
//!   get one -- both carrying their full tag set (the tag inspection
//!   the overview can't afford) and `fid`, the row's ordinal in
//!   `conflated.parquet`.
//!
//! - the **spider cells** layer (`spider_cells`, z0 to
//!   [`SPIDER_CELLS_MAX_ZOOM`]): for each spider and each cell of a grid
//!   [`SPIDER_CELL_ZOOM_OFFSET`] zooms finer than the tile, one point
//!   at the centroid of the spider's features in the cell, with their
//!   counts. Built in its own tippecanoe pass that never drops anything,
//!   so that a map filtered to one spider shows where all its features
//!   are even at zooms where the overview is thinned out
//!   ([#949](https://github.com/brawer/osmdiffs/issues/949)).
//!
//! - the **deletion candidate** layers (`deletion_candidate`, overview
//!   and detail): OSM features that nothing in AllThePlaces matched but
//!   that may be gone (see `conflate::deletion`), drawn as their OSM
//!   shape. Every feature carries `rule`, the row's
//!   `match.evidence.rule`, so a reviewer can tell why it was flagged
//!   ([#877](https://github.com/brawer/osmdiffs/issues/877)). These
//!   rows have no ATP side, so no `spider`, and they don't count
//!   towards `spider_cells`.
//!
//! Every feature carries `relation`, the row's `match.relation`. Only
//! `same` rows go to the `matched` layers. A `located_in` row, such as
//! a pharmacy inside a supermarket, goes to the `unmatched` layers,
//! since the store itself is missing in OSM; its OSM feature is the
//! host, drawn at z13+ as `part: "host"` with a link to the ATP point,
//! so a mapper sees where to add it.
//!
//! To go from a clicked overview feature to its full record: matched
//! features carry `osm:type`/`osm:id`, so fetch the detail tile for
//! that spot and match on those (or just open the OSM object). For
//! unmatched features -- which have no stable id, `conflated.parquet`
//! carries none for the ATP side -- match the nearest `part: "atp"`
//! detail feature of the same `spider`. Deletion candidates carry
//! `osm:type`/`osm:id` too, and join the same way as matched ones. See
//! [#775](https://github.com/brawer/osmdiffs/issues/775) for
//! the full design discussion.

use super::tiles::round_importance;
use crate::utils::parquet::{get_binary, get_f32, get_string, get_struct, get_tags, get_u64};
use crate::{TileLayer, make_progress_bar};
use anyhow::{Context, Result};
use arrow::array::{Array, RecordBatch, StructArray};
use geo::{Centroid, Distance, Haversine, MapCoordsInPlace};
use geo_traits::to_geo::ToGeoGeometry;
use indicatif::{MultiProgress, ProgressBar};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use rayon::prelude::*;
use serde_json::{Map, Value};
use std::fs::{File, rename};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use wkb::reader::read_wkb;

/// The tippecanoe layer name matched features belong to, whether they
/// come from the overview pass's single-feature representation or the
/// detail pass's up-to-three-feature one. A named constant, not a
/// string literal repeated at each construction site: both
/// `ConflatedLayers::overview_matched` and
/// `ConflatedLayers::detail_matched` are built from this same symbol,
/// so `tile-join` is guaranteed to merge them into one continuous
/// `matched` layer spanning the whole zoom range -- see
/// `DETAIL_MIN_ZOOM`'s doc comment for why that split exists at all.
/// If these ever named different strings, `tile-join` wouldn't error;
/// it would just silently produce two separately-toggleable layers
/// instead of one, so keeping this a single source of truth matters
/// more than it might look.
const MATCHED_LAYER_NAME: &str = "matched";

/// The tippecanoe layer name unmatched features belong to -- the
/// `unmatched` counterpart to [`MATCHED_LAYER_NAME`], and used for the
/// same reason: `ConflatedLayers::overview_unmatched` (z0..12) and
/// `ConflatedLayers::detail_unmatched` (z13+) both name it, so
/// `tile-join` stitches them into one continuous `unmatched` layer.
const UNMATCHED_LAYER_NAME: &str = "unmatched";

/// The tippecanoe layer name of deletion candidates, the same as their
/// `match.relation`. Named by both
/// `ConflatedLayers::overview_deletion_candidates` and
/// `ConflatedLayers::detail_deletion_candidates`, for the same
/// `tile-join` reason as [`MATCHED_LAYER_NAME`].
const DELETION_CANDIDATE_LAYER_NAME: &str = "deletion_candidate";

/// `match.relation` of rows whose ATP and OSM features are the same.
const RELATION_SAME: &str = "same";

/// Zoom level at which `conflated.pmtiles`' detail pass (fed by
/// [`ConflatedLayers::detail_matched`] / [`ConflatedLayers::detail_unmatched`])
/// takes over from the coarse overview pass (fed by
/// [`ConflatedLayers::overview_matched`] / [`ConflatedLayers::overview_unmatched`]).
/// The overview pass is built up to `DETAIL_MIN_ZOOM - 1`; the detail
/// pass starts at `DETAIL_MIN_ZOOM`. Keep both call sites (`pipeline::mod`'s
/// `render_conflated_overview`/`render_conflated_detail` steps) in
/// sync with this constant -- there must be neither a gap nor an
/// overlap between the two ranges, or `tile-join` either leaves a hole
/// or duplicates content at the boundary zoom.
pub(crate) const DETAIL_MIN_ZOOM: u8 = 13;

/// Highest zoom level the detail pass builds. Deliberately an explicit
/// ceiling, not tippecanoe's `-zg`/`--extend-zooms-if-still-dropping`:
/// that combination keeps adding zoom levels until every crowded
/// feature has separated into its own tile, which never happens for a
/// connector line whose two ends are nearly coincident (a *good*
/// match!) -- such a line never spatially separates from its own
/// endpoint no matter how far you zoom in. An early, unbounded
/// prototype of this feature reached z19 chasing that impossible
/// separation before an unrelated internal limit stopped it (see the
/// investigation note linked from #775). `MIN_CONNECTOR_LENGTH_METERS`
/// below heads off most such cases at the source, but this ceiling is
/// the hard backstop regardless.
pub(crate) const DETAIL_MAX_ZOOM: u8 = 16;

/// Below this length, a connector line's endpoints are close enough
/// that drawing a line between them adds no visual information to a
/// reviewer -- and is exactly the degenerate geometry
/// `DETAIL_MAX_ZOOM`'s doc comment describes. Comfortably above
/// typical GPS/geocoding noise (a few meters) and well below "this
/// match looks meaningfully offset" (tens of meters); real offsets
/// measured against a full-planet run ranged from ~0.2m (a coincident
/// match) up to the matcher's own ~400m search-radius cap. Tune if
/// real-world review turns up either false positives (connectors
/// shown for noise-level offsets) or missed ones (a genuine offset
/// silently dropped).
pub(crate) const MIN_CONNECTOR_LENGTH_METERS: f64 = 5.0;

/// The tippecanoe layer name of [`ConflatedLayers::spider_cells`].
const SPIDER_CELLS_LAYER_NAME: &str = "spider_cells";

/// Highest zoom level of the `spider_cells` layer: the highest zoom at
/// which tippecanoe's `--drop-densest-as-needed` still thins out the
/// overview somewhere on the planet. From the next zoom on, the overview
/// shows every feature, so a map can switch back to it. Measured on the
/// 2026-11-02 planet; see #949.
pub(crate) const SPIDER_CELLS_MAX_ZOOM: u8 = 8;

/// At zoom `z`, the `spider_cells` layer has one point per spider in
/// each tile of zoom `z + SPIDER_CELL_ZOOM_OFFSET`, so at most 64 per
/// spider in a tile.
const SPIDER_CELL_ZOOM_OFFSET: u8 = 3;

/// The property of a `spider_cells` feature that holds the one zoom
/// level at which it is shown; see `tiles::Thinning::Never`. Not
/// tippecanoe's per-feature `minzoom`/`maxzoom`: with tippecanoe v2.49,
/// that kept one feature per tile on the planet.
pub(crate) const SPIDER_CELL_ZOOM_ATTR: &str = "z";

/// Every GeoJSON Lines layer [`extract_conflated_layers`] produces,
/// grouped by which of `conflated.pmtiles`' two tippecanoe passes
/// consumes them. Named fields, not a `Vec` some caller has to index
/// positionally and remember the order of.
pub struct ConflatedLayers {
    /// Fed to the overview pass (`ZoomRange::Bounded { min: 0, max:
    /// DETAIL_MIN_ZOOM - 1 }`) together with [`Self::overview_unmatched`].
    pub overview_matched: TileLayer,
    /// Fed to the overview pass together with [`Self::overview_matched`].
    pub overview_unmatched: TileLayer,
    /// Fed to the overview pass together with [`Self::overview_matched`]:
    /// one minimal OSM shape per deletion candidate.
    pub overview_deletion_candidates: TileLayer,
    /// Fed to the detail pass (`ZoomRange::Bounded { min:
    /// DETAIL_MIN_ZOOM, max: DETAIL_MAX_ZOOM }`): matched rows, up to
    /// three features each. Named `MATCHED_LAYER_NAME` -- the same
    /// layer name [`Self::overview_matched`] uses -- so `tile-join`
    /// merges them into one continuous layer; see that constant's doc
    /// comment.
    pub detail_matched: TileLayer,
    /// Fed to the detail pass together with [`Self::detail_matched`]:
    /// unmatched rows, one full-tag ATP point each, plus the host and
    /// a link for `located_in` rows. Named
    /// `UNMATCHED_LAYER_NAME`, matching [`Self::overview_unmatched`],
    /// for the same `tile-join` reason.
    pub detail_unmatched: TileLayer,
    /// Fed to the detail pass together with [`Self::detail_matched`]:
    /// one full-tag OSM shape per deletion candidate. Named
    /// `DELETION_CANDIDATE_LAYER_NAME`, matching
    /// [`Self::overview_deletion_candidates`].
    pub detail_deletion_candidates: TileLayer,
    /// Fed to its own pass (`ZoomRange::Bounded { min: 0, max:
    /// SPIDER_CELLS_MAX_ZOOM }`, `tiles::Thinning::Never`, never
    /// dropping anything): per spider
    /// and grid cell, one point with counts; see [`SpiderCells`].
    pub spider_cells: TileLayer,
}

/// One decoded row of `conflated.parquet`, kept whether or not it has
/// an OSM match -- unlike `edits::ConflatedRow`, which only exists for
/// matched rows (nothing to suggest an edit for otherwise). A row
/// without an ATP side is a [`DeletionTileRow`] instead.
struct ConflatedTileRow {
    spider: String,
    /// `match.relation`, such as `same` or `located_in`; see
    /// `docs/outputs/CONFLATED_PARQUET.md`.
    relation: String,
    /// See `pipeline::importance`.
    importance: f32,
    atp_tags: Vec<(String, String)>,
    atp_geometry_wkb: Vec<u8>,
    osm: Option<OsmSide>,
}

/// One row of `conflated.parquet` without an ATP side: an OSM feature
/// that nothing in AllThePlaces matched, but that may be gone; see
/// `conflate::deletion`.
struct DeletionTileRow {
    /// `match.relation`, always `deletion_candidate` today.
    relation: String,
    /// `match.evidence.rule`, such as `wikidata` or
    /// `swiss_transit_stop`: why the feature was flagged.
    rule: String,
    /// `match.evidence.local_share`, for the brand rules: the share of
    /// OSM features of the brand nearby that ATP matched.
    local_share: Option<f32>,
    /// See `pipeline::importance`.
    importance: f32,
    osm: OsmSide,
}

/// A decoded row of `conflated.parquet`, by whether it has an ATP side.
enum TileRow {
    Atp(ConflatedTileRow),
    Deletion(DeletionTileRow),
}

struct OsmSide {
    osm_type: String,
    osm_id: u64,
    osm_tags: Vec<(String, String)>,
    osm_geometry_wkb: Vec<u8>,
}

impl ConflatedTileRow {
    /// Whether this row belongs to the `matched` layers: the ATP and
    /// OSM features are the same thing. A `located_in` row has an OSM
    /// feature too, but that is its host.
    fn is_matched(&self) -> bool {
        self.osm.is_some() && self.relation == RELATION_SAME
    }

    /// The coarse overview layer's representation: one deliberately
    /// minimal feature per row. Geometry is the actual OSM shape when
    /// matched (what a reviewer is meant to review -- point/line/
    /// polygon, all valid within one GeoJSON/MVT layer), the raw
    /// ATP-provided shape otherwise, also for a `located_in` row,
    /// whose OSM feature is only its host.
    ///
    /// Properties are just `spider`, `relation`, and -- rows with an
    /// OSM feature only -- `osm:type`/`osm:id`. `relation` costs no
    /// more than the `matched` flag it replaced, which only repeated
    /// the layer name. **No tags, and no `fid`**: see this
    /// module's doc comment for why every low-zoom byte counts, and how
    /// a viewer gets from one of these features to the full record in
    /// the z13+ detail layer ([`Self::to_detail_geojson_lines`] /
    /// [`Self::to_unmatched_detail_geojson_line`]).
    ///
    /// Also carries the row's `importance`, rounded to two decimals so
    /// that it costs the tile at most 101 distinct values, which the
    /// overview's tippecanoe filter uses to show the most important
    /// places at low zooms (see `tiles::importance_filter`).
    fn to_overview_geojson_line(&self) -> Result<String> {
        let geometry_wkb: &[u8] = match &self.osm {
            Some(osm) if self.is_matched() => &osm.osm_geometry_wkb,
            _ => &self.atp_geometry_wkb,
        };
        let geometry = decode_and_round(geometry_wkb)?;

        let mut properties: Map<String, Value> = Map::new();
        properties.insert("spider".to_string(), Value::String(self.spider.clone()));
        properties.insert("relation".to_string(), Value::String(self.relation.clone()));
        if let Some(osm) = &self.osm {
            properties.insert("osm:type".to_string(), Value::String(osm.osm_type.clone()));
            properties.insert("osm:id".to_string(), Value::from(osm.osm_id));
        }
        properties.insert(
            "importance".to_string(),
            Value::from(round_importance(self.importance)),
        );
        geometry_to_geojson_line(&geometry, properties)
    }

    /// The detail layer's representation of a *matched* row -- see
    /// [`Self::to_unmatched_detail_geojson_line`] for the unmatched
    /// case. `osm` is passed explicitly, rather than read from
    /// `self.osm`, so that precondition is enforced by the type system
    /// instead of an internal `Option::unwrap`.
    ///
    /// Yields the ATP point and the OSM shape as two separate
    /// features -- each carrying only its own side's tags (`atp:*` or
    /// `osm:*`), so a tile inspector shows which end is which -- plus
    /// a connector line between their centroids, unless that line
    /// would be shorter than `MIN_CONNECTOR_LENGTH_METERS` (see that
    /// constant's doc comment for why). Every feature carries `fid`
    /// (this row's ordinal in `conflated.parquet` -- shared by all of a
    /// row's detail features, so a viewer can group the up-to-three
    /// back together, and a cross-reference into `conflated.parquet`)
    /// and a `part` property (`"atp"` | `"osm"` | `"link"`) identifying
    /// its role; the connector line -- the only feature that isn't
    /// clearly one side or the other -- keeps both tag sets. For a
    /// `located_in` row, the OSM feature is the host, so its `part` is
    /// `"host"`. Every feature also carries `relation`.
    fn to_detail_geojson_lines(&self, osm: &OsmSide, fid: u64) -> Result<Vec<String>> {
        let atp_geometry = decode_and_round(&self.atp_geometry_wkb)?;
        let osm_geometry = decode_and_round(&osm.osm_geometry_wkb)?;

        let osm_part = if self.relation == RELATION_SAME {
            "osm"
        } else {
            "host"
        };
        let mut atp_properties = self.detail_properties(fid, "atp");
        for (k, v) in &self.atp_tags {
            atp_properties.insert(format!("atp:{k}"), Value::String(v.clone()));
        }

        let mut osm_properties = self.detail_properties(fid, osm_part);
        osm_properties.insert("osm:type".to_string(), Value::String(osm.osm_type.clone()));
        osm_properties.insert("osm:id".to_string(), Value::from(osm.osm_id));
        for (k, v) in &osm.osm_tags {
            osm_properties.insert(format!("osm:{k}"), Value::String(v.clone()));
        }

        let mut lines = vec![
            geometry_to_geojson_line(&atp_geometry, atp_properties)?,
            geometry_to_geojson_line(&osm_geometry, osm_properties)?,
        ];

        let atp_centroid = atp_geometry
            .centroid()
            .context("atp_geometry has no centroid")?;
        let osm_centroid = osm_geometry
            .centroid()
            .context("osm_geometry has no centroid")?;
        if Haversine.distance(atp_centroid, osm_centroid) >= MIN_CONNECTOR_LENGTH_METERS {
            let connector = geo::Geometry::LineString(geo::LineString::new(vec![
                atp_centroid.into(),
                osm_centroid.into(),
            ]));
            let mut link_properties = self.detail_properties(fid, "link");
            for (k, v) in &self.atp_tags {
                link_properties.insert(format!("atp:{k}"), Value::String(v.clone()));
            }
            link_properties.insert("osm:type".to_string(), Value::String(osm.osm_type.clone()));
            link_properties.insert("osm:id".to_string(), Value::from(osm.osm_id));
            for (k, v) in &osm.osm_tags {
                link_properties.insert(format!("osm:{k}"), Value::String(v.clone()));
            }
            lines.push(geometry_to_geojson_line(&connector, link_properties)?);
        }

        Ok(lines)
    }

    /// The detail layer's representation of an *unmatched* row: a
    /// single ATP point carrying its full `atp:*` tag set -- the tag
    /// inspection the minimal overview layer deliberately can't afford
    /// (see [`Self::to_overview_geojson_line`]), reachable one zoom
    /// step in. `part` is `"atp"`, matching the matched detail's ATP
    /// feature; `fid` is this row's ordinal in `conflated.parquet`.
    /// The clicked overview feature carries no id, so a viewer finds
    /// this one by nearest-of-same-`spider` -- see the module doc.
    fn to_unmatched_detail_geojson_line(&self, fid: u64) -> Result<String> {
        let geometry = decode_and_round(&self.atp_geometry_wkb)?;

        let mut properties = self.detail_properties(fid, "atp");
        for (k, v) in &self.atp_tags {
            properties.insert(format!("atp:{k}"), Value::String(v.clone()));
        }
        geometry_to_geojson_line(&geometry, properties)
    }

    /// The properties that every detail feature of this row carries.
    fn detail_properties(&self, fid: u64, part: &str) -> Map<String, Value> {
        let mut properties: Map<String, Value> = Map::new();
        properties.insert("fid".to_string(), Value::from(fid));
        properties.insert("spider".to_string(), Value::String(self.spider.clone()));
        properties.insert("relation".to_string(), Value::String(self.relation.clone()));
        properties.insert("part".to_string(), Value::String(part.to_string()));
        properties
    }
}

impl DeletionTileRow {
    /// The overview layer's representation: the OSM shape, with only
    /// `relation`, `rule`, `importance` and `osm:type`/`osm:id`. No
    /// tags and no `fid`, as for the other overview layers; a viewer
    /// joins on `osm:type`/`osm:id` to reach the detail feature.
    fn to_overview_geojson_line(&self) -> Result<String> {
        let geometry = decode_and_round(&self.osm.osm_geometry_wkb)?;
        let mut properties = self.common_properties();
        properties.insert(
            "importance".to_string(),
            Value::from(round_importance(self.importance)),
        );
        geometry_to_geojson_line(&geometry, properties)
    }

    /// The detail layer's representation: the OSM shape with its full
    /// `osm:*` tag set, `part: "osm"`, `fid` (the row's ordinal in
    /// `conflated.parquet`) and, for the brand rules, `local_share`
    /// rounded to two decimals.
    fn to_detail_geojson_line(&self, fid: u64) -> Result<String> {
        let geometry = decode_and_round(&self.osm.osm_geometry_wkb)?;
        let mut properties = self.common_properties();
        properties.insert("fid".to_string(), Value::from(fid));
        properties.insert("part".to_string(), Value::String("osm".to_string()));
        if let Some(share) = self.local_share {
            properties.insert(
                "local_share".to_string(),
                Value::from(round_importance(share)),
            );
        }
        for (k, v) in &self.osm.osm_tags {
            properties.insert(format!("osm:{k}"), Value::String(v.clone()));
        }
        geometry_to_geojson_line(&geometry, properties)
    }

    /// The properties that both of this row's features carry.
    fn common_properties(&self) -> Map<String, Value> {
        let mut properties: Map<String, Value> = Map::new();
        properties.insert("relation".to_string(), Value::String(self.relation.clone()));
        properties.insert("rule".to_string(), Value::String(self.rule.clone()));
        properties.insert(
            "osm:type".to_string(),
            Value::String(self.osm.osm_type.clone()),
        );
        properties.insert("osm:id".to_string(), Value::from(self.osm.osm_id));
        properties
    }
}

/// The `spider_cells` layer, accumulated row by row: for every spider,
/// zoom level and grid cell, the counts and the sum of the positions of
/// the spider's features there.
#[derive(Default)]
struct SpiderCells {
    /// Spider names, indexed by the ids in `cells`' keys.
    spiders: Vec<String>,
    spider_ids: std::collections::HashMap<String, u32>,
    /// By spider id, zoom, and the grid cell's x and y.
    cells: std::collections::HashMap<(u32, u8, u32, u32), SpiderCell>,
}

#[derive(Debug, Default, PartialEq)]
struct SpiderCell {
    n: u64,
    same: u64,
    missing_in_osm: u64,
    located_in: u64,
    sum_lon: f64,
    sum_lat: f64,
}

impl SpiderCells {
    /// Adds a feature of `spider` with `match.relation` `relation` at
    /// `(lon, lat)` to its cell at every zoom.
    fn add(&mut self, spider: &str, relation: &str, (lon, lat): (f64, f64)) {
        let id = match self.spider_ids.get(spider) {
            Some(&id) => id,
            None => {
                let id = self.spiders.len() as u32;
                self.spiders.push(spider.to_string());
                self.spider_ids.insert(spider.to_string(), id);
                id
            }
        };
        let finest = SPIDER_CELLS_MAX_ZOOM + SPIDER_CELL_ZOOM_OFFSET;
        let (x, y) = tile_of(lon, lat, finest);
        for zoom in 0..=SPIDER_CELLS_MAX_ZOOM {
            let shift = SPIDER_CELLS_MAX_ZOOM - zoom;
            let cell = self
                .cells
                .entry((id, zoom, x >> shift, y >> shift))
                .or_default();
            cell.n += 1;
            match relation {
                RELATION_SAME => cell.same += 1,
                "missing_in_osm" => cell.missing_in_osm += 1,
                "located_in" => cell.located_in += 1,
                _ => {}
            }
            cell.sum_lon += lon;
            cell.sum_lat += lat;
        }
    }

    /// Writes one GeoJSON Lines feature per cell, sorted by spider,
    /// zoom and cell, so that the output is reproducible. Each carries
    /// its zoom as [`SPIDER_CELL_ZOOM_ATTR`], so that tippecanoe shows
    /// it at that zoom only. Returns how many it wrote.
    fn write(&self, out: &mut dyn Write) -> Result<u64> {
        let mut keys: Vec<&(u32, u8, u32, u32)> = self.cells.keys().collect();
        keys.sort_by_key(|&&(id, zoom, x, y)| (&self.spiders[id as usize], zoom, x, y));
        for key in &keys {
            let (id, zoom, _, _) = **key;
            let cell = &self.cells[*key];
            let round = |v: f64| (v * 1e7).round() / 1e7;
            let n = cell.n as f64;
            let point = geo::Geometry::Point(geo::point!(
                x: round(cell.sum_lon / n),
                y: round(cell.sum_lat / n)
            ));
            let mut properties = Map::new();
            properties.insert(
                "spider".to_string(),
                Value::String(self.spiders[id as usize].clone()),
            );
            properties.insert("n".to_string(), Value::from(cell.n));
            properties.insert("same".to_string(), Value::from(cell.same));
            properties.insert(
                "missing_in_osm".to_string(),
                Value::from(cell.missing_in_osm),
            );
            properties.insert("located_in".to_string(), Value::from(cell.located_in));
            properties.insert(SPIDER_CELL_ZOOM_ATTR.to_string(), Value::from(zoom));
            out.write_all(geometry_to_geojson_line(&point, properties)?.as_bytes())?;
        }
        Ok(keys.len() as u64)
    }
}

/// The x and y of the Web Mercator tile at `zoom` that holds
/// `(lon, lat)`, clamped to the map at the poles and the antimeridian.
fn tile_of(lon: f64, lat: f64, zoom: u8) -> (u32, u32) {
    let n = f64::from(1u32 << zoom);
    let lat = lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0 * n;
    let clamp = |v: f64| (v.floor().max(0.0) as u32).min((1u32 << zoom) - 1);
    (clamp(x), clamp(y))
}

/// Decodes WKB and rounds coordinates to 1e-7 degrees (about 1cm at
/// the equator) -- same precision `SuggestedEdit::to_geojson` already
/// applies to its own point, so we're not emitting fake sub-millimeter
/// precision, particularly for `atp_geometry`, which is whatever
/// precision AllThePlaces' upstream source happened to provide.
fn decode_and_round(wkb: &[u8]) -> Result<geo::Geometry<f64>> {
    let mut geometry = read_wkb(wkb)
        .context("failed to decode geometry")?
        .to_geometry();
    geometry.map_coords_in_place(|c| geo::Coord {
        x: (c.x * 1e7).round() / 1e7,
        y: (c.y * 1e7).round() / 1e7,
    });
    Ok(geometry)
}

fn geometry_to_geojson_line(
    geometry: &geo::Geometry<f64>,
    properties: Map<String, Value>,
) -> Result<String> {
    let feature = geojson::Feature {
        bbox: None,
        geometry: Some(geojson::Geometry::from(geometry)),
        id: None,
        properties: Some(properties),
        foreign_members: None,
    };
    let mut line = feature.to_string();
    line.push('\n');
    Ok(line)
}

/// Scans `conflated.parquet` once and writes every GeoJSON Lines layer
/// `conflated.pmtiles`' overview and detail passes need -- see
/// [`ConflatedLayers`]. One pass, not two: every row's decode work
/// (WKB, tags, geometry rounding) happens once, even though matched
/// rows contribute to both the overview and detail layers.
pub fn extract_conflated_layers(
    conflated: &Path,
    progress: &MultiProgress,
    workdir: &Path,
) -> Result<ConflatedLayers> {
    assert!(workdir.exists());

    let layers = ConflatedLayers {
        overview_matched: TileLayer {
            name: String::from(MATCHED_LAYER_NAME),
            path: workdir.join("matched.jsonl"),
        },
        overview_unmatched: TileLayer {
            name: String::from(UNMATCHED_LAYER_NAME),
            path: workdir.join("unmatched.jsonl"),
        },
        detail_matched: TileLayer {
            name: String::from(MATCHED_LAYER_NAME),
            path: workdir.join("matched-detail.jsonl"),
        },
        detail_unmatched: TileLayer {
            name: String::from(UNMATCHED_LAYER_NAME),
            path: workdir.join("unmatched-detail.jsonl"),
        },
        overview_deletion_candidates: TileLayer {
            name: String::from(DELETION_CANDIDATE_LAYER_NAME),
            path: workdir.join("deletion-candidates.jsonl"),
        },
        detail_deletion_candidates: TileLayer {
            name: String::from(DELETION_CANDIDATE_LAYER_NAME),
            path: workdir.join("deletion-candidates-detail.jsonl"),
        },
        spider_cells: TileLayer {
            name: String::from(SPIDER_CELLS_LAYER_NAME),
            path: workdir.join("spider-cells.jsonl"),
        },
    };
    let all_layers = [
        &layers.overview_matched,
        &layers.overview_unmatched,
        &layers.detail_matched,
        &layers.detail_unmatched,
        &layers.overview_deletion_candidates,
        &layers.detail_deletion_candidates,
        &layers.spider_cells,
    ];
    let outputs: Vec<&Path> = all_layers
        .iter()
        .map(|layer| layer.path.as_path())
        .collect();
    if super::is_current(&outputs, &[conflated])? {
        return Ok(layers);
    }

    let num_rows = {
        // Parquet row counts live in file metadata -- cheap, no row I/O.
        let file = File::open(conflated)?;
        ParquetRecordBatchReaderBuilder::try_new(file)?
            .metadata()
            .file_metadata()
            .num_rows() as u64
    };
    let progress_bar = make_progress_bar(progress, "confl-tiles", num_rows, "conflated features");

    // Each layer is written to a sibling `.tmp` file, then renamed into
    // place once the whole scan succeeds -- so a crash mid-scan can't
    // leave a half-written layer that the memoization check above would
    // then trust.
    let open_tmp = |layer: &TileLayer| -> Result<(PathBuf, BufWriter<File>)> {
        let mut tmp = PathBuf::from(&layer.path);
        tmp.add_extension("tmp");
        let writer = BufWriter::with_capacity(32768, File::create(&tmp)?);
        Ok((tmp, writer))
    };
    let (overview_matched_tmp, mut overview_matched_w) = open_tmp(&layers.overview_matched)?;
    let (overview_unmatched_tmp, mut overview_unmatched_w) = open_tmp(&layers.overview_unmatched)?;
    let (detail_matched_tmp, mut detail_matched_w) = open_tmp(&layers.detail_matched)?;
    let (detail_unmatched_tmp, mut detail_unmatched_w) = open_tmp(&layers.detail_unmatched)?;
    let (overview_deletion_tmp, mut overview_deletion_w) =
        open_tmp(&layers.overview_deletion_candidates)?;
    let (detail_deletion_tmp, mut detail_deletion_w) =
        open_tmp(&layers.detail_deletion_candidates)?;
    let (spider_cells_tmp, mut spider_cells_w) = open_tmp(&layers.spider_cells)?;

    let counts = extract_rows(
        conflated,
        &progress_bar,
        Writers {
            overview_matched: &mut overview_matched_w,
            overview_unmatched: &mut overview_unmatched_w,
            detail_matched: &mut detail_matched_w,
            detail_unmatched: &mut detail_unmatched_w,
            overview_deletion_candidates: &mut overview_deletion_w,
            detail_deletion_candidates: &mut detail_deletion_w,
            spider_cells: &mut spider_cells_w,
        },
    )?;

    for mut writer in [
        overview_matched_w,
        overview_unmatched_w,
        detail_matched_w,
        detail_unmatched_w,
        overview_deletion_w,
        detail_deletion_w,
        spider_cells_w,
    ] {
        writer.flush()?;
    }
    rename(&overview_matched_tmp, &layers.overview_matched.path)?;
    rename(&overview_unmatched_tmp, &layers.overview_unmatched.path)?;
    rename(&detail_matched_tmp, &layers.detail_matched.path)?;
    rename(&detail_unmatched_tmp, &layers.detail_unmatched.path)?;
    rename(
        &overview_deletion_tmp,
        &layers.overview_deletion_candidates.path,
    )?;
    rename(
        &detail_deletion_tmp,
        &layers.detail_deletion_candidates.path,
    )?;
    rename(&spider_cells_tmp, &layers.spider_cells.path)?;

    progress_bar.finish_with_message(format!(
        "{num_rows} conflated features → {} matched / {} unmatched / \
         {} deletion candidate rows, {} matched-detail / {} unmatched-detail \
         features, {} spider cells",
        counts.matched,
        counts.unmatched,
        counts.deletion_candidates,
        counts.matched_detail_features,
        counts.unmatched_detail_features,
        counts.spider_cells
    ));

    Ok(layers)
}

/// The sinks [`extract_rows`] writes to, one per [`ConflatedLayers`]
/// field -- a struct rather than positional `&mut impl Write`
/// parameters so a call site can't transpose two of them.
struct Writers<'a> {
    overview_matched: &'a mut dyn Write,
    overview_unmatched: &'a mut dyn Write,
    detail_matched: &'a mut dyn Write,
    detail_unmatched: &'a mut dyn Write,
    overview_deletion_candidates: &'a mut dyn Write,
    detail_deletion_candidates: &'a mut dyn Write,
    spider_cells: &'a mut dyn Write,
}

#[derive(Default)]
struct RowCounts {
    matched: u64,
    unmatched: u64,
    matched_detail_features: u64,
    unmatched_detail_features: u64,
    deletion_candidates: u64,
    spider_cells: u64,
}

fn extract_rows(
    conflated: &Path,
    progress_bar: &ProgressBar,
    writers: Writers,
) -> Result<RowCounts> {
    let start = Instant::now();
    let mut counts = RowCounts::default();
    let mut spider_cells = SpiderCells::default();

    let file = File::open(conflated)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    // `fid` is the row's ordinal in `conflated.parquet` -- carried onto
    // every detail feature (not the overview: see the module doc for
    // why every low-zoom byte counts) so a viewer can group a matched
    // row's up-to-three detail features and cross-reference the parquet.
    // Batches arrive in file order, so a running offset plus the
    // in-batch index reproduces that ordinal without a shared counter.
    let mut batch_offset: u64 = 0;
    for batch in reader {
        let batch = batch?;
        // Decoding + GeoJSON serialization is the CPU-heavy part of
        // this loop; parallelize it per batch with Rayon, same as
        // edits.rs's produce_edits, then write out sequentially --
        // there's nothing to deduplicate or sort here, unlike edits.rs,
        // so no channels/external sort are needed, just an ordered
        // in-memory Vec per batch.
        type AtpResult = (ConflatedTileRow, Option<(f64, f64)>, String, Vec<String>);
        enum Extracted {
            Atp(Box<AtpResult>),
            Deletion { overview: String, detail: String },
        }
        let rows: Vec<Extracted> = (0..batch.num_rows())
            .into_par_iter()
            .map(|row| -> Result<Extracted> {
                let fid = batch_offset + row as u64;
                let tile_row = match extract_conflated_tile_row(&batch, row)? {
                    TileRow::Atp(tile_row) => tile_row,
                    TileRow::Deletion(deletion) => {
                        return Ok(Extracted::Deletion {
                            overview: deletion.to_overview_geojson_line()?,
                            detail: deletion.to_detail_geojson_line(fid)?,
                        });
                    }
                };
                let overview_line = tile_row.to_overview_geojson_line()?;
                let detail_lines = match &tile_row.osm {
                    Some(osm) => tile_row.to_detail_geojson_lines(osm, fid)?,
                    None => vec![tile_row.to_unmatched_detail_geojson_line(fid)?],
                };
                let location = read_wkb(&tile_row.atp_geometry_wkb)
                    .ok()
                    .and_then(|g| g.to_geometry().centroid())
                    .map(|c| (c.x(), c.y()));
                Ok(Extracted::Atp(Box::new((
                    tile_row,
                    location,
                    overview_line,
                    detail_lines,
                ))))
            })
            .collect::<Result<Vec<_>>>()?;
        batch_offset += batch.num_rows() as u64;

        for entry in rows {
            progress_bar.inc(1);
            let (tile_row, location, overview_line, detail_lines) = match entry {
                Extracted::Atp(entry) => *entry,
                Extracted::Deletion { overview, detail } => {
                    writers
                        .overview_deletion_candidates
                        .write_all(overview.as_bytes())?;
                    writers
                        .detail_deletion_candidates
                        .write_all(detail.as_bytes())?;
                    counts.deletion_candidates += 1;
                    continue;
                }
            };
            if let Some(location) = location {
                spider_cells.add(&tile_row.spider, &tile_row.relation, location);
            }
            if tile_row.is_matched() {
                writers
                    .overview_matched
                    .write_all(overview_line.as_bytes())?;
                counts.matched += 1;
                for line in detail_lines {
                    writers.detail_matched.write_all(line.as_bytes())?;
                    counts.matched_detail_features += 1;
                }
            } else {
                writers
                    .overview_unmatched
                    .write_all(overview_line.as_bytes())?;
                counts.unmatched += 1;
                for line in detail_lines {
                    writers.detail_unmatched.write_all(line.as_bytes())?;
                    counts.unmatched_detail_features += 1;
                }
            }
        }
    }

    counts.spider_cells = spider_cells.write(writers.spider_cells)?;

    log::info!(
        elapsed_seconds = start.elapsed().as_secs_f64(),
        spider_cells = counts.spider_cells,
        matched = counts.matched,
        unmatched = counts.unmatched,
        matched_detail_features = counts.matched_detail_features,
        unmatched_detail_features = counts.unmatched_detail_features,
        deletion_candidates = counts.deletion_candidates;
        "extract_conflated_layers: done"
    );
    Ok(counts)
}

/// Decodes `row`. A row without an ATP side is an OSM feature that may
/// be gone (see `conflate::deletion`), and becomes a [`DeletionTileRow`].
/// Unlike `edits::extract_conflated_row`, a row without an `osm` side
/// is kept, not skipped: an unmatched ATP feature is exactly what this
/// module exists to show.
fn extract_conflated_tile_row(batch: &RecordBatch, row: usize) -> Result<TileRow> {
    let atp = get_struct(batch, "atp")?;
    let osm = get_struct(batch, "osm")?;
    let matching = get_struct(batch, "match")?;
    let relation = get_string(matching, "relation", row)?;
    if atp.is_null(row) {
        anyhow::ensure!(
            !osm.is_null(row),
            "row {row} has neither an atp nor an osm side"
        );
        let evidence = get_struct(matching, "evidence")?;
        return Ok(TileRow::Deletion(DeletionTileRow {
            relation,
            rule: get_optional_string(evidence, "rule", row)?.unwrap_or_default(),
            local_share: get_optional_f32(evidence, "local_share", row)?,
            importance: get_f32(batch, "importance", row)?,
            osm: osm_side(batch, osm, row)?,
        }));
    }
    let fetched = get_struct(atp, "fetched")?;

    let osm = if osm.is_null(row) {
        None
    } else {
        Some(osm_side(batch, osm, row)?)
    };

    Ok(TileRow::Atp(ConflatedTileRow {
        spider: get_string(fetched, "spider", row)?,
        relation,
        importance: get_f32(batch, "importance", row)?,
        atp_tags: get_tags(atp, "tags", row)?,
        atp_geometry_wkb: get_binary(batch, "atp_geometry", row)?,
        osm,
    }))
}

/// Decodes the `osm` side of `row`, which must not be null.
fn osm_side(batch: &RecordBatch, osm: &StructArray, row: usize) -> Result<OsmSide> {
    Ok(OsmSide {
        osm_type: get_string(osm, "type", row)?,
        osm_id: get_u64(osm, "id", row)?,
        osm_tags: get_tags(osm, "tags", row)?,
        // Top-level, not nested inside `osm` -- GeoParquet 2.0
        // requires geometry columns to live at the schema root
        // (see `pipeline::conflate::writer::GEO_METADATA_KEY`'s
        // doc comment).
        osm_geometry_wkb: get_binary(batch, "osm_geometry", row)?,
    })
}

/// The string at `row` of the column `name` of `src`, or `None` if null.
fn get_optional_string(src: &StructArray, name: &str, row: usize) -> Result<Option<String>> {
    let column = src
        .column_by_name(name)
        .with_context(|| format!("missing column '{name}'"))?;
    if column.is_null(row) {
        return Ok(None);
    }
    get_string(src, name, row).map(Some)
}

/// The float at `row` of the column `name` of `src`, or `None` if null.
fn get_optional_f32(src: &StructArray, name: &str, row: usize) -> Result<Option<f32>> {
    let column = src
        .column_by_name(name)
        .with_context(|| format!("missing column '{name}'"))?;
    if column.is_null(row) {
        return Ok(None);
    }
    get_f32(src, name, row).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indicatif::ProgressDrawTarget;
    use tempfile::TempDir;
    use wkb::writer::{WriteOptions, write_geometry};

    fn hidden_progress() -> MultiProgress {
        MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
    }

    #[test]
    fn memoized_layers_carry_the_right_tippecanoe_layer_names() {
        // Exercises the real extract_conflated_layers/ConflatedLayers
        // construction, not just the constants in isolation: pre-create
        // all four output files, newer than a placeholder `conflated`
        // that isn't a parquet file at all, so the memoization check
        // short-circuits before ever reading it, then confirm each overview layer and its detail counterpart
        // carry the identical tippecanoe layer name -- the whole reason
        // MATCHED_LAYER_NAME / UNMATCHED_LAYER_NAME exist: if a future
        // edit ever let a pair drift apart, tile-join wouldn't error, it
        // would just silently produce two half-zoom-range layers instead
        // of one continuous one.
        let workdir = TempDir::new().expect("tempdir");
        let conflated = workdir.path().join("conflated.parquet");
        std::fs::write(&conflated, b"").expect("write placeholder");
        let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        File::options()
            .write(true)
            .open(&conflated)
            .and_then(|f| f.set_modified(earlier))
            .expect("backdate placeholder");
        for name in [
            "matched.jsonl",
            "unmatched.jsonl",
            "matched-detail.jsonl",
            "unmatched-detail.jsonl",
            "deletion-candidates.jsonl",
            "deletion-candidates-detail.jsonl",
            "spider-cells.jsonl",
        ] {
            std::fs::write(workdir.path().join(name), b"").expect("write placeholder");
        }

        let layers = extract_conflated_layers(&conflated, &hidden_progress(), workdir.path())
            .expect("memoized path should not read `conflated` at all");

        assert_eq!(layers.overview_matched.name, layers.detail_matched.name);
        assert_eq!(layers.detail_matched.name, MATCHED_LAYER_NAME);
        assert_eq!(layers.overview_unmatched.name, layers.detail_unmatched.name);
        assert_eq!(layers.detail_unmatched.name, UNMATCHED_LAYER_NAME);
        assert_eq!(
            layers.overview_deletion_candidates.name,
            layers.detail_deletion_candidates.name
        );
        assert_eq!(
            layers.detail_deletion_candidates.name,
            DELETION_CANDIDATE_LAYER_NAME
        );
        assert_eq!(layers.spider_cells.name, SPIDER_CELLS_LAYER_NAME);
    }

    #[test]
    fn tile_of_web_mercator() {
        assert_eq!(tile_of(0.0, 0.0, 0), (0, 0));
        assert_eq!(tile_of(8.5, 47.4, 1), (1, 0));
        // Zürich, as on openstreetmap.org.
        assert_eq!(tile_of(8.5417, 47.3769, 13), (4290, 2868));
        // Clamped at the poles and the antimeridian.
        assert_eq!(tile_of(180.0, -90.0, 3), (7, 7));
        assert_eq!(tile_of(-180.0, 90.0, 3), (0, 0));
    }

    #[test]
    fn spider_cells_one_point_per_spider_cell_and_zoom() {
        let mut cells = SpiderCells::default();
        // Two features in Zürich, one in Geneva, and one of another spider.
        cells.add("acme", "same", (8.54, 47.37));
        cells.add("acme", "missing_in_osm", (8.56, 47.39));
        cells.add("acme", "located_in", (6.14, 46.20));
        cells.add("other", "same", (8.54, 47.37));
        let mut out = Vec::new();
        let written = cells.write(&mut out).expect("write");
        let features: Vec<Value> = String::from_utf8(out)
            .expect("utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("json"))
            .collect();
        assert_eq!(written, features.len() as u64);

        let at = |spider: &str, zoom: u8| -> Vec<&Value> {
            features
                .iter()
                .filter(|f| f["properties"]["spider"] == spider && f["properties"]["z"] == zoom)
                .collect()
        };
        // At z0 the cells are z3 tiles, so all of Switzerland is one.
        let z0 = at("acme", 0);
        assert_eq!(z0.len(), 1);
        let props = &z0[0]["properties"];
        assert_eq!(
            (
                &props["n"],
                &props["same"],
                &props["missing_in_osm"],
                &props["located_in"]
            ),
            (
                &Value::from(3),
                &Value::from(1),
                &Value::from(1),
                &Value::from(1)
            )
        );
        // The centroid of the members, not the middle of the cell.
        assert_eq!(
            z0[0]["geometry"]["coordinates"],
            Value::from(vec![7.7466667, 46.9866667])
        );
        // At the highest zoom, Zürich and Geneva are separate cells.
        assert_eq!(at("acme", SPIDER_CELLS_MAX_ZOOM).len(), 2);
        assert_eq!(at("other", 0).len(), 1);
        // Sorted by spider, then zoom.
        assert_eq!(features[0]["properties"]["spider"], "acme");
        assert_eq!(features[0]["properties"]["z"], 0);
        let last = features.last().expect("features");
        assert_eq!(last["properties"]["spider"], "other");
        assert_eq!(last["properties"]["z"], SPIDER_CELLS_MAX_ZOOM);
    }

    fn encode_point(lon: f64, lat: f64) -> Vec<u8> {
        let point = geo::point!(x: lon, y: lat);
        let mut buf = Vec::new();
        write_geometry(&mut buf, &point, &WriteOptions::default()).expect("encode point");
        buf
    }

    fn parse(line: &str) -> Value {
        assert!(line.ends_with('\n'));
        serde_json::from_str(line.trim_end()).expect("valid json")
    }

    #[test]
    fn overview_unmatched_row_is_the_atp_point_with_no_tags() {
        let row = ConflatedTileRow {
            spider: "acme".to_string(),
            relation: "missing_in_osm".to_string(),
            importance: 0.5,
            atp_tags: vec![("shop".to_string(), "bakery".to_string())],
            atp_geometry_wkb: encode_point(8.5, 47.2),
            osm: None,
        };
        let value = parse(&row.to_overview_geojson_line().expect("overview line"));

        assert_eq!(value["type"], "Feature");
        assert_eq!(value["geometry"]["type"], "Point");
        assert_eq!(
            value["geometry"]["coordinates"],
            Value::from(vec![8.5, 47.2])
        );
        assert_eq!(value["properties"]["spider"], "acme");
        assert_eq!(value["properties"]["relation"], "missing_in_osm");
        // The overview carries no tags, and no fid -- every low-zoom
        // byte counts (see the module doc).
        assert!(value["properties"].get("fid").is_none());
        assert!(value["properties"].get("atp:shop").is_none());
        assert!(value["properties"].get("osm:type").is_none());
    }

    #[test]
    fn overview_matched_row_is_the_osm_shape_with_only_the_osm_id() {
        let row = ConflatedTileRow {
            spider: "acme".to_string(),
            relation: "same".to_string(),
            importance: 0.5,
            atp_tags: vec![("shop".to_string(), "bakery".to_string())],
            atp_geometry_wkb: encode_point(8.5, 47.2),
            osm: Some(OsmSide {
                osm_type: "node".to_string(),
                osm_id: 42,
                osm_tags: vec![("shop".to_string(), "bakery".to_string())],
                osm_geometry_wkb: encode_point(8.50001, 47.20001),
            }),
        };
        let value = parse(&row.to_overview_geojson_line().expect("overview line"));
        let props = &value["properties"];

        // The OSM shape, not the ATP one -- that's the whole point of
        // preferring it once matched.
        assert_eq!(
            value["geometry"]["coordinates"],
            Value::from(vec![8.50001, 47.20001])
        );
        assert_eq!(props["relation"], "same");
        assert!(props.get("fid").is_none());
        // osm:type / osm:id are kept -- they double as the join key to
        // the detail layer and a click-through to the OSM object; every
        // other tag is not.
        assert_eq!(props["osm:id"], 42);
        assert_eq!(props["osm:type"], "node");
        assert!(props.get("osm:shop").is_none());
        assert!(props.get("atp:shop").is_none());
    }

    #[test]
    fn overview_carries_rounded_importance() {
        let row = ConflatedTileRow {
            spider: "acme".to_string(),
            relation: "missing_in_osm".to_string(),
            importance: 0.98765,
            atp_tags: vec![],
            atp_geometry_wkb: encode_point(8.5, 47.2),
            osm: None,
        };
        let value = parse(&row.to_overview_geojson_line().expect("overview line"));
        assert_eq!(value["properties"]["importance"], 0.99);

        // The detail features don't: at z13+ every row is shown.
        let detail = parse(
            &row.to_unmatched_detail_geojson_line(7)
                .expect("detail line"),
        );
        assert!(detail["properties"].get("importance").is_none());
    }

    #[test]
    fn overview_geometry_coordinates_are_rounded_to_seven_decimal_places() {
        let row = ConflatedTileRow {
            spider: "acme".to_string(),
            relation: "missing_in_osm".to_string(),
            importance: 0.5,
            atp_tags: vec![],
            atp_geometry_wkb: encode_point(8.123_456_789, 47.987_654_321),
            osm: None,
        };
        let value = parse(&row.to_overview_geojson_line().expect("overview line"));

        assert_eq!(
            value["geometry"]["coordinates"],
            Value::from(vec![8.1234568, 47.9876543])
        );
    }

    fn osm_side_far_from(atp_lon: f64, atp_lat: f64) -> OsmSide {
        // Roughly 50m north -- comfortably above MIN_CONNECTOR_LENGTH_METERS.
        OsmSide {
            osm_type: "way".to_string(),
            osm_id: 99,
            osm_tags: vec![("shop".to_string(), "supermarket".to_string())],
            osm_geometry_wkb: encode_point(atp_lon, atp_lat + 0.00045),
        }
    }

    #[test]
    fn matched_detail_features_carry_their_own_tags_and_a_shared_fid() {
        let row = ConflatedTileRow {
            spider: "acme".to_string(),
            relation: "same".to_string(),
            importance: 0.5,
            atp_tags: vec![("name".to_string(), "Acme ATP".to_string())],
            atp_geometry_wkb: encode_point(8.5, 47.2),
            osm: None, // unused; osm side passed explicitly below
        };
        let osm = osm_side_far_from(8.5, 47.2);
        let lines = row
            .to_detail_geojson_lines(&osm, 55)
            .expect("to_detail_geojson_lines");
        let features: Vec<Value> = lines.iter().map(|l| parse(l)).collect();

        // Every detail feature shares the row's fid -- that's the join
        // key back to the clicked overview feature.
        assert!(
            features.iter().all(|f| f["properties"]["fid"] == 55),
            "every detail feature must carry fid=55: {features:?}"
        );
        assert!(
            features
                .iter()
                .all(|f| f["properties"]["relation"] == "same")
        );

        let atp = features
            .iter()
            .find(|f| f["properties"]["part"] == "atp")
            .expect("expected an atp feature");
        assert_eq!(atp["properties"]["atp:name"], "Acme ATP");
        assert!(
            atp["properties"].get("osm:type").is_none(),
            "atp feature should carry no osm:* properties: {atp}"
        );

        let osm_feature = features
            .iter()
            .find(|f| f["properties"]["part"] == "osm")
            .expect("expected an osm feature");
        assert_eq!(osm_feature["properties"]["osm:shop"], "supermarket");
        assert_eq!(osm_feature["properties"]["osm:id"], 99);
        assert!(
            osm_feature["properties"].get("atp:name").is_none(),
            "osm feature should carry no atp:* properties: {osm_feature}"
        );

        let link = features
            .iter()
            .find(|f| f["properties"]["part"] == "link")
            .expect("expected a link feature -- offset is well above the threshold");
        assert_eq!(link["properties"]["atp:name"], "Acme ATP");
        assert_eq!(link["properties"]["osm:shop"], "supermarket");
    }

    #[test]
    fn matched_detail_omits_the_connector_when_offset_is_below_threshold() {
        let row = ConflatedTileRow {
            spider: "acme".to_string(),
            relation: "same".to_string(),
            importance: 0.5,
            atp_tags: vec![],
            atp_geometry_wkb: encode_point(8.5, 47.2),
            osm: None,
        };
        // Same coordinates as the ATP point -- a coincident match.
        let osm = OsmSide {
            osm_type: "node".to_string(),
            osm_id: 7,
            osm_tags: vec![],
            osm_geometry_wkb: encode_point(8.5, 47.2),
        };
        let lines = row
            .to_detail_geojson_lines(&osm, 1)
            .expect("to_detail_geojson_lines");
        assert_eq!(
            lines.len(),
            2,
            "expected only atp+osm, no link, for a coincident match: {lines:?}"
        );
        let features: Vec<Value> = lines.iter().map(|l| parse(l)).collect();
        assert!(features.iter().all(|f| f["properties"]["part"] != "link"));
    }

    #[test]
    fn unmatched_detail_is_a_single_full_tag_atp_point() {
        let row = ConflatedTileRow {
            spider: "acme".to_string(),
            relation: "missing_in_osm".to_string(),
            importance: 0.5,
            atp_tags: vec![
                ("name".to_string(), "Acme Foods".to_string()),
                ("addr:city".to_string(), "Zürich".to_string()),
                ("opening_hours".to_string(), "24/7".to_string()),
            ],
            atp_geometry_wkb: encode_point(8.5, 47.2),
            osm: None,
        };
        let value = parse(
            &row.to_unmatched_detail_geojson_line(88)
                .expect("unmatched detail line"),
        );
        let props = &value["properties"];

        assert_eq!(value["geometry"]["type"], "Point");
        assert_eq!(props["fid"], 88);
        assert_eq!(props["spider"], "acme");
        assert_eq!(props["part"], "atp");
        // Full tags here -- not trimmed the way the overview would be:
        // z13+ tiles have the room, and this is the only place an
        // unmatched row's tags are inspectable at all.
        assert_eq!(props["atp:name"], "Acme Foods");
        assert_eq!(props["atp:addr:city"], "Zürich");
        assert_eq!(props["atp:opening_hours"], "24/7");
    }

    fn located_in_row() -> ConflatedTileRow {
        ConflatedTileRow {
            spider: "publix".to_string(),
            relation: "located_in".to_string(),
            importance: 0.5,
            atp_tags: vec![("amenity".to_string(), "pharmacy".to_string())],
            atp_geometry_wkb: encode_point(8.5, 47.2),
            osm: Some(osm_side_far_from(8.5, 47.2)),
        }
    }

    #[test]
    fn located_in_row_is_unmatched_and_shown_at_the_atp_point() {
        let row = located_in_row();
        assert!(!row.is_matched());
        let value = parse(&row.to_overview_geojson_line().expect("overview line"));
        let props = &value["properties"];
        // The ATP point, not the host's shape: the store is what is
        // missing in OSM.
        assert_eq!(
            value["geometry"]["coordinates"],
            Value::from(vec![8.5, 47.2])
        );
        assert_eq!(props["relation"], "located_in");
        assert_eq!(props["osm:type"], "way");
        assert_eq!(props["osm:id"], 99);
    }

    #[test]
    fn located_in_detail_has_atp_point_host_and_link() {
        let row = located_in_row();
        let osm = row.osm.as_ref().expect("host");
        let lines = row.to_detail_geojson_lines(osm, 3).expect("detail lines");
        let features: Vec<Value> = lines.iter().map(|l| parse(l)).collect();
        let parts: Vec<&str> = features
            .iter()
            .map(|f| f["properties"]["part"].as_str().expect("part"))
            .collect();
        assert_eq!(parts, vec!["atp", "host", "link"]);
        assert!(
            features
                .iter()
                .all(|f| f["properties"]["relation"] == "located_in")
        );
        assert_eq!(features[1]["properties"]["osm:shop"], "supermarket");
        assert_eq!(features[0]["properties"]["atp:amenity"], "pharmacy");
    }

    fn deletion_row(local_share: Option<f32>) -> DeletionTileRow {
        DeletionTileRow {
            relation: "deletion_candidate".to_string(),
            rule: "wikidata".to_string(),
            local_share,
            importance: 0.4321,
            osm: osm_side_far_from(8.5, 47.2),
        }
    }

    #[test]
    fn overview_deletion_candidate_is_the_osm_shape_with_rule_and_no_tags() {
        let value = parse(
            &deletion_row(Some(0.75))
                .to_overview_geojson_line()
                .expect("overview line"),
        );
        let props = &value["properties"];
        assert_eq!(value["geometry"]["type"], "Point");
        assert_eq!(props["relation"], "deletion_candidate");
        assert_eq!(props["rule"], "wikidata");
        assert_eq!(props["osm:type"], "way");
        assert_eq!(props["osm:id"], 99);
        assert_eq!(props["importance"], 0.43);
        // No ATP side, so no spider; and no tags, fid or local_share,
        // since every low-zoom byte counts.
        assert!(props.get("spider").is_none());
        assert!(props.get("fid").is_none());
        assert!(props.get("local_share").is_none());
        assert!(props.get("osm:shop").is_none());
    }

    #[test]
    fn deletion_candidate_detail_is_the_full_tag_osm_shape() {
        let value = parse(
            &deletion_row(Some(0.666))
                .to_detail_geojson_line(42)
                .expect("detail line"),
        );
        let props = &value["properties"];
        assert_eq!(props["fid"], 42);
        assert_eq!(props["part"], "osm");
        assert_eq!(props["relation"], "deletion_candidate");
        assert_eq!(props["rule"], "wikidata");
        assert_eq!(props["local_share"], 0.67);
        assert_eq!(props["osm:id"], 99);
        assert_eq!(props["osm:shop"], "supermarket");
        assert!(props.get("spider").is_none());
    }

    #[test]
    fn deletion_candidate_detail_omits_a_missing_local_share() {
        let value = parse(
            &deletion_row(None)
                .to_detail_geojson_line(1)
                .expect("detail line"),
        );
        assert!(value["properties"].get("local_share").is_none());
    }

    /// A one-row batch with only the columns that
    /// [extract_conflated_tile_row] reads of a row without an ATP side,
    /// shaped as in `conflated.parquet`.
    fn deletion_batch(local_share: Option<f32>) -> RecordBatch {
        use arrow::array::{
            ArrayRef, BinaryArray, Float32Array, MapBuilder, MapFieldNames, StringArray,
            StringBuilder, UInt64Array,
        };
        use arrow::buffer::NullBuffer;
        use arrow::datatypes::{DataType, Field, Fields};
        use std::sync::Arc;

        let mut tags = MapBuilder::new(
            Some(MapFieldNames {
                entry: "key_value".into(),
                key: "key".into(),
                value: "value".into(),
            }),
            StringBuilder::new(),
            StringBuilder::new(),
        );
        tags.keys().append_value("brand:wikidata");
        tags.values().append_value("Q123");
        tags.append(true).expect("map entry");
        let tags = Arc::new(tags.finish()) as ArrayRef;

        let osm = StructArray::try_new(
            Fields::from(vec![
                Field::new("type", DataType::Utf8, false),
                Field::new("id", DataType::UInt64, false),
                Field::new("tags", tags.data_type().clone(), false),
            ]),
            vec![
                Arc::new(StringArray::from(vec!["node"])) as ArrayRef,
                Arc::new(UInt64Array::from(vec![7_u64])),
                tags,
            ],
            None,
        )
        .expect("osm");
        let atp = StructArray::try_new(
            Fields::from(vec![Field::new("placeholder", DataType::Utf8, true)]),
            vec![Arc::new(StringArray::from(vec![None::<&str>])) as ArrayRef],
            Some(NullBuffer::new_null(1)),
        )
        .expect("atp");
        let evidence = StructArray::try_new(
            Fields::from(vec![
                Field::new("rule", DataType::Utf8, true),
                Field::new("local_share", DataType::Float32, true),
            ]),
            vec![
                Arc::new(StringArray::from(vec![Some("wikidata")])) as ArrayRef,
                Arc::new(Float32Array::from(vec![local_share])),
            ],
            None,
        )
        .expect("evidence");
        let matching = StructArray::try_new(
            Fields::from(vec![
                Field::new("relation", DataType::Utf8, false),
                Field::new("evidence", evidence.data_type().clone(), false),
            ]),
            vec![
                Arc::new(StringArray::from(vec!["deletion_candidate"])) as ArrayRef,
                Arc::new(evidence),
            ],
            None,
        )
        .expect("match");
        RecordBatch::try_from_iter(vec![
            ("atp", Arc::new(atp) as ArrayRef),
            ("osm", Arc::new(osm)),
            (
                "osm_geometry",
                Arc::new(BinaryArray::from(vec![encode_point(8.5, 47.2).as_slice()])),
            ),
            ("match", Arc::new(matching)),
            ("importance", Arc::new(Float32Array::from(vec![0.25_f32]))),
        ])
        .expect("batch")
    }

    #[test]
    fn row_without_atp_side_is_a_deletion_candidate() {
        let TileRow::Deletion(row) =
            extract_conflated_tile_row(&deletion_batch(Some(0.5)), 0).expect("decodes")
        else {
            panic!("expected a deletion candidate");
        };
        assert_eq!(row.relation, "deletion_candidate");
        assert_eq!(row.rule, "wikidata");
        assert_eq!(row.local_share, Some(0.5));
        assert_eq!(row.importance, 0.25);
        assert_eq!(row.osm.osm_type, "node");
        assert_eq!(row.osm.osm_id, 7);
        assert_eq!(
            row.osm.osm_tags,
            vec![("brand:wikidata".to_string(), "Q123".to_string())]
        );

        let TileRow::Deletion(row) =
            extract_conflated_tile_row(&deletion_batch(None), 0).expect("decodes")
        else {
            panic!("expected a deletion candidate");
        };
        assert_eq!(row.local_share, None);
    }
}
