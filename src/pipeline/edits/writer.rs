//! Writes `edits.parquet`; see `docs/outputs/EDITS_PARQUET.md` for the
//! format. The `osm` struct is the same as in `conflated.parquet`, so
//! that the two files read alike.

use super::{Change, Source};
use crate::geometry::GeometryTally;
use anyhow::Result;
use arrow::array::{
    ArrayRef, BinaryBuilder, Float32Builder, ListArray, MapBuilder, RecordBatch, StringBuilder,
    StructArray, TimestampMillisecondBuilder, UInt32Builder, UInt64Builder,
};
use arrow::buffer::{NullBuffer, OffsetBuffer};
use arrow_schema::{DataType, Field, Fields, Schema, SchemaRef, TimeUnit};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_writer::ArrowWriterOptions;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use parquet_geospatial::{WkbEdges, WkbMetadata, WkbType};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Key-value metadata that says the format may change at any time, for
/// tools that never read our docs.
pub(super) const FORMAT_STATUS_KEY: &str = "format_status";

/// The same notice as at the top of `docs/outputs/EDITS_PARQUET.md`
/// and in the resource's description in `datapackage.json`.
pub(crate) const DISCLAIMER: &str = "Experimental. The format isn't settled and may change at \
    any time, without notice. Don't build anything you can't easily rewrite.";

/// Same key as `conflated.parquet`'s embedded BOM.
const PROVENANCE_KEY: &str = "org.cyclonedx.bom";

const ROWS_PER_GROUP: usize = 50_000;

/// One member of an OSM relation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RelationMember {
    pub member_type: String,
    pub id: u64,
    pub role: Option<String>,
}

/// The OSM side of an edit, as in `conflated.parquet`'s `osm` column.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct OsmSide {
    pub osm_type: String,
    pub osm_id: u64,
    pub tags: Vec<(String, String)>,
    pub timestamp_millis: i64,
    pub version: u32,
    pub way_members: Option<Vec<u64>>,
    pub relation_members: Option<Vec<RelationMember>>,
}

/// The OSM feature that a create is in, for the `host` column.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Host {
    pub osm_type: String,
    pub osm_id: u64,
    pub name: Option<String>,
}

/// One row of `edits.parquet`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Edit {
    pub id: String,
    #[serde(deserialize_with = "super::spill::static_str")]
    pub action: super::spill::StaticStr,
    #[serde(deserialize_with = "super::spill::static_strs")]
    pub topics: Vec<&'static str>,
    /// `None` for a create.
    pub osm: Option<OsmSide>,
    pub changes: Vec<Change>,
    pub sources: Vec<Source>,
    pub geometry_wkb: Vec<u8>,
    pub atp_geometry_wkb: Option<Vec<u8>>,
    /// For a create inside another place, that place.
    pub host: Option<Host>,
    pub warnings: Vec<String>,
    pub importance: f32,
    /// Sort key only, not written.
    pub s2_cell_id: u64,
}

/// Counts for the log.
#[derive(Debug, Default)]
pub(super) struct Stats {
    pub edits: usize,
    pub changes: usize,
    pub topics: BTreeMap<&'static str, usize>,
}

impl Stats {
    pub fn topic(&self, topic: &str) -> usize {
        self.topics.get(topic).copied().unwrap_or(0)
    }
}

const NOT_NULLABLE: bool = false;
const NULLABLE: bool = true;

fn timestamp_type() -> DataType {
    DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into()))
}

fn geo_field(name: &str, nullable: bool) -> Field {
    let metadata = WkbMetadata::new(Some("OGC:CRS84"), Some(WkbEdges::Spherical));
    Field::new(name, DataType::Binary, nullable).with_extension_type(WkbType::new(Some(metadata)))
}

fn string_list_field(name: &str) -> Field {
    Field::new_list(
        name,
        Field::new("item", DataType::Utf8, NOT_NULLABLE),
        NOT_NULLABLE,
    )
}

fn tags_entry_fields() -> Fields {
    vec![
        Field::new("key", DataType::Utf8, NOT_NULLABLE),
        Field::new("value", DataType::Utf8, NOT_NULLABLE),
    ]
    .into()
}

fn modified_fields() -> Fields {
    vec![
        Field::new("timestamp", timestamp_type(), NOT_NULLABLE),
        Field::new("version", DataType::UInt32, NOT_NULLABLE),
    ]
    .into()
}

fn relation_member_fields() -> Fields {
    vec![
        Field::new("type", DataType::Utf8, NOT_NULLABLE),
        Field::new("id", DataType::UInt64, NOT_NULLABLE),
        Field::new("role", DataType::Utf8, NULLABLE),
    ]
    .into()
}

