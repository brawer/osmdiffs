//! The presets of [id-tagging-schema](https://github.com/openstreetmap/id-tagging-schema),
//! which the iD editor uses to tell what kind of place an OpenStreetMap
//! element is, and which icon to draw for it. `edits.pmtiles` takes the
//! `icon` of each edit from here; see `pipeline::edits_tiles` and the
//! plan in `docs/notes/2026-11-28-edits-map.md`.
//!
//! id-tagging-schema publishes `dist/presets.min.json` to npm with
//! every release; [super::npm] fetches the latest.
//!
//! Matching follows iD's `presetIndex.match` in a simplified form,
//! without iD's location sets and fallback presets: each preset whose
//! `tags` all match scores `matchScore` (1 by default) per tag with the
//! same value, half that per `*` tag, and `matchScore` again per
//! matching tag of `addTags`; the highest score wins. So a preset with
//! more matching tags wins, and ties between presets with as many go to
//! the higher `matchScore`.
//!
//! We keep only [Maki](https://github.com/mapbox/maki) and
//! [Temaki](https://github.com/rapideditor/temaki) icons, which are
//! CC0 and which the web app turns into a sprite. When the winning
//! preset has another icon, such as a Font Awesome one, its icon is that
//! of the nearest parent preset (`amenity/fast_food` for
//! `amenity/fast_food/burger`) that has a Maki or Temaki icon, or none.

use super::npm::NpmFile;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// The file we fetch.
pub(crate) const ID_PRESETS: NpmFile = NpmFile {
    package: "@openstreetmap/id-tagging-schema",
    file_in_package: "/dist/presets.min.json",
    filename: "id-presets.json",
    meta_filename: "id-presets.meta.json",
    label: "presets.fetch ",
};

/// id-tagging-schema's license, as an SPDX ID; for the provenance BOM.
pub(crate) const LICENSE: &str = "ISC";

/// id-tagging-schema's license text.
pub(crate) const LICENSE_URL: &str =
    "https://github.com/openstreetmap/id-tagging-schema/blob/main/LICENSE.md";

/// The base name of the BOM component, for [crate::pipeline::provenance].
pub(crate) const BOM_REF: &str = ID_PRESETS.filename;

/// Where the pipeline keeps the presets in `workdir`.
pub(crate) fn path(workdir: &Path) -> PathBuf {
    ID_PRESETS.path(workdir)
}

/// What kind of shape an element has, in iD's terms; a preset applies
/// only to the geometries it lists.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Geometry {
    /// A node, or a create, which is always a node.
    Point,
    /// A way, which a place is drawn as when it is mapped as a building
    /// or an area.
    Way,
    /// A relation, such as a multipolygon.
    Relation,
}

impl Geometry {
    /// The geometry of an element of OpenStreetMap type `osm_type`.
    pub(crate) fn of_osm_type(osm_type: &str) -> Geometry {
        match osm_type {
            "way" => Geometry::Way,
            "relation" => Geometry::Relation,
            _ => Geometry::Point,
        }
    }

    /// Whether a preset with iD's `geometry` list applies to this one.
    fn is_in(self, geometry: &[String]) -> bool {
        let names: &[&str] = match self {
            Geometry::Point => &["point", "vertex"],
            Geometry::Way => &["area", "line"],
            Geometry::Relation => &["area", "relation"],
        };
        geometry.iter().any(|g| names.contains(&g.as_str()))
    }
}

/// One preset, as `presets.min.json` gives it; only what matching reads.
#[derive(Deserialize)]
struct RawPreset {
    #[serde(default)]
    tags: BTreeMap<String, String>,
    #[serde(default, rename = "addTags")]
    add_tags: BTreeMap<String, String>,
    #[serde(default)]
    geometry: Vec<String>,
    #[serde(rename = "matchScore")]
    match_score: Option<f64>,
    icon: Option<String>,
    #[serde(rename = "locationSet")]
    location_set: Option<LocationSet>,
}

#[derive(Deserialize)]
struct LocationSet {
    #[serde(default)]
    include: Vec<String>,
}

struct Preset {
    tags: Vec<(String, String)>,
    add_tags: Vec<(String, String)>,
    geometry: Vec<String>,
    match_score: f64,
    /// The Maki or Temaki icon to draw, after falling back to parents.
    icon: Option<String>,
}

