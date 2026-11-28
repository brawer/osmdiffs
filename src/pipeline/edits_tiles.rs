//! Turns `edits.parquet` (produced by `crate::pipeline::edits`) into
//! the GeoJSON Lines layers of `edits.pmtiles`, for reviewing the
//! suggested edits on a map. See `docs/outputs/EDITS_TILES.md` and
//! the plan in `docs/notes/2026-11-28-edits-map.md`.
//!
//! Built like `conflated.pmtiles` (see `pipeline::conflated_tiles`),
//! with the same zoom ranges, in one layer called `edits`:
//!
//! - the coarse **overview** (z0 to `DETAIL_MIN_ZOOM - 1`): one point
//!   per edit, with only `icon`, `action`, `topic` and `importance`.
//!   `icon` comes from the id-tagging-schema preset that best matches
//!   the element's tags after the edit; see `pipeline::id_presets`.
//!   No `id`, since a unique value per feature defeats the vector
//!   tile's value dedup and thins the coarse zooms to a fraction.
//! - the **detail** (`DETAIL_MIN_ZOOM` to `DETAIL_MAX_ZOOM`): one point
//!   per edit with everything a mapper needs to perform it, except the
//!   element's tags and members, which a viewer fetches from the OSM
//!   API; plus a line from the AllThePlaces position to the point
//!   (`part: "link"`) where the two are at least
//!   `MIN_CONNECTOR_LENGTH_METERS` apart.
//!
//! Every edit yields exactly one point per pass, so a sequential scan,
//! parallel per batch, is enough.

use super::conflated_tiles::{MIN_CONNECTOR_LENGTH_METERS, geometry_to_geojson_line};
use super::id_presets::{Geometry, Presets};
use super::tiles::round_importance;
use crate::utils::parquet::get_tags;
use crate::{TileLayer, make_progress_bar};
use anyhow::{Context, Result};
use arrow::array::{
    Array, BinaryArray, Float32Array, ListArray, RecordBatch, StringArray, StructArray,
    TimestampMillisecondArray, UInt32Array, UInt64Array,
};
use geo::{Centroid, Distance, Haversine, InteriorPoint};
use geo_traits::to_geo::ToGeoGeometry;
use indicatif::MultiProgress;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use rayon::prelude::*;
use serde_json::{Map, Value};
use std::fs::{File, rename};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use wkb::reader::read_wkb;

/// The one layer of `edits.pmtiles`. Both passes name it, so that
/// `tile-join` merges them into one layer over all zooms.
const EDITS_LAYER_NAME: &str = "edits";

/// The GeoJSON Lines layers [`extract_edits_layers`] writes.
pub struct EditsLayers {
    /// Fed to the overview pass, z0 to `DETAIL_MIN_ZOOM - 1`.
    pub overview: TileLayer,
    /// Fed to the detail pass, `DETAIL_MIN_ZOOM` to `DETAIL_MAX_ZOOM`.
    pub detail: TileLayer,
}

/// One decoded row of `edits.parquet`, with what the tiles need.
#[derive(Debug)]
struct EditTileRow {
    id: String,
    action: String,
    topics: Vec<String>,
    /// The Maki or Temaki icon of the place after the edit; see
    /// `pipeline::id_presets`. Empty if no preset with one matches.
    /// until the pipeline matches id-tagging-schema presets.
    icon: String,
    importance: f32,
    /// `osm.type`, `osm.id` and `osm.modified.version`; `None` for a
    /// create.
    osm: Option<(String, u64, u32)>,
    changes: Vec<Value>,
    sources: Vec<Value>,
    warnings: Vec<String>,
    /// `host.type`, `host.id` and `host.name`, for a create inside
    /// another place.
    host: Option<(String, u64, Option<String>)>,
    /// Where the edit is drawn: the node, or a point on the surface of
    /// the way or relation.
    point: geo::Point<f64>,
    /// The centroid of `atp_geometry`, if any.
    atp_point: Option<geo::Point<f64>>,
}

impl EditTileRow {
    /// The overview feature: the point with only `icon`, `action`,
    /// `topic` (the first of `topics`) and `importance`, rounded to two
    /// decimals for the importance filter of `tiles::render_tiles`.
    fn to_overview_geojson_line(&self) -> Result<String> {
        let mut properties = Map::new();
        properties.insert("icon".to_string(), Value::String(self.icon.clone()));
        properties.insert("action".to_string(), Value::String(self.action.clone()));
        properties.insert(
            "topic".to_string(),
            Value::String(self.topics.first().cloned().unwrap_or_default()),
        );
        properties.insert(
            "importance".to_string(),
            Value::from(round_importance(self.importance)),
        );
        geometry_to_geojson_line(&geo::Geometry::Point(self.point), properties)
    }

