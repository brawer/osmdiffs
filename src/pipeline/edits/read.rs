//! The two passes over `conflated.parquet`; see the module doc of
//! [super].

use super::create::{self, Candidate, DeletionCandidates, HostFeature, MatchedSites, Positions};
use super::post_partner::{PostPartnerEditSuggester, has_partner_edits};
use super::spill::Spill;
use super::writer::{Edit, OsmSide, RelationMember};
use super::{Decision, Element, Source, Tags};
use crate::tables::{RecordReader, RecordWriter};
use crate::utils::parquet::{
    get_binary, get_f32, get_optional_f32, get_optional_string, get_string, get_struct, get_tags,
    get_u32, get_u64,
};
use anyhow::{Context, Result};
use arrow::array::{
    Array, ListArray, RecordBatch, StringArray, StructArray, TimestampMillisecondArray, UInt64Array,
};
use geo::Centroid;
use geo_traits::to_geo::ToGeoGeometry;
use indicatif::ProgressBar;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;

/// Whether the edit rules read tag `key`, of either side. The first
/// pass keeps only these, so that it needs little memory.
pub(super) fn is_rule_key(key: &str) -> bool {
    let base = key.strip_prefix("contact:").unwrap_or(key);
    let base = base.strip_suffix(":wikidata").unwrap_or(base);
    matches!(
        base,
        "phone"
            | "fax"
            | "email"
            | "website"
            | "mobile"
            | "url"
            | "brand"
            | "operator"
            | "network"
            | "amenity"
            | "shop"
            | "name"
            | "ref"
            | "addr:country"
    ) || key == "post_office"
        || key.starts_with("post_office:")
        || key == "heritage:website"
        || key == "opening_hours"
}

fn rule_tags(tags: &[(String, String)]) -> Tags {
    tags.iter()
        .filter(|(k, _)| is_rule_key(k))
        .cloned()
        .collect()
}

fn get_tag<'a>(tags: &'a [(String, String)], key: &str) -> Option<&'a str> {
    tags.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.trim().is_empty())
}

/// The centroid of a WKB geometry, as (longitude, latitude).
fn centroid(wkb: &[u8]) -> Option<(f64, f64)> {
    let geometry = wkb::reader::read_wkb(wkb).ok()?.to_geometry();
    geometry.centroid().map(|p| (p.x(), p.y()))
}

pub(super) fn s2_cell_id((lng, lat): (f64, f64)) -> u64 {
    s2::cellid::CellID::from(s2::latlng::LatLng::from_degrees(lat, lng)).0
}

fn batches(path: &Path) -> Result<impl Iterator<Item = Result<RecordBatch>>> {
    let file = File::open(path).with_context(|| format!("could not open {path:?}"))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    Ok(reader.map(|batch| Ok(batch?)))
}

/// One row as the first pass needs it.
enum FirstPassRow {
    /// A source for a modify of an OSM feature.
    Modify(Box<ModifyRow>),
    /// An ATP feature without an OSM feature of its own.
    Create(Box<Candidate>),
    /// A deletion candidate with a `brand:wikidata`, at a position.
    Deletion(String, (f64, f64)),
}

/// A row that matched an OSM feature, as the first pass needs it.
struct ModifyRow {
    key: (String, u64),
    osm_tags: Tags,
    position: Option<(f64, f64)>,
    source: Source,
}

/// Rows that matched an OSM feature, by OSM type, OSM ID and row in
/// `conflated.parquet`; the item is `(osm_tags, position, source)`.
pub(super) type ModifySpill = Spill<(String, u64, u64), (Tags, Option<(f64, f64)>, Source)>;

/// Create candidates, by latitude ([ordered]) and row.
pub(super) type CandidateSpill = Spill<(u64, u64), Candidate>;

/// The bits of `x`, in the order of the numbers; for sorting by a
/// coordinate.
fn ordered(x: f64) -> u64 {
    let bits = x.to_bits();
    if bits >> 63 == 1 {
        !bits
    } else {
        bits | (1 << 63)
    }
}

/// What the first pass finds: the rows that may become edits, on disk,
/// and the statistics over all rows that the rules need, in memory.
pub(super) struct FirstPass {
    /// The rows that matched OSM features.
    pub modifies: ModifySpill,
    /// The ATP features that may become creates.
    pub candidates: CandidateSpill,
    pub deletion_candidates: DeletionCandidates,
    pub positions: Positions,
    pub matched_sites: MatchedSites,
    /// Candidates left out in the first pass, counted with the others;
    /// see [create::early_skip].
    pub early_skips: create::CreateStats,
}