impl Preset {
    /// iD's score of this preset for an element with `tags`, or `None`
    /// if it doesn't apply.
    fn score(&self, tags: &HashMap<&str, &str>) -> Option<f64> {
        let mut score = 0.0;
        for (k, v) in &self.tags {
            match tags.get(k.as_str()) {
                Some(value) if value == v => score += self.match_score,
                Some(_) if v == "*" => score += self.match_score / 2.0,
                _ => return None,
            }
        }
        for (k, v) in &self.add_tags {
            if !self.tags.iter().any(|(key, _)| key == k)
                && tags.get(k.as_str()) == Some(&v.as_str())
            {
                score += self.match_score;
            }
        }
        Some(score)
    }
}

/// The presets of id-tagging-schema, indexed by the keys they need.
pub(crate) struct Presets {
    presets: Vec<Preset>,
    /// For each key, the presets whose `tags` have it.
    by_key: HashMap<String, Vec<usize>>,
}

/// Whether `icon` is from one of the icon sets we draw.
fn is_drawn_icon(icon: &str) -> bool {
    icon.starts_with("maki-") || icon.starts_with("temaki-")
}

impl Presets {
    /// Reads `presets.min.json` from `path`.
    pub(crate) fn open(path: &Path) -> Result<Presets> {
        let data =
            std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        Presets::parse(&data).with_context(|| format!("could not parse {}", path.display()))
    }

    fn parse(data: &[u8]) -> Result<Presets> {
        let raw: BTreeMap<String, RawPreset> = serde_json::from_slice(data)?;
        let drawn_icon = |id: &str| {
            raw.get(id)
                .and_then(|p| p.icon.as_deref())
                .filter(|icon| is_drawn_icon(icon))
        };
        let mut presets = Vec::new();
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        for (id, preset) in &raw {
            // Templates only lend fields to other presets, and presets
            // without tags (`point`, `area`) match everything.
            if id.starts_with('@') || preset.tags.is_empty() {
                continue;
            }
            // Presets for a few countries, such as `amenity/townhall/barangay-PH`.
            if let Some(set) = &preset.location_set
                && !set.include.iter().any(|l| l == "Planet" || l == "001")
            {
                continue;
            }
            let mut icon = None;
            let mut ancestor = id.as_str();
            loop {
                if let Some(found) = drawn_icon(ancestor) {
                    icon = Some(found.to_string());
                    break;
                }
                match ancestor.rsplit_once('/') {
                    Some((parent, _)) => ancestor = parent,
                    None => break,
                }
            }
            let index = presets.len();
            for key in preset.tags.keys() {
                by_key.entry(key.clone()).or_default().push(index);
            }
            presets.push(Preset {
                tags: preset.tags.clone().into_iter().collect(),
                add_tags: preset.add_tags.clone().into_iter().collect(),
                geometry: preset.geometry.clone(),
                match_score: preset.match_score.unwrap_or(1.0),
                icon,
            });
        }
        Ok(Presets { presets, by_key })
    }