    /// The detail features: the point with every property, and a
    /// `part: "link"` line from the AllThePlaces position, if that is
    /// at least `MIN_CONNECTOR_LENGTH_METERS` away.
    fn to_detail_geojson_lines(&self) -> Result<Vec<String>> {
        let mut properties = Map::new();
        properties.insert("id".to_string(), Value::String(self.id.clone()));
        properties.insert("action".to_string(), Value::String(self.action.clone()));
        properties.insert("topics".to_string(), Value::String(self.topics.join(";")));
        properties.insert("icon".to_string(), Value::String(self.icon.clone()));
        properties.insert(
            "importance".to_string(),
            Value::from(round_importance(self.importance)),
        );
        if let Some((osm_type, osm_id, version)) = &self.osm {
            properties.insert("osm:type".to_string(), Value::String(osm_type.clone()));
            properties.insert("osm:id".to_string(), Value::from(*osm_id));
            properties.insert("osm:version".to_string(), Value::from(*version));
        }
        properties.insert(
            "changes".to_string(),
            Value::String(Value::Array(self.changes.clone()).to_string()),
        );
        if !self.warnings.is_empty() {
            properties.insert(
                "warnings".to_string(),
                Value::String(self.warnings.join(";")),
            );
        }
        properties.insert(
            "sources".to_string(),
            Value::String(Value::Array(self.sources.clone()).to_string()),
        );
        if let Some((host_type, host_id, host_name)) = &self.host {
            properties.insert("host:type".to_string(), Value::String(host_type.clone()));
            properties.insert("host:id".to_string(), Value::from(*host_id));
            if let Some(name) = host_name {
                properties.insert("host:name".to_string(), Value::String(name.clone()));
            }
        }
        let mut lines = vec![geometry_to_geojson_line(
            &geo::Geometry::Point(self.point),
            properties,
        )?];

        if let Some(atp_point) = self.atp_point
            && Haversine.distance(atp_point, self.point) >= MIN_CONNECTOR_LENGTH_METERS
        {
            let link = geo::Geometry::LineString(geo::LineString::new(vec![
                atp_point.into(),
                self.point.into(),
            ]));
            let mut link_properties = Map::new();
            link_properties.insert("id".to_string(), Value::String(self.id.clone()));
            link_properties.insert("action".to_string(), Value::String(self.action.clone()));
            link_properties.insert("part".to_string(), Value::String("link".to_string()));
            lines.push(geometry_to_geojson_line(&link, link_properties)?);
        }
        Ok(lines)
    }
}

/// Scans `edits.parquet` once and writes the overview and detail
/// layers of `edits.pmtiles`, with icons from the id-tagging-schema
/// presets in `presets`; see [`EditsLayers`].
pub fn extract_edits_layers(
    edits: &Path,
    presets: &Path,
    progress: &MultiProgress,
    workdir: &Path,
) -> Result<EditsLayers> {
    let layers = EditsLayers {
        overview: TileLayer {
            name: String::from(EDITS_LAYER_NAME),
            path: workdir.join("edits.jsonl"),
        },
        detail: TileLayer {
            name: String::from(EDITS_LAYER_NAME),
            path: workdir.join("edits-detail.jsonl"),
        },
    };
    if super::is_current(
        &[&layers.overview.path, &layers.detail.path],
        &[edits, presets],
    )? {
        return Ok(layers);
    }
    let presets = Presets::open(presets)?;

    let start = Instant::now();
    let file = File::open(edits).with_context(|| format!("could not open {edits:?}"))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let num_rows = builder.metadata().file_metadata().num_rows() as u64;
    let progress_bar = make_progress_bar(progress, "edits-tiles", num_rows, "edits");

    // Written to `.tmp` siblings and renamed once the scan succeeds, so
    // that a crash can't leave a half-written layer that the check
    // above would trust.
    let tmp = |layer: &TileLayer| {
        let mut path = PathBuf::from(&layer.path);
        path.add_extension("tmp");
        path
    };
    let (overview_tmp, detail_tmp) = (tmp(&layers.overview), tmp(&layers.detail));
    let mut overview_w = BufWriter::with_capacity(32768, File::create(&overview_tmp)?);
    let mut detail_w = BufWriter::with_capacity(32768, File::create(&detail_tmp)?);

    let (mut num_edits, mut num_links, mut num_without_icon) = (0u64, 0u64, 0u64);
    for batch in builder.build()? {
        let batch = batch?;
        let lines: Vec<(bool, String, Vec<String>)> = (0..batch.num_rows())
            .into_par_iter()
            .map(|row| {
                let edit = read_edit(&batch, row, &presets)
                    .with_context(|| format!("could not read edit in row {row} of batch"))?;
                Ok((
                    edit.icon.is_empty(),
                    edit.to_overview_geojson_line()?,
                    edit.to_detail_geojson_lines()?,
                ))
            })
            .collect::<Result<_>>()?;
        for (without_icon, overview, detail) in lines {
            num_without_icon += u64::from(without_icon);
            overview_w.write_all(overview.as_bytes())?;
            num_links += detail.len() as u64 - 1;
            for line in detail {
                detail_w.write_all(line.as_bytes())?;
            }
            num_edits += 1;
        }
        progress_bar.inc(batch.num_rows() as u64);
    }
    overview_w.flush()?;
    detail_w.flush()?;
    drop((overview_w, detail_w));
    rename(&overview_tmp, &layers.overview.path)?;
    rename(&detail_tmp, &layers.detail.path)?;

    progress_bar.finish_with_message(format!(
        "{num_edits} edits, {num_links} links, {num_without_icon} without icon"
    ));
    log::info!(
        elapsed_seconds = start.elapsed().as_secs_f64(),
        edits = num_edits,
        links = num_links,
        without_icon = num_without_icon;
        "extract_edits_layers: done"
    );
    Ok(layers)
}