fn osm_fields() -> Fields {
    vec![
        Field::new("type", DataType::Utf8, NOT_NULLABLE),
        Field::new("id", DataType::UInt64, NOT_NULLABLE),
        Field::new_map(
            "tags",
            "key_value",
            Field::new("key", DataType::Utf8, NOT_NULLABLE),
            Field::new("value", DataType::Utf8, NOT_NULLABLE),
            false,
            NOT_NULLABLE,
        ),
        Field::new_struct("modified", modified_fields(), NOT_NULLABLE),
        Field::new_list(
            "way_members",
            Field::new("item", DataType::UInt64, NOT_NULLABLE),
            NULLABLE,
        ),
        Field::new_list(
            "relation_members",
            Field::new(
                "item",
                DataType::Struct(relation_member_fields()),
                NOT_NULLABLE,
            ),
            NULLABLE,
        ),
    ]
    .into()
}

fn change_fields() -> Fields {
    vec![
        Field::new("key", DataType::Utf8, NOT_NULLABLE),
        Field::new("old", DataType::Utf8, NULLABLE),
        Field::new("new", DataType::Utf8, NULLABLE),
        Field::new("reason", DataType::Utf8, NOT_NULLABLE),
        Field::new("atp", DataType::Utf8, NULLABLE),
    ]
    .into()
}

fn source_fields() -> Fields {
    vec![
        Field::new("spider", DataType::Utf8, NOT_NULLABLE),
        Field::new("fetched", timestamp_type(), NOT_NULLABLE),
        Field::new("atp_ref", DataType::Utf8, NULLABLE),
        Field::new("source_url", DataType::Utf8, NULLABLE),
        Field::new("relation", DataType::Utf8, NOT_NULLABLE),
        Field::new("rule", DataType::Utf8, NULLABLE),
        Field::new("score", DataType::Float32, NULLABLE),
        Field::new("distance_m", DataType::Float32, NULLABLE),
    ]
    .into()
}

fn host_fields() -> Fields {
    vec![
        Field::new("type", DataType::Utf8, NOT_NULLABLE),
        Field::new("id", DataType::UInt64, NOT_NULLABLE),
        Field::new("name", DataType::Utf8, NULLABLE),
    ]
    .into()
}

fn struct_list_field(name: &str, fields: Fields) -> Field {
    Field::new_list(
        name,
        Field::new("item", DataType::Struct(fields), NOT_NULLABLE),
        NOT_NULLABLE,
    )
}

pub(super) fn schema() -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, NOT_NULLABLE),
        Field::new("action", DataType::Utf8, NOT_NULLABLE),
        string_list_field("topics"),
        Field::new_struct("osm", osm_fields(), NULLABLE),
        struct_list_field("changes", change_fields()),
        struct_list_field("sources", source_fields()),
        geo_field("geometry", NOT_NULLABLE),
        geo_field("atp_geometry", NULLABLE),
        Field::new_struct("host", host_fields(), NULLABLE),
        string_list_field("warnings"),
        Field::new("importance", DataType::Float32, NOT_NULLABLE),
    ])
}

/// Offsets for a list column whose rows have `lengths` items.
fn offsets(lengths: impl Iterator<Item = usize>) -> OffsetBuffer<i32> {
    OffsetBuffer::from_lengths(lengths)
}

fn list(field: &Field, lengths: impl Iterator<Item = usize>, values: ArrayRef) -> ArrayRef {
    let DataType::List(item) = field.data_type() else {
        panic!("{} is not a list", field.name());
    };
    Arc::new(ListArray::new(item.clone(), offsets(lengths), values, None))
}

fn strings<'a>(values: impl Iterator<Item = Option<&'a str>>) -> ArrayRef {
    let mut b = StringBuilder::new();
    for v in values {
        b.append_option(v);
    }
    Arc::new(b.finish())
}

fn string_list(field: &Field, rows: &[&[String]]) -> ArrayRef {
    list(
        field,
        rows.iter().map(|r| r.len()),
        strings(rows.iter().flat_map(|r| r.iter().map(|s| Some(s.as_str())))),
    )
}

