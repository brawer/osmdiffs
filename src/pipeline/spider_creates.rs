//! What the edits step did with each spider's candidates for new
//! places, for the published `spider-stats.parquet` and for whoever
//! reviews a weekly run; see `docs/WEEKLY_REVIEW.md`.
//!
//! `spider-stats.parquet` is computed before the edits step, which
//! reads it to decide which spiders may propose creates. So the edits
//! step writes its counts to `edits-spiders.json` next to
//! `edits.parquet`, and [publishable] adds them to a copy of
//! `spider-stats.parquet`, as the column `creates`, which is the file
//! that gets published. The local `spider-stats.parquet` stays as the
//! edits step read it.
//!
//! [review] then logs warnings about spiders whose data likely has a
//! bug: brand IDs that OSM doesn't know in the spider's countries,
//! sudden jumps in creates, and spiders that list places twice. Like
//! `spider_churn`, it never fails the pipeline.

use super::edits::create::{CreateStats, Skip, SpiderCreates};
use anyhow::{Context, Result, anyhow};
use arrow::array::{
    Array, ArrayRef, BooleanArray, BooleanBuilder, ListArray, ListBuilder, RecordBatch,
    StringArray, StringBuilder, StructArray, StructBuilder, UInt64Array, UInt64Builder,
};
use arrow::datatypes::{DataType, Field, Fields, Schema};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where the edits step leaves its per-spider counts.
pub fn sidecar_path(workdir: &Path) -> PathBuf {
    workdir.join("edits-spiders.json")
}

/// Where [publishable] writes the file that gets published as
/// `spider-stats.parquet`.
pub fn published_path(workdir: &Path) -> PathBuf {
    workdir.join("spider-stats.published.parquet")
}