/// Decodes `row` of a batch of `edits.parquet`.
fn read_edit(batch: &RecordBatch, row: usize, presets: &Presets) -> Result<EditTileRow> {
    let osm_struct = column::<StructArray>(batch, "osm")?;
    let (osm, osm_tags) = if osm_struct.is_null(row) {
        (None, Vec::new())
    } else {
        let modified = field::<StructArray>(osm_struct, "modified")?;
        (
            Some((
                field::<StringArray>(osm_struct, "type")?
                    .value(row)
                    .to_string(),
                field::<UInt64Array>(osm_struct, "id")?.value(row),
                field::<UInt32Array>(modified, "version")?.value(row),
            )),
            get_tags(osm_struct, "tags", row)?,
        )
    };
    let changes = read_changes(column::<ListArray>(batch, "changes")?, row)?;
    let geometry_kind = match &osm {
        Some((osm_type, _, _)) => Geometry::of_osm_type(osm_type),
        None => Geometry::Point,
    };
    let icon = presets
        .icon(&tags_after(osm_tags, &changes), geometry_kind)
        .to_string();
    let host = column::<StructArray>(batch, "host")?;
    let host = if host.is_null(row) {
        None
    } else {
        Some((
            field::<StringArray>(host, "type")?.value(row).to_string(),
            field::<UInt64Array>(host, "id")?.value(row),
            optional_string(field::<StringArray>(host, "name")?, row),
        ))
    };

    let geometry = read_wkb(column::<BinaryArray>(batch, "geometry")?.value(row))
        .context("could not decode geometry")?
        .to_geometry();
    let point = match geometry {
        geo::Geometry::Point(point) => point,
        other => other
            .interior_point()
            .context("geometry has no interior point")?,
    };
    let atp_geometry = column::<BinaryArray>(batch, "atp_geometry")?;
    let atp_point = if atp_geometry.is_null(row) {
        None
    } else {
        read_wkb(atp_geometry.value(row))
            .context("could not decode atp_geometry")?
            .to_geometry()
            .centroid()
    };

    Ok(EditTileRow {
        id: column::<StringArray>(batch, "id")?.value(row).to_string(),
        action: column::<StringArray>(batch, "action")?
            .value(row)
            .to_string(),
        topics: string_list(column::<ListArray>(batch, "topics")?, row)?,
        icon,
        importance: column::<Float32Array>(batch, "importance")?.value(row),
        osm,
        changes,
        sources: read_sources(column::<ListArray>(batch, "sources")?, row)?,
        warnings: string_list(column::<ListArray>(batch, "warnings")?, row)?,
        host,
        point: round_point(point),
        atp_point: atp_point.map(round_point),
    })
}

/// The tags of an element after the edit: `tags` with each of
/// `changes` (as [`read_changes`] gives them) applied.
fn tags_after(mut tags: Vec<(String, String)>, changes: &[Value]) -> Vec<(String, String)> {
    for change in changes {
        let Some(key) = change["key"].as_str() else {
            continue;
        };
        tags.retain(|(k, _)| k != key);
        if let Some(new) = change["new"].as_str() {
            tags.push((key.to_string(), new.to_string()));
        }
    }
    tags
}

