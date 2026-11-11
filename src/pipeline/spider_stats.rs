//! Writes `spider-stats.parquet`: for each AllThePlaces spider, what
//! became of its features, how they matched OpenStreetMap, and how far
//! their tags agree, so that a dashboard can show where conflation goes
//! badly and why; see brawer/osmdiffs#924 and
//! [docs/outputs/SPIDER_STATS.md](../../docs/outputs/SPIDER_STATS.md).
//!
//! One row per spider, sorted by spider, in small row groups, so that a
//! web page can fetch one spider's row with HTTP range requests. All
//! numbers are counts, so that a reader can add them up across spiders,
//! countries or brands before dividing.
//!
//! Reads `conflated.parquet` once, and the side files that earlier
//! steps wrote: `alltheplaces.spider-counts.tsv` and
//! `alltheplaces.stats.json` from `import_atp`, and `osm-brands.tsv` and
//! `brand-names.tsv` from `collect_wikidata_ids`.

use super::atp::{run_stats, spider_counts};
use super::brand_totals;
use super::conflate::diagnostics::{self, SpiderDiagnostics};
use super::conflate::evidence::{compared_tags, has_compared_tag};
use super::wikidata_ids::WIKIDATA_KEYS;
use crate::make_progress_bar;
use crate::matchers::{contact::main_category, parse_wikidata_ids};
use crate::utils::countries::{Country, country};
use crate::utils::parquet::{get_binary, get_string, get_struct, get_tags, get_u64};
use anyhow::{Context, Result};
use arrow::array::{
    Array, ArrayRef, Float32Array, Float32Builder, ListArray, ListBuilder, RecordBatch,
    StringArray, StringBuilder, StructBuilder, UInt64Builder,
};
use arrow::datatypes::{DataType, Field, Fields, Schema};
use geo::Centroid;
use geo_traits::to_geo::ToGeoGeometry;
use indicatif::MultiProgress;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wkb::reader::read_wkb;

/// Filename, within `workdir`, of the file [compute_spider_stats]
/// writes.
const FILENAME: &str = "spider-stats.parquet";

/// Spiders per row group: few, so that a web page fetching one spider
/// with HTTP range requests reads little more than that spider.
const ROWS_PER_GROUP: usize = 64;

/// Upper bounds, in meters, of the bins of the `distance_m` histogram;
/// the last bin has no upper bound.
pub(in crate::pipeline) const DISTANCE_BINS_M: [f32; 7] =
    [1.0, 10.0, 25.0, 50.0, 100.0, 250.0, 1000.0];

/// Computes the stats of every spider from `conflated` and the side
/// files in `workdir`, and writes them to `workdir/spider-stats.parquet`,
/// unless that is already newer than `conflated`.
pub fn compute_spider_stats(
    conflated: &Path,
    progress: &MultiProgress,
    workdir: &Path,
) -> Result<PathBuf> {
    let out = workdir.join(FILENAME);
    if out.exists() && super::modified(&out)? >= super::modified(conflated)? {
        return Ok(out);
    }

    let counts: HashMap<String, spider_counts::SpiderCounts> =
        spider_counts::read(&spider_counts::path(workdir))?
            .into_iter()
            .map(|c| (c.spider.clone(), c))
            .collect();
    let run_stats = run_stats::read(workdir)?.unwrap_or_default();
    let osm_brands = brand_totals::read_features(workdir)?;
    let names = brand_totals::read_names(workdir)?;
    let diagnostics = diagnostics::read(&diagnostics::path(workdir))?;

    let stats = scan_conflated(conflated, progress)?;

    let mut spiders: BTreeSet<&str> = counts.keys().map(String::as_str).collect();
    spiders.extend(run_stats.keys().map(String::as_str));
    spiders.extend(stats.spiders.keys().map(String::as_str));
    let empty = Spider::default();
    let rows: Vec<Row> = spiders
        .into_iter()
        .map(|spider| Row {
            spider,
            counts: counts.get(spider),
            run_stats: run_stats.get(spider),
            diagnostics: diagnostics.get(spider),
            stats: stats.spiders.get(spider).unwrap_or(&empty),
        })
        .collect();
    let lookups = Lookups {
        osm_brands: &osm_brands,
        names: &names,
        deletions: &stats.deletions,
    };
    write(&rows, &lookups, &out)?;
    log::info!(
        spiders = rows.len(),
        conflated_rows = stats.rows,
        deletion_candidates = stats.deletions.values().sum::<u64>(),
        bytes = std::fs::metadata(&out)?.len();
        "compute_spider_stats: done"
    );
    Ok(out)
}