fn osm_column(edits: &[Edit]) -> ArrayRef {
    let fields = osm_fields();
    let empty = OsmSide::default();
    let valid: Vec<bool> = edits.iter().map(|e| e.osm.is_some()).collect();
    let sides: Vec<&OsmSide> = edits
        .iter()
        .map(|e| e.osm.as_ref().unwrap_or(&empty))
        .collect();
    let mut tags = MapBuilder::new(None, StringBuilder::new(), StringBuilder::new());
    let mut timestamps = TimestampMillisecondBuilder::new().with_timezone("UTC");
    let mut versions = UInt32Builder::new();
    let mut ids = UInt64Builder::new();
    for osm in &sides {
        for (k, v) in &osm.tags {
            tags.keys().append_value(k);
            tags.values().append_value(v);
        }
        tags.append(true).expect("map append");
        timestamps.append_value(osm.timestamp_millis);
        versions.append_value(osm.version);
        ids.append_value(osm.osm_id);
    }
    let tags = tags.finish();
    let map_field = fields[2].clone();
    let DataType::Map(entries, sorted) = map_field.data_type() else {
        unreachable!()
    };
    let tags = arrow::array::MapArray::new(
        entries.clone(),
        tags.offsets().clone(),
        StructArray::new(
            tags_entry_fields(),
            vec![tags.keys().clone(), tags.values().clone()],
            None,
        ),
        None,
        *sorted,
    );
    let modified = StructArray::new(
        modified_fields(),
        vec![Arc::new(timestamps.finish()), Arc::new(versions.finish())],
        None,
    );

    let mut way_ids = UInt64Builder::new();
    let mut way_lengths = Vec::new();
    let mut way_valid = Vec::new();
    let mut member_types = StringBuilder::new();
    let mut member_ids = UInt64Builder::new();
    let mut member_roles = StringBuilder::new();
    let mut rel_lengths = Vec::new();
    let mut rel_valid = Vec::new();
    for osm in &sides {
        match &osm.way_members {
            Some(m) => {
                way_ids.append_slice(m);
                way_lengths.push(m.len());
                way_valid.push(true);
            }
            None => {
                way_lengths.push(0);
                way_valid.push(false);
            }
        }
        match &osm.relation_members {
            Some(m) => {
                for member in m {
                    member_types.append_value(&member.member_type);
                    member_ids.append_value(member.id);
                    member_roles.append_option(member.role.as_deref());
                }
                rel_lengths.push(m.len());
                rel_valid.push(true);
            }
            None => {
                rel_lengths.push(0);
                rel_valid.push(false);
            }
        }
    }
    let item = |f: &Field| match f.data_type() {
        DataType::List(item) => item.clone(),
        _ => unreachable!(),
    };
    let way_members = ListArray::new(
        item(&fields[4]),
        offsets(way_lengths.into_iter()),
        Arc::new(way_ids.finish()),
        Some(NullBuffer::from(way_valid)),
    );
    let relation_members = ListArray::new(
        item(&fields[5]),
        offsets(rel_lengths.into_iter()),
        Arc::new(StructArray::new(
            relation_member_fields(),
            vec![
                Arc::new(member_types.finish()),
                Arc::new(member_ids.finish()),
                Arc::new(member_roles.finish()),
            ],
            None,
        )),
        Some(NullBuffer::from(rel_valid)),
    );
    Arc::new(StructArray::new(
        fields,
        vec![
            strings(sides.iter().map(|o| Some(o.osm_type.as_str()))),
            Arc::new(ids.finish()),
            Arc::new(tags),
            Arc::new(modified),
            Arc::new(way_members),
            Arc::new(relation_members),
        ],
        Some(NullBuffer::from(valid)),
    ))
}

fn host_column(edits: &[Edit]) -> ArrayRef {
    let mut ids = UInt64Builder::new();
    for e in edits {
        ids.append_value(e.host.as_ref().map(|h| h.osm_id).unwrap_or(0));
    }
    Arc::new(StructArray::new(
        host_fields(),
        vec![
            strings(
                edits
                    .iter()
                    .map(|e| Some(e.host.as_ref().map(|h| h.osm_type.as_str()).unwrap_or(""))),
            ),
            Arc::new(ids.finish()),
            strings(
                edits
                    .iter()
                    .map(|e| e.host.as_ref().and_then(|h| h.name.as_deref())),
            ),
        ],
        Some(NullBuffer::from(
            edits
                .iter()
                .map(|e| e.host.is_some())
                .collect::<Vec<bool>>(),
        )),
    ))
}

