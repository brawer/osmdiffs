//! Modifies of `brand:wikidata`, `operator:wikidata` and
//! `network:wikidata`, so that their IDs line up with the names value
//! by value; see "Modify: Wikidata IDs" in
//! `docs/notes/2026-11-23-edits-parquet.md`. The names themselves never
//! change. `post_office:brand:wikidata` is up to the postal-partner
//! rules.

use super::{Change, Reason, Tags};
use crate::matchers::brand_names::{NsiNames, name_key};
use crate::matchers::parse_wikidata_id;
use std::collections::HashMap;

/// The keys whose Wikidata IDs we add or line up.
pub(super) const KEYS: [&str; 3] = ["brand", "operator", "network"];

fn get<'a>(tags: &'a Tags, key: &str) -> Option<&'a str> {
    tags.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.trim().is_empty())
}

/// The parts of a `;`-separated value.
fn parts(value: &str) -> Vec<&str> {
    value
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

/// What the AllThePlaces `sources` say the Wikidata ID of each name of
/// `key` is, by [name_key]. Names that sources give different IDs for
/// map to `None`.
fn ids_from_sources(key: &str, sources: &[&Tags]) -> HashMap<String, Option<u64>> {
    let mut ids: HashMap<String, Option<u64>> = HashMap::new();
    let id_key = format!("{key}:wikidata");
    for tags in sources {
        let (Some(names), Some(values)) = (get(tags, key), get(tags, &id_key)) else {
            continue;
        };
        let (names, values) = (parts(names), parts(values));
        if names.len() != values.len() {
            continue;
        }
        for (name, value) in names.iter().zip(values) {
            let Some(id) = parse_wikidata_id(value) else {
                continue;
            };
            ids.entry(name_key(name))
                .and_modify(|old| {
                    if *old != Some(id) {
                        *old = None;
                    }
                })
                .or_insert(Some(id));
        }
    }
    ids
}

/// The Wikidata ID changes for an OSM feature with tags `osm` at
/// `position`, given the tags of the AllThePlaces `sources` that
/// matched it as `same`. Uses NSI to tell which of OSM's own IDs
/// belongs to which name. Adds `wikidata_unmatched` to `warnings` for a
/// list it would have to drop an OSM ID from.
pub(super) fn wikidata_changes(
    osm: &Tags,
    sources: &[&Tags],
    nsi: &NsiNames,
    position: Option<(f64, f64)>,
    warnings: &mut Vec<String>,
) -> Vec<Change> {
    let mut changes = Vec::new();
    for key in KEYS {
        let Some(names) = get(osm, key).map(parts) else {
            continue;
        };
        let id_key = format!("{key}:wikidata");
        let old = get(osm, &id_key);
        let old_ids: Vec<Option<u64>> = old
            .map(|v| parts(v).into_iter().map(parse_wikidata_id).collect())
            .unwrap_or_default();
        if old_ids.len() == names.len() {
            continue;
        }
        let from_atp = ids_from_sources(key, sources);
        let mut new_ids = Vec::with_capacity(names.len());
        for name in &names {
            let name = name_key(name);
            let mut id = from_atp.get(&name).copied().flatten();
            if id.is_none()
                && let Some((lng, lat)) = position
            {
                // One of OSM's own IDs whose NSI names include this one.
                id = old_ids.iter().flatten().copied().find(|osm_id| {
                    let mut nsi_names = Vec::new();
                    nsi.add_names(&[*osm_id], None, lng, lat, &mut nsi_names);
                    nsi_names.contains(&name)
                });
            }
            new_ids.push(id);
        }
        let Some(new_ids) = new_ids.into_iter().collect::<Option<Vec<u64>>>() else {
            continue;
        };
        if old_ids
            .iter()
            .any(|id| id.is_none_or(|id| !new_ids.contains(&id)))
        {
            warnings.push("wikidata_unmatched".to_string());
            continue;
        }
        let new = new_ids
            .iter()
            .map(|id| format!("Q{id}"))
            .collect::<Vec<_>>()
            .join(";");
        let reason = if old.is_some() {
            Reason::Misaligned
        } else {
            Reason::MissingInOsm
        };
        changes.push(Change::new(&id_key, old, Some(new), reason));
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    type Triple = (String, Option<String>, Option<String>);

    fn tags(t: &[(&str, &str)]) -> Vec<(String, String)> {
        t.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn run(osm: &[(&str, &str)], atp: &[&[(&str, &str)]]) -> (Vec<Triple>, Vec<String>) {
        let atp: Vec<Vec<(String, String)>> = atp.iter().map(|t| tags(t)).collect();
        let refs: Vec<&Tags> = atp.iter().collect();
        let mut warnings = Vec::new();
        let changes =
            wikidata_changes(&tags(osm), &refs, &NsiNames::default(), None, &mut warnings)
                .into_iter()
                .map(|c| (c.key, c.old, c.new))
                .collect();
        (changes, warnings)
    }

    #[test]
    fn adds_missing_id() {
        let (changes, _) = run(
            &[("brand", "Coop"), ("shop", "supermarket")],
            &[&[("brand", "COOP"), ("brand:wikidata", "Q432564")]],
        );
        assert_eq!(
            changes,
            [("brand:wikidata".into(), None, Some("Q432564".into()))]
        );
    }

    #[test]
    fn leaves_lined_up_ids_alone() {
        let (changes, _) = run(
            &[("brand", "Coop"), ("brand:wikidata", "Q1")],
            &[&[("brand", "Coop"), ("brand:wikidata", "Q432564")]],
        );
        assert!(changes.is_empty());
    }

    #[test]
    fn lines_up_ids_of_several_brands() {
        let audi: &[(&str, &str)] = &[("brand", "Audi"), ("brand:wikidata", "Q23317")];
        let vw: &[(&str, &str)] = &[("brand", "Volkswagen"), ("brand:wikidata", "Q246")];
        let (changes, _) = run(
            &[("brand", "Audi;Volkswagen"), ("brand:wikidata", "Q23317")],
            &[vw, audi],
        );
        assert_eq!(
            changes,
            [(
                "brand:wikidata".into(),
                Some("Q23317".into()),
                Some("Q23317;Q246".into())
            )]
        );
        // Without an ID for every name, nothing.
        let (changes, _) = run(&[("brand", "Audi;Volkswagen")], &[audi]);
        assert!(changes.is_empty());
    }

    #[test]
    fn never_drops_an_osm_id() {
        let (changes, warnings) = run(
            &[("brand", "Coop"), ("brand:wikidata", "Q1;Q2")],
            &[&[("brand", "Coop"), ("brand:wikidata", "Q1")]],
        );
        assert!(changes.is_empty());
        assert_eq!(warnings, ["wikidata_unmatched"]);
    }

    #[test]
    fn sources_that_disagree_give_nothing() {
        let (changes, _) = run(
            &[("operator", "Stadt Zürich")],
            &[
                &[("operator", "Stadt Zürich"), ("operator:wikidata", "Q72")],
                &[("operator", "Stadt Zürich"), ("operator:wikidata", "Q73")],
            ],
        );
        assert!(changes.is_empty());
    }

    #[test]
    fn needs_osm_name() {
        // OSM has no brand: adding one would be a name edit.
        let (changes, _) = run(
            &[("name", "Coop Bahnhof")],
            &[&[("brand", "Coop"), ("brand:wikidata", "Q432564")]],
        );
        assert!(changes.is_empty());
    }
}
