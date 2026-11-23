//! Deletes of OSM features that are likely gone, from the
//! `deletion_candidate` rows of `conflated.parquet`.
//!
//! What a delete does depends on the element:
//!
//! - An element that a relation uses is never deleted, nor stripped
//!   of its tags: a bus stop in a route or a shop in a site would
//!   leave the relation broken. It gets a `needs_review` edit instead.
//! - An element that keeps something besides its POI tags, such as a
//!   building, an address with a house number, or an entrance, loses
//!   its POI tags and keeps the rest ([is_poi_tag]). A bare way or
//!   relation that would keep only an address gets a `needs_review`
//!   edit.
//! - A node that a way uses loses its tags and stays in the way.
//! - Any other node is deleted.
//! - A bare way is deleted together with its nodes that have no tags
//!   and that no other way or relation uses. If one of its nodes has
//!   tags, such as an entrance, a person decides whether the way is a
//!   building, so it gets a `needs_review` edit.
//! - A bare relation is deleted together with its members, and their
//!   nodes, that have no tags and that nothing else uses. One with
//!   relations among its members gets a `needs_review` edit.
//!
//! Which ways and relations use an element comes from the
//! reference-count pass, [crate::pipeline::osm::references].

use super::writer::{DeletedMember, OsmSide};
use super::{Change, Reason};
use crate::pipeline::osm::references::{ElementType, References, is_meaningful_tag};
use std::collections::HashSet;

/// Whether removing a deleted business takes away tag `key`. We keep
/// what describes the structure or the place rather than the
/// business: the building, the address, entrances, the land use.
/// Everything else goes, since a business has many tags of its own
/// (`cuisine`, `payment:*`, `fuel:*`) that are no use without it.
pub(crate) fn is_poi_tag(key: &str, value: &str) -> bool {
    let kept_prefix = [
        "addr:", "building", "roof:", "source", "note", "fixme", "FIXME",
    ];
    if kept_prefix.iter().any(|p| key.starts_with(p)) {
        return false;
    }
    match key {
        "type" | "created_by" | "height" | "min_height" | "ele" | "layer" | "level" | "indoor"
        | "area" | "landuse" | "man_made" | "natural" | "barrier" | "entrance" | "door" => false,
        // A road or a railway stays; a stop or station on it goes.
        "highway" => matches!(
            value,
            "bus_stop" | "platform" | "services" | "rest_area" | "elevator"
        ),
        "railway" => matches!(
            value,
            "station" | "halt" | "stop" | "tram_stop" | "platform" | "subway_entrance"
        ),
        _ => true,
    }
}

/// Whether tag `key`, once the POI tags are gone, still says what the
/// element is. Metadata such as `source` or `note` doesn't, nor do
/// an address on its own or a level or elevation: a shop node in a
/// mall with only `level=1` left would mean nothing. Nor does
/// `indoor=yes`, which only says that the place was inside; a room
/// (`indoor=room`) does.
fn is_structure(key: &str, value: &str) -> bool {
    is_meaningful_tag(key)
        && !(key == "indoor" && value == "yes")
        && !matches!(
            key,
            "type" | "level" | "ele" | "layer" | "height" | "min_height"
        )
        && !key.starts_with("addr:")
        && !key.starts_with("source")
        && !key.starts_with("note")
        && !key.to_lowercase().starts_with("fixme")
}

/// Whether the tags hold an address of their own: a house number or
/// name. A street and city alone only say roughly where a business
/// was, and go with it.
fn has_address(tags: &[(String, String)]) -> bool {
    tags.iter()
        .any(|(k, _)| k == "addr:housenumber" || k == "addr:housename")
}

/// What to do with a deletion candidate.
#[derive(Debug, PartialEq)]
pub(crate) struct DeleteDecision {
    /// `delete`, or `modify` when the element stays.
    pub action: &'static str,
    pub changes: Vec<Change>,
    /// For a delete, the members that go with the element.
    pub deleted_members: Vec<DeletedMember>,
    pub warnings: Vec<String>,
}

