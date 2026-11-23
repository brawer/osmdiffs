//! The reference-count pass for deletes: which ways and relations use
//! a few given OSM elements, and whether those elements have tags.
//!
//! An OSM node doesn't say which ways or relations use it, nor a way
//! which relations, so whether a deletion candidate can be deleted
//! can't be told from the candidate alone. This pass reads the ways,
//! relations and nodes of the planet file once, and keeps what it
//! learns only for the elements in [Wanted]. See `pipeline::edits` and
//! `docs/notes/2026-11-23-edits-parquet.md`.

use super::BlobReader;
use crate::make_progress_bar;
use anyhow::{Context, Result};
use indicatif::MultiProgress;
use osm_pbf_iter::{Blob, Primitive, PrimitiveBlock, RelationMemberType};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::sync_channel;
use std::thread;

/// The type of an OSM element, as a key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ElementType {
    Node,
    Way,
    Relation,
}

impl From<RelationMemberType> for ElementType {
    fn from(t: RelationMemberType) -> Self {
        match t {
            RelationMemberType::Node => ElementType::Node,
            RelationMemberType::Way => ElementType::Way,
            RelationMemberType::Relation => ElementType::Relation,
        }
    }
}

/// Which part of the planet file a pass reads.
#[derive(Clone, Copy)]
enum Section {
    Nodes,
    Ways,
    Relations,
}

/// The elements that the pass looks up.
#[derive(Debug, Default)]
pub(crate) struct Wanted {
    pub nodes: HashSet<u64>,
    pub ways: HashSet<u64>,
    pub relations: HashSet<u64>,
    /// Ways whose node references and tags the pass should return,
    /// such as the members of a relation that may be deleted. Their
    /// nodes become wanted too.
    pub way_details: HashSet<u64>,
}

/// A way of [Wanted::way_details].
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WayDetails {
    pub refs: Vec<u64>,
    pub tagged: bool,
    pub version: u32,
}

/// A node of [Wanted::nodes], or of a way of [Wanted::way_details].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct NodeDetails {
    pub tagged: bool,
    pub version: u32,
}

/// What the pass found.
#[derive(Debug, Default)]
pub(crate) struct References {
    /// For each wanted node, the ways that use it.
    pub node_ways: HashMap<u64, Vec<u64>>,
    /// For each wanted element, the relations that have it as a member.
    pub parents: HashMap<(ElementType, u64), Vec<u64>>,
    pub ways: HashMap<u64, WayDetails>,
    pub nodes: HashMap<u64, NodeDetails>,
}

/// Whether a tag says anything about an element. Editor metadata
/// such as `created_by` doesn't.
pub(crate) fn is_meaningful_tag(key: &str) -> bool {
    !matches!(
        key,
        "created_by" | "source" | "converted_by" | "odbl" | "odbl:note"
    )
}

/// Calls `f` on every blob that `send` reads, in parallel.
fn for_each_blob<F>(reader: &mut BlobReader<File>, section: Section, f: F) -> Result<()>
where
    F: Fn(&PrimitiveBlock) + Sync,
{
    thread::scope(|s| {
        let num_workers = usize::from(thread::available_parallelism()?);
        let (tx, rx) = sync_channel::<Blob>(num_workers);
        let producer = s.spawn(move || match section {
            Section::Nodes => reader.send_node_blobs(tx),
            Section::Ways => reader.send_way_blobs(tx),
            Section::Relations => reader.send_relation_blobs(tx),
        });
        rx.into_iter().par_bridge().for_each(|blob| {
            let data = blob.into_data();
            f(&PrimitiveBlock::parse(&data));
        });
        producer.join().expect("panic in blob producer")
    })
}