/// From `match.evidence.similar_osm`, the distance to the nearest OSM
/// feature like the ATP one by category, and the distance to the
/// nearest one by name.
fn similar_m(evidence: &StructArray, row: usize) -> Result<(Option<f32>, Option<f32>)> {
    let list = evidence
        .column_by_name("similar_osm")
        .context("missing match.evidence.similar_osm")?
        .as_any()
        .downcast_ref::<ListArray>()
        .context("match.evidence.similar_osm is not a list")?;
    if list.is_null(row) {
        return Ok((None, None));
    }
    let values = list.value(row);
    let entries = values
        .as_any()
        .downcast_ref::<StructArray>()
        .context("match.evidence.similar_osm items are not structs")?;
    let mut found = (None, None);
    for i in 0..entries.len() {
        let d = get_f32(entries, "distance_m", i)?;
        let slot = match get_string(entries, "by", i)?.as_str() {
            "category" => &mut found.0,
            "name" => &mut found.1,
            _ => continue,
        };
        if slot.is_none_or(|b: f32| d < b) {
            *slot = Some(d);
        }
    }
    Ok(found)
}

/// The deletion candidate in `row`, if it has a `brand:wikidata`.
fn deletion_row(
    batch: &RecordBatch,
    osm: &StructArray,
    row: usize,
) -> Result<Option<FirstPassRow>> {
    let tags = get_tags(osm, "tags", row)?;
    let Some(brand) = get_tag(&tags, "brand:wikidata") else {
        return Ok(None);
    };
    let Some(position) = centroid(&get_binary(batch, "osm_geometry", row)?) else {
        return Ok(None);
    };
    Ok(Some(FirstPassRow::Deletion(brand.to_string(), position)))
}

fn first_pass_row(
    batch: &RecordBatch,
    row: usize,
    unhealthy: &BTreeSet<String>,
) -> Result<Option<FirstPassRow>> {
    let atp = get_struct(batch, "atp")?;
    let osm = get_struct(batch, "osm")?;
    let matching = get_struct(batch, "match")?;
    let relation = get_string(matching, "relation", row)?;
    if atp.is_null(row) {
        if relation == "deletion_candidate" && osm.is_valid(row) {
            return deletion_row(batch, osm, row);
        }
        return Ok(None);
    }
    if relation != "same" && relation != "located_in" && relation != "missing_in_osm" {
        return Ok(None);
    }
    let fetched = get_struct(atp, "fetched")?;
    let spider = get_string(fetched, "spider", row)?;
    if unhealthy.contains(&spider) {
        return Ok(None);
    }
    let evidence = get_struct(matching, "evidence")?;
    let rule = get_optional_string(evidence, "rule", row)?;
    let atp_tags = get_tags(atp, "tags", row)?;
    // No charge point edits for now; see the plan's decisions.
    if rule.as_deref() == Some("charging_station")
        || get_tag(&atp_tags, "amenity") == Some("charging_station")
    {
        return Ok(None);
    }
    let partner = has_partner_edits(&atp_tags, &relation);
    if relation == "missing_in_osm" || (relation == "located_in" && !partner) {
        // A tenant inside its host becomes a create, never an edit of
        // the host.
        return create_row(batch, row, spider, relation, rule, atp_tags);
    }
    if osm.is_null(row) {
        return Ok(None);
    }
    let osm_tags = get_tags(osm, "tags", row)?;
    let partner_tags = partner.then(|| {
        PostPartnerEditSuggester::new(&spider, &atp_tags)
            .suggest_edit(&osm_tags)
            .unwrap_or_default()
    });
    let source_url = get_tag(&atp_tags, "website")
        .or_else(|| get_tag(&atp_tags, "contact:website"))
        .map(str::to_string)
        .or(get_optional_string(fetched, "source_uri", row)?);
    let source = Source {
        spider,
        fetched_millis: fetched_millis(fetched, row)?,
        atp_ref: get_tag(&atp_tags, "ref").map(str::to_string),
        source_url,
        relation,
        rule,
        score: get_optional_f32(matching, "score", row)?,
        distance_m: get_optional_f32(evidence, "distance_m", row)?,
        importance: get_f32(batch, "importance", row)?,
        atp_point: centroid(&get_binary(batch, "atp_geometry", row)?),
        tags: rule_tags(&atp_tags),
        partner_tags,
    };
    Ok(Some(FirstPassRow::Modify(Box::new(ModifyRow {
        key: (get_string(osm, "type", row)?, get_u64(osm, "id", row)?),
        osm_tags: rule_tags(&osm_tags),
        position: centroid(&get_binary(batch, "osm_geometry", row)?),
        source,
    }))))
}