/// The `changes` of `row`, each as `{"key", "old", "new", "reason"}`,
/// where `old` and `new` may be null, plus `atp` when it is set.
fn read_changes(changes: &ListArray, row: usize) -> Result<Vec<Value>> {
    let values = changes.value(row);
    let changes = downcast::<StructArray>(values.as_ref(), "changes")?;
    let key = field::<StringArray>(changes, "key")?;
    let old = field::<StringArray>(changes, "old")?;
    let new = field::<StringArray>(changes, "new")?;
    let reason = field::<StringArray>(changes, "reason")?;
    let atp = field::<StringArray>(changes, "atp")?;
    Ok((0..changes.len())
        .map(|i| {
            let mut change = Map::new();
            change.insert("key".to_string(), Value::from(key.value(i)));
            change.insert("old".to_string(), optional_string(old, i).into());
            change.insert("new".to_string(), optional_string(new, i).into());
            change.insert("reason".to_string(), Value::from(reason.value(i)));
            if let Some(atp) = optional_string(atp, i) {
                change.insert("atp".to_string(), Value::from(atp));
            }
            Value::Object(change)
        })
        .collect())
}

/// The `sources` of `row`, each with `spider`, `fetched` (the date),
/// `relation`, and `atp_ref`, `source_url`, `rule` and `distance_m`
/// (rounded to decimeters) where they are set.
fn read_sources(sources: &ListArray, row: usize) -> Result<Vec<Value>> {
    let values = sources.value(row);
    let sources = downcast::<StructArray>(values.as_ref(), "sources")?;
    let spider = field::<StringArray>(sources, "spider")?;
    let fetched = field::<TimestampMillisecondArray>(sources, "fetched")?;
    let atp_ref = field::<StringArray>(sources, "atp_ref")?;
    let source_url = field::<StringArray>(sources, "source_url")?;
    let relation = field::<StringArray>(sources, "relation")?;
    let rule = field::<StringArray>(sources, "rule")?;
    let distance_m = field::<Float32Array>(sources, "distance_m")?;
    (0..sources.len())
        .map(|i| {
            let mut source = Map::new();
            source.insert("spider".to_string(), Value::from(spider.value(i)));
            source.insert(
                "fetched".to_string(),
                Value::from(date_of_millis(fetched.value(i))?),
            );
            if let Some(v) = optional_string(atp_ref, i) {
                source.insert("atp_ref".to_string(), Value::from(v));
            }
            if let Some(v) = optional_string(source_url, i) {
                source.insert("source_url".to_string(), Value::from(v));
            }
            source.insert("relation".to_string(), Value::from(relation.value(i)));
            if let Some(v) = optional_string(rule, i) {
                source.insert("rule".to_string(), Value::from(v));
            }
            if !distance_m.is_null(i) {
                let d = (f64::from(distance_m.value(i)) * 10.0).round() / 10.0;
                source.insert("distance_m".to_string(), Value::from(d));
            }
            Ok(Value::Object(source))
        })
        .collect()
}

/// The UTC date of a timestamp in milliseconds, as `YYYY-MM-DD`.
fn date_of_millis(millis: i64) -> Result<String> {
    let t = time::UtcDateTime::from_unix_timestamp(millis.div_euclid(1000))
        .with_context(|| format!("timestamp {millis} out of range"))?;
    Ok(t.date().to_string())
}

/// Rounds to 1e-7 degrees, about 1 cm, like `conflated_tiles`.
fn round_point(p: geo::Point<f64>) -> geo::Point<f64> {
    let round = |v: f64| (v * 1e7).round() / 1e7;
    geo::point!(x: round(p.x()), y: round(p.y()))
}

fn string_list(list: &ListArray, row: usize) -> Result<Vec<String>> {
    let values = list.value(row);
    let strings = downcast::<StringArray>(values.as_ref(), "list item")?;
    Ok(strings.iter().flatten().map(str::to_string).collect())
}

fn optional_string(array: &StringArray, i: usize) -> Option<String> {
    (!array.is_null(i)).then(|| array.value(i).to_string())
}

fn downcast<'a, T: 'static>(array: &'a dyn Array, name: &str) -> Result<&'a T> {
    array
        .as_any()
        .downcast_ref::<T>()
        .with_context(|| format!("column '{name}' has an unexpected type"))
}

fn column<'a, T: 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a T> {
    let array = batch
        .column_by_name(name)
        .with_context(|| format!("no column '{name}'"))?;
    downcast(array.as_ref(), name)
}

