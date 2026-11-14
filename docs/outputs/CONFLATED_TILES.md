# `conflated.pmtiles`

*Looking to download this file rather than read about its format? See
[downloading the current output](README.md#downloading-the-current-output).*

Visualizes [`conflated.parquet`](CONFLATED_PARQUET.md) itself — every
[AllThePlaces](https://alltheplaces.xyz/) feature, matched to
[OpenStreetMap](https://www.openstreetmap.org/) or not, and the
OpenStreetMap features that may be gone — so the
*matching* step can be reviewed on its own, independent of whatever
[`edits.pmtiles`](../TECHNICAL_DESIGN.md) separately proposes changing.
It's a [PMTiles](https://protomaps.com/docs/pmtiles) archive, built with
[Tippecanoe](https://github.com/felt/tippecanoe) the same way
`edits.pmtiles` is — see [`pmtiles.io`](https://pmtiles.io) for how to
open one in a browser, no server required.

⚠️ **This file is for visualization only** — a debugging aid for
reviewing matching/conflation, not a data product. Its structure may
change at any time, or we may stop producing it altogether, without
notice. If you need something stable to build on, use
[`conflated.parquet`](CONFLATED_PARQUET.md) instead.

## Layers

Three layers, split by whether `conflate()` found an OpenStreetMap match:

- **`matched`** — every `conflated.parquet` row whose
  [`match.relation`](CONFLATED_PARQUET.md#match) is `same`.
- **`unmatched`** — every other row with an AllThePlaces side. That
  includes `located_in` rows, such as a pharmacy inside a
  supermarket: the store is missing in OpenStreetMap, and its `osm`
  side is only the host it is in.
- **`deletion_candidate`** — every row without an AllThePlaces side:
  an OpenStreetMap feature that nothing in AllThePlaces matched, but
  that may be gone, such as a shop of a brand that AllThePlaces covers
  well nearby. These have not been checked by hand, so treat them as
  leads for review, not as closures.

Every feature carries `relation`, the row's `match.relation`, so a
style can tell `missing_in_osm` from `located_in`. Features in
`deletion_candidate` also carry `rule`, the row's
[`match.evidence.rule`](CONFLATED_PARQUET.md#match), which says why it
was flagged: `wikidata` or `name` for a brand, network or operator
that AllThePlaces has in the same country, `swiss_transit_id` for a
Swiss stop whose ID the register doesn't know, and
`swiss_transit_stop` for a Swiss stop without IDs, far from any in the
register.

A third layer, **`spider_cells`** (z0–8), summarizes where each
spider's features are; see [below](#spider_cells-z08).

Each is built as two PMTiles archives — a coarse **overview** (z0–12)
and a fine **detail** (z13–16) — merged with `tile-join`, so what a
row looks like depends on how far you're zoomed in (see below). A
feature's coordinates are rounded to 1e-7 degrees (about 1 cm at the
equator) before being written out — the same precision `edits.pmtiles`
uses — so raw source-data precision noise doesn't inflate the file.

## The two zoom ranges

### z0–12: the overview

**One deliberately minimal feature per row — no tags, no `fid`.** At z0
a single tile has to hold the entire planet, and every per-feature byte
counts. Tags push the tile so far past tippecanoe's size limit that
`--drop-densest-as-needed` throws away all but a few hundred features
worldwide; even a per-feature `fid` (unique, so it defeats the vector
tile's columnar value dedup) costs ~20× the low-zoom density (measured
against a full planet: full tags → ~370 features visible at z0; `+fid`
→ ~7k; overview as it stands → ~40k, a workable world map).

Geometry is `osm_geometry` (the matched OpenStreetMap shape — point,
line, or polygon) for `matched` and `deletion_candidate` rows,
`atp_geometry` for `unmatched`. Properties: `spider` (not on
`deletion_candidate`, which has no AllThePlaces side), `relation`,
`importance`, `rule` (`deletion_candidate` only), and — rows with an
OpenStreetMap feature only — `osm:type` / `osm:id`, which for a
`located_in` row is the host.

Coarse zooms show the most important rows, by their
[`importance`](CONFLATED_PARQUET.md#importance): the top 1% at z0
(`importance >= 0.99`), 4% at z1 (`>= 0.96`), 16% at z2 (`>= 0.84`),
64% at z3 (`>= 0.36`), and every row from z4 on. Each zoom shows four
times as many as the one before, since a tile splits into four, so a
tile holds about as many features at every zoom. Where a tile would
still be too large, tippecanoe's `--drop-densest-as-needed` thins it
out further. To see a row's tags, click through to the detail layer
(below): `matched` and `deletion_candidate` rows join on
`osm:type`+`osm:id`, `unmatched` rows on nearest `part: "atp"` feature
of the same `spider`.

### z13–16: the detail

**Full-tag features**, where a tile covers a few km and the tag
payload costs nothing. Every zoom from z13 to z16 shows every feature:
there is no random dot-dropping (`-r 1`), and only
`--drop-densest-as-needed` would thin out a tile that gets too large.
For the 2026-11-02 planet, the largest z13 tile then comes to about
240 KB gzipped (estimated in #916), well below tippecanoe's 500 KB limit.

- A **`matched`** row yields up to three features:
  - the AllThePlaces point (`part: "atp"`),
  - the OpenStreetMap shape (`part: "osm"`),
  - a connector line between their centroids (`part: "link"`) —
    *unless* that offset is under 5 meters (too close to draw a
    meaningful line; see `MIN_CONNECTOR_LENGTH_METERS` in
    [`src/pipeline/conflated_tiles.rs`](../../src/pipeline/conflated_tiles.rs)
    for the exact, documented threshold).
- A **`located_in`** row, in the `unmatched` layer, yields the same
  three, except that the OpenStreetMap shape is the host
  (`part: "host"`). The link then shows a mapper which store to add
  the feature in.
- Any other **`unmatched`** row yields one feature: its AllThePlaces point
  (`part: "atp"`), with the full `atp:*` tag set. `conflated.parquet`
  carries no stable id for the ATP side, so a viewer that clicked the
  (id-less) overview feature finds this one by taking the nearest
  `part: "atp"` feature of the same `spider` to the click.
- A **`deletion_candidate`** row yields one feature: its OpenStreetMap
  shape (`part: "osm"`), with the full `osm:*` tag set, `rule`, and —
  for the `wikidata` and `name` rules — `local_share`.

The matched split into separate `atp`/`osm`/`link` features exists
because a single Mapbox Vector Tile feature can only be one geometry
type — "ATP point ∪ OSM shape ∪ connector line" can't be one feature —
and because showing the connector at low zoom turned out to be actively
harmful: tippecanoe's automatic zoom selection never terminates for a
connector line whose ends are nearly coincident (a *good* match), since
such a line never spatially separates from its own endpoint no matter
how far you zoom in. Bounding the detail pass to a fixed zoom range
side-steps that entirely — see
[`docs/notes/2026-08-27-connector-line-zoom-runaway.md`](../notes/2026-08-27-connector-line-zoom-runaway.md)
for the full investigation.

### `spider_cells` (z0–8)

The overview is thinned out at coarse zooms, the same way for all
spiders, so a map filtered to one spider (with a filter on `spider`)
shows only a few of its features there. `spider_cells` is for such a
map. At zoom `z`, it has one point per spider in each cell of a grid
of tiles at zoom `z + 3`, so at most 64 points of one spider in a tile.
The point lies at the centroid of the spider's AllThePlaces features
in the cell, not at the middle of the cell, so the points don't line
up on a grid, and a spider whose features are shifted shows shifted.

Properties:

- `spider` — the spider.
- `n` — the spider's features in the cell: rows of `conflated.parquet`
  with an AllThePlaces side.
- `same`, `missing_in_osm`, `located_in` — how many of them have each
  `match.relation`. Together with `n`, enough to size a point and
  colour it by its mix.
- `z` — the zoom level the point is for. Each point is in the tiles of
  that zoom only.

This layer is built in its own tippecanoe pass that never drops a
feature, and joined to the other two. It stops at z8 because that is
the highest zoom at which `--drop-densest-as-needed` still thins out
the overview somewhere; from z9 on, the overview has every feature.
On the 2026-11-02 planet, the layer has 11,406 points at z0 and 1.2
million at z8. It adds about 155 KB to the z0 tile (245 KB → 400 KB)
and 30 MB to the archive. The largest joined tile is 634 KB, at z4,
which is over tippecanoe's usual 500 KB limit, so `tile-join` runs
with `--no-tile-size-limit`.

## Properties

- `fid` — the row's ordinal in `conflated.parquet` for this pipeline
  run. On **z13–16 detail features only** (not the overview — see
  above), where it groups a matched row's up-to-three features and
  cross-references `conflated.parquet`. Stable within a run only (both
  archives are always rebuilt together); not a cross-run identifier.
- `spider` — which AllThePlaces spider produced this feature (its
  `atp.fetched.spider` in `conflated.parquet`). On every feature except
  those in `deletion_candidate`, which have no AllThePlaces side.
- `rule` — the row's `match.evidence.rule`: why a feature in
  `deletion_candidate` was flagged. In that layer only, on both
  overview and detail features.
- `local_share` — the row's `match.evidence.local_share`, rounded to
  two decimals: for a deletion candidate by brand, the share of
  OpenStreetMap features of that brand nearby that AllThePlaces
  matched. On z13–16 `deletion_candidate` features only, and only for
  the `wikidata` and `name` rules.
- `matched` (`true` | `false`) — overview features only; the detail
  layer conveys the same thing through its layer name and `part`.
- `importance` — the row's `importance` from `conflated.parquet`,
  rounded to two decimals. Overview features only.
- `osm:type`, `osm:id` — the matched OpenStreetMap object. On the
  `matched` and `deletion_candidate` overview features (where they
  double as the join key to the detail layer) and the z13–16
  `osm`/`link` detail features.
- `osm:<tag>` for every tag in `osm.tags` — z13–16 `osm`/`link` detail
  features only.
- `atp:<tag>` for every tag in `atp.tags` — z13–16 `atp`/`link` detail
  features only.
- `part` (`"atp"` | `"osm"` | `"link"`) — z13–16 detail features only,
  identifying which of the (up to) three a given feature is.

On the `link` feature, tag properties are prefixed (`atp:name` vs.
`osm:name`) rather than merged, since the two sides can and do disagree
— that disagreement is often exactly what's worth reviewing. The
`atp`/`osm` detail features carry only their own side's tags (no
`osm:*` on the `atp` feature or vice versa), so a tile inspector shows
unambiguously which end of a pair you're looking at.
