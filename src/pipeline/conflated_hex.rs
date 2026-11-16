//! H3 hexagons for the overview zooms of `conflated.pmtiles`
//! ([#957](https://github.com/brawer/osmdiffs/issues/957)).
//!
//! At coarse zooms, there are far too many features to draw them all,
//! and thinning them out makes a busy area look empty. So instead, for
//! every overview feature, we compute its **point zoom**: the lowest
//! zoom at which it is shown as itself. Below that zoom, it is counted
//! in a hexagon of the [H3](https://h3geo.org/) grid, at a resolution
//! that keeps hexagons about the same size on screen at every zoom.
//!
//! A feature gets shown as a point at zoom `z` once its H3 cell at
//! that zoom's resolution holds at most [`MAX_FEATURES_PER_CELL`]
//! features, unless its tile at `z` already has
//! [`MAX_POINTS_PER_TILE`] points; then the most important features
//! win. Once shown, a feature stays shown at every finer zoom. Since
//! nothing is left for tippecanoe to drop, the overview pass keeps
//! every feature (see `tiles::Thinning::FromZoom`).
//!
//! The `hex` layer has, for each zoom and cell, one polygon with the
//! counts of the features in that cell that are still hidden at that
//! zoom. The `spider_hex` layer has the same per spider, so that a map
//! filtered to one spider can show where its hidden features are.
//!
//! Both layers are internal to osmdiffs-app; see
//! `docs/outputs/CONFLATED_TILES.md`.

use super::conflated_tiles::{DETAIL_MIN_ZOOM, tile_of};
use anyhow::Result;
use geo::{BooleanOps, Coord, LineString, MultiPolygon, Polygon, Rect};
use h3o::{CellIndex, LatLng, Resolution};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Write;

/// The tippecanoe layer name of the global hexagons.
pub(crate) const HEX_LAYER_NAME: &str = "hex";

/// The tippecanoe layer name of the per-spider hexagons.
pub(crate) const SPIDER_HEX_LAYER_NAME: &str = "spider_hex";

/// The property of a hexagon that holds the one zoom level at which it
/// is shown; see `tiles::Thinning::Never`.
pub(crate) const HEX_ZOOM_ATTR: &str = "z";

/// The property of an overview feature that holds its point zoom, the
/// lowest zoom at which it is shown; see `tiles::Thinning::FromZoom`.
pub(crate) const POINT_ZOOM_ATTR: &str = "pz";

/// Highest zoom of the overview, and thus of the hexagons.
pub(crate) const HEX_MAX_ZOOM: u8 = DETAIL_MIN_ZOOM - 1;

/// A feature is shown as a point once its cell holds at most this many
/// features. With 50, at most about 16,000 hexagons need drawing
/// worldwide at any zoom (2026-11-02 planet; see #957).
pub(crate) const MAX_FEATURES_PER_CELL: u32 = 50;

/// At most this many points in one overview tile. Without it, many
/// sparse cells would add up to tens of thousands of points in the
/// coarse tiles over Europe or the US; see #957.
pub(crate) const MAX_POINTS_PER_TILE: u32 = 2000;

/// The finest H3 resolution we use, that of [`HEX_MAX_ZOOM`].
const FINEST_RESOLUTION: Resolution = Resolution::Seven;

/// The H3 resolution of the hexagons at `zoom`. Each step makes
/// cells about 7 times smaller, and two zooms make tiles 16 times
/// smaller, so this keeps hexagons roughly the same size on screen.
/// At z0, a res 1 cell is a few pixels wide.
pub(crate) fn resolution_at_zoom(zoom: u8) -> Resolution {
    match zoom {
        0 => Resolution::One,
        1..=2 => Resolution::Two,
        3..=4 => Resolution::Three,
        5..=6 => Resolution::Four,
        7..=8 => Resolution::Five,
        9..=10 => Resolution::Six,
        _ => FINEST_RESOLUTION,
    }
}

/// How a feature counts in a hexagon, from its `match.relation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Same,
    MissingInOsm,
    LocatedIn,
    DeletionCandidate,
    /// Any other relation; counted in `n` only.
    Other,
}