    /// The Maki or Temaki icon, such as `maki-cafe`, of the preset that
    /// best matches an element with `tags` and `geometry`, or an empty
    /// string if no preset matches or the best has no such icon.
    pub(crate) fn icon(&self, tags: &[(String, String)], geometry: Geometry) -> &str {
        let tags: HashMap<&str, &str> =
            tags.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut best: Option<(f64, usize)> = None;
        for key in tags.keys() {
            for &index in self.by_key.get(*key).into_iter().flatten() {
                let preset = &self.presets[index];
                if !geometry.is_in(&preset.geometry) {
                    continue;
                }
                let Some(score) = preset.score(&tags) else {
                    continue;
                };
                // A tie goes to the preset listed first, which is the
                // first by id, so that the result doesn't depend on the
                // order of `tags`.
                let better = match best {
                    None => true,
                    Some((best_score, best_index)) => {
                        score > best_score || (score == best_score && index < best_index)
                    }
                };
                if better {
                    best = Some((score, index));
                }
            }
        }
        best.and_then(|(_, index)| self.presets[index].icon.as_deref())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESETS: &str = r#"{
        "@templates/poi": {"tags": {}, "geometry": ["point"], "locationSet": {"include": ["Planet"]}},
        "point": {"tags": {}, "geometry": ["point"], "matchScore": 0.1},
        "amenity": {"tags": {"amenity": "*"}, "geometry": ["point", "area"], "icon": "maki-marker", "matchScore": 0.5},
        "amenity/cafe": {"tags": {"amenity": "cafe"}, "geometry": ["point", "area"], "icon": "maki-cafe"},
        "amenity/fast_food": {"tags": {"amenity": "fast_food"}, "geometry": ["point", "area"], "icon": "maki-fast-food"},
        "amenity/fast_food/burger": {"tags": {"amenity": "fast_food", "cuisine": "burger"}, "geometry": ["point", "area"], "icon": "fas-burger"},
        "amenity/townhall": {"tags": {"amenity": "townhall"}, "geometry": ["point", "area"], "icon": "temaki-town_hall"},
        "amenity/townhall/barangay-PH": {"tags": {"amenity": "townhall", "townhall:type": "barangay"}, "geometry": ["point", "area"], "icon": "maki-town-hall", "locationSet": {"include": ["ph"]}},
        "amenity/bench": {"tags": {"amenity": "bench"}, "geometry": ["point", "line"], "icon": "temaki-bench"},
        "amenity/post_box": {"tags": {"amenity": "post_box"}, "geometry": ["point"], "icon": "fas-envelope"},
        "craft/brewery": {"tags": {"craft": "brewery"}, "geometry": ["point", "area"], "icon": "fas-beer"},
        "shop": {"tags": {"shop": "*"}, "geometry": ["point", "area"], "icon": "maki-shop", "matchScore": 0.7},
        "shop/supermarket": {"tags": {"shop": "supermarket"}, "geometry": ["point", "area"], "icon": "maki-grocery"},
        "shop/convenience": {"tags": {"shop": "convenience"}, "addTags": {"shop": "convenience", "fuel": "yes"}, "geometry": ["point", "area"], "icon": "maki-shop"},
        "building": {"tags": {"building": "*"}, "geometry": ["area"], "icon": "maki-building", "matchScore": 0.6}
    }"#;

    fn tags(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn icon(pairs: &[(&str, &str)], geometry: Geometry) -> String {
        let presets = Presets::parse(PRESETS.as_bytes()).expect("valid presets");
        presets.icon(&tags(pairs), geometry).to_string()
    }

    #[test]
    fn exact_value_beats_wildcard() {
        assert_eq!(icon(&[("amenity", "cafe")], Geometry::Point), "maki-cafe");
        assert_eq!(
            icon(&[("amenity", "library")], Geometry::Point),
            "maki-marker"
        );
        assert_eq!(
            icon(
                &[("shop", "supermarket"), ("name", "Denner")],
                Geometry::Way
            ),
            "maki-grocery"
        );
    }

    #[test]
    fn more_matching_tags_win() {
        // A shop in a building: `shop=*` (0.35) beats `building=*` (0.3).
        assert_eq!(
            icon(&[("shop", "bakery"), ("building", "yes")], Geometry::Way),
            "maki-shop"
        );
        assert_eq!(
            icon(&[("building", "yes")], Geometry::Relation),
            "maki-building"
        );
    }

    #[test]
    fn falls_back_to_parent_icon() {
        assert_eq!(
            icon(
                &[("amenity", "fast_food"), ("cuisine", "burger")],
                Geometry::Point
            ),
            "maki-fast-food"
        );
        assert_eq!(
            icon(&[("amenity", "post_box")], Geometry::Point),
            "maki-marker"
        );
        // No parent with a Maki or Temaki icon.
        assert_eq!(icon(&[("craft", "brewery")], Geometry::Point), "");
    }

    #[test]
    fn checks_geometry() {
        assert_eq!(icon(&[("building", "yes")], Geometry::Point), "");
        assert_eq!(icon(&[("amenity", "bench")], Geometry::Way), "temaki-bench");
        assert_eq!(
            icon(&[("amenity", "post_box")], Geometry::Way),
            "maki-marker"
        );
    }

    #[test]
    fn skips_regional_presets() {
        assert_eq!(
            icon(
                &[("amenity", "townhall"), ("townhall:type", "barangay")],
                Geometry::Point
            ),
            "temaki-town_hall"
        );
    }

    #[test]
    fn no_match() {
        assert_eq!(icon(&[], Geometry::Point), "");
        assert_eq!(icon(&[("name", "Foo")], Geometry::Point), "");
    }

    #[test]
    fn of_osm_type() {
        assert_eq!(Geometry::of_osm_type("node"), Geometry::Point);
        assert_eq!(Geometry::of_osm_type("way"), Geometry::Way);
        assert_eq!(Geometry::of_osm_type("relation"), Geometry::Relation);
    }
}