/// What one row of `conflated.parquet` contributes to the stats.
#[derive(Debug, Default)]
struct RowInfo {
    /// `None` for a deletion candidate, which has no ATP side.
    spider: Option<String>,
    relation: Relation,
    rule: Option<String>,
    distance_m: Option<f32>,
    /// Of the ATP feature, or for a deletion candidate, of the OSM one.
    country: Option<Country>,
    /// The brand, operator and network IDs of the ATP feature, or for a
    /// deletion candidate, of the OSM one.
    wikidata: Vec<u64>,
    category: Option<String>,
    /// For each tag of [compared_tags] that the ATP feature has, its
    /// position there and how it compares with OSM.
    tags: Vec<(usize, TagState)>,
    opening_hours: Option<String>,
    /// The OSM feature of a `same` row, as `id * 4 + type`.
    osm_ref: Option<u64>,
    /// For a `same` row, how far the ATP feature lies east and north of
    /// the OSM one, in meters, from centroid to centroid.
    offset: Option<(f32, f32)>,
    /// Hash of the ATP geometry, to find features at the same position.
    position: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Relation {
    #[default]
    Same,
    LocatedIn,
    MissingInOsm,
    DeletionCandidate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TagState {
    Agreeing,
    Differing,
    MissingInOsm,
}

/// What we count for one brand, operator or network of a spider in one
/// country.
#[derive(Debug, Default)]
struct Brand {
    atp_features: u64,
    same: u64,
    located_in: u64,
    missing_in_osm: u64,
    /// The main categories of the ATP features, so that the OSM total
    /// counts features of the same kinds only.
    categories: HashSet<String>,
    offsets: Vec<(f32, f32)>,
}

/// What we count for one spider from `conflated.parquet`.
#[derive(Debug, Default)]
struct Spider {
    rows: u64,
    same: u64,
    located_in: u64,
    missing_in_osm: u64,
    rules: HashMap<String, u64>,
    distance_m: [u64; DISTANCE_BINS_M.len() + 1],
    /// Per tag of [compared_tags]: ATP features with the tag, and how
    /// many of them agree, differ, or have no such tag in OSM.
    tags: Vec<[u64; 4]>,
    opening_hours: HashMap<String, u64>,
    osm_refs: HashMap<u64, u64>,
    offsets: Vec<(f32, f32)>,
    positions: HashMap<u64, u64>,
    /// By country and Wikidata ID; 0 for features without one.
    brands: HashMap<(Option<Country>, u64), Brand>,
}

impl Spider {
    fn add(&mut self, row: RowInfo) {
        self.rows += 1;
        match row.relation {
            Relation::Same => self.same += 1,
            Relation::LocatedIn => self.located_in += 1,
            Relation::MissingInOsm => self.missing_in_osm += 1,
            Relation::DeletionCandidate => {}
        }
        if let Some(rule) = row.rule {
            *self.rules.entry(rule).or_default() += 1;
        }
        if row.relation == Relation::Same
            && let Some(d) = row.distance_m
        {
            let bin = DISTANCE_BINS_M.partition_point(|&upper| upper <= d);
            self.distance_m[bin] += 1;
        }
        if self.tags.is_empty() {
            self.tags = vec![[0; 4]; compared_tags().count()];
        }
        for (i, state) in row.tags {
            let t = &mut self.tags[i];
            t[0] += 1;
            match state {
                TagState::Agreeing => t[1] += 1,
                TagState::Differing => t[2] += 1,
                TagState::MissingInOsm => t[3] += 1,
            }
        }
        if let Some(oh) = row.opening_hours {
            *self.opening_hours.entry(oh).or_default() += 1;
        }
        if let Some(r) = row.osm_ref {
            *self.osm_refs.entry(r).or_default() += 1;
        }
        if let Some(offset) = row.offset {
            self.offsets.push(offset);
        }
        if let Some(p) = row.position {
            *self.positions.entry(p).or_default() += 1;
        }
        let ids = if row.wikidata.is_empty() {
            vec![0]
        } else {
            row.wikidata
        };
        for id in ids {
            let brand = self.brands.entry((row.country, id)).or_default();
            brand.atp_features += 1;
            match row.relation {
                Relation::Same => brand.same += 1,
                Relation::LocatedIn => brand.located_in += 1,
                Relation::MissingInOsm => brand.missing_in_osm += 1,
                Relation::DeletionCandidate => {}
            }
            if let Some(c) = &row.category
                && !brand.categories.contains(c)
            {
                brand.categories.insert(c.clone());
            }
            if let Some(offset) = row.offset {
                brand.offsets.push(offset);
            }
        }
    }

    /// ATP features whose OSM feature also matched another feature of
    /// this spider.
    fn shared_osm_features(&self) -> u64 {
        self.osm_refs.values().filter(|&&n| n > 1).sum()
    }

    /// ATP features at the same position as another feature of this
    /// spider.
    fn shared_positions(&self) -> u64 {
        self.positions.values().filter(|&&n| n > 1).sum()
    }

    /// The share of ATP features with `opening_hours` that have the
    /// most common value, or `None` if none have any.
    fn opening_hours_top_share(&self) -> Option<f32> {
        let total: u64 = self.opening_hours.values().sum();
        let top = self.opening_hours.values().max()?;
        Some(*top as f32 / total as f32)
    }
}

/// The median of the east and north components of `offsets`, each
/// on its own, or `None` if there are none.
fn median_offset(offsets: &[(f32, f32)]) -> Option<(f32, f32)> {
    if offsets.is_empty() {
        return None;
    }
    let median = |mut values: Vec<f32>| {
        let mid = values.len() / 2;
        let (_, m, _) = values.select_nth_unstable_by(mid, f32::total_cmp);
        *m
    };
    Some((
        median(offsets.iter().map(|o| o.0).collect()),
        median(offsets.iter().map(|o| o.1).collect()),
    ))
}

#[derive(Debug, Default)]
struct Scanned {
    rows: u64,
    spiders: HashMap<String, Spider>,
    /// Deletion candidates by country and Wikidata ID.
    deletions: HashMap<(Option<Country>, u64), u64>,
}

impl Scanned {
    fn add(&mut self, row: RowInfo) {
        self.rows += 1;
        if row.relation == Relation::DeletionCandidate {
            for &id in &row.wikidata {
                *self.deletions.entry((row.country, id)).or_default() += 1;
            }
            return;
        }
        let Some(spider) = &row.spider else { return };
        if !self.spiders.contains_key(spider) {
            self.spiders.insert(spider.clone(), Spider::default());
        }
        self.spiders.get_mut(spider).expect("inserted").add(row);
    }
}

fn scan_conflated(conflated: &Path, progress: &MultiProgress) -> Result<Scanned> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(conflated)?)?;
    let num_rows = builder.metadata().file_metadata().num_rows() as u64;
    let bar = make_progress_bar(progress, "spider_stats", num_rows, "conflated rows");
    let mut scanned = Scanned::default();
    for batch in builder.build()? {
        let batch = batch?;
        let rows: Vec<RowInfo> = (0..batch.num_rows())
            .into_par_iter()
            .map(|i| row_info(&batch, i))
            .collect::<Result<_>>()?;
        bar.inc(rows.len() as u64);
        for row in rows {
            scanned.add(row);
        }
    }
    bar.finish();
    Ok(scanned)
}