impl Kind {
    pub(crate) fn from_relation(relation: &str) -> Kind {
        match relation {
            "same" => Kind::Same,
            "missing_in_osm" => Kind::MissingInOsm,
            "located_in" => Kind::LocatedIn,
            "deletion_candidate" => Kind::DeletionCandidate,
            _ => Kind::Other,
        }
    }
}

/// What [`point_zooms`] and [`HexCells`] need to know about one
/// overview feature.
#[derive(Clone, Debug)]
pub(crate) struct OverviewFeature {
    /// Where the feature is, and its cell at [`FINEST_RESOLUTION`].
    /// `None` for a feature without a usable position, which then is
    /// shown at every zoom.
    position: Option<((f64, f64), CellIndex)>,
    importance: f32,
    kind: Kind,
    /// Index into the spider names, `None` for deletion candidates.
    spider: Option<u32>,
}

impl OverviewFeature {
    pub(crate) fn new(
        position: Option<(f64, f64)>,
        importance: f32,
        kind: Kind,
        spider: Option<u32>,
    ) -> OverviewFeature {
        let position = position.and_then(|(lon, lat)| {
            let cell = LatLng::new(lat, lon).ok()?.to_cell(FINEST_RESOLUTION);
            Some(((lon, lat), cell))
        });
        OverviewFeature {
            position,
            importance,
            kind,
            spider,
        }
    }
}

/// The point zoom of every feature, in the same order: the lowest zoom
/// at which it is shown as a point. A feature that is still hidden at
/// [`HEX_MAX_ZOOM`] gets [`DETAIL_MIN_ZOOM`], where the detail layers
/// take over.
pub(crate) fn point_zooms(
    features: &[OverviewFeature],
    max_per_cell: u32,
    max_per_tile: u32,
) -> Vec<u8> {
    // Taking the parent of the finest cell, rather than the cell at each
    // resolution directly, makes cells nest: H3 children don't exactly
    // tile their parent. Then a cell's count is never more than its
    // parent's, so a feature that is shown at one zoom by this rule is
    // shown at every finer zoom too.
    let mut cell_counts: HashMap<CellIndex, u32> = HashMap::new();
    for feature in features {
        if let Some((_, cell)) = feature.position {
            for zoom in [0, 1, 3, 5, 7, 9, 11] {
                let parent = cell
                    .parent(resolution_at_zoom(zoom))
                    .expect("coarser resolution");
                *cell_counts.entry(parent).or_default() += 1;
            }
        }
    }

    let mut zooms: Vec<u8> = features
        .iter()
        .map(|f| {
            if f.position.is_some() {
                DETAIL_MIN_ZOOM
            } else {
                0
            }
        })
        .collect();
    for zoom in 0..=HEX_MAX_ZOOM {
        let resolution = resolution_at_zoom(zoom);
        // By tile: how many points it already shows from coarser
        // zooms, and which hidden features could become points now.
        let mut tiles: HashMap<(u32, u32), (u32, Vec<usize>)> = HashMap::new();
        for (i, feature) in features.iter().enumerate() {
            let Some(((lon, lat), cell)) = feature.position else {
                continue;
            };
            let tile = tiles.entry(tile_of(lon, lat, zoom)).or_default();
            if zooms[i] < zoom {
                tile.0 += 1;
            } else {
                let parent = cell.parent(resolution).expect("coarser resolution");
                if cell_counts[&parent] <= max_per_cell {
                    tile.1.push(i);
                }
            }
        }
        for (shown, mut candidates) in tiles.into_values() {
            let room = max_per_tile.saturating_sub(shown) as usize;
            if candidates.len() > room {
                // Most important first; ties by position in the input,
                // so the result is reproducible.
                candidates.sort_by(|&a, &b| {
                    features[b]
                        .importance
                        .total_cmp(&features[a].importance)
                        .then(a.cmp(&b))
                });
                candidates.truncate(room);
            }
            for i in candidates {
                zooms[i] = zoom;
            }
        }
    }
    zooms
}

#[derive(Debug, Default, PartialEq)]
struct HexCounts {
    n: u64,
    same: u64,
    missing_in_osm: u64,
    located_in: u64,
    deletion_candidate: u64,
}