/// `atp.fetched.timestamp` of `row`, in Unix milliseconds.
fn fetched_millis(fetched: &StructArray, row: usize) -> Result<i64> {
    Ok(fetched
        .column_by_name("timestamp")
        .context("missing atp.fetched.timestamp")?
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .context("atp.fetched.timestamp is not a millisecond timestamp")?
        .value(row))
}

/// The create candidate of a `missing_in_osm` row, or of a tenant's
/// `located_in` row, with its host.
fn create_row(
    batch: &RecordBatch,
    row: usize,
    spider: String,
    relation: String,
    rule: Option<String>,
    atp_tags: Tags,
) -> Result<Option<FirstPassRow>> {
    let Some(category) = super::create::create_category(&atp_tags) else {
        return Ok(None);
    };
    let Some(point) = centroid(&get_binary(batch, "atp_geometry", row)?) else {
        return Ok(None);
    };
    let atp = get_struct(batch, "atp")?;
    let fetched = get_struct(atp, "fetched")?;
    let matching = get_struct(batch, "match")?;
    let evidence = get_struct(matching, "evidence")?;
    let osm = get_struct(batch, "osm")?;
    let host = if relation == "located_in" && osm.is_valid(row) {
        let osm_tags = get_tags(osm, "tags", row)?;
        let wkb = get_binary(batch, "osm_geometry", row)?;
        let Ok(geometry) = wkb::reader::read_wkb(&wkb).map(|g| g.to_geometry()) else {
            return Ok(None);
        };
        Some(HostFeature {
            osm_type: get_string(osm, "type", row)?,
            osm_id: get_u64(osm, "id", row)?,
            name: get_tag(&osm_tags, "name").map(str::to_string),
            opening_hours: get_tag(&osm_tags, "opening_hours").map(str::to_string),
            geometry,
        })
    } else {
        None
    };
    let source_url = get_tag(&atp_tags, "website")
        .or_else(|| get_tag(&atp_tags, "contact:website"))
        .map(str::to_string)
        .or(get_optional_string(fetched, "source_uri", row)?);
    let source = Source {
        spider,
        fetched_millis: fetched_millis(fetched, row)?,
        atp_ref: get_tag(&atp_tags, "ref").map(str::to_string),
        source_url,
        relation,
        rule,
        score: None,
        distance_m: get_optional_f32(evidence, "distance_m", row)?,
        importance: get_f32(batch, "importance", row)?,
        atp_point: Some(point),
        tags: atp_tags,
        partner_tags: None,
    };
    let (similar_m, similar_name_m) = similar_m(evidence, row)?;
    Ok(Some(FirstPassRow::Create(Box::new(Candidate {
        source,
        category,
        point,
        similar_m,
        similar_name_m,
        host,
    }))))
}

/// First pass: the rows that matched OSM features, and the ATP
/// features that may become creates, leaving out the rows of
/// `unhealthy` spiders, spilled to disk below `workdir`; and the
/// statistics that the create rules need.
pub(super) fn first_pass(
    conflated: &Path,
    unhealthy: &BTreeSet<String>,
    workdir: &Path,
    bar: &ProgressBar,
) -> Result<FirstPass> {
    // Both spills fill at the same time.
    let budget = super::SPILL_BYTES / 2;
    let mut pass = FirstPass {
        modifies: Spill::new(workdir, "modifies", budget)?,
        candidates: Spill::new(workdir, "candidates", budget)?,
        deletion_candidates: Default::default(),
        positions: Default::default(),
        matched_sites: Default::default(),
        early_skips: Default::default(),
    };
    let mut base = 0u64;
    for batch in batches(conflated)? {
        let batch = batch?;
        bar.inc_length(batch.num_rows() as u64);
        let rows: Vec<Option<FirstPassRow>> = (0..batch.num_rows())
            .into_par_iter()
            .map(|row| first_pass_row(&batch, row, unhealthy))
            .collect::<Result<_>>()?;
        for (i, row) in rows.into_iter().enumerate() {
            let Some(row) = row else {
                continue;
            };
            let row_index = base + i as u64;
            match row {
                FirstPassRow::Modify(m) => {
                    let ModifyRow {
                        key,
                        osm_tags,
                        position,
                        source,
                    } = *m;
                    if source.relation == "same"
                        && let Some(point) = source.atp_point
                        && let Some(category) = super::create::create_category(&source.tags)
                    {
                        pass.matched_sites.add(&source.spider, category, point);
                    }
                    let (osm_type, osm_id) = key;
                    pass.modifies
                        .push((osm_type, osm_id, row_index), &(osm_tags, position, source))?;
                }
                FirstPassRow::Create(candidate) => {
                    pass.positions.add(
                        &candidate.source.spider,
                        candidate.point,
                        candidate.source.atp_ref.as_deref(),
                    );
                    // Kinds we never create, and places that OSM
                    // likely has, are only counted, to save disk.
                    match create::early_skip(&candidate) {
                        Some(skip) => {
                            pass.early_skips.count_candidate(&candidate);
                            pass.early_skips.count_skip(&candidate.source.spider, skip);
                        }
                        None => pass
                            .candidates
                            .push((ordered(candidate.point.1), row_index), &candidate)?,
                    }
                }
                FirstPassRow::Deletion(brand, position) => {
                    pass.deletion_candidates.add(&brand, position);
                }
            }
        }
        base += batch.num_rows() as u64;
        bar.inc(batch.num_rows() as u64);
    }
    Ok(pass)
}