/// Writes `stats` to `path`, for [publishable].
pub(crate) fn write_sidecar(stats: &CreateStats, path: &Path) -> Result<()> {
    let mut spiders = Map::new();
    for (spider, s) in &stats.spiders {
        let skipped: Map<String, Value> = s
            .skipped
            .iter()
            .map(|(skip, n)| (skip.as_str().to_string(), json!(n)))
            .collect();
        spiders.insert(
            spider.clone(),
            json!({
                "candidates": s.candidates,
                "creates": s.creates,
                "merged": s.merged,
                "similar_name_30m": s.similar_name_30m,
                "own_names": s.own_names,
                "skipped": skipped,
            }),
        );
    }
    let mut tmp = path.to_path_buf();
    tmp.add_extension("tmp");
    serde_json::to_writer(File::create(&tmp)?, &json!({ "spiders": spiders }))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_sidecar(path: &Path) -> Result<BTreeMap<String, SpiderCreates>> {
    let file = File::open(path).with_context(|| format!("could not open {path:?}"))?;
    let value: Value =
        serde_json::from_reader(file).with_context(|| format!("could not read {path:?}"))?;
    let spiders = value
        .get("spiders")
        .and_then(Value::as_object)
        .with_context(|| format!("no \"spiders\" in {path:?}"))?;
    let by_name: HashMap<&str, Skip> = Skip::ALL.iter().map(|s| (s.as_str(), *s)).collect();
    let mut out = BTreeMap::new();
    for (spider, v) in spiders {
        let n = |key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
        let mut skipped = BTreeMap::new();
        if let Some(map) = v.get("skipped").and_then(Value::as_object) {
            for (reason, count) in map {
                let skip = by_name
                    .get(reason.as_str())
                    .ok_or_else(|| anyhow!("unknown skip reason {reason:?} in {path:?}"))?;
                skipped.insert(*skip, count.as_u64().unwrap_or(0));
            }
        }
        out.insert(
            spider.clone(),
            SpiderCreates {
                candidates: n("candidates"),
                creates: n("creates"),
                merged: n("merged"),
                skipped,
                similar_name_30m: n("similar_name_30m"),
                own_names: v.get("own_names").and_then(Value::as_bool).unwrap_or(false),
            },
        );
    }
    Ok(out)
}

fn skipped_fields() -> Fields {
    Fields::from(vec![
        Field::new("reason", DataType::Utf8, false),
        Field::new("rows", DataType::UInt64, false),
    ])
}

/// The fields of the column `creates`.
fn creates_fields() -> Fields {
    Fields::from(vec![
        Field::new("candidates", DataType::UInt64, false),
        Field::new("creates", DataType::UInt64, false),
        Field::new("merged", DataType::UInt64, false),
        Field::new("similar_name_30m", DataType::UInt64, false),
        Field::new("own_names", DataType::Boolean, false),
        Field::new(
            "skipped",
            DataType::List(Arc::new(Field::new(
                "item",
                DataType::Struct(skipped_fields()),
                false,
            ))),
            false,
        ),
    ])
}

fn creates_field() -> Field {
    Field::new("creates", DataType::Struct(creates_fields()), true)
}

fn creates_column(spiders: &StringArray, counts: &BTreeMap<String, SpiderCreates>) -> ArrayRef {
    let skipped = ListBuilder::new(StructBuilder::from_fields(skipped_fields(), 0)).with_field(
        Field::new("item", DataType::Struct(skipped_fields()), false),
    );
    let mut b = StructBuilder::new(
        creates_fields(),
        vec![
            Box::new(UInt64Builder::new()),
            Box::new(UInt64Builder::new()),
            Box::new(UInt64Builder::new()),
            Box::new(UInt64Builder::new()),
            Box::new(BooleanBuilder::new()),
            Box::new(skipped),
        ],
    );
    for i in 0..spiders.len() {
        let c = counts.get(spiders.value(i));
        let d = SpiderCreates::default();
        let s = c.unwrap_or(&d);
        for (field, value) in [s.candidates, s.creates, s.merged, s.similar_name_30m]
            .into_iter()
            .enumerate()
        {
            b.field_builder::<UInt64Builder>(field)
                .expect("u64 field")
                .append_value(value);
        }
        b.field_builder::<BooleanBuilder>(4)
            .expect("own_names")
            .append_value(s.own_names);
        let list = b
            .field_builder::<ListBuilder<StructBuilder>>(5)
            .expect("skipped");
        for skip in Skip::ALL {
            let Some(&n) = s.skipped.get(&skip).filter(|n| **n > 0) else {
                continue;
            };
            let entry = list.values();
            entry
                .field_builder::<StringBuilder>(0)
                .expect("reason")
                .append_value(skip.as_str());
            entry
                .field_builder::<UInt64Builder>(1)
                .expect("rows")
                .append_value(n);
            entry.append(true);
        }
        list.append(true);
        b.append(c.is_some());
    }
    Arc::new(b.finish())
}

/// Writes [published_path]: `spider_stats` with the column `creates`
/// from `sidecar`, null for spiders without candidates.
pub fn publishable(spider_stats: &Path, sidecar: &Path, workdir: &Path) -> Result<PathBuf> {
    let out = published_path(workdir);
    if super::is_current(&[&out], &[spider_stats, sidecar])? {
        return Ok(out);
    }
    let counts = read_sidecar(sidecar)?;
    let file =
        File::open(spider_stats).with_context(|| format!("could not open {spider_stats:?}"))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let metadata = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned();
    let mut fields: Vec<Field> = builder
        .schema()
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    fields.push(creates_field());
    let schema = Arc::new(Schema::new(fields));
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(9)?))
        .set_key_value_metadata(metadata)
        .build();
    let mut tmp = out.clone();
    tmp.add_extension("tmp");
    let mut writer = ArrowWriter::try_new(File::create(&tmp)?, schema.clone(), Some(properties))?;
    let mut spiders_with_counts = 0;
    for batch in builder.build()? {
        let batch = batch?;
        let spiders = strings(&batch, "spider")?;
        spiders_with_counts += (0..spiders.len())
            .filter(|&i| counts.contains_key(spiders.value(i)))
            .count();
        let mut columns = batch.columns().to_vec();
        columns.push(creates_column(spiders, &counts));
        writer.write(&RecordBatch::try_new(schema.clone(), columns)?)?;
    }
    writer.close()?;
    std::fs::rename(&tmp, &out)?;
    log::info!(
        spiders_with_candidates = spiders_with_counts;
        "spider_creates: added create counts to spider-stats.parquet"
    );
    Ok(out)
}