impl HexCounts {
    fn add(&mut self, kind: Kind) {
        self.n += 1;
        match kind {
            Kind::Same => self.same += 1,
            Kind::MissingInOsm => self.missing_in_osm += 1,
            Kind::LocatedIn => self.located_in += 1,
            Kind::DeletionCandidate => self.deletion_candidate += 1,
            Kind::Other => {}
        }
    }
}

/// How many hexagons [`write_hex_cells`] wrote.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct HexStats {
    pub(crate) hex: u64,
    pub(crate) spider_hex: u64,
}

/// Writes the `hex` and `spider_hex` layers: for every zoom up to
/// [`HEX_MAX_ZOOM`] and every cell at that zoom's resolution, the
/// counts of the features in it that are still hidden at that zoom
/// (whose point zoom is higher), overall and per spider. `spiders`
/// holds the spider names that [`OverviewFeature::spider`] indexes.
pub(crate) fn write_hex_cells(
    features: &[OverviewFeature],
    point_zooms: &[u8],
    spiders: &[String],
    hex_out: &mut dyn Write,
    spider_hex_out: &mut dyn Write,
) -> Result<HexStats> {
    assert_eq!(features.len(), point_zooms.len());
    let mut stats = HexStats::default();
    for zoom in 0..=HEX_MAX_ZOOM {
        let resolution = resolution_at_zoom(zoom);
        let mut cells: HashMap<CellIndex, HexCounts> = HashMap::new();
        let mut spider_cells: HashMap<(u32, CellIndex), HexCounts> = HashMap::new();
        for (feature, &point_zoom) in features.iter().zip(point_zooms) {
            let Some((_, cell)) = feature.position else {
                continue;
            };
            if point_zoom <= zoom {
                continue;
            }
            let parent = cell.parent(resolution).expect("coarser resolution");
            cells.entry(parent).or_default().add(feature.kind);
            if let Some(spider) = feature.spider {
                spider_cells
                    .entry((spider, parent))
                    .or_default()
                    .add(feature.kind);
            }
        }

        let mut keys: Vec<CellIndex> = cells.keys().copied().collect();
        keys.sort();
        for cell in keys {
            if write_hex(hex_out, cell, None, zoom, &cells[&cell])? {
                stats.hex += 1;
            }
        }

        let mut keys: Vec<(u32, CellIndex)> = spider_cells.keys().copied().collect();
        keys.sort_by(|a, b| (&spiders[a.0 as usize], a.1).cmp(&(&spiders[b.0 as usize], b.1)));
        for key in keys {
            let spider = &spiders[key.0 as usize];
            if write_hex(
                spider_hex_out,
                key.1,
                Some(spider),
                zoom,
                &spider_cells[&key],
            )? {
                stats.spider_hex += 1;
            }
        }
    }
    Ok(stats)
}

/// Writes one hexagon as a GeoJSON Lines feature. Returns false, and
/// writes nothing, for a cell around a pole, which Web Mercator can't
/// show anyway.
fn write_hex(
    out: &mut dyn Write,
    cell: CellIndex,
    spider: Option<&str>,
    zoom: u8,
    counts: &HexCounts,
) -> Result<bool> {
    let Some(geometry) = cell_geometry(cell) else {
        return Ok(false);
    };
    let mut properties = Map::new();
    properties.insert("h3".to_string(), Value::String(cell.to_string()));
    if let Some(spider) = spider {
        properties.insert("spider".to_string(), Value::String(spider.to_string()));
    }
    properties.insert("n".to_string(), Value::from(counts.n));
    properties.insert("same".to_string(), Value::from(counts.same));
    properties.insert(
        "missing_in_osm".to_string(),
        Value::from(counts.missing_in_osm),
    );
    properties.insert("located_in".to_string(), Value::from(counts.located_in));
    if spider.is_none() {
        properties.insert(
            "deletion_candidate".to_string(),
            Value::from(counts.deletion_candidate),
        );
    }
    properties.insert(HEX_ZOOM_ATTR.to_string(), Value::from(zoom));
    let feature = geojson::Feature {
        bbox: None,
        geometry: Some(geojson::Geometry::from(&geometry)),
        id: None,
        properties: Some(properties),
        foreign_members: None,
    };
    writeln!(out, "{feature}")?;
    Ok(true)
}