/// An OSM feature that AllThePlaces matched, with all the rows that
/// matched it, in the order of the rows, and the first of these rows.
#[derive(Serialize, Deserialize)]
pub(super) struct GroupedElement {
    pub key: (String, u64),
    pub first_row: u64,
    pub element: Element,
}

/// The rows of `modifies`, grouped by OSM feature, in a spool file at
/// `path`, which [grouped_elements] reads.
pub(super) fn group_elements(modifies: ModifySpill, path: &Path) -> Result<RecordReader> {
    let mut writer = RecordWriter::create(path)?;
    let mut current: Option<GroupedElement> = None;
    for item in modifies.sorted()? {
        let ((osm_type, osm_id, row), (tags, position, source)) = item?;
        if let Some(c) = &mut current
            && c.key.0 == osm_type
            && c.key.1 == osm_id
        {
            c.element.sources.push(source);
            continue;
        }
        if let Some(done) = current.take() {
            writer.write(&rmp_serde::to_vec(&done)?)?;
        }
        current = Some(GroupedElement {
            key: (osm_type, osm_id),
            first_row: row,
            element: Element {
                tags,
                position,
                sources: vec![source],
            },
        });
    }
    if let Some(done) = current.take() {
        writer.write(&rmp_serde::to_vec(&done)?)?;
    }
    writer.close()?;
    RecordReader::open(path)
}

/// The features of a spool file written by [group_elements].
pub(super) fn grouped_elements(
    spool: &RecordReader,
) -> Result<impl Iterator<Item = Result<GroupedElement>> + use<>> {
    Ok(spool
        .iter()?
        .map(|record| Ok(rmp_serde::from_slice(&record?)?)))
}

/// What a decided feature needs for its edit, besides its OSM side.
#[derive(Serialize, Deserialize)]
pub(super) struct DecidedElement {
    pub position: Option<(f64, f64)>,
    pub sources: Vec<Source>,
    pub decision: Decision,
}

/// The key that `edits.parquet` is sorted by: along the Hilbert curve
/// of S2 cells, as `conflated.parquet`, then by OSM feature (none for a
/// create) and ID, and last by a sequence number that keeps equal keys
/// in the order they came.
pub(super) type EditKey = (u64, Option<(String, u64)>, String, u64);

pub(super) fn edit_key(edit: &Edit, seq: u64) -> EditKey {
    let osm = edit.osm.as_ref().map(|o| (o.osm_type.clone(), o.osm_id));
    (edit.s2_cell_id, osm, edit.id.clone(), seq)
}

fn way_members(osm: &StructArray, row: usize) -> Result<Option<Vec<u64>>> {
    let list = osm
        .column_by_name("way_members")
        .context("missing osm.way_members")?
        .as_any()
        .downcast_ref::<ListArray>()
        .context("osm.way_members is not a list")?;
    if list.is_null(row) {
        return Ok(None);
    }
    let values = list.value(row);
    let ids = values
        .as_any()
        .downcast_ref::<UInt64Array>()
        .context("osm.way_members items are not UInt64")?;
    Ok(Some(ids.values().to_vec()))
}

fn relation_members(osm: &StructArray, row: usize) -> Result<Option<Vec<RelationMember>>> {
    let list = osm
        .column_by_name("relation_members")
        .context("missing osm.relation_members")?
        .as_any()
        .downcast_ref::<ListArray>()
        .context("osm.relation_members is not a list")?;
    if list.is_null(row) {
        return Ok(None);
    }
    let values = list.value(row);
    let members = values
        .as_any()
        .downcast_ref::<StructArray>()
        .context("osm.relation_members items are not structs")?;
    let strings = |name: &str| -> Result<&StringArray> {
        members
            .column_by_name(name)
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .with_context(|| format!("osm.relation_members.{name} is not a string"))
    };
    let (types, roles) = (strings("type")?, strings("role")?);
    let ids = members
        .column_by_name("id")
        .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
        .context("osm.relation_members.id is not UInt64")?;
    Ok(Some(
        (0..members.len())
            .map(|i| RelationMember {
                member_type: types.value(i).to_string(),
                id: ids.value(i),
                role: roles.is_valid(i).then(|| roles.value(i).to_string()),
            })
            .collect(),
    ))
}