fn changes(
    tags: &[(String, String)],
    take: impl Fn(&str, &str) -> bool,
    review: bool,
) -> Vec<Change> {
    let mut changes: Vec<Change> = tags
        .iter()
        .filter(|(k, v)| take(k, v))
        .map(|(k, v)| {
            if review {
                Change::new(k, Some(v), Some(v.clone()), Reason::NeedsReview)
            } else {
                Change::new(k, Some(v), None, Reason::Obsolete)
            }
        })
        .collect();
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    changes
}

/// An edit that a person decides on: the POI tags as they are, so
/// that applying it does nothing.
pub(crate) fn review(osm: &OsmSide, warning: &str) -> DeleteDecision {
    DeleteDecision {
        action: "modify",
        changes: changes(&osm.tags, is_poi_tag, true),
        deleted_members: Vec::new(),
        warnings: vec![warning.to_string()],
    }
}

/// Removes the POI tags, and with them any metadata if nothing else
/// stays.
fn strip(osm: &OsmSide) -> DeleteDecision {
    let keeps_something = osm
        .tags
        .iter()
        .any(|(k, v)| !is_poi_tag(k, v) && is_structure(k, v))
        || has_address(&osm.tags);
    let take = |k: &str, v: &str| is_poi_tag(k, v) || !keeps_something;
    DeleteDecision {
        action: "modify",
        changes: changes(&osm.tags, take, false),
        deleted_members: Vec::new(),
        warnings: Vec::new(),
    }
}

fn delete(osm: &OsmSide, deleted_members: Vec<DeletedMember>) -> DeleteDecision {
    DeleteDecision {
        action: "delete",
        changes: changes(&osm.tags, |_, _| true, false),
        deleted_members,
        warnings: Vec::new(),
    }
}

fn member_type(osm_type: &str) -> Option<ElementType> {
    match osm_type {
        "node" => Some(ElementType::Node),
        "way" => Some(ElementType::Way),
        "relation" => Some(ElementType::Relation),
        _ => None,
    }
}

/// The relations that use (`t`, `id`), other than `except`.
fn parents_except(refs: &References, t: ElementType, id: u64, except: Option<u64>) -> usize {
    refs.parents
        .get(&(t, id))
        .map_or(0, |p| p.iter().filter(|&&r| Some(r) != except).count())
}

/// The nodes in `nodes` that have no tags, and that no way outside
/// `deleted_ways` and no relation other than `relation` uses, in
/// order and without repeats.
fn free_nodes(
    nodes: &[u64],
    deleted_ways: &HashSet<u64>,
    relation: Option<u64>,
    refs: &References,
    out: &mut Vec<DeletedMember>,
    seen: &mut HashSet<u64>,
) {
    for &node in nodes {
        if !seen.insert(node) {
            continue;
        }
        let Some(details) = refs.nodes.get(&node) else {
            continue;
        };
        let other_ways = refs
            .node_ways
            .get(&node)
            .is_some_and(|ways| ways.iter().any(|w| !deleted_ways.contains(w)));
        if details.tagged
            || other_ways
            || parents_except(refs, ElementType::Node, node, relation) > 0
        {
            continue;
        }
        out.push(DeletedMember {
            member_type: "node",
            id: node,
            version: details.version,
        });
    }
}

