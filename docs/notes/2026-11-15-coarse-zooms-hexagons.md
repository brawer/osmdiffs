# Coarse zooms: hexagons instead of thinned points, and stopping at z14

This note is about how `conflated.pmtiles` looks when zoomed out, and
about how big it is. It records a discussion and the numbers behind it.
Only one part is implemented so far: the detail pass now stops at z14
instead of z16 (see the last section). The hexagon layer is a proposal,
a follow-up to [#949](https://github.com/brawer/osmdiffs/issues/949).

All numbers come from the 2026-11-02 planet run:
`conflated-20261102-a4a70f0f.parquet` and
`conflated-20261102-0e8c8fa5.pmtiles`.

## The problem

At z13 and below, the overview thins busy tiles with
`--drop-densest-as-needed` and the importance filter (#713). Thinning
hides information: a sparse map reads as "nothing here" when it really
means "too much here". On a spider page this is worse, since the
spider's few points may not survive thinning at all. Tweaking the
thinning doesn't fix this. Seeing the delta between OpenStreetMap and
AllThePlaces needs individual features, and these only make sense at
deep zooms.

The obvious fix is to aggregate: at coarse zooms, show per-area counts
of matched, missing and deletion candidate features instead of points.
H3 hexagons are a natural choice of area. Two concerns:

- It introduces a visual jump between the aggregated and the point
  view.
- There is no single good cut-off zoom. In rural Australia, a lone
  feature could be shown as a point at much coarser zooms than any
  feature in central London.

## Proposal

**Several H3 resolutions, not one.** An H3 res 6 cell is about 36 km²,
which suits z9–10, but would be a speck at z4. Stepping the resolution
with the zoom keeps hexagons roughly the same size on screen, so
zooming in looks like cells splitting into children:

| Zoom | H3 resolution |
|---|---|
| ≤ z4 | 3 |
| z5–6 | 4 |
| z7–8 | 5 |
| z9–10 | 6 |
| z11–12 | 7 |
| z13 | 8 |

**A cut-off per cell, not per map.** For each feature, the pipeline
computes the lowest zoom at which its cell (at that zoom's resolution)
holds at most N features, and shows the feature as a point from that
zoom on. A hexagon is drawn only where its features are still hidden.
Each part of the map switches from hexagons to points when it has
room to, and a short crossfade in the client softens the change. To
guarantee nesting, a feature's cell at resolution r should be the
parent of its finest cell, not `latLngToCell` at r directly; H3
children don't tile their parent exactly.

**What a hexagon shows.** Color by the share of missing versus matched
features, and let the total drive opacity or a label. Coloring by the
total alone would just show where people live. On a spider page, the
counts are for that spider only.

**Alternatives considered:**

- *Clustered circles with counts.* Familiar, but they hide the
  matched/missing ratio, which is what this map is about.
- *A heatmap layer.* Can't be read precisely, and can't be clicked.
- *Choropleth by country or region.* "Victoria: 72% matched" means
  something to people, but regions vary hugely in size, and it doesn't
  show where inside a region the gaps are.

## Numbers

The planet has 3,686,474 features with a position: 2,087,038 missing in
OpenStreetMap, 1,418,431 matched (`same`), 164,352 deletion candidates
and 16,653 `located_in`, from 4,103 spiders. Each feature was placed at
the point-on-surface of its OpenStreetMap geometry, or of its
AllThePlaces geometry if there is no OpenStreetMap feature.

### Features per H3 cell

| Res | Occupied cells | Median | p90 | p99 | Max | Cell × spider pairs |
|---|---:|---:|---:|---:|---:|---:|
| 2 | 2,016 | 48 | 3,190 | 33,685 | 105,608 | 100,692 |
| 3 | 7,761 | 21 | 855 | 8,509 | 56,506 | 260,925 |
| 4 | 28,278 | 10 | 236 | 2,203 | 28,986 | 587,476 |
| 5 | 89,951 | 5 | 78 | 656 | 17,120 | 1,087,425 |
| 6 | 226,776 | 3 | 34 | 224 | 8,526 | 1,672,958 |
| 7 | 483,343 | 2 | 19 | 78 | 2,369 | 2,232,245 |
| 8 | 930,843 | 2 | 9 | 31 | 683 | 2,663,628 |

The distribution is very skewed: most cells hold a handful of features,
a few hold thousands.

### Per-cell cut-off

Share of all features shown as points, with the zoom-to-resolution
mapping above:

| Zoom | N = 25 | N = 50 | N = 100 |
|---|---:|---:|---:|
| z3 | 0.7% | 1.4% | 2.6% |
| z5 | 3.1% | 5.7% | 10.2% |
| z7 | 11.1% | 17.5% | 27.2% |
| z9 | 25.2% | 37.0% | 52.9% |
| z11 | 49.8% | 70.8% | 88.3% |
| z13 | 83.5% | 94.1% | 98.2% |

At any zoom, at most about 34,000 hexagons need drawing worldwide for
N = 25, and about 16,000 for N = 50.

With N = 50, by region:

| Zoom | Central London | Zurich | Rural Australia |
|---|---:|---:|---:|
| z3 | 0% | 0% | 9% |
| z5 | 0% | 0% | 22% |
| z7 | 0% | 0% | 36% |
| z9 | 0% | 0% | 50% |
| z11 | 0% | 0% | 72% |
| z13 | 22% | 5% | 91% |

"Rural Australia" is Australia (including Tasmania) more than 0.8° from the
five largest cities, so it still includes country towns. City centres
need res 8 or 9 at z14 for points to take over.

**Catch: a limit per cell doesn't limit tile size.** Many sparse cells
add up inside one coarse tile, such as in Europe or the US. The fullest
tile, in points shown:

| Zoom | N = 25 | N = 50 | N = 100 |
|---|---:|---:|---:|
| z3 | 3,376 | 6,450 | 13,654 |
| z5 | 3,293 | 6,255 | 15,769 |
| z7 | 2,261 | 3,411 | 6,508 |
| z9 | 985 | 1,462 | 3,415 |

With N = 50, 34 tiles at z5 and 65 at z7 would hold more than 2,000
points. So coarse zooms need a second limit per tile, or a smaller N.

## Transport: vector tiles, not a GeoTIFF

One idea was a Cloud-Optimized GeoTIFF with one channel per count,
shifting every second row to fit hexagons onto a raster. This doesn't
work well:

- **H3 isn't a flat hex grid.** It's built on an icosahedron, so the
  grid's orientation changes from face to face, there are 12 pentagons,
  and child cells are rotated against their parents. Offset rows only
  fit a hex grid defined in a flat projection, such as Web Mercator,
  and that would no longer be H3.
- **MapLibre can't style counts in a GeoTIFF.** The client would need
  geotiff.js and a custom WebGL layer.
- **Spider pages don't fit a raster.** 4,103 spiders times three counts
  is too many channels. As sparse rows, the data is small: 1.67 million
  cell × spider pairs at res 6, and only cells still shown as hexagons
  need writing.

Instead, add a `hex` layer to `conflated.pmtiles`: one polygon per
cell, with its H3 index and counts, never dropped. At 16,000–34,000
cells per zoom it's cheap, and MapLibre styles it with ordinary fill
expressions. It could also replace the `spider_cells` layer (#949).

## File size

Bytes per zoom in the 2026-11-02 `conflated.pmtiles` (4.06 GB), from
its PMTiles directory:

| Zooms | Tiles | Size | Share |
|---|---:|---:|---:|
| z0–12 | 380,672 | 350 MB | 8.6% |
| z13 | 359,672 | 622 MB | 15.3% |
| z14 | 595,144 | 793 MB | 19.5% |
| z15 | 949,481 | 1,012 MB | 24.9% |
| z16 | 1,463,329 | 1,281 MB | 31.6% |

Hexagons would replace most of the 350 MB of z0–12, minus the points
the per-cell cut-off keeps. That's a saving of roughly 250–320 MB, or
about 7%. It's not the big lever.

### This archive predates #918

The 2026-11-02 archive was built before #918 switched the detail pass
to `-r 1`. Counting the AllThePlaces points inside one z13 tile and
inside its descendants confirms the loss #918 describes:

| Area | z13 | z14 | z15 | z16 |
|---|---:|---:|---:|---:|
| Central London | 669 | 803 | 964 | 1,157 |
| Zurich | 1,953 | 2,345 | 2,817 | 3,379 |
| Manhattan | 802 | 957 | 1,161 | 1,375 |
| Dubbo, NSW | 25 | 28 | 37 | 44 |

So z13 holds about 58% of the points, z14 69% and z15 83%, even in a
small town where no tile is anywhere near the size limit. Only z16 is
complete. From #918 on, every detail zoom holds every feature.

### Stopping at z14

With every feature at every detail zoom, z15 and z16 only repeat z14 in
smaller tiles. Scaling each zoom of the 2026-11-02 archive by its
missing points gives an estimate for a build after #918:

| Detail range | z13 | z14 | z15 | z16 | Whole archive |
|---|---:|---:|---:|---:|---:|
| z13–16 | 1.07 GB | 1.15 GB | 1.22 GB | 1.28 GB | ≈ 5.1 GB |
| z13–14 | 1.07 GB | 1.15 GB | — | — | ≈ 2.6 GB |

These are estimates from four sampled areas, not a real build. With
hexagons on top, the archive would come to about 2.3 GB.

Why z14 is enough:

- **Precision.** A z14 tile is 4,096 units across, so positions resolve
  to about 0.6 m at the equator and 0.4 m in London. Connector lines are
  only drawn from 5 m (`MIN_CONNECTOR_LENGTH_METERS`).
- **Overzoom.** MapLibre takes the source's max zoom from the PMTiles
  header and overzooms beyond it. The app's detail layers only set
  `minzoom: 13`, and fitting the map to a spider already stops at z14.
- **Lookups.** The app's fallback for shared links
  (osmdiffs-app#18) reads the deepest zoom from the header, so it
  follows the change on its own.
- **Tile size.** The largest z13 tile is about 240 KB gzipped (#916),
  and z14 tiles are smaller, well under tippecanoe's 500 KB limit.

`DETAIL_MAX_ZOOM` is therefore now 14.

## Method

The counts come from a Python script with `pyarrow`, `shapely` and
`h3`, reading `conflated.parquet` directly. The per-zoom sizes come from
walking the PMTiles directory over HTTP range requests with the
`pmtiles` package. The point counts per tile come from decoding tiles
with `mapbox-vector-tile`, counting only `part: "atp"` points inside
the tile, not in its buffer.