fn changes_column(field: &Field, edits: &[Edit]) -> ArrayRef {
    let all = || edits.iter().flat_map(|e| e.changes.iter());
    let values = StructArray::new(
        change_fields(),
        vec![
            strings(all().map(|c| Some(c.key.as_str()))),
            strings(all().map(|c| c.old.as_deref())),
            strings(all().map(|c| c.new.as_deref())),
            strings(all().map(|c| Some(c.reason.as_str()))),
            strings(all().map(|c| c.atp.as_deref())),
        ],
        None,
    );
    list(
        field,
        edits.iter().map(|e| e.changes.len()),
        Arc::new(values),
    )
}

fn sources_column(field: &Field, edits: &[Edit]) -> ArrayRef {
    let all = || edits.iter().flat_map(|e| e.sources.iter());
    let mut fetched = TimestampMillisecondBuilder::new().with_timezone("UTC");
    let mut score = Float32Builder::new();
    let mut distance = Float32Builder::new();
    for s in all() {
        fetched.append_value(s.fetched_millis);
        score.append_option(s.score);
        distance.append_option(s.distance_m);
    }
    let values = StructArray::new(
        source_fields(),
        vec![
            strings(all().map(|s| Some(s.spider.as_str()))),
            Arc::new(fetched.finish()),
            strings(all().map(|s| s.atp_ref.as_deref())),
            strings(all().map(|s| s.source_url.as_deref())),
            strings(all().map(|s| Some(s.relation.as_str()))),
            strings(all().map(|s| s.rule.as_deref())),
            Arc::new(score.finish()),
            Arc::new(distance.finish()),
        ],
        None,
    );
    list(
        field,
        edits.iter().map(|e| e.sources.len()),
        Arc::new(values),
    )
}

fn batch(schema: &SchemaRef, edits: &[Edit]) -> Result<RecordBatch> {
    let field = |name: &str| schema.field_with_name(name).expect("field").clone();
    let mut geometry = BinaryBuilder::new();
    let mut atp_geometry = BinaryBuilder::new();
    let mut importance = Float32Builder::new();
    for e in edits {
        geometry.append_value(&e.geometry_wkb);
        atp_geometry.append_option(e.atp_geometry_wkb.as_deref());
        importance.append_value(e.importance);
    }
    let topics: Vec<Vec<String>> = edits
        .iter()
        .map(|e| e.topics.iter().map(|t| t.to_string()).collect())
        .collect();
    let topic_refs: Vec<&[String]> = topics.iter().map(Vec::as_slice).collect();
    let warnings: Vec<&[String]> = edits.iter().map(|e| e.warnings.as_slice()).collect();
    let columns: Vec<ArrayRef> = vec![
        strings(edits.iter().map(|e| Some(e.id.as_str()))),
        strings(edits.iter().map(|e| Some(e.action))),
        string_list(&field("topics"), &topic_refs),
        osm_column(edits),
        changes_column(&field("changes"), edits),
        sources_column(&field("sources"), edits),
        Arc::new(geometry.finish()),
        Arc::new(atp_geometry.finish()),
        host_column(edits),
        string_list(&field("warnings"), &warnings),
        Arc::new(importance.finish()),
    ];
    Ok(RecordBatch::try_new(schema.clone(), columns)?)
}