/// Decides what to do with the deletion candidate `osm`.
pub(crate) fn decide_delete(osm: &OsmSide, refs: &References) -> Option<DeleteDecision> {
    let t = member_type(&osm.osm_type)?;
    if parents_except(refs, t, osm.osm_id, None) > 0 {
        return Some(review(osm, "relation_member"));
    }
    let has_structure = osm
        .tags
        .iter()
        .any(|(k, v)| !is_poi_tag(k, v) && is_structure(k, v));
    let has_address = has_address(&osm.tags);
    match t {
        ElementType::Node => {
            let in_way = refs
                .node_ways
                .get(&osm.osm_id)
                .is_some_and(|w| !w.is_empty());
            if has_structure || has_address || in_way {
                Some(strip(osm))
            } else {
                Some(delete(osm, Vec::new()))
            }
        }
        ElementType::Way => {
            if has_structure {
                return Some(strip(osm));
            }
            if has_address {
                return Some(review(osm, "address_only"));
            }
            let nodes = osm.way_members.as_deref().unwrap_or_default();
            if nodes
                .iter()
                .any(|n| refs.nodes.get(n).is_some_and(|d| d.tagged))
            {
                return Some(review(osm, "tagged_member_node"));
            }
            let mut members = Vec::new();
            let ways = HashSet::from([osm.osm_id]);
            free_nodes(nodes, &ways, None, refs, &mut members, &mut HashSet::new());
            Some(delete(osm, members))
        }
        ElementType::Relation => {
            if has_structure {
                return Some(strip(osm));
            }
            if has_address {
                return Some(review(osm, "address_only"));
            }
            let members = osm.relation_members.as_deref().unwrap_or_default();
            if members.iter().any(|m| m.member_type == "relation") {
                return Some(review(osm, "has_subrelation"));
            }
            let me = Some(osm.osm_id);
            let mut deleted_ways = HashSet::new();
            let mut out = Vec::new();
            let mut seen_ways = HashSet::new();
            for m in members.iter().filter(|m| m.member_type == "way") {
                if !seen_ways.insert(m.id) {
                    continue;
                }
                let Some(way) = refs.ways.get(&m.id) else {
                    continue;
                };
                if way.tagged || parents_except(refs, ElementType::Way, m.id, me) > 0 {
                    continue;
                }
                deleted_ways.insert(m.id);
                out.push(DeletedMember {
                    member_type: "way",
                    id: m.id,
                    version: way.version,
                });
            }
            let mut seen = HashSet::new();
            let direct: Vec<u64> = members
                .iter()
                .filter(|m| m.member_type == "node")
                .map(|m| m.id)
                .collect();
            free_nodes(&direct, &deleted_ways, me, refs, &mut out, &mut seen);
            for m in members.iter().filter(|m| m.member_type == "way") {
                if deleted_ways.contains(&m.id) {
                    let refs_of_way = &refs.ways[&m.id].refs;
                    free_nodes(refs_of_way, &deleted_ways, me, refs, &mut out, &mut seen);
                }
            }
            Some(delete(osm, out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::writer::RelationMember;
    use super::*;
    use crate::pipeline::osm::references::{NodeDetails, WayDetails};

    fn side(osm_type: &str, id: u64, tags: &[(&str, &str)]) -> OsmSide {
        OsmSide {
            osm_type: osm_type.to_string(),
            osm_id: id,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            version: 3,
            ..OsmSide::default()
        }
    }

    fn node(refs: &mut References, id: u64, tagged: bool, ways: &[u64]) {
        refs.nodes.insert(id, NodeDetails { tagged, version: 2 });
        if !ways.is_empty() {
            refs.node_ways.insert(id, ways.to_vec());
        }
    }

    fn keys(d: &DeleteDecision) -> Vec<(&str, Option<&str>)> {
        d.changes
            .iter()
            .map(|c| (c.key.as_str(), c.new.as_deref()))
            .collect()
    }

    fn members(d: &DeleteDecision) -> Vec<(&str, u64)> {
        d.deleted_members
            .iter()
            .map(|m| (m.member_type, m.id))
            .collect()
    }

    const SHOP: [(&str, &str); 3] = [("shop", "bakery"), ("name", "Beck"), ("brand", "Beck")];

    #[test]
    fn deletes_a_lone_node() {
        let d = decide_delete(&side("node", 1, &SHOP), &References::default()).unwrap();
        assert_eq!(d.action, "delete");
        assert_eq!(keys(&d), [("brand", None), ("name", None), ("shop", None)]);
        assert!(d.changes.iter().all(|c| c.reason == Reason::Obsolete));
    }

    #[test]
    fn a_street_name_alone_is_no_address() {
        let mut tags = SHOP.to_vec();
        tags.extend([("addr:street", "Hauptstrasse"), ("addr:city", "Zug")]);
        let d = decide_delete(&side("node", 1, &tags), &References::default()).unwrap();
        assert_eq!(d.action, "delete");
    }

    #[test]
    fn a_node_with_an_address_stays() {
        let mut tags = SHOP.to_vec();
        tags.extend([("addr:street", "Hauptstrasse"), ("addr:housenumber", "1")]);
        tags.push(("source", "survey"));
        let d = decide_delete(&side("node", 1, &tags), &References::default()).unwrap();
        assert_eq!(d.action, "modify");
        assert_eq!(keys(&d), [("brand", None), ("name", None), ("shop", None)]);
    }

    #[test]
    fn a_level_alone_is_nothing() {
        let mut tags = SHOP.to_vec();
        tags.extend([("level", "1"), ("ele", "470")]);
        let d = decide_delete(&side("node", 1, &tags), &References::default()).unwrap();
        assert_eq!(d.action, "delete");
        tags.push(("indoor", "yes"));
        let d = decide_delete(&side("node", 1, &tags), &References::default()).unwrap();
        assert_eq!(d.action, "delete");
        tags.pop();
        tags.push(("indoor", "room"));
        let d = decide_delete(&side("node", 1, &tags), &References::default()).unwrap();
        assert_eq!(d.action, "modify");
    }

    #[test]
    fn a_way_vertex_loses_all_its_tags() {
        let mut refs = References::default();
        node(&mut refs, 1, true, &[9]);
        let mut tags = SHOP.to_vec();
        tags.push(("source", "survey"));
        let d = decide_delete(&side("node", 1, &tags), &refs).unwrap();
        assert_eq!(d.action, "modify");
        assert_eq!(d.changes.len(), 4);
        assert!(d.changes.iter().all(|c| c.new.is_none()));
    }

    #[test]
    fn relation_members_need_review() {
        let mut refs = References::default();
        refs.parents.insert((ElementType::Node, 1), vec![77]);
        let d = decide_delete(&side("node", 1, &SHOP), &refs).unwrap();
        assert_eq!(d.action, "modify");
        assert_eq!(d.warnings, ["relation_member"]);
        assert!(d.changes.iter().all(|c| c.new == c.old));
        assert!(d.changes.iter().all(|c| c.reason == Reason::NeedsReview));
        assert!(d.deleted_members.is_empty());
    }

    #[test]
    fn a_building_keeps_its_structure() {
        let mut tags = SHOP.to_vec();
        tags.extend([
            ("building", "retail"),
            ("building:levels", "2"),
            ("addr:city", "Zug"),
            ("opening_hours", "Mo-Fr 08:00-18:00"),
        ]);
        let d = decide_delete(&side("way", 5, &tags), &References::default()).unwrap();
        assert_eq!(d.action, "modify");
        assert_eq!(
            keys(&d),
            [
                ("brand", None),
                ("name", None),
                ("opening_hours", None),
                ("shop", None)
            ]
        );
    }

    #[test]
    fn deletes_a_bare_way_with_its_free_nodes() {
        let mut refs = References::default();
        node(&mut refs, 1, false, &[5]);
        node(&mut refs, 2, false, &[5, 6]); // shared with way 6
        node(&mut refs, 3, false, &[5]);
        refs.parents.insert((ElementType::Node, 3), vec![70]);
        node(&mut refs, 4, false, &[5]);
        let mut way = side("way", 5, &SHOP);
        way.way_members = Some(vec![1, 2, 3, 4, 1]);
        let d = decide_delete(&way, &refs).unwrap();
        assert_eq!(d.action, "delete");
        assert_eq!(members(&d), [("node", 1), ("node", 4)]);
        assert_eq!(d.deleted_members[0].version, 2);
    }

    #[test]
    fn a_bare_way_with_a_door_needs_review() {
        let mut refs = References::default();
        node(&mut refs, 1, false, &[5]);
        node(&mut refs, 2, true, &[5]);
        let mut way = side("way", 5, &SHOP);
        way.way_members = Some(vec![1, 2, 1]);
        let d = decide_delete(&way, &refs).unwrap();
        assert_eq!(d.warnings, ["tagged_member_node"]);
        assert_eq!(d.action, "modify");
    }

    #[test]
    fn a_bare_way_with_an_address_needs_review() {
        let mut tags = SHOP.to_vec();
        tags.push(("addr:housenumber", "7"));
        let d = decide_delete(&side("way", 5, &tags), &References::default()).unwrap();
        assert_eq!(d.warnings, ["address_only"]);
    }

    fn multipolygon(members: &[(&str, u64)]) -> OsmSide {
        let mut tags = SHOP.to_vec();
        tags.push(("type", "multipolygon"));
        let mut rel = side("relation", 9, &tags);
        rel.relation_members = Some(
            members
                .iter()
                .map(|(t, id)| RelationMember {
                    member_type: t.to_string(),
                    id: *id,
                    role: Some("outer".into()),
                })
                .collect(),
        );
        rel
    }

    #[test]
    fn deletes_a_bare_relation_with_its_free_members() {
        let mut refs = References::default();
        // Way 5 is free; way 6 has tags; way 7 is also in relation 99.
        let way = |refs: &mut References, id: u64, tagged: bool, nodes: &[u64]| {
            refs.ways.insert(
                id,
                WayDetails {
                    refs: nodes.to_vec(),
                    tagged,
                    version: 4,
                },
            );
            refs.parents.insert((ElementType::Way, id), vec![9]);
        };
        way(&mut refs, 5, false, &[1, 2, 1]);
        way(&mut refs, 6, true, &[2, 3, 2]);
        way(&mut refs, 7, false, &[4, 4]);
        refs.parents.insert((ElementType::Way, 7), vec![9, 99]);
        node(&mut refs, 1, false, &[5]);
        node(&mut refs, 2, false, &[5, 6]);
        node(&mut refs, 3, false, &[6]);
        node(&mut refs, 4, false, &[7]);
        let rel = multipolygon(&[("way", 5), ("way", 6), ("way", 7)]);
        let d = decide_delete(&rel, &refs).unwrap();
        assert_eq!(d.action, "delete");
        assert_eq!(members(&d), [("way", 5), ("node", 1)]);
        assert!(d.changes.iter().any(|c| c.key == "type"));
    }

    #[test]
    fn a_relation_of_relations_needs_review() {
        let rel = multipolygon(&[("relation", 5)]);
        let d = decide_delete(&rel, &References::default()).unwrap();
        assert_eq!(d.warnings, ["has_subrelation"]);
    }

    #[test]
    fn a_member_of_another_relation_is_kept() {
        let mut refs = References::default();
        refs.parents.insert((ElementType::Relation, 9), vec![100]);
        let d = decide_delete(&multipolygon(&[("way", 5)]), &refs).unwrap();
        assert_eq!(d.warnings, ["relation_member"]);
    }

    #[test]
    fn test_is_poi_tag() {
        for (k, v) in [
            ("shop", "bakery"),
            ("highway", "bus_stop"),
            ("railway", "station"),
            ("public_transport", "platform"),
            ("cuisine", "pizza"),
            ("wheelchair", "yes"),
        ] {
            assert!(is_poi_tag(k, v), "{k}={v}");
        }
        for (k, v) in [
            ("building", "yes"),
            ("building:levels", "3"),
            ("addr:street", "x"),
            ("highway", "residential"),
            ("entrance", "main"),
            ("type", "multipolygon"),
        ] {
            assert!(!is_poi_tag(k, v), "{k}={v}");
        }
    }
}