/// The outline of `cell` in longitude and latitude, split in two at
/// the antimeridian if it crosses it. `None` for a cell around a pole.
fn cell_geometry(cell: CellIndex) -> Option<geo::Geometry<f64>> {
    let round = |v: f64| (v * 1e6).round() / 1e6;
    let mut coords: Vec<Coord<f64>> = cell
        .boundary()
        .iter()
        .map(|ll| Coord {
            x: round(ll.lng()),
            y: round(ll.lat()),
        })
        .collect();
    let span = |coords: &[Coord<f64>]| {
        let (min, max) = coords.iter().fold((f64::MAX, f64::MIN), |(lo, hi), c| {
            (lo.min(c.x), hi.max(c.x))
        });
        max - min
    };
    if span(&coords) <= 180.0 {
        coords.push(coords[0]);
        return Some(geo::Geometry::Polygon(Polygon::new(
            LineString::new(coords),
            vec![],
        )));
    }

    // Crosses the antimeridian: make longitudes continuous, then cut
    // what lies east of it back into the map.
    for c in &mut coords {
        if c.x < 0.0 {
            c.x += 360.0;
        }
    }
    if span(&coords) > 180.0 {
        return None; // around a pole
    }
    coords.push(coords[0]);
    let shape = Polygon::new(LineString::new(coords), vec![]);
    let shifted = Polygon::new(
        shape
            .exterior()
            .coords()
            .map(|c| Coord {
                x: c.x - 360.0,
                y: c.y,
            })
            .collect(),
        vec![],
    );
    let world = Rect::new(
        Coord {
            x: -180.0,
            y: -90.0,
        },
        Coord { x: 180.0, y: 90.0 },
    )
    .to_polygon();
    let mut parts: Vec<Polygon<f64>> = Vec::new();
    for p in [shape, shifted] {
        parts.extend(p.intersection(&world).0);
    }
    for p in &mut parts {
        use geo::MapCoordsInPlace;
        p.map_coords_in_place(|c| Coord {
            x: round(c.x),
            y: round(c.y),
        });
    }
    Some(geo::Geometry::MultiPolygon(MultiPolygon::new(parts)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(lon: f64, lat: f64, importance: f32) -> OverviewFeature {
        OverviewFeature::new(Some((lon, lat)), importance, Kind::MissingInOsm, Some(0))
    }

    #[test]
    fn resolution_steps_with_zoom() {
        assert_eq!(resolution_at_zoom(0), Resolution::One);
        assert_eq!(resolution_at_zoom(4), Resolution::Three);
        assert_eq!(resolution_at_zoom(9), Resolution::Six);
        assert_eq!(resolution_at_zoom(HEX_MAX_ZOOM), FINEST_RESOLUTION);
    }

    #[test]
    fn lone_feature_is_a_point_at_every_zoom() {
        let features = vec![feature(133.0, -25.0, 0.5)];
        assert_eq!(point_zooms(&features, 50, 2000), vec![0]);
    }

    #[test]
    fn feature_without_position_is_shown_everywhere() {
        let features = vec![OverviewFeature::new(None, 0.5, Kind::Same, None)];
        assert_eq!(point_zooms(&features, 1, 1), vec![0]);
    }

    #[test]
    fn crowded_cell_shows_points_once_cells_get_small() {
        // Three features about 2 km apart in Zurich, with at most two
        // per cell: hidden while one cell holds them all.
        let features = vec![
            feature(8.54, 47.37, 0.1),
            feature(8.54, 47.39, 0.2),
            feature(8.57, 47.37, 0.3),
        ];
        let zooms = point_zooms(&features, 2, 2000);
        assert!(zooms.iter().all(|&z| z > 0), "{zooms:?}");
        assert!(zooms.iter().all(|&z| z <= DETAIL_MIN_ZOOM), "{zooms:?}");
    }

    #[test]
    fn point_zooms_are_monotonic_and_respect_tile_limit() {
        // A grid of features, more than a tile allows at coarse zooms.
        let mut features = Vec::new();
        for i in 0..40 {
            for j in 0..40 {
                let lon = 5.0 + f64::from(i) * 0.1;
                let lat = 45.0 + f64::from(j) * 0.1;
                features.push(feature(lon, lat, ((i * 40 + j) % 97) as f32 / 97.0));
            }
        }
        let max_per_tile = 100;
        let zooms = point_zooms(&features, 50, max_per_tile);
        for zoom in 0..=HEX_MAX_ZOOM {
            let mut per_tile: HashMap<(u32, u32), u32> = HashMap::new();
            for (f, &z) in features.iter().zip(&zooms) {
                if z <= zoom {
                    let ((lon, lat), _) = f.position.unwrap();
                    *per_tile.entry(tile_of(lon, lat, zoom)).or_default() += 1;
                }
            }
            let max = per_tile.values().copied().max().unwrap_or(0);
            assert!(max <= max_per_tile, "z{zoom}: {max} points in a tile");
        }
        // Once shown, always shown: checked by construction, since a
        // feature's point zoom is a single number.
        assert!(
            zooms.iter().any(|&z| z > 0 && z <= HEX_MAX_ZOOM),
            "{zooms:?}"
        );
    }

    #[test]
    fn full_tile_shows_the_most_important_features() {
        let features: Vec<OverviewFeature> = (0..10)
            .map(|i| feature(8.5 + f64::from(i) * 0.01, 47.4, i as f32 / 10.0))
            .collect();
        let zooms = point_zooms(&features, 1000, 3);
        let shown: Vec<usize> = (0..10).filter(|&i| zooms[i] == 0).collect();
        assert_eq!(shown, vec![7, 8, 9]);
    }

    #[test]
    fn hexagons_count_hidden_features_only() {
        let features = vec![
            OverviewFeature::new(Some((8.54, 47.37)), 0.1, Kind::Same, Some(0)),
            OverviewFeature::new(Some((8.54, 47.371)), 0.2, Kind::MissingInOsm, Some(1)),
            OverviewFeature::new(Some((8.541, 47.37)), 0.3, Kind::DeletionCandidate, None),
        ];
        // Hidden until z11, then shown.
        let zooms = vec![11, 11, 11];
        let spiders = vec!["a".to_string(), "b".to_string()];
        let mut hex = Vec::new();
        let mut spider_hex = Vec::new();
        let stats =
            write_hex_cells(&features, &zooms, &spiders, &mut hex, &mut spider_hex).expect("write");
        // One cell at each of z0..=10, and per spider.
        assert_eq!(
            stats,
            HexStats {
                hex: 11,
                spider_hex: 22
            }
        );

        let hex = String::from_utf8(hex).unwrap();
        let first: Value = serde_json::from_str(hex.lines().next().unwrap()).unwrap();
        let props = &first["properties"];
        assert_eq!(props["n"], 3);
        assert_eq!(props["same"], 1);
        assert_eq!(props["missing_in_osm"], 1);
        assert_eq!(props["deletion_candidate"], 1);
        assert_eq!(props["z"], 0);
        assert_eq!(first["geometry"]["type"], "Polygon");
        let cell: CellIndex = props["h3"].as_str().unwrap().parse().unwrap();
        assert_eq!(cell.resolution(), Resolution::One);

        let spider_hex = String::from_utf8(spider_hex).unwrap();
        let first: Value = serde_json::from_str(spider_hex.lines().next().unwrap()).unwrap();
        assert_eq!(first["properties"]["spider"], "a");
        assert_eq!(first["properties"]["n"], 1);
        assert!(first["properties"].get("deletion_candidate").is_none());
    }

    #[test]
    fn cell_across_antimeridian_is_split() {
        let cell = LatLng::new(-17.0, 180.0).unwrap().to_cell(Resolution::Two);
        let geometry = cell_geometry(cell).expect("geometry");
        let geo::Geometry::MultiPolygon(parts) = geometry else {
            panic!("expected a multipolygon, got {geometry:?}");
        };
        assert_eq!(parts.0.len(), 2);
        for p in &parts.0 {
            for c in p.exterior().coords() {
                assert!((-180.0..=180.0).contains(&c.x), "{c:?}");
            }
        }
    }

    #[test]
    fn cell_around_pole_is_skipped() {
        let cell = LatLng::new(90.0, 0.0).unwrap().to_cell(Resolution::One);
        assert!(cell_geometry(cell).is_none());
    }
}