/// A brand ID whose features lie mostly in countries where OSM has no
/// feature with that ID, while it has many elsewhere: most likely the
/// spider gives the ID of another country's company, as
/// `standard_chartered_bank` gives Standard Chartered Taiwan's.
#[derive(Debug, PartialEq)]
struct ForeignBrand {
    spider: String,
    wikidata: String,
    name: String,
    /// The spider's features with this ID in countries where OSM has
    /// none with it, and those countries, most features first.
    atp_features: u64,
    countries: Vec<String>,
    /// OSM features with this ID in the other countries.
    osm_features: u64,
}

/// How many features of a spider's brand, and of OSM with that brand
/// ID elsewhere, make a [ForeignBrand].
const MIN_FOREIGN_FEATURES: u64 = 20;

/// One `brands` entry of `spider-stats.parquet`.
struct BrandRow {
    country: Option<String>,
    wikidata: String,
    name: String,
    atp_features: u64,
    osm_features: u64,
}

fn foreign_brands(brands: &BTreeMap<String, Vec<BrandRow>>) -> Vec<ForeignBrand> {
    let mut found = Vec::new();
    for (spider, rows) in brands {
        let mut by_id: BTreeMap<&str, Vec<&BrandRow>> = BTreeMap::new();
        for r in rows {
            by_id.entry(r.wikidata.as_str()).or_default().push(r);
        }
        for (wikidata, rows) in by_id {
            let all: u64 = rows.iter().map(|r| r.atp_features).sum();
            let osm: u64 = rows.iter().map(|r| r.osm_features).sum();
            let mut without: Vec<&&BrandRow> = rows
                .iter()
                .filter(|r| r.country.is_some() && r.osm_features == 0)
                .collect();
            without.sort_by_key(|b| std::cmp::Reverse(b.atp_features));
            let atp_without: u64 = without.iter().map(|r| r.atp_features).sum();
            if osm >= MIN_FOREIGN_FEATURES
                && atp_without >= MIN_FOREIGN_FEATURES
                && 2 * atp_without > all
            {
                found.push(ForeignBrand {
                    spider: spider.clone(),
                    wikidata: wikidata.to_string(),
                    name: rows[0].name.clone(),
                    atp_features: atp_without,
                    countries: without.iter().filter_map(|r| r.country.clone()).collect(),
                    osm_features: osm,
                });
            }
        }
    }
    found
}

/// What [review] reads of one spider.
#[derive(Default)]
struct Week {
    creates: Option<SpiderCreates>,
    brands: Vec<BrandRow>,
}

