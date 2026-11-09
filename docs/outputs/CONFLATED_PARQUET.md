# `conflated.parquet`

*Looking to download this file rather than read about its format? See
[downloading the current output](README.md#downloading-the-current-output).*

For every feature [AllThePlaces](https://alltheplaces.xyz/) scrapes
that plausibly maps to something in
[OpenStreetMap](https://www.openstreetmap.org/), this file has one row
pairing the AllThePlaces feature with whichever OpenStreetMap feature
it was matched to — or with no OpenStreetMap match at all, if none was
found.

It also has rows with an OpenStreetMap feature but no AllThePlaces
feature, for OpenStreetMap features that may no longer exist; see
below.

This is a [GeoParquet 2.0-rc.1](https://github.com/opengeospatial/geoparquet/blob/v2.0.0-rc.1/format-specs/geoparquet.md)
file (2.0 hasn’t had its final release yet, but this is the newest spec
available, and the on-disk format isn’t expected to change again before
it does). In practice that means any GeoParquet-aware tool — QGIS,
kepler.gl, GeoPandas — can open it directly and will find both
geometry columns on its own, without being told where to look.

## Schema

```text
atp                                          STRUCT, nullable
  tags        MAP<UTF8, UTF8>                non-null map, non-null keys and values
  fetched                                    STRUCT, non-null
    timestamp TIMESTAMP(ms, UTC)             non-null
    spider    UTF8                           non-null
atp_geometry  BINARY (WKB)                   nullable — null exactly when atp is null

osm                                          STRUCT, nullable
  type        UTF8                           non-null ("node" | "way" | "relation")
  id          UINT64                         non-null
  tags        MAP<UTF8, UTF8>                non-null map, non-null keys and values
  modified                                   STRUCT, non-null
    timestamp TIMESTAMP(ms, UTC)             non-null
    version   UINT32                         non-null
  way_members LIST<UINT64>                   nullable — present only when type = "way"
  relation_members LIST<STRUCT>              nullable — present only when type = "relation"
    type      UTF8                           non-null ("node" | "way" | "relation")
    id        UINT64                         non-null
    role      UTF8                           nullable — null if OSM gives the member no role
osm_geometry  BINARY (WKB)                   nullable — null exactly when osm is null

match                                        STRUCT, non-null
  relation    UTF8                           non-null ("same" | "located_in" | "missing_in_osm" | "deletion_candidate")
  score       FLOAT                          nullable — 0.0 to 1.0, null unless relation = "same"
  evidence                                   STRUCT, non-null
    rule        UTF8                         nullable — which rule decided, see below
    distance_m  FLOAT                        nullable — null unless both atp and osm are present
    agreeing    LIST<UTF8>                   non-null, may be empty
    local_share FLOAT                        nullable — only for deletion candidates by brand

importance    FLOAT                          non-null, 0.0 to 1.0
```

A few things worth calling out that aren’t obvious from the above:

- **`osm` is `null` whenever nothing in OpenStreetMap matched.** That
  can happen for perfectly ordinary reasons — it doesn’t mean anything
  is wrong with the conflation pipeline; rather, it’s a signal that
  the feature may be missing from OpenStreetMap.
- **`atp` is `null` for OpenStreetMap features that may be gone.**
  AllThePlaces covers far less than OpenStreetMap, so an OpenStreetMap
  feature without a match usually means nothing, and gets no row. It
  only gets one, with `atp` null, when there is a strong signal that
  AllThePlaces would list it if it still existed: its
  `brand:wikidata`, `network:wikidata` or `operator:wikidata` names a
  brand, network or operator whose features of the same kind
  AllThePlaces lists in that country; or, without such Wikidata IDs,
  its `brand`, `network` or `operator` has a name for which
  AllThePlaces lists such features; or it is a public transport stop
  in Switzerland or Liechtenstein, or in the enclaves of Büsingen and
  Campione that Swiss public transport serves, with an ID that the
  Swiss register doesn’t know; or it is such a stop without IDs, far
  from any stop in the register. For brands, networks and operators,
  enough of their OpenStreetMap features nearby must also have been
  matched, since AllThePlaces often lists only part of a brand. See
  [the evaluation](../notes/2026-11-05-deletion-candidates.md) and
  [local coverage](../notes/2026-11-09-local-deletion-coverage.md).
- **`atp_geometry`/`osm_geometry` sit outside the `atp`/`osm` structs**,
  at the top level of the file, rather than next to the rest of each
  side’s data — GeoParquet requires geometry columns to live at the
  schema root, which is what lets a generic GIS tool discover them on
  its own instead of needing to be told where to look.
  `osm_geometry` is the file’s designated primary geometry column,
  since OpenStreetMap is the dataset a reviewer is actually meant to
  correct.
- **`atp.fetched` and `osm.modified` mirror each other on purpose**:
  both answer “who produced this side of the row, and when” —
  `atp.fetched` for AllThePlaces’ own scrape, `osm.modified` for
  OpenStreetMap’s own edit metadata (the timestamp and version number
  of that feature’s most recent edit). We don’t preserve OpenStreetMap’s
  full edit history here, only its current state as of the database
  snapshot this file was built from.
- **We deliberately don’t carry OpenStreetMap’s changeset ID.** A
  changeset number resolves, via OSM’s own public API, to the account
  that made the edit — unlike a node/way/relation ID, which only
  identifies a place. See
  [#730](https://github.com/brawer/osmdiffs/issues/730) for the
  full reasoning and trade-offs.
- **`osm.way_members`/`osm.relation_members` are `null`, not an empty
  list, when they don’t apply** — a `null` `way_members` means “this
  isn’t a way”, not “this way has no members”. Only one of the two is
  ever non-`null` for a given row, matching `osm.type`. `way_members`
  is a way’s node references, in OpenStreetMap’s declared order.
  `relation_members` is a relation’s members, also in declared order,
  each with its own type (node/way/relation), ID, and role (e.g.
  “outer”, “inner”, or `null` if OpenStreetMap doesn’t record one).

## Match

`match` says what the pipeline decided about a row, and why, so you
can pick the rows you trust enough for your purpose without redoing
the matching.

`match.relation` is one of:

- `same`: the AllThePlaces and OpenStreetMap features are the same
  thing.
- `located_in`: the AllThePlaces feature is inside another business,
  such as a pharmacy inside a supermarket, and OpenStreetMap has no
  feature of its own for it. `osm` is the host, so this row says where
  to add it. Never use its tags for editing the host.
- `missing_in_osm`: nothing in OpenStreetMap matched the AllThePlaces
  feature (`osm` is null).
- `deletion_candidate`: nothing in AllThePlaces matched the
  OpenStreetMap feature, and something suggests that it is gone (`atp`
  is null).

More relations may come. Treat values you don't know as "none of
these".

`match.score` is the matcher's score, from 0.0 to 1.0. Scores of
different rules don't mean the same: a transit stop matched by name
scores lower than one matched by its ID, even when both are right.

`match.evidence.rule` says which rule decided:

| relation | rule | meaning |
|---|---|---|
| `same` | `brand_wikidata` | same `brand:wikidata` |
| `same` | `contact` | same phone number or email address |
| `same` | `brand_name` | same `brand`, `operator` or `network` name |
| `same` | `brand_domain` | website on a domain of the brand |
| `same` | `same_id` | transit stop with the same IFOPT ID or `uic_ref` |
| `same` | `same_stop` | another platform of the same transit stop |
| `same` | `name` | transit stop with a similar name |
| `same` | `ref` | street furniture, such as a post box, with the same reference |
| `same` | `kind` | street furniture of the same kind close by |
| `located_in` | `department` | a department of a store of the same brand, such as a Publix pharmacy in a Publix supermarket |
| `located_in` | `located_in` | the host named by the `located_in` or `located_in:wikidata` tags, such as a Starbucks in a Target |
| `deletion_candidate` | `wikidata` | brand, network or operator by Wikidata ID |
| `deletion_candidate` | `name` | brand, network or operator by name |
| `deletion_candidate` | `swiss_transit_id` | Swiss stop whose ID the register doesn't know |
| `deletion_candidate` | `swiss_transit_stop` | Swiss stop without IDs, far from any in the register |

`match.evidence.distance_m` is the shortest distance in meters between
the two geometries: 0 when the AllThePlaces point lies inside the
OpenStreetMap building. `match.evidence.agreeing` lists the tags whose
values agree on both sides, such as `phone` or `opening_hours`,
compared without regard to case and spacing. Phone numbers, email
addresses and websites are compared as the matchers compare them:
phone numbers in their international form, so `+41 44 123 45 67` and
`044 123 45 67` agree in Switzerland, and websites without scheme,
`www.` or tracking parameters. `match.evidence.local_share` is, for deletion candidates by
brand, the share of OpenStreetMap features of that brand nearby that
AllThePlaces matched; it is at least 0.5, or the row wouldn't be here.

```sh
duckdb -c "
SELECT match.relation, match.evidence.rule, count(*)
FROM read_parquet('conflated.parquet')
GROUP BY ALL ORDER BY 3 DESC;
"
```

Some SQL engines reserve `MATCH` as a keyword; there, write `"match"`.

## Importance

`importance` says how much a row’s place matters to map users, from
0.0 (least) to 1.0 (most), so you can work through the file in
priority order instead of in whatever order it happens to be in. It
is a *relative* position among the rows of this file: `importance >=
0.9` always selects the top tenth of rows, and the same place can get
a different value from one week to the next as other places come and
go.

It combines three signals:

- **Location**: how much the place’s location is looked at on
  OpenStreetMap-based maps, according to
  [OSMViews](https://osmviews.brawer.ch/). A shop on a busy downtown
  street ranks above the same shop in a remote valley.
- **Brand**: how often the Wikipedia articles about the place’s brand,
  operator or network (from its `brand:wikidata`, `operator:wikidata`
  and `network:wikidata` tags) were viewed, according to
  [QRank](https://qrank.toolforge.org/). A place without such a tag, or
  whose brand has no Wikipedia article, is ranked on the other two
  signals alone rather than pushed down.
- **Kind of place**: a coarse, hand-made score by category, so that a
  defibrillator, pharmacy or hospital ranks above a bench or a tree.

The exact weights and categories are an implementation detail that
may change; see
[`src/pipeline/importance.rs`](../../src/pipeline/importance.rs) and
[#713](https://github.com/brawer/osmdiffs/issues/713).

## Geometry

`atp_geometry`/`osm_geometry` are standard [OGC Simple
Features](https://postgis.net/workshops/postgis-intro/geometries.html)
geometries — the same model QGIS and PostGIS use, and that formats
like GeoJSON and GeoPackage are built on too. `atp_geometry` is
whatever AllThePlaces’ upstream source provides for that feature —
nearly always a point, though a handful of sources provide lines or
polygons instead. `osm_geometry` is the
matched OpenStreetMap feature’s shape: a polygon for an area, a line
for a way that isn’t an area, a point for a node. Both columns use
[OGC:CRS84](https://en.wikipedia.org/wiki/World_Geodetic_System)
(WGS84 longitude/latitude, the same coordinates GPS uses) — GeoParquet’s
default CRS, so it isn’t spelled out explicitly in the file.

`osm_geometry` isn’t always point-for-point what’s in OpenStreetMap,
though: we automatically repair certain geometry errors (like
self-intersecting lines), and if a shape would otherwise end up with a
very large number of coordinates, we simplify it — trading a small
amount of positional accuracy so that you, the downstream client, get
a geometry that’s directly usable for display without having to
simplify it yourself first (say, before sending it to a mobile phone
over a cellular connection). We currently do this using a
topology-preserving variant of the
[Visvalingam–Whyatt algorithm](https://en.wikipedia.org/wiki/Visvalingam%E2%80%93Whyatt_algorithm)
(this may change in the future). Treat `osm_geometry` as good enough
to display — e.g. to mark the affected OSM feature on a map — but not
as a guaranteed-exact copy of OpenStreetMap’s own data. If you need to
generate an actual edit against OpenStreetMap, use `way_members`/
`relation_members` instead, which always refer to OpenStreetMap’s real
nodes.

Rows are sorted along a
[Hilbert curve](https://towardsdatascience.com/spatial-index-space-filling-curves-e67baec2186a/),
for better compression and faster spatial queries. At the moment, we
compute this from the S2 cell ID of each geometry’s centroid, but we
don’t want to commit to that particular way of doing the sort, so the
sort key itself isn’t exposed as a column.

## A quick look

```sh
duckdb -c "
INSTALL spatial; LOAD spatial;
SELECT osm.type, osm.id, atp.fetched.spider, ST_GeometryType(osm_geometry) AS osm_shape
FROM read_parquet('conflated.parquet')
WHERE osm.id IS NOT NULL
LIMIT 3;
"
```

```text
┌─────────┬───────────┬────────────┬───────────────┐
│  type   │    id     │   spider   │   osm_shape   │
│ varchar │  uint64   │  varchar   │ geometry_type │
├─────────┼───────────┼────────────┼───────────────┤
│ way     │ 608979139 │ tchibo     │ POLYGON       │
│ way     │ 737021556 │ mediamarkt │ POLYGON       │
│ way     │ 737021557 │ denner_ch  │ POLYGON       │
└─────────┴───────────┴────────────┴───────────────┘
```

## Data provenance

Every file carries a [CycloneDX](https://cyclonedx.org/) document
embedded in its own metadata — CycloneDX being an established,
[standardized](https://ecma-international.org/wp-content/uploads/ECMA-424_2nd_edition_december_2025.pdf)
format for exactly this kind of “where did this data come from”
record. It answers questions like which AllThePlaces run and which
OpenStreetMap snapshot went into this file, which QRank and OSMViews
builds the `importance` column was computed from, and which version of
our pipeline produced it. It also records licensing information for
every input and the output.

The same document is also published as a **standalone file** next to the
Parquet — the `bom` resource in
[`datapackage.json`](README.md#downloading-the-current-output),
`conflated-<date>-<hash8>.cdx.json` — so you can read the provenance, and
verify the Parquet’s SHA-256/512, without downloading the ~750 MB file
first. It is byte-for-byte reproducible from the same inputs (timestamps
are anchored to the inputs, not the wall clock; the `serialNumber` is
derived from the inputs’ hashes).

```sh
duckdb -c "
SELECT
    json_extract_string(decode(value), '\$.metadata.tools.components[0].version') AS pipeline_version,
    json_extract_string(decode(value), '\$.components[0].version') AS alltheplaces_run,
    json_extract_string(decode(value), '\$.components[1].version') AS openstreetmap_snapshot
FROM parquet_kv_metadata('conflated.parquet')
WHERE key::VARCHAR = 'org.cyclonedx.bom';
"
```

```text
┌──────────────────┬──────────────────────┬────────────────────────┐
│ pipeline_version │   alltheplaces_run   │ openstreetmap_snapshot │
│     varchar      │       varchar        │         varchar        │
├──────────────────┼──────────────────────┼────────────────────────┤
│ 0.6.10           │ 2026-01-01T00:00:00Z │ 2026-01-27T08:11:02Z   │
└──────────────────┴──────────────────────┴────────────────────────┘
```