fn field<'a, T: 'static>(src: &'a StructArray, name: &str) -> Result<&'a T> {
    let array = src
        .column_by_name(name)
        .with_context(|| format!("no field '{name}'"))?;
    downcast(array.as_ref(), name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EditTileRow {
        EditTileRow {
            id: "m-abc".to_string(),
            action: "modify".to_string(),
            topics: vec!["contact".to_string(), "wikidata".to_string()],
            icon: String::new(),
            importance: 0.98765,
            osm: Some(("way".to_string(), 42, 7)),
            changes: vec![serde_json::json!({
                "key": "fax", "old": "+41 1", "new": null, "reason": "differs"
            })],
            sources: vec![serde_json::json!({
                "spider": "denner_ch", "fetched": "2026-11-20", "relation": "same"
            })],
            warnings: vec!["replaces_email".to_string()],
            host: None,
            point: geo::point!(x: 8.5, y: 47.4),
            atp_point: Some(geo::point!(x: 8.5, y: 47.4001)),
        }
    }

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).expect("valid GeoJSON")
    }

    #[test]
    fn overview_is_minimal() -> Result<()> {
        let feature = parse(&sample().to_overview_geojson_line()?);
        assert_eq!(
            feature["properties"],
            serde_json::json!({
                "icon": "", "action": "modify", "topic": "contact", "importance": 0.99
            })
        );
        assert_eq!(feature["geometry"]["type"], "Point");
        Ok(())
    }

    #[test]
    fn detail_has_point_and_link() -> Result<()> {
        let lines = sample().to_detail_geojson_lines()?;
        assert_eq!(lines.len(), 2);
        let point = parse(&lines[0]);
        let props = &point["properties"];
        assert_eq!(props["id"], "m-abc");
        assert_eq!(props["topics"], "contact;wikidata");
        assert_eq!(props["osm:type"], "way");
        assert_eq!(props["osm:id"], 42);
        assert_eq!(props["osm:version"], 7);
        assert_eq!(props["warnings"], "replaces_email");
        assert!(props.get("part").is_none(), "{props}");
        assert!(props.get("host:id").is_none(), "{props}");
        let changes: Value = serde_json::from_str(props["changes"].as_str().expect("string"))?;
        assert_eq!(changes[0]["new"], Value::Null);
        assert!(changes[0].get("atp").is_none());
        let sources: Value = serde_json::from_str(props["sources"].as_str().expect("string"))?;
        assert_eq!(sources[0]["spider"], "denner_ch");

        let link = parse(&lines[1]);
        assert_eq!(link["geometry"]["type"], "LineString");
        assert_eq!(
            link["properties"],
            serde_json::json!({"id": "m-abc", "action": "modify", "part": "link"})
        );
        Ok(())
    }

    #[test]
    fn detail_of_create() -> Result<()> {
        let mut edit = sample();
        edit.action = "create".to_string();
        edit.osm = None;
        edit.warnings.clear();
        edit.host = Some(("way".to_string(), 9, Some("Glatt".to_string())));
        edit.atp_point = Some(edit.point);
        let lines = edit.to_detail_geojson_lines()?;
        assert_eq!(lines.len(), 1, "no link for a point at the same spot");
        let props = &parse(&lines[0])["properties"];
        assert!(props.get("osm:id").is_none(), "{props}");
        assert!(props.get("warnings").is_none(), "{props}");
        assert_eq!(props["host:type"], "way");
        assert_eq!(props["host:id"], 9);
        assert_eq!(props["host:name"], "Glatt");
        Ok(())
    }

    #[test]
    fn test_tags_after() {
        let tags = vec![
            ("shop".to_string(), "kiosk".to_string()),
            ("fax".to_string(), "+41 1".to_string()),
        ];
        let changes = [
            serde_json::json!({"key": "fax", "old": "+41 1", "new": null, "reason": "differs"}),
            serde_json::json!({"key": "shop", "old": "kiosk", "new": "convenience", "reason": "differs"}),
            serde_json::json!({"key": "name", "old": null, "new": "Avec", "reason": "missing_in_osm"}),
        ];
        assert_eq!(
            tags_after(tags, &changes),
            [
                ("shop".to_string(), "convenience".to_string()),
                ("name".to_string(), "Avec".to_string()),
            ]
        );
    }

    #[test]
    fn test_date_of_millis() -> Result<()> {
        assert_eq!(date_of_millis(0)?, "1970-01-01");
        assert_eq!(date_of_millis(1_795_000_000_000)?, "2026-11-18");
        Ok(())
    }
}