fn read(path: &Path) -> Result<BTreeMap<String, Week>> {
    let file = File::open(path).with_context(|| format!("could not open {path:?}"))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let by_name: HashMap<&str, Skip> = Skip::ALL.iter().map(|s| (s.as_str(), *s)).collect();
    let mut weeks = BTreeMap::new();
    for batch in reader {
        let batch = batch?;
        let spiders = strings(&batch, "spider")?;
        let brands = batch
            .column_by_name("brands")
            .and_then(|c| c.as_any().downcast_ref::<ListArray>())
            .context("no list column \"brands\"")?;
        let creates = batch
            .column_by_name("creates")
            .and_then(|c| c.as_any().downcast_ref::<StructArray>());
        for i in 0..batch.num_rows() {
            let mut week = Week::default();
            let entries = brands.value(i);
            let entries = entries
                .as_any()
                .downcast_ref::<StructArray>()
                .context("brands items are not structs")?;
            let country = struct_strings(entries, "country")?;
            let wikidata = struct_strings(entries, "wikidata_id")?;
            let name = struct_strings(entries, "name")?;
            let atp = struct_numbers(entries, "atp_features")?;
            let osm = struct_numbers(entries, "osm_features")?;
            for j in 0..entries.len() {
                if !wikidata.is_valid(j) {
                    continue;
                }
                week.brands.push(BrandRow {
                    country: country.is_valid(j).then(|| country.value(j).to_string()),
                    wikidata: wikidata.value(j).to_string(),
                    name: if name.is_valid(j) {
                        name.value(j).to_string()
                    } else {
                        String::new()
                    },
                    atp_features: atp.value(j),
                    osm_features: if osm.is_valid(j) { osm.value(j) } else { 0 },
                });
            }
            if let Some(c) = creates
                && c.is_valid(i)
            {
                let n = |name: &str| -> Result<u64> { Ok(struct_numbers(c, name)?.value(i)) };
                let own = c
                    .column_by_name("own_names")
                    .and_then(|a| a.as_any().downcast_ref::<BooleanArray>())
                    .context("no own_names")?;
                let list = c
                    .column_by_name("skipped")
                    .and_then(|a| a.as_any().downcast_ref::<ListArray>())
                    .context("no skipped")?;
                let entries = list.value(i);
                let entries = entries
                    .as_any()
                    .downcast_ref::<StructArray>()
                    .context("skipped items are not structs")?;
                let reasons = struct_strings(entries, "reason")?;
                let rows = struct_numbers(entries, "rows")?;
                let mut skipped = BTreeMap::new();
                for j in 0..entries.len() {
                    if let Some(skip) = by_name.get(reasons.value(j)) {
                        skipped.insert(*skip, rows.value(j));
                    }
                }
                week.creates = Some(SpiderCreates {
                    candidates: n("candidates")?,
                    creates: n("creates")?,
                    merged: n("merged")?,
                    skipped,
                    similar_name_30m: n("similar_name_30m")?,
                    own_names: own.value(i),
                });
            }
            weeks.insert(spiders.value(i).to_string(), week);
        }
    }
    Ok(weeks)
}

fn strings<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .with_context(|| format!("no string column {name:?}"))
}

fn struct_strings<'a>(s: &'a StructArray, name: &str) -> Result<&'a StringArray> {
    s.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .with_context(|| format!("no string field {name:?}"))
}

fn struct_numbers<'a>(s: &'a StructArray, name: &str) -> Result<&'a UInt64Array> {
    s.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
        .with_context(|| format!("no UInt64 field {name:?}"))
}

/// The fewest creates that a spider must have this week for a jump to
/// be logged, and how many times last week's count that must be.
const MIN_JUMP_CREATES: u64 = 100;
const JUMP_FACTOR: u64 = 2;

/// The fewest twins that get a spider a warning, and the share of its
/// candidates that they must be.
const MIN_TWINS: u64 = 20;
const MIN_TWIN_SHARE: f64 = 0.05;

/// Spiders whose creates jumped since last week: (spider, last week,
/// this week). A spider new this week counts from 0.
fn jumps(
    previous: &BTreeMap<String, Week>,
    current: &BTreeMap<String, Week>,
) -> Vec<(String, u64, u64)> {
    let mut found = Vec::new();
    for (spider, week) in current {
        let now = week.creates.as_ref().map_or(0, |c| c.creates);
        let before = previous
            .get(spider)
            .and_then(|w| w.creates.as_ref())
            .map_or(0, |c| c.creates);
        if now >= MIN_JUMP_CREATES && now >= JUMP_FACTOR * before.max(1) {
            found.push((spider.clone(), before, now));
        }
    }
    found
}

/// Spiders whose names started or stopped being their own labels:
/// (spider, now).
fn name_flips(
    previous: &BTreeMap<String, Week>,
    current: &BTreeMap<String, Week>,
) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    for (spider, week) in current {
        let (Some(now), Some(before)) = (
            week.creates.as_ref(),
            previous.get(spider).and_then(|w| w.creates.as_ref()),
        ) else {
            continue;
        };
        if now.own_names != before.own_names {
            found.push((spider.clone(), now.own_names));
        }
    }
    found
}

/// Spiders that list many places twice: (spider, twins, candidates).
fn twin_spiders(current: &BTreeMap<String, Week>) -> Vec<(String, u64, u64)> {
    let mut found = Vec::new();
    for (spider, week) in current {
        let Some(c) = &week.creates else { continue };
        let twins = c.skipped.get(&Skip::Twin).copied().unwrap_or(0);
        if twins >= MIN_TWINS && twins as f64 >= MIN_TWIN_SHARE * c.candidates as f64 {
            found.push((spider.clone(), twins, c.candidates));
        }
    }
    found
}

