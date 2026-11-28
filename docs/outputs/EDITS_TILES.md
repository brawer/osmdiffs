# `edits.pmtiles`

*Looking to download this file rather than read about its format? See
[downloading the current output](README.md#downloading-the-current-output).
The resource is called `edits-tiles`.*

Draws [`edits.parquet`](EDITS_PARQUET.md) on a map, one point per
suggested edit, so that people can review the edits and debug the
rules that make them. It's a [PMTiles](https://protomaps.com/docs/pmtiles)
archive, built with [Tippecanoe](https://github.com/felt/tippecanoe) —
see [`pmtiles.io`](https://pmtiles.io) for how to open one in a
browser, no server required.

⚠️ **This file is for visualization only**, like
[`conflated.pmtiles`](CONFLATED_TILES.md). Its structure may change at
any time, or we may stop producing it, without notice. To build on the
edits, use [`edits.parquet`](EDITS_PARQUET.md) instead.

## Layer

One layer, **`edits`**, built like `conflated.pmtiles` as two
archives — a coarse **overview** (z0–12) and a **detail** (z13–14) —
merged with `tile-join`. A feature's coordinates are rounded to 1e-7
degrees, about 1 cm.

Every edit is a point: at the node for an edit of a node or a create,
and at a point on the surface of the way or relation otherwise. The
tiles don't repeat the element's shape; `conflated.pmtiles` has it,
under the same `osm:type` and `osm:id`.

### z0–12: the overview

One point per edit, with only four properties:

- `icon` — the [Maki](https://github.com/mapbox/maki) or
  [Temaki](https://github.com/rapideditor/temaki) icon for the kind of
  place, such as `maki-cafe`. Always empty for now; see the
  [plan](../notes/2026-11-28-edits-map.md#milestones).
- `action` — `modify` or `create`.
- `topic` — the first of the edit's `topics`, such as `contact`.
- `importance` — the edit's `importance`, rounded to two decimals.

There's no `id` here: a value that differs for every feature defeats
the vector tile's value dedup and would thin out the coarse zooms
(see [`CONFLATED_TILES.md`](CONFLATED_TILES.md#z012-the-overview)). To
get from a clicked overview point to its edit, fetch the z14 tile at
that spot and take the nearest detail point.

Coarse zooms show the most important edits, with the same filter as
`conflated.pmtiles`: `importance >= 0.99` at z0, `>= 0.96` at z1,
`>= 0.84` at z2, `>= 0.36` at z3, and every edit from z4 on. Where a
tile would still be too large, tippecanoe's `--drop-densest-as-needed`
thins it out further.

### z13–14: the detail

Every edit, with everything a mapper needs to perform it except the
element's tags and members, which a viewer fetches from the
OpenStreetMap API by `osm:type` and `osm:id` when someone clicks the
edit, as for `conflated.pmtiles`
([#1026](https://github.com/brawer/osmdiffs/issues/1026)). Every zoom
shows every edit (no random dot-dropping).

- `id` — the edit's stable `id` in `edits.parquet`.
- `action` — `modify` or `create`.
- `topics` — the edit's `topics`, joined with `;`, such as
  `contact;wikidata`.
- `icon` — as in the overview.
- `importance` — as in the overview.
- `osm:type`, `osm:id`, `osm:version` — the element and the version
  the edit was made for (`osm.modified.version`). Modifies only. An
  editor compares `osm:version` with the live element to tell whether
  it changed since.
- `changes` — the edit's `changes` as a JSON string: an array of
  objects with `key`, `old`, `new` and `reason`, where `old` or `new`
  may be `null` (no such tag in OpenStreetMap, or remove the tag), and
  `atp`, AllThePlaces' value, for a `needs_review` change only. See
  [Reasons](EDITS_PARQUET.md#reasons).
- `warnings` — the edit's `warnings`, joined with `;`. Only on edits
  that have any; see [Warnings](EDITS_PARQUET.md#warnings).
- `sources` — the edit's `sources` as a JSON string: an array of
  objects with `spider`, `fetched` (the date, such as `2026-11-20`) and
  `relation`, and `atp_ref`, `source_url`, `rule` and `distance_m`
  (rounded to 0.1 m) where `edits.parquet` has them.
- `host:type`, `host:id`, `host:name` — for a create inside another
  place, the OpenStreetMap element it goes into, and its name if it
  has one.

Where the AllThePlaces position (the centroid of `atp_geometry`) is 5
m or more from the edit's point, the detail also has a line from it to
the point, with only `id`, `action` and `part: "link"`. The points
have no `part`.

## Size

Measured on `edits.parquet` of the 2026-11-23 planet run, with its 1.73
million edits (1.13 million modifies, 594,000 creates), on a machine
with 4 cores:

- The archive is 940 MB: 146 MB for z0–12 and 794 MB for z13–14. The
  detail has 839,000 `link` lines beside the 1.73 million points.
- **z0** holds 23,669 edits in 25 KB, every edit with
  `importance >= 0.99`; none are dropped for size.
- From z3 to z7, `--drop-densest-as-needed` thins out the densest
  tiles: at z3, the tile over western Europe (3/4/2) keeps 86,600 of
  its 356,000 edits. From z8 on, every tile has every edit.
- The largest tile is 467 KB, at z2 (2/2/1, Europe and Africa, 183,000
  points), under tippecanoe's 500 KB limit. The largest detail tile is
  119 KB at z13 and 47 KB at z14, so the detail drops nothing.
- Building takes about 7½ minutes: 16 s to write the GeoJSON layers,
  1 min 39 s for the overview, 2 min 56 s for the detail, and 2 min
  35 s for `tile-join`.