fn row_info(batch: &RecordBatch, row: usize) -> Result<RowInfo> {
    let m = get_struct(batch, "match")?;
    let relation = match get_string(m, "relation", row)?.as_str() {
        "same" => Relation::Same,
        "located_in" => Relation::LocatedIn,
        "missing_in_osm" => Relation::MissingInOsm,
        "deletion_candidate" => Relation::DeletionCandidate,
        // A relation added later; see CONFLATED_PARQUET.md.
        _ => return Ok(RowInfo::default()),
    };
    let evidence = get_struct(m, "evidence")?;
    let rule = string_or_null(evidence, "rule", row)?;
    let distance_m = f32_or_null(evidence, "distance_m", row)?;
    let agreeing = string_list(evidence, "agreeing", row)?;

    let atp = get_struct(batch, "atp")?;
    let osm = get_struct(batch, "osm")?;
    let osm_tags = if osm.is_null(row) {
        Vec::new()
    } else {
        get_tags(osm, "tags", row)?
    };
    let osm_tag_refs = || osm_tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));

    if relation == Relation::DeletionCandidate {
        let geometry = get_binary(batch, "osm_geometry", row)?;
        return Ok(RowInfo {
            relation,
            country: wkb_country(&geometry),
            wikidata: wikidata_ids(osm_tag_refs()),
            ..Default::default()
        });
    }
    if atp.is_null(row) {
        return Ok(RowInfo::default());
    }
    let atp_tags = get_tags(atp, "tags", row)?;
    let atp_tag_refs = || atp_tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
    let spider = get_string(get_struct(atp, "fetched")?, "spider", row)?;
    let geometry = get_binary(batch, "atp_geometry", row)?;
    let position = {
        let mut hasher = std::hash::DefaultHasher::new();
        geometry.hash(&mut hasher);
        hasher.finish()
    };

    let mut tags = Vec::new();
    if relation == Relation::Same {
        for (i, name) in compared_tags().enumerate() {
            if !has_compared_tag(name, atp_tag_refs()) {
                continue;
            }
            let state = if agreeing.iter().any(|a| a == name) {
                TagState::Agreeing
            } else if has_compared_tag(name, osm_tag_refs()) {
                TagState::Differing
            } else {
                TagState::MissingInOsm
            };
            tags.push((i, state));
        }
    }
    let (osm_ref, offset) = if relation == Relation::Same {
        let kind = match get_string(osm, "type", row)?.as_str() {
            "node" => 1,
            "way" => 2,
            _ => 3,
        };
        let osm_geometry = get_binary(batch, "osm_geometry", row)?;
        (
            Some(get_u64(osm, "id", row)? * 4 + kind),
            wkb_offset(&geometry, &osm_geometry),
        )
    } else {
        (None, None)
    };
    Ok(RowInfo {
        spider: Some(spider),
        relation,
        rule,
        distance_m,
        country: wkb_country(&geometry),
        wikidata: wikidata_ids(atp_tag_refs()),
        category: main_category(atp_tag_refs()),
        tags,
        opening_hours: atp_tag_refs()
            .find(|(k, _)| *k == "opening_hours")
            .map(|(_, v)| v.to_string()),
        osm_ref,
        offset,
        position: Some(position),
    })
}