/// Logs warnings about this week's spiders, from `published` (see
/// [publishable]) and, if there is one, last week's published file
/// `previous`, which `fetch_inputs.previous` put into `workdir`.
pub fn review(published: &Path, previous: Option<&str>, workdir: &Path) {
    if let Err(err) = try_review(published, previous, workdir) {
        log::warn!(
            error = format!("{err:#}").as_str();
            "spider_creates: could not review spiders"
        );
    }
}

fn try_review(published: &Path, previous: Option<&str>, workdir: &Path) -> Result<()> {
    let current = read(published)?;
    let brands: BTreeMap<String, Vec<BrandRow>> = current
        .iter()
        .map(|(s, w)| {
            let rows = w
                .brands
                .iter()
                .map(|b| BrandRow {
                    country: b.country.clone(),
                    wikidata: b.wikidata.clone(),
                    name: b.name.clone(),
                    atp_features: b.atp_features,
                    osm_features: b.osm_features,
                })
                .collect();
            (s.clone(), rows)
        })
        .collect();
    let foreign = foreign_brands(&brands);
    for f in &foreign {
        log::warn!(
            spider = f.spider.as_str(),
            wikidata = f.wikidata.as_str(),
            name = f.name.as_str(),
            atp_features = f.atp_features,
            countries = f.countries.iter().take(10).cloned().collect::<Vec<_>>().join(",").as_str(),
            osm_features_elsewhere = f.osm_features;
            "spider_creates: brand ID that OSM doesn't use in these countries"
        );
    }
    let twins = twin_spiders(&current);
    for (spider, twins, candidates) in &twins {
        log::warn!(
            spider = spider.as_str(),
            twins = *twins,
            candidates = *candidates;
            "spider_creates: spider lists places twice"
        );
    }
    let previous_weeks = match previous {
        Some(p) => match read(&workdir.join(p)) {
            Ok(weeks) if weeks.values().any(|w| w.creates.is_some()) => Some(weeks),
            Ok(_) => {
                log::info!(previous = p; "spider_creates: last week has no create counts");
                None
            }
            Err(err) => {
                log::warn!(
                    previous = p,
                    error = format!("{err:#}").as_str();
                    "spider_creates: could not read last week's spider-stats.parquet"
                );
                None
            }
        },
        None => None,
    };
    let (mut num_jumps, mut num_flips) = (0, 0);
    if let Some(previous_weeks) = &previous_weeks {
        let found = jumps(previous_weeks, &current);
        num_jumps = found.len();
        for (spider, before, now) in found {
            log::warn!(
                spider = spider.as_str(),
                previous = before,
                current = now;
                "spider_creates: spider's creates jumped since last week"
            );
        }
        let flips = name_flips(previous_weeks, &current);
        num_flips = flips.len();
        for (spider, own) in flips {
            log::warn!(
                spider = spider.as_str(),
                own_names = own;
                "spider_creates: spider's names changed since last week"
            );
        }
    }
    let creates: u64 = current
        .values()
        .filter_map(|w| w.creates.as_ref())
        .map(|c| c.creates)
        .sum();
    log::info!(
        spiders = current.len(),
        creates = creates,
        foreign_brands = foreign.len(),
        twin_spiders = twins.len(),
        jumps = num_jumps,
        name_changes = num_flips,
        compared_with_last_week = previous_weeks.is_some();
        "spider_creates: review"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brand(country: Option<&str>, atp: u64, osm: u64) -> BrandRow {
        BrandRow {
            country: country.map(str::to_string),
            wikidata: "Q62267023".to_string(),
            name: "渣打國際商業銀行".to_string(),
            atp_features: atp,
            osm_features: osm,
        }
    }

    #[test]
    fn test_foreign_brands() {
        let mut brands = BTreeMap::new();
        brands.insert(
            "standard_chartered_bank".to_string(),
            vec![
                brand(Some("TW"), 66, 51),
                brand(Some("IN"), 81, 0),
                brand(Some("KE"), 20, 0),
                brand(None, 1, 0),
            ],
        );
        // A brand that OSM doesn't have anywhere yet is just new.
        brands.insert("new_chain".to_string(), vec![brand(Some("CH"), 500, 0)]);
        // A chain that OSM has in most of its countries.
        brands.insert(
            "growing_chain".to_string(),
            vec![brand(Some("DE"), 500, 400), brand(Some("AT"), 30, 0)],
        );
        let found = foreign_brands(&brands);
        assert_eq!(
            found,
            [ForeignBrand {
                spider: "standard_chartered_bank".to_string(),
                wikidata: "Q62267023".to_string(),
                name: "渣打國際商業銀行".to_string(),
                atp_features: 101,
                countries: vec!["IN".to_string(), "KE".to_string()],
                osm_features: 51,
            }]
        );
    }

    fn week(creates: u64, own_names: bool, twins: u64, candidates: u64) -> Week {
        Week {
            creates: Some(SpiderCreates {
                candidates,
                creates,
                skipped: BTreeMap::from([(Skip::Twin, twins)]),
                own_names,
                ..SpiderCreates::default()
            }),
            brands: Vec::new(),
        }
    }

    #[test]
    fn test_week_over_week() {
        let previous = BTreeMap::from([
            ("a".to_string(), week(100, false, 0, 1000)),
            ("b".to_string(), week(100, false, 0, 1000)),
            ("c".to_string(), week(5, true, 0, 1000)),
        ]);
        let current = BTreeMap::from([
            ("a".to_string(), week(250, false, 0, 1000)),
            ("b".to_string(), week(150, false, 0, 1000)),
            ("c".to_string(), week(5, false, 0, 1000)),
            ("new".to_string(), week(300, false, 60, 1000)),
        ]);
        assert_eq!(
            jumps(&previous, &current),
            [("a".to_string(), 100, 250), ("new".to_string(), 0, 300)]
        );
        assert_eq!(name_flips(&previous, &current), [("c".to_string(), false)]);
        assert_eq!(twin_spiders(&current), [("new".to_string(), 60, 1000)]);
    }

    #[test]
    fn test_publishable() {
        let dir = tempfile::tempdir().unwrap();
        let stats_path = dir.path().join("spider-stats.parquet");
        let schema = Arc::new(Schema::new(vec![Field::new(
            "spider",
            DataType::Utf8,
            false,
        )]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(vec!["bp", "other"])) as ArrayRef],
        )
        .unwrap();
        let mut w = ArrowWriter::try_new(File::create(&stats_path).unwrap(), schema, None).unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();

        let mut stats = CreateStats::default();
        stats.spiders.insert(
            "bp".to_string(),
            SpiderCreates {
                candidates: 10,
                creates: 3,
                skipped: BTreeMap::from([(Skip::Twin, 4), (Skip::Kind, 3)]),
                own_names: true,
                ..SpiderCreates::default()
            },
        );
        let sidecar = sidecar_path(dir.path());
        write_sidecar(&stats, &sidecar).unwrap();
        let out = publishable(&stats_path, &sidecar, dir.path()).unwrap();

        let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&out).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.into_iter().next().unwrap().unwrap();
        let creates = batch
            .column_by_name("creates")
            .unwrap()
            .as_any()
            .downcast_ref::<StructArray>()
            .unwrap();
        assert!(creates.is_valid(0));
        assert!(!creates.is_valid(1));
        assert_eq!(struct_numbers(creates, "creates").unwrap().value(0), 3);
        let skipped = creates
            .column_by_name("skipped")
            .unwrap()
            .as_any()
            .downcast_ref::<ListArray>()
            .unwrap()
            .value(0);
        let skipped = skipped.as_any().downcast_ref::<StructArray>().unwrap();
        let reasons = struct_strings(skipped, "reason").unwrap();
        assert_eq!(reasons.value(0), "kind");
        assert_eq!(reasons.value(1), "twin");
        assert_eq!(read_sidecar(&sidecar).unwrap()["bp"], stats.spiders["bp"]);
    }
}