/// Runs the pass over the planet file at `pbf`.
pub(crate) fn scan_references(
    pbf: &Path,
    mut wanted: Wanted,
    progress: &MultiProgress,
) -> Result<References> {
    let mut file = File::open(pbf).with_context(|| format!("could not open {pbf:?}"))?;
    let mut reader = BlobReader::open(&mut file)?;
    let bar = make_progress_bar(progress, "edits.references", 0, "blobs");
    let mut refs = References::default();

    // The ways whose nodes we need, before counting who uses them.
    if !wanted.way_details.is_empty() {
        bar.inc_length(reader.count_way_blobs() as u64);
        let ways = Mutex::new(HashMap::new());
        for_each_blob(&mut reader, Section::Ways, |block| {
            let mut found = Vec::new();
            for primitive in block.primitives() {
                if let Primitive::Way(way) = primitive
                    && wanted.way_details.contains(&way.id)
                {
                    let details = WayDetails {
                        refs: way.refs().filter(|&r| r > 0).map(|r| r as u64).collect(),
                        tagged: way.tags().any(|(k, _)| is_meaningful_tag(k)),
                        version: way.info.as_ref().and_then(|i| i.version).unwrap_or(0),
                    };
                    found.push((way.id, details));
                }
            }
            ways.lock().expect("ways lock").extend(found);
            bar.inc(1);
        })?;
        refs.ways = ways.into_inner().expect("ways lock");
        for details in refs.ways.values() {
            wanted.nodes.extend(details.refs.iter().copied());
        }
    }

    // Which ways use the wanted nodes.
    bar.inc_length(reader.count_way_blobs() as u64);
    let node_ways: Mutex<HashMap<u64, Vec<u64>>> = Mutex::new(HashMap::new());
    for_each_blob(&mut reader, Section::Ways, |block| {
        let mut found: Vec<(u64, u64)> = Vec::new();
        for primitive in block.primitives() {
            if let Primitive::Way(way) = primitive {
                for r in way.refs() {
                    if r > 0 && wanted.nodes.contains(&(r as u64)) {
                        found.push((r as u64, way.id));
                    }
                }
            }
        }
        if !found.is_empty() {
            let mut node_ways = node_ways.lock().expect("node_ways lock");
            for (node, way) in found {
                let ways = node_ways.entry(node).or_default();
                // A closed way lists its first node twice.
                if !ways.contains(&way) {
                    ways.push(way);
                }
            }
        }
        bar.inc(1);
    })?;
    refs.node_ways = node_ways.into_inner().expect("node_ways lock");

    // Which relations use the wanted elements.
    bar.inc_length(reader.count_relation_blobs() as u64);
    let parents: Mutex<HashMap<(ElementType, u64), Vec<u64>>> = Mutex::new(HashMap::new());
    let is_wanted = |t: ElementType, id: u64| match t {
        ElementType::Node => wanted.nodes.contains(&id),
        ElementType::Way => wanted.ways.contains(&id) || wanted.way_details.contains(&id),
        ElementType::Relation => wanted.relations.contains(&id),
    };
    for_each_blob(&mut reader, Section::Relations, |block| {
        let mut found = Vec::new();
        for primitive in block.primitives() {
            if let Primitive::Relation(rel) = primitive {
                for (_, id, member_type) in rel.members() {
                    let member_type = ElementType::from(member_type);
                    if is_wanted(member_type, id) {
                        found.push(((member_type, id), rel.id));
                    }
                }
            }
        }
        if !found.is_empty() {
            let mut parents = parents.lock().expect("parents lock");
            for (member, rel) in found {
                let rels = parents.entry(member).or_default();
                if !rels.contains(&rel) {
                    rels.push(rel);
                }
            }
        }
        bar.inc(1);
    })?;
    refs.parents = parents.into_inner().expect("parents lock");

    // Whether the wanted nodes have tags, and their versions, which an
    // OSMChange delete needs.
    bar.inc_length(reader.count_node_blobs() as u64);
    let nodes = Mutex::new(HashMap::new());
    for_each_blob(&mut reader, Section::Nodes, |block| {
        let mut found = Vec::new();
        for primitive in block.primitives() {
            if let Primitive::Node(node) = primitive
                && wanted.nodes.contains(&node.id)
            {
                let details = NodeDetails {
                    tagged: node.tags.iter().any(|(k, _)| is_meaningful_tag(k)),
                    version: node.info.as_ref().and_then(|i| i.version).unwrap_or(0),
                };
                found.push((node.id, details));
            }
        }
        if !found.is_empty() {
            nodes.lock().expect("nodes lock").extend(found);
        }
        bar.inc(1);
    })?;
    refs.nodes = nodes.into_inner().expect("nodes lock");
    for list in refs.node_ways.values_mut() {
        list.sort_unstable();
    }
    for list in refs.parents.values_mut() {
        list.sort_unstable();
    }
    bar.finish();
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn zugerland() -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/test_data/zugerland.osm.pbf");
        path
    }

    /// Finds, in the test file, the first way with its first node, and
    /// the first relation with a way member in the file.
    fn sample() -> Result<(u64, u64, u64, u64)> {
        let mut file = File::open(zugerland())?;
        let mut reader = BlobReader::open(&mut file)?;
        let ways: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());
        for_each_blob(&mut reader, Section::Ways, |block| {
            for p in block.primitives() {
                if let Primitive::Way(way) = p
                    && let Some(node) = way.refs().next()
                {
                    ways.lock().unwrap().push((way.id, node as u64));
                }
            }
        })?;
        let rels: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());
        for_each_blob(&mut reader, Section::Relations, |block| {
            for p in block.primitives() {
                if let Primitive::Relation(rel) = p {
                    for (_, id, t) in rel.members() {
                        if t == RelationMemberType::Way {
                            rels.lock().unwrap().push((rel.id, id));
                        }
                    }
                }
            }
        })?;
        let ways = ways.into_inner().unwrap();
        let (way, node) = *ways.iter().min().expect("a way");
        // The file is an extract, so not every member is in it.
        let in_file: HashSet<u64> = ways.iter().map(|(w, _)| *w).collect();
        let (rel, member) = *rels
            .into_inner()
            .unwrap()
            .iter()
            .filter(|(_, m)| in_file.contains(m))
            .min()
            .expect("a relation");
        Ok((way, node, rel, member))
    }

    #[test]
    fn finds_users_and_details() -> Result<()> {
        let (way, node, rel, member) = sample()?;
        let wanted = Wanted {
            nodes: HashSet::from([node]),
            ways: HashSet::from([way]),
            way_details: HashSet::from([member]),
            ..Wanted::default()
        };
        let refs = scan_references(&zugerland(), wanted, &MultiProgress::new())?;
        assert!(refs.node_ways[&node].contains(&way));
        assert!(refs.parents[&(ElementType::Way, member)].contains(&rel));
        let details = &refs.ways[&member];
        assert!(!details.refs.is_empty());
        assert!(details.version > 0);
        // The member way's nodes are looked up too.
        for r in &details.refs {
            assert!(refs.nodes.contains_key(r), "node {r}");
            assert!(refs.node_ways[r].contains(&member));
        }
        assert!(refs.nodes[&node].version > 0);
        Ok(())
    }
}
