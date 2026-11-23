//! The two passes over `conflated.parquet`; see the module doc of
//! [super].

use super::post_partner::{PostPartnerEditSuggester, has_partner_edits};
use super::writer::{Edit, OsmSide, RelationMember};
use super::{Decision, Element, Source, Tags};
use crate::matchers::category::main_category;
use crate::matchers::{normalize_name, parse_wikidata_ids};
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
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
pub(super) fn centroid(wkb: &[u8]) -> Option<(f64, f64)> {
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
struct FirstPassRow {
    key: (String, u64),
    osm_tags: Tags,
    position: Option<(f64, f64)>,
    source: Source,
}

/// A `deletion_candidate` row of `conflated.parquet`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Candidate {
    pub osm: OsmSide,
    pub geometry_wkb: Vec<u8>,
    /// `match.evidence.rule`, the deletion signal.
    pub rule: String,
    pub local_share: Option<f32>,
    pub importance: f32,
}

/// What the first pass learns from one row.
#[derive(Default)]
struct RowInfo {
    matched: Option<FirstPassRow>,
    candidate: Option<((String, u64), Candidate)>,
    /// The spider, when fetched, and the [brand_keys] of its feature.
    atp: Option<(String, i64, Vec<String>)>,
    /// The OSM feature that the ATP feature is `located_in`.
    host: Option<(String, u64)>,
    /// For a `missing_in_osm` row, the spider, ATP's point and its
    /// main category.
    unmatched: Option<(String, (f64, f64), Option<String>)>,
}

/// Size of a cell of [Unmatched], in degrees: about 110 m north to
/// south, and less east to west.
const UNMATCHED_CELL_DEG: f64 = 0.001;

/// The AllThePlaces features that nothing in OSM matched, by spider
/// and grid cell, for finding a delete that is more likely a missed
/// match or a new brand: the spider lists a place of the same kind
/// right there.
#[derive(Debug, Default)]
pub(crate) struct Unmatched {
    cells: HashMap<(String, i64, i64), Vec<UnmatchedFeature>>,
}

/// A feature of [Unmatched]: its point and its main category.
type UnmatchedFeature = ((f64, f64), Option<String>);

impl Unmatched {
    fn cell((lng, lat): (f64, f64)) -> (i64, i64) {
        (
            (lng / UNMATCHED_CELL_DEG).floor() as i64,
            (lat / UNMATCHED_CELL_DEG).floor() as i64,
        )
    }

    pub(crate) fn add(&mut self, spider: &str, point: (f64, f64), category: Option<String>) {
        let (x, y) = Self::cell(point);
        self.cells
            .entry((spider.to_string(), x, y))
            .or_default()
            .push((point, category));
    }