/// How far the centroid of `atp` lies east and north of the centroid
/// of `osm`, in meters.
fn wkb_offset(atp: &[u8], osm: &[u8]) -> Option<(f32, f32)> {
    let a = read_wkb(atp).ok()?.to_geometry().centroid()?;
    let o = read_wkb(osm).ok()?.to_geometry().centroid()?;
    const METERS_PER_DEGREE: f64 = 111_320.0;
    let east = (a.x() - o.x()) * METERS_PER_DEGREE * o.y().to_radians().cos();
    let north = (a.y() - o.y()) * METERS_PER_DEGREE;
    Some((east as f32, north as f32))
}

/// The distinct brand, operator and network Wikidata IDs in `tags`.
fn wikidata_ids<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<u64> {
    let mut ids: Vec<u64> = tags
        .filter(|(k, _)| WIKIDATA_KEYS.contains(k))
        .flat_map(|(_, v)| parse_wikidata_ids(v))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The country at the centroid of a WKB geometry.
fn wkb_country(wkb: &[u8]) -> Option<Country> {
    let c = read_wkb(wkb).ok()?.to_geometry().centroid()?;
    country(c.x(), c.y())
}

fn string_or_null(
    src: &arrow::array::StructArray,
    name: &str,
    row: usize,
) -> Result<Option<String>> {
    let col = src
        .column_by_name(name)
        .with_context(|| format!("missing column '{name}'"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("column '{name}' is not a string"))?;
    Ok((!col.is_null(row)).then(|| col.value(row).to_string()))
}

fn f32_or_null(src: &arrow::array::StructArray, name: &str, row: usize) -> Result<Option<f32>> {
    let col = src
        .column_by_name(name)
        .with_context(|| format!("missing column '{name}'"))?
        .as_any()
        .downcast_ref::<Float32Array>()
        .with_context(|| format!("column '{name}' is not Float32"))?;
    Ok((!col.is_null(row)).then(|| col.value(row)))
}

fn string_list(src: &arrow::array::StructArray, name: &str, row: usize) -> Result<Vec<String>> {
    let col = src
        .column_by_name(name)
        .with_context(|| format!("missing column '{name}'"))?
        .as_any()
        .downcast_ref::<ListArray>()
        .with_context(|| format!("column '{name}' is not a list"))?;
    let values = col.value(row);
    let values = values
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("column '{name}' is not a list of strings"))?;
    Ok((0..values.len())
        .map(|i| values.value(i).to_string())
        .collect())
}

/// One spider's row, from everything we know about it.
struct Row<'a> {
    spider: &'a str,
    counts: Option<&'a spider_counts::SpiderCounts>,
    run_stats: Option<&'a run_stats::SpiderRunStats>,
    diagnostics: Option<&'a SpiderDiagnostics>,
    stats: &'a Spider,
}