fn osm_side(osm: &StructArray, row: usize) -> Result<OsmSide> {
    let modified = get_struct(osm, "modified")?;
    let timestamp_millis = modified
        .column_by_name("timestamp")
        .context("missing osm.modified.timestamp")?
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .context("osm.modified.timestamp is not a millisecond timestamp")?
        .value(row);
    Ok(OsmSide {
        osm_type: get_string(osm, "type", row)?,
        osm_id: get_u64(osm, "id", row)?,
        tags: get_tags(osm, "tags", row)?,
        timestamp_millis,
        version: get_u32(modified, "version", row)?,
        way_members: way_members(osm, row)?,
        relation_members: relation_members(osm, row)?,
    })
}

/// ATP's points of `sources`: one point, or a multipoint for several.
fn atp_geometry(sources: &[Source]) -> Option<Vec<u8>> {
    let points: Vec<geo::Point<f64>> = sources
        .iter()
        .filter_map(|s| s.atp_point)
        .map(|(x, y)| geo::Point::new(x, y))
        .collect();
    let geometry = match points.len() {
        0 => return None,
        1 => geo::Geometry::Point(points[0]),
        _ => geo::Geometry::MultiPoint(geo::MultiPoint(points)),
    };
    Some(crate::geometry::encode_wkb(&geometry))
}

/// Last pass: the full OSM side of every feature with a decision,
/// taken from the first row that matched the feature. `decided` comes
/// sorted by that row; each edit goes to `emit`, with its row.
pub(super) fn complete_edits(
    conflated: &Path,
    decided: impl Iterator<Item = Result<(u64, DecidedElement)>>,
    bar: &ProgressBar,
    mut emit: impl FnMut(u64, Edit) -> Result<()>,
) -> Result<()> {
    let mut decided = decided.peekable();
    let mut base = 0u64;
    for batch in batches(conflated)? {
        if decided.peek().is_none() {
            break;
        }
        let batch = batch?;
        bar.inc_length(batch.num_rows() as u64);
        let osm = get_struct(&batch, "osm")?;
        let end = base + batch.num_rows() as u64;
        while let Some(next) = decided.peek() {
            let row_index = match next {
                Ok((row, _)) => *row,
                Err(_) => {
                    return Err(match decided.next() {
                        Some(Err(e)) => e,
                        _ => unreachable!("peeked an error"),
                    });
                }
            };
            if row_index >= end {
                break;
            }
            let (_, element) = decided.next().expect("peeked")?;
            let row = (row_index - base) as usize;
            anyhow::ensure!(osm.is_valid(row), "row {row_index} has no OSM feature");
            let DecidedElement {
                position,
                sources,
                decision,
            } = element;
            let osm_side = osm_side(osm, row)?;
            let geometry_wkb = get_binary(&batch, "osm_geometry", row)?;
            let position = position.or_else(|| centroid(&geometry_wkb));
            let edit = Edit {
                id: super::modify_id(&osm_side.osm_type, osm_side.osm_id, &decision.changes),
                action: "modify",
                topics: decision.topics,
                osm: Some(osm_side),
                changes: decision.changes,
                atp_geometry_wkb: atp_geometry(&sources),
                importance: sources.iter().map(|s| s.importance).fold(0.0, f32::max),
                sources,
                geometry_wkb,
                host: None,
                warnings: decision.warnings,
                s2_cell_id: position.map(s2_cell_id).unwrap_or(0),
            };
            emit(row_index, edit)?;
        }
        base = end;
        bar.inc(batch.num_rows() as u64);
    }
    anyhow::ensure!(
        decided.peek().is_none(),
        "edited OSM features not found in the last pass"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_rule_key() {
        for key in [
            "phone",
            "contact:phone",
            "brand:wikidata",
            "post_office:brand:wikidata",
            "addr:country",
            "url",
            "opening_hours",
        ] {
            assert!(is_rule_key(key), "{key}");
        }
        for key in [
            "opening_hours:covid19",
            "addr:street",
            "building",
            "brand:en",
        ] {
            assert!(!is_rule_key(key), "{key}");
        }
    }
}