    /// Whether `spider` has an unmatched feature within `max_m` meters
    /// (at most [UNMATCHED_CELL_DEG]) of `point` whose category passes
    /// `agrees`.
    pub(crate) fn any_near(
        &self,
        spider: &str,
        point: (f64, f64),
        max_m: f64,
        agrees: impl Fn(Option<&str>) -> bool,
    ) -> bool {
        let (x, y) = Self::cell(point);
        for dx in -1..=1 {
            for dy in -1..=1 {
                let Some(features) = self.cells.get(&(spider.to_string(), x + dx, y + dy)) else {
                    continue;
                };
                for (p, category) in features {
                    if meters(point, *p) <= max_m && agrees(category.as_deref()) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// The distance between two (longitude, latitude) points, in meters,
/// for short distances.
fn meters(a: (f64, f64), b: (f64, f64)) -> f64 {
    let lat = ((a.1 + b.1) / 2.0).to_radians();
    let dx = (a.0 - b.0) * lat.cos() * 111_320.0;
    let dy = (a.1 - b.1) * 110_540.0;
    dx.hypot(dy)
}

/// The spiders that list a brand, operator or network, by [brand_keys],
/// with the time each fetched its earliest feature of it, in Unix
/// milliseconds; and every spider's earliest fetch.
#[derive(Debug, Default)]
pub(crate) struct BrandSpiders {
    pub by_key: HashMap<String, BTreeMap<String, i64>>,
    pub by_spider: BTreeMap<String, i64>,
}

impl BrandSpiders {
    fn add(&mut self, spider: &str, fetched: i64, keys: Vec<String>) {
        let earliest = |map: &mut BTreeMap<String, i64>| match map.get_mut(spider) {
            Some(t) => *t = (*t).min(fetched),
            None => {
                map.insert(spider.to_string(), fetched);
            }
        };
        earliest(&mut self.by_spider);
        for key in keys {
            earliest(self.by_key.entry(key).or_default());
        }
    }
}

/// The Wikidata IDs and folded names of a feature's brand, operator and
/// network, as `Q123` and `name:<folded name>`, for looking up which
/// spiders list a deletion candidate's brand.
pub(crate) fn brand_keys(tags: &[(String, String)]) -> Vec<String> {
    let mut keys = Vec::new();
    for key in ["brand", "operator", "network"] {
        if let Some(ids) = get_tag(tags, &format!("{key}:wikidata")) {
            keys.extend(parse_wikidata_ids(ids).map(|id| format!("Q{id}")));
        }
        if let Some(names) = get_tag(tags, key) {
            keys.extend(
                names
                    .split(';')
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(|n| format!("name:{}", normalize_name(n))),
            );
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

fn timestamp_millis(src: &StructArray, name: &str, row: usize) -> Result<i64> {
    Ok(src
        .column_by_name(name)
        .with_context(|| format!("missing {name}"))?
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .with_context(|| format!("{name} is not a millisecond timestamp"))?
        .value(row))
}

fn candidate_row(batch: &RecordBatch, row: usize) -> Result<Option<((String, u64), Candidate)>> {
    let osm = get_struct(batch, "osm")?;
    let matching = get_struct(batch, "match")?;
    let evidence = get_struct(matching, "evidence")?;
    let osm = osm_side(osm, row)?;
    let key = (osm.osm_type.clone(), osm.osm_id);
    Ok(Some((
        key,
        Candidate {
            osm,
            geometry_wkb: get_binary(batch, "osm_geometry", row)?,
            rule: get_optional_string(evidence, "rule", row)?.unwrap_or_default(),
            local_share: get_optional_f32(evidence, "local_share", row)?,
            importance: get_f32(batch, "importance", row)?,
        },
    )))
}

fn row_info(batch: &RecordBatch, row: usize, unhealthy: &BTreeSet<String>) -> Result<RowInfo> {
    let atp = get_struct(batch, "atp")?;
    let osm = get_struct(batch, "osm")?;
    let matching = get_struct(batch, "match")?;
    let relation = get_string(matching, "relation", row)?;
    if atp.is_null(row) {
        if relation == "deletion_candidate" && !osm.is_null(row) {
            return Ok(RowInfo {
                candidate: candidate_row(batch, row)?,
                ..RowInfo::default()
            });
        }
        return Ok(RowInfo::default());
    }
    let fetched = get_struct(atp, "fetched")?;
    let spider = get_string(fetched, "spider", row)?;
    let fetched_millis = timestamp_millis(fetched, "timestamp", row)?;
    let atp_tags = get_tags(atp, "tags", row)?;
    let osm_key = if osm.is_null(row) {
        None
    } else {
        Some((get_string(osm, "type", row)?, get_u64(osm, "id", row)?))
    };
    let mut info = RowInfo {
        atp: Some((spider.clone(), fetched_millis, brand_keys(&atp_tags))),
        host: osm_key.clone().filter(|_| relation == "located_in"),
        ..RowInfo::default()
    };
    let Some(key) = osm_key else {
        if relation == "missing_in_osm"
            && let Some(point) = centroid(&get_binary(batch, "atp_geometry", row)?)
        {
            let category = main_category(atp_tags.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            info.unmatched = Some((spider, point, category));
        }
        return Ok(info);
    };
    if relation != "same" && relation != "located_in" {
        return Ok(info);
    }
    if unhealthy.contains(&spider) {
        return Ok(info);
    }
    let evidence = get_struct(matching, "evidence")?;
    let rule = get_optional_string(evidence, "rule", row)?;
    // No charge point edits for now; see the plan's decisions.
    if rule.as_deref() == Some("charging_station")
        || get_tag(&atp_tags, "amenity") == Some("charging_station")
    {
        return Ok(info);
    }
    let partner = has_partner_edits(&atp_tags, &relation);
    if relation == "located_in" && !partner {
        // A tenant inside its host becomes a create, not an edit of
        // the host; that comes in a later milestone.
        return Ok(info);
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
        fetched_millis,
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
    info.matched = Some(FirstPassRow {
        key,
        osm_tags: rule_tags(&osm_tags),
        position: centroid(&get_binary(batch, "osm_geometry", row)?),
        source,
    });
    Ok(info)
}

/// What the first pass collects.
#[derive(Default)]
pub(super) struct FirstPass {
    /// The OSM features that AllThePlaces matched, with the rows that
    /// matched them.
    pub elements: HashMap<(String, u64), Element>,
    pub candidates: HashMap<(String, u64), Candidate>,
    /// For each OSM feature that some AllThePlaces feature is
    /// `located_in`, the spiders of those features.
    pub hosts: HashMap<(String, u64), BTreeSet<String>>,
    pub brand_spiders: BrandSpiders,
    pub unmatched: Unmatched,
}

/// First pass over `conflated.parquet`, leaving the rows of
/// `unhealthy` spiders out of [FirstPass::elements].
pub(super) fn collect_elements(
    conflated: &Path,
    unhealthy: &BTreeSet<String>,
    bar: &ProgressBar,
) -> Result<FirstPass> {
    let mut pass = FirstPass::default();
    for batch in batches(conflated)? {
        let batch = batch?;
        bar.inc_length(batch.num_rows() as u64);
        let rows: Vec<RowInfo> = (0..batch.num_rows())
            .into_par_iter()
            .map(|row| row_info(&batch, row, unhealthy))
            .collect::<Result<_>>()?;
        for info in rows {
            if let Some((spider, fetched, keys)) = info.atp {
                if let Some(host) = info.host {
                    pass.hosts.entry(host).or_default().insert(spider.clone());
                }
                pass.brand_spiders.add(&spider, fetched, keys);
            }
            if let Some((spider, point, category)) = info.unmatched {
                pass.unmatched.add(&spider, point, category);
            }
            if let Some((key, candidate)) = info.candidate {
                pass.candidates.insert(key, candidate);
            }
            if let Some(row) = info.matched {
                let element = pass.elements.entry(row.key).or_insert_with(|| Element {
                    tags: row.osm_tags,
                    position: row.position,
                    sources: Vec::new(),
                });
                element.sources.push(row.source);
            }
        }
        bar.inc(batch.num_rows() as u64);
    }
    Ok(pass)
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
    let timestamp_millis = timestamp_millis(modified, "timestamp", row)?;
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

/// Second pass: the full OSM side of every feature with a decision,
/// taken from the first row that has the feature.
pub(super) fn complete_edits(
    conflated: &Path,
    mut elements: HashMap<(String, u64), Element>,
    mut decisions: HashMap<(String, u64), Decision>,
    bar: &ProgressBar,
) -> Result<Vec<Edit>> {
    let mut edits = Vec::with_capacity(decisions.len());
    for batch in batches(conflated)? {
        let batch = batch?;
        bar.inc_length(batch.num_rows() as u64);
        let osm = get_struct(&batch, "osm")?;
        for row in 0..batch.num_rows() {
            if decisions.is_empty() {
                break;
            }
            if osm.is_null(row) {
                continue;
            }
            let key = (get_string(osm, "type", row)?, get_u64(osm, "id", row)?);
            let Some(decision) = decisions.remove(&key) else {
                continue;
            };
            let element = elements.remove(&key).context("decision without element")?;
            let osm_side = osm_side(osm, row)?;
            let geometry_wkb = get_binary(&batch, "osm_geometry", row)?;
            let position = element.position.or_else(|| centroid(&geometry_wkb));
            edits.push(Edit {
                id: super::modify_id(&osm_side.osm_type, osm_side.osm_id, &decision.changes),
                action: "modify",
                topics: decision.topics,
                osm: osm_side,
                changes: decision.changes,
                atp_geometry_wkb: atp_geometry(&element.sources),
                importance: element
                    .sources
                    .iter()
                    .map(|s| s.importance)
                    .fold(0.0, f32::max),
                sources: element.sources,
                geometry_wkb,
                warnings: decision.warnings,
                deleted_members: Vec::new(),
                s2_cell_id: position.map(s2_cell_id).unwrap_or(0),
            });
        }
        bar.inc(batch.num_rows() as u64);
    }
    anyhow::ensure!(
        decisions.is_empty(),
        "{} edited OSM features not found in the second pass",
        decisions.len()
    );
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmatched_near() {
        let mut u = Unmatched::default();
        u.add("migros_ch", (8.5, 47.0), Some("shop=supermarket".into()));
        // Across a cell border, about 30 m away.
        let p = (8.50039, 47.0001);
        let any = |_: Option<&str>| true;
        assert!(u.any_near("migros_ch", p, 50.0, any));
        assert!(!u.any_near("migros_ch", p, 20.0, any));
        assert!(!u.any_near("coop_ch", p, 50.0, any));
        assert!(!u.any_near("migros_ch", p, 50.0, |c| c == Some("shop=florist")));
        assert!(!u.any_near("migros_ch", (8.51, 47.0), 50.0, any));
    }

    #[test]
    fn test_brand_keys() {
        let tags: Tags = [
            ("brand", "Coop; Coop Pronto"),
            ("brand:wikidata", "Q432564"),
            ("operator", "Coop Mineraloel AG"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(
            brand_keys(&tags),
            [
                "Q432564",
                "name:coop",
                "name:coop mineraloel ag",
                "name:coop pronto"
            ]
        );
    }

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