/// What a spider's brands are compared with.
struct Lookups<'a> {
    osm_brands: &'a brand_totals::OsmFeatureCounts,
    names: &'a HashMap<u64, String>,
    deletions: &'a HashMap<(Option<Country>, u64), u64>,
}

fn u64_field(name: &str) -> Field {
    Field::new(name, DataType::UInt64, false)
}

fn brand_fields() -> Fields {
    Fields::from(vec![
        Field::new("country", DataType::Utf8, true),
        Field::new("wikidata_id", DataType::Utf8, true),
        Field::new("name", DataType::Utf8, true),
        u64_field("atp_features"),
        u64_field("same"),
        u64_field("located_in"),
        u64_field("missing_in_osm"),
        Field::new("osm_features", DataType::UInt64, true),
        Field::new("deletion_candidates", DataType::UInt64, true),
        Field::new("offset_east_m", DataType::Float32, true),
        Field::new("offset_north_m", DataType::Float32, true),
    ])
}

fn tag_fields() -> Fields {
    Fields::from(vec![
        Field::new("tag", DataType::Utf8, false),
        u64_field("atp"),
        u64_field("agreeing"),
        u64_field("differing"),
        u64_field("missing_in_osm"),
    ])
}

fn rule_fields() -> Fields {
    Fields::from(vec![
        Field::new("rule", DataType::Utf8, false),
        u64_field("rows"),
    ])
}

fn histogram_field(name: &str) -> Field {
    Field::new(
        name,
        DataType::List(Arc::new(Field::new("item", DataType::UInt64, false))),
        false,
    )
}

fn list_of(fields: Fields) -> DataType {
    DataType::List(Arc::new(Field::new(
        "item",
        DataType::Struct(fields),
        false,
    )))
}

pub(super) fn schema() -> Schema {
    Schema::new(vec![
        Field::new("spider", DataType::Utf8, false),
        Field::new("atp_features", DataType::UInt64, true),
        Field::new("atp_errors", DataType::UInt64, true),
        Field::new("dump_features", DataType::UInt64, true),
        Field::new("not_for_osm", DataType::UInt64, true),
        Field::new("invalid", DataType::UInt64, true),
        Field::new("no_matcher", DataType::UInt64, true),
        Field::new("in_china", DataType::UInt64, true),
        Field::new("datum", DataType::Utf8, true),
        u64_field("rows"),
        u64_field("same"),
        u64_field("located_in"),
        u64_field("missing_in_osm"),
        u64_field("shared_osm_features"),
        u64_field("shared_positions"),
        Field::new("offset_east_m", DataType::Float32, true),
        Field::new("offset_north_m", DataType::Float32, true),
        histogram_field("distance_m"),
        u64_field("missing_no_candidate"),
        u64_field("missing_rejected"),
        u64_field("missing_taken"),
        histogram_field("rejected_distance_m"),
        u64_field("chance_sampled"),
        u64_field("chance_matched"),
        Field::new("opening_hours_top_share", DataType::Float32, true),
        Field::new("rules", list_of(rule_fields()), false),
        Field::new("tags", list_of(tag_fields()), false),
        Field::new("brands", list_of(brand_fields()), false),
    ])
}

fn write(rows: &[Row], lookups: &Lookups, out: &Path) -> Result<()> {
    let schema = Arc::new(schema());
    let bins = DISTANCE_BINS_M
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(9)?))
        .set_max_row_group_row_count(Some(ROWS_PER_GROUP))
        .set_key_value_metadata(Some(vec![KeyValue::new(
            "osmdiffs:distance_bins_m".to_string(),
            bins,
        )]))
        .build();
    let mut tmp = out.to_path_buf();
    tmp.add_extension("tmp");
    let mut writer = ArrowWriter::try_new(File::create(&tmp)?, schema.clone(), Some(properties))?;
    for chunk in rows.chunks(ROWS_PER_GROUP) {
        writer.write(&batch(chunk, lookups, &schema)?)?;
        writer.flush()?;
    }
    writer.close()?;
    std::fs::rename(&tmp, out)?;
    Ok(())
}