/// Writes `edits`, already in their final order, to `path`.
/// Writes `edits`, which come in the order of the file, a row group
/// at a time.
pub(super) fn write(
    path: &Path,
    edits: impl IntoIterator<Item = Result<Edit>>,
    provenance_bom: &str,
) -> Result<Stats> {
    let mut tmp = PathBuf::from(path);
    tmp.add_extension("tmp");
    let schema = SchemaRef::new(schema());
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(22)?))
        .set_max_row_group_row_count(Some(ROWS_PER_GROUP))
        .build();
    let options = ArrowWriterOptions::new().with_properties(properties);
    let mut writer =
        ArrowWriter::try_new_with_options(File::create(&tmp)?, schema.clone(), options)?;
    let mut stats = Stats::default();
    let (mut geometry_tally, mut atp_tally) = (GeometryTally::default(), GeometryTally::default());
    let mut chunk: Vec<Edit> = Vec::with_capacity(ROWS_PER_GROUP);
    for edit in edits {
        let edit = edit?;
        stats.edits += 1;
        stats.changes += edit.changes.len();
        for topic in &edit.topics {
            *stats.topics.entry(topic).or_default() += 1;
        }
        geometry_tally.record(&edit.geometry_wkb, || edit.id.clone());
        if let Some(wkb) = &edit.atp_geometry_wkb {
            atp_tally.record(wkb, || edit.id.clone());
        }
        chunk.push(edit);
        if chunk.len() == ROWS_PER_GROUP {
            writer.write(&batch(&schema, &chunk)?)?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        writer.write(&batch(&schema, &chunk)?)?;
    }
    let column = |tally: &GeometryTally| {
        json!({
            "encoding": "WKB",
            "geometry_types": tally.geoparquet_types(),
            "edges": "spherical",
        })
    };
    let geo = json!({
        "version": "2.0.0",
        "primary_column": "geometry",
        "columns": {
            "geometry": column(&geometry_tally),
            "atp_geometry": column(&atp_tally),
        },
    });
    writer.append_key_value_metadata(KeyValue::new(
        PROVENANCE_KEY.to_string(),
        provenance_bom.to_string(),
    ));
    writer.append_key_value_metadata(KeyValue::new("geo".to_string(), geo.to_string()));
    writer.append_key_value_metadata(KeyValue::new(
        FORMAT_STATUS_KEY.to_string(),
        "experimental".to_string(),
    ));
    writer.append_key_value_metadata(KeyValue::new(
        "format_notice".to_string(),
        DISCLAIMER.to_string(),
    ));
    writer.close()?;
    std::fs::rename(&tmp, path)?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::super::Reason;
    use super::*;
    use arrow::array::Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use tempfile::TempDir;

    fn point(x: f64, y: f64) -> Vec<u8> {
        crate::geometry::encode_wkb(&geo::Geometry::Point(geo::Point::new(x, y)))
    }

    #[test]
    fn writes_and_reads_back() -> Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().join("edits.parquet");
        let edit = Edit {
            id: "0123456789abcdef".into(),
            action: "modify",
            topics: vec!["contact"],
            osm: Some(OsmSide {
                osm_type: "way".into(),
                osm_id: 7,
                tags: vec![("shop".into(), "bakery".into())],
                timestamp_millis: 1_700_000_000_000,
                version: 3,
                way_members: Some(vec![1, 2, 3, 1]),
                relation_members: None,
            }),
            changes: vec![Change::new(
                "phone",
                None,
                Some("+41 44 123 45 67".into()),
                Reason::MissingInOsm,
            )],
            sources: vec![Source {
                spider: "bakery_ch".into(),
                relation: "same".into(),
                ..Source::default()
            }],
            geometry_wkb: point(8.5, 47.4),
            atp_geometry_wkb: Some(point(8.5001, 47.4)),
            host: None,
            warnings: vec![],
            importance: 0.5,
            s2_cell_id: 0,
        };
        let relation = Edit {
            id: "fedcba9876543210".into(),
            osm: Some(OsmSide {
                osm_type: "relation".into(),
                osm_id: 9,
                relation_members: Some(vec![RelationMember {
                    member_type: "way".into(),
                    id: 7,
                    role: Some("outer".into()),
                }]),
                ..OsmSide::default()
            }),
            atp_geometry_wkb: None,
            warnings: vec!["wikidata_unmatched".into()],
            ..edit.clone()
        };
        let create = Edit {
            id: "00112233445566aa".into(),
            action: "create",
            topics: vec!["new_poi"],
            osm: None,
            host: Some(Host {
                osm_type: "way".into(),
                osm_id: 11,
                name: Some("Target".into()),
            }),
            ..edit.clone()
        };
        let stats = write(&path, [edit, relation, create].map(Ok), "{}")?;
        assert_eq!(stats.changes, 3);
        assert_eq!(stats.topic("contact"), 2);
        assert_eq!(stats.topic("new_poi"), 1);

        let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(&path)?)?;
        let metadata = builder
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .cloned();
        let kv: BTreeMap<String, Option<String>> = metadata
            .into_iter()
            .flatten()
            .map(|kv| (kv.key, kv.value))
            .collect();
        assert_eq!(kv[FORMAT_STATUS_KEY].as_deref(), Some("experimental"));
        assert!(
            kv["geo"]
                .as_deref()
                .unwrap_or("")
                .contains("\"primary_column\":\"geometry\"")
        );
        let batches: Vec<RecordBatch> = builder.build()?.collect::<Result<_, _>>()?;
        let batch = &batches[0];
        assert_eq!(batch.num_rows(), 3);
        assert_eq!(batch.schema().fields().len(), 11);
        let osm = crate::utils::parquet::get_struct(batch, "osm")?;
        assert_eq!((osm.is_valid(0), osm.is_valid(2)), (true, false));
        let host = crate::utils::parquet::get_struct(batch, "host")?;
        assert_eq!((host.is_valid(0), host.is_valid(2)), (false, true));
        assert_eq!(crate::utils::parquet::get_u64(host, "id", 2)?, 11);
        Ok(())
    }
}