fn batch(rows: &[Row], lookups: &Lookups, schema: &Arc<Schema>) -> Result<RecordBatch> {
    let tag_names: Vec<&str> = compared_tags().collect();
    let mut spider = StringBuilder::new();
    let mut atp_features = UInt64Builder::new();
    let mut atp_errors = UInt64Builder::new();
    let mut dump_features = UInt64Builder::new();
    let mut not_for_osm = UInt64Builder::new();
    let mut invalid = UInt64Builder::new();
    let mut no_matcher = UInt64Builder::new();
    let mut in_china = UInt64Builder::new();
    let mut datum = StringBuilder::new();
    let mut num_rows = UInt64Builder::new();
    let mut same = UInt64Builder::new();
    let mut located_in = UInt64Builder::new();
    let mut missing_in_osm = UInt64Builder::new();
    let mut shared = UInt64Builder::new();
    let mut shared_positions = UInt64Builder::new();
    let mut offset_east = Float32Builder::new();
    let mut offset_north = Float32Builder::new();
    let histogram = || {
        ListBuilder::new(UInt64Builder::new()).with_field(Arc::new(Field::new(
            "item",
            DataType::UInt64,
            false,
        )))
    };
    let mut distance = histogram();
    let mut no_candidate = UInt64Builder::new();
    let mut rejected = UInt64Builder::new();
    let mut taken = UInt64Builder::new();
    let mut rejected_distance = histogram();
    let mut chance_sampled = UInt64Builder::new();
    let mut chance_matched = UInt64Builder::new();
    let mut oh_share = Float32Builder::new();
    let struct_list = |fields: Fields| {
        ListBuilder::new(StructBuilder::from_fields(fields.clone(), 0)).with_field(Arc::new(
            Field::new("item", DataType::Struct(fields), false),
        ))
    };
    let mut rules = struct_list(rule_fields());
    let mut tags = struct_list(tag_fields());
    let mut brands = struct_list(brand_fields());

    for row in rows {
        let s = row.stats;
        spider.append_value(row.spider);
        atp_features.append_option(row.run_stats.map(|r| r.features));
        atp_errors.append_option(row.run_stats.map(|r| r.errors));
        dump_features.append_option(row.counts.map(|c| c.features));
        not_for_osm.append_option(row.counts.map(|c| c.not_for_osm));
        invalid.append_option(row.counts.map(|c| c.invalid));
        // Every feature that a matcher handles gets exactly one row.
        no_matcher.append_option(row.counts.map(|c| c.written().saturating_sub(s.rows)));
        in_china.append_option(row.counts.map(|c| c.in_china));
        datum.append_option(
            row.counts
                .map(|c| c.datum.as_str())
                .filter(|d| !d.is_empty()),
        );
        num_rows.append_value(s.rows);
        same.append_value(s.same);
        located_in.append_value(s.located_in);
        missing_in_osm.append_value(s.missing_in_osm);
        shared.append_value(s.shared_osm_features());
        shared_positions.append_value(s.shared_positions());
        let offset = median_offset(&s.offsets);
        offset_east.append_option(offset.map(|o| o.0));
        offset_north.append_option(offset.map(|o| o.1));
        distance.values().append_slice(&s.distance_m);
        distance.append(true);
        let d = row
            .diagnostics
            .cloned()
            .unwrap_or_else(|| SpiderDiagnostics {
                rejected_distance_m: vec![0; DISTANCE_BINS_M.len() + 1],
                ..Default::default()
            });
        no_candidate.append_value(d.no_candidate);
        rejected.append_value(d.rejected);
        taken.append_value(d.taken);
        rejected_distance
            .values()
            .append_slice(&d.rejected_distance_m);
        rejected_distance.append(true);
        chance_sampled.append_value(d.chance_sampled);
        chance_matched.append_value(d.chance_matched);
        oh_share.append_option(s.opening_hours_top_share());

        let mut rule_counts: Vec<(&String, &u64)> = s.rules.iter().collect();
        rule_counts.sort();
        let b = rules.values();
        for (rule, n) in rule_counts {
            b.field_builder::<StringBuilder>(0)
                .unwrap()
                .append_value(rule);
            b.field_builder::<UInt64Builder>(1)
                .unwrap()
                .append_value(*n);
            b.append(true);
        }
        rules.append(true);

        let b = tags.values();
        for (i, t) in s.tags.iter().enumerate() {
            if t[0] == 0 {
                continue;
            }
            b.field_builder::<StringBuilder>(0)
                .unwrap()
                .append_value(tag_names[i]);
            for (j, n) in t.iter().enumerate() {
                b.field_builder::<UInt64Builder>(j + 1)
                    .unwrap()
                    .append_value(*n);
            }
            b.append(true);
        }
        tags.append(true);

        let mut keys: Vec<&(Option<Country>, u64)> = s.brands.keys().collect();
        keys.sort_by_key(|(c, id)| (c.map(|c| c.code_bytes()), *id));
        let b = brands.values();
        for key in keys {
            let (country, id) = *key;
            let brand = &s.brands[key];
            b.field_builder::<StringBuilder>(0)
                .unwrap()
                .append_option(country.map(|c| c.code().to_string()));
            b.field_builder::<StringBuilder>(1)
                .unwrap()
                .append_option((id != 0).then(|| format!("Q{id}")));
            b.field_builder::<StringBuilder>(2)
                .unwrap()
                .append_option(lookups.names.get(&id));
            let counts = [
                brand.atp_features,
                brand.same,
                brand.located_in,
                brand.missing_in_osm,
            ];
            for (j, n) in counts.iter().enumerate() {
                b.field_builder::<UInt64Builder>(3 + j)
                    .unwrap()
                    .append_value(*n);
            }
            // Only for a Wikidata ID: features without one have nothing
            // to compare with.
            let osm = (id != 0).then(|| {
                brand
                    .categories
                    .iter()
                    .map(|c| {
                        lookups
                            .osm_brands
                            .get(&(id, country, c.clone()))
                            .copied()
                            .unwrap_or(0)
                    })
                    .sum::<u64>()
            });
            b.field_builder::<UInt64Builder>(7)
                .unwrap()
                .append_option(osm);
            let deletions =
                (id != 0).then(|| lookups.deletions.get(&(country, id)).copied().unwrap_or(0));
            b.field_builder::<UInt64Builder>(8)
                .unwrap()
                .append_option(deletions);
            let offset = median_offset(&brand.offsets);
            b.field_builder::<Float32Builder>(9)
                .unwrap()
                .append_option(offset.map(|o| o.0));
            b.field_builder::<Float32Builder>(10)
                .unwrap()
                .append_option(offset.map(|o| o.1));
            b.append(true);
        }
        brands.append(true);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(spider.finish()),
        Arc::new(atp_features.finish()),
        Arc::new(atp_errors.finish()),
        Arc::new(dump_features.finish()),
        Arc::new(not_for_osm.finish()),
        Arc::new(invalid.finish()),
        Arc::new(no_matcher.finish()),
        Arc::new(in_china.finish()),
        Arc::new(datum.finish()),
        Arc::new(num_rows.finish()),
        Arc::new(same.finish()),
        Arc::new(located_in.finish()),
        Arc::new(missing_in_osm.finish()),
        Arc::new(shared.finish()),
        Arc::new(shared_positions.finish()),
        Arc::new(offset_east.finish()),
        Arc::new(offset_north.finish()),
        Arc::new(distance.finish()),
        Arc::new(no_candidate.finish()),
        Arc::new(rejected.finish()),
        Arc::new(taken.finish()),
        Arc::new(rejected_distance.finish()),
        Arc::new(chance_sampled.finish()),
        Arc::new(chance_matched.finish()),
        Arc::new(oh_share.finish()),
        Arc::new(rules.finish()),
        Arc::new(tags.finish()),
        Arc::new(brands.finish()),
    ];
    Ok(RecordBatch::try_new(schema.clone(), columns)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{StructArray, UInt64Array};

    fn same(spider: &str, distance_m: f32, osm_ref: u64) -> RowInfo {
        RowInfo {
            spider: Some(spider.into()),
            relation: Relation::Same,
            rule: Some("brand_wikidata".into()),
            distance_m: Some(distance_m),
            country: Country::from_code(*b"CH"),
            wikidata: vec![379911],
            category: Some("shop=supermarket".into()),
            tags: vec![(0, TagState::Agreeing), (1, TagState::Differing)],
            opening_hours: Some("Mo-Sa 08:00-20:00".into()),
            osm_ref: Some(osm_ref),
            offset: Some((distance_m, -1.0)),
            position: Some(osm_ref),
        }
    }

    #[test]
    fn test_spider_add() {
        let mut s = Spider::default();
        // A distance on a bin's upper bound goes into the next bin.
        for (d, osm) in [(0.0, 1), (1.0, 2), (24.9, 3), (5000.0, 3)] {
            s.add(same("x", d, osm));
        }
        s.add(RowInfo {
            spider: Some("x".into()),
            relation: Relation::MissingInOsm,
            opening_hours: Some("24/7".into()),
            ..Default::default()
        });
        assert_eq!((s.rows, s.same, s.missing_in_osm), (5, 4, 1));
        assert_eq!(s.distance_m, [1, 1, 1, 0, 0, 0, 0, 1]);
        assert_eq!(s.rules["brand_wikidata"], 4);
        assert_eq!(s.tags[0], [4, 4, 0, 0]);
        assert_eq!(s.tags[1], [4, 0, 4, 0]);
        assert_eq!(s.shared_osm_features(), 2);
        assert_eq!(s.shared_positions(), 2);
        assert_eq!(median_offset(&s.offsets), Some((24.9, -1.0)));
        assert_eq!(median_offset(&[]), None);
        assert_eq!(s.opening_hours_top_share(), Some(0.8));
        let ch = Country::from_code(*b"CH");
        assert_eq!(s.brands[&(ch, 379911)].same, 4);
        assert_eq!(s.brands[&(None, 0)].missing_in_osm, 1);
        assert_eq!(Spider::default().opening_hours_top_share(), None);
    }

    #[test]
    fn test_scanned_add() {
        let mut scanned = Scanned::default();
        scanned.add(same("a", 3.0, 1));
        scanned.add(RowInfo {
            relation: Relation::DeletionCandidate,
            country: Country::from_code(*b"CH"),
            wikidata: vec![1, 2],
            ..Default::default()
        });
        assert_eq!(scanned.rows, 2);
        assert_eq!(scanned.spiders["a"].rows, 1);
        assert_eq!(scanned.deletions.len(), 2);
    }

    #[test]
    fn test_write() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let out = dir.path().join(FILENAME);
        let ch = Country::from_code(*b"CH");
        let mut stats = Spider::default();
        stats.add(same("denner", 3.0, 1));
        let counts = spider_counts::SpiderCounts {
            spider: "denner".into(),
            features: 3,
            invalid: 1,
            ..Default::default()
        };
        let empty = Spider::default();
        let rows = [
            Row {
                spider: "denner",
                counts: Some(&counts),
                run_stats: None,
                diagnostics: None,
                stats: &stats,
            },
            Row {
                spider: "unknown",
                counts: None,
                run_stats: None,
                diagnostics: None,
                stats: &empty,
            },
        ];
        let osm_brands = HashMap::from([
            ((379911, ch, "shop=supermarket".to_string()), 7),
            ((379911, ch, "amenity=cafe".to_string()), 100),
        ]);
        let names = HashMap::from([(379911, "Denner".to_string())]);
        let deletions = HashMap::from([((ch, 379911), 2)]);
        let lookups = Lookups {
            osm_brands: &osm_brands,
            names: &names,
            deletions: &deletions,
        };
        write(&rows, &lookups, &out)?;

        let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&out)?)?.build()?;
        let batch = reader.into_iter().next().context("no batch")??;
        assert_eq!(batch.schema().as_ref(), &schema());
        assert_eq!(batch.num_rows(), 2);
        let u64s = |name: &str| {
            batch
                .column_by_name(name)
                .unwrap()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .clone()
        };
        // 3 features, 1 invalid, 1 conflated row: 1 without a matcher.
        assert_eq!(u64s("no_matcher").value(0), 1);
        assert!(u64s("no_matcher").is_null(1));
        assert!(u64s("atp_features").is_null(0));
        assert_eq!(u64s("missing_taken").value(1), 0);

        let brands = batch.column_by_name("brands").unwrap();
        let brands = brands.as_any().downcast_ref::<ListArray>().unwrap();
        let brand = brands.value(0);
        let brand = brand.as_any().downcast_ref::<StructArray>().unwrap();
        let field = |name: &str| {
            brand
                .column_by_name(name)
                .unwrap()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .value(0)
        };
        // Only the supermarkets count, since the spider has no cafes.
        assert_eq!(field("osm_features"), 7);
        assert_eq!(field("deletion_candidates"), 2);
        assert_eq!(brands.value(1).len(), 0);
        Ok(())
    }
}
