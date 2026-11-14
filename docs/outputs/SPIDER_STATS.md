# `spider-stats.parquet`

`spider-stats.parquet` says, for each
[AllThePlaces](https://www.alltheplaces.xyz/) spider, what became of
its features: how many it wrote, how many we left out and why, how
they matched OpenStreetMap, how far their tags agree, and how its
brands compare with OpenStreetMap. It is meant for a dashboard that
shows where conflation goes badly and why (see
[brawer/osmdiffs#924](https://github.com/brawer/osmdiffs/issues/924)).

This file is a debugging aid, not a data product. Its structure may
change, or it may stop being published, without notice. To build
something on top of this project, use
[`conflated.parquet`](CONFLATED_PARQUET.md) instead.

## Layout

There is one row per spider, sorted by spider name. Row groups hold 64
spiders each, so that a web page can fetch one spider with HTTP range
requests, for example with
[hyparquet](https://github.com/hyparam/hyparquet), without
downloading the whole file.

All numbers are counts, never shares (with one exception,
`opening_hours_top_share`). That way, you can add them up across
spiders, countries or brands before you divide.

The list includes every spider that AllThePlaces lists in its run
statistics, has a file in its dump, or has a row in `conflated.parquet`.
A spider that AllThePlaces lists but that has no file in the dump has
nulls for the dump's counts.

## Schema

```
spider                   UTF8     non-null
atp_features             UINT64   nullable — features AllThePlaces says the spider wrote
atp_errors               UINT64   nullable — errors AllThePlaces says the spider logged
dump_features            UINT64   nullable — features in the spider's file of the dump
not_for_osm              UINT64   nullable — left out: license doesn't allow OpenStreetMap
invalid                  UINT64   nullable — left out: no usable geometry, or no spider name
no_matcher               UINT64   nullable — kept, but no matcher handles their kind
in_china                 UINT64   nullable — features in mainland China
datum                    UTF8     nullable — "wgs84", "gcj02" or "bd09", for in_china
dataset_attributes       MAP<UTF8, UTF8> nullable — the spider's dataset attributes, such as its license
rows                     UINT64   non-null — rows in conflated.parquet
same                     UINT64   non-null — rows with match.relation = "same"
located_in               UINT64   non-null — rows with match.relation = "located_in"
missing_in_osm           UINT64   non-null — rows with match.relation = "missing_in_osm"
shared_osm_features      UINT64   non-null — same rows whose OSM feature another feature of the spider also matched
shared_positions         UINT64   non-null — features at the same position as another feature of the spider
offset_east_m            FLOAT    nullable — median of how far same rows lie east of their OSM feature
offset_north_m           FLOAT    nullable — ... and north
bbox                     STRUCT   nullable — where the spider's features are, see below
  xmin                   FLOAT    non-null — west, in degrees of longitude
  ymin                   FLOAT    non-null — south, in degrees of latitude
  xmax                   FLOAT    non-null — east
  ymax                   FLOAT    non-null — north
distance_m               LIST<UINT64> non-null — histogram of match distances, see below
missing_no_candidate     UINT64   non-null — missing_in_osm: no OSM feature of the kind close enough
missing_rejected         UINT64   non-null — missing_in_osm: the matcher found no evidence for any
missing_taken            UINT64   non-null — missing_in_osm: another feature of the spider took them
rejected_distance_m      LIST<UINT64> non-null — histogram of the distance to the nearest rejected one
chance_sampled           UINT64   non-null — features moved 2 km to find chance pairs
chance_matched           UINT64   non-null — ... and how many of them still matched
opening_hours_top_share  FLOAT    nullable — share of features with the most common opening_hours
rules                    LIST<STRUCT> non-null
  rule                   UTF8     non-null — match.evidence.rule
  rows                   UINT64   non-null
tags                     LIST<STRUCT> non-null
  tag                    UTF8     non-null — "name", "phone", "opening_hours", ...
  atp                    UINT64   non-null — same rows whose ATP feature has the tag
  agreeing               UINT64   non-null — ... and OSM agrees
  differing              UINT64   non-null — ... and OSM has a different value
  missing_in_osm         UINT64   non-null — ... and OSM has none
brands                   LIST<STRUCT> non-null
  country                UTF8     nullable — ISO 3166-1 alpha-2 code
  wikidata_id            UTF8     nullable — "Q379911"; null for features without one
  name                   UTF8     nullable — a display name for wikidata_id
  atp_features           UINT64   non-null
  same                   UINT64   non-null
  located_in             UINT64   non-null
  missing_in_osm         UINT64   non-null
  osm_features           UINT64   nullable — OSM features with this ID in this country
  deletion_candidates    UINT64   nullable — deletion candidates with this ID in this country
  offset_east_m          FLOAT    nullable — like offset_east_m above, for this brand and country
  offset_north_m         FLOAT    nullable
  bbox                   STRUCT   nullable — like bbox above, for this brand and country
```

### What became of the features

The counts add up like this:

```
dump_features  = not_for_osm + invalid + no_matcher + rows
rows           = same + located_in + missing_in_osm
missing_in_osm = missing_no_candidate + missing_rejected + missing_taken
```

`atp_features` and `atp_errors` come from AllThePlaces' own statistics
for the run. `atp_features` normally equals `dump_features`. Where it
doesn't, something went wrong between the spider and the dump. A
spider that crashed often has errors and no features at all.

`in_china` and `datum` concern features in mainland China. Many
Chinese sources give coordinates in the GCJ-02 or BD-09 datum, which
are hundreds of meters off. We find out which datum a spider uses by
comparing its features with OpenStreetMap, and convert them to WGS-84
before matching.

### Dataset attributes

`dataset_attributes` holds the `dataset_attributes` that AllThePlaces
puts on the spider's GeoJSON FeatureCollection, such as `license`,
`license:wikidata`, `attribution:name`, `use:openstreetmap` and
`spider:lineage`; see AllThePlaces'
[documentation](https://github.com/alltheplaces/alltheplaces/blob/master/docs/SPIDER_LINEAGE.md).
They are there so that import documents and edits can cite the source,
license and attribution of a feature without repeating them on every
row of `conflated.parquet`: join on `atp.fetched.spider` there. The
entries are sorted by key. A value that isn't a string in the GeoJSON,
such as a number, is given as its JSON text. The map is empty for a
spider whose file in the dump is empty, and null for one without a
file. We keep the attributes of spiders whose features we leave out
too, since their license says why.

### How they matched

`rules` counts rows by `match.evidence.rule`; see
[`CONFLATED_PARQUET.md`](CONFLATED_PARQUET.md#match)
for what each rule means. Rows without a rule, such as
`missing_in_osm`, are not listed.

`distance_m` is a histogram of `match.evidence.distance_m` over the
`same` rows. Its bins are bounded by 1, 10, 25, 50, 100, 250 and 1000
meters: the first counts distances below 1 m, the second from 1 m up
to 10 m, and the last 1000 m or more. The file's key-value metadata
`osmdiffs:distance_bins_m` lists the bounds, in case they change. A
spider whose matches pile up far away likely has wrong coordinates;
one whose matches are all at exactly the same distance may place every
feature at the center of its town.

`shared_osm_features` counts `same` rows whose OpenStreetMap feature
also matched another feature of the same spider. A high count hints at
duplicates in the spider, or at OpenStreetMap having one feature where
there are several.

`shared_positions` counts features of the spider at exactly the same
position as another one of its features. Some are right, such as the
departments of one store, but many hint at a spider that puts features
it can't place at a default position.

`offset_east_m` and `offset_north_m` are the medians of how far the
AllThePlaces feature of a `same` row lies east and north of its
OpenStreetMap feature, from centroid to centroid, in meters (negative
for west and south). Single features scatter in all directions, so
the medians stay near zero, unless a spider shifts all its features
the same way. For example, `c_and_a` once placed all its stores
about 150 m west of where they are. The same medians per brand and
country show a shift in only one country.

`bbox` is a box around the spider's features, so that a map can zoom
to where they are without reading any tiles. It is the box around the
centroids of the AllThePlaces features of the spider's rows in
`conflated.parquet`, in WGS-84 (after any conversion from GCJ-02 or
BD-09), named like a GeoParquet 1.1 `bbox` covering. It is trimmed:
along each axis, it may leave out up to one in 50 features, as long
as that makes the box smaller, so that a few features in the wrong
place, such as on Null Island, don't make it span the world. So don't
use it to find outliers. Its corners are rounded outward to `FLOAT`.
When `xmin` is greater than `xmax`, the box crosses the antimeridian
and goes east from `xmin` past 180° to `xmax`; to fit a map to it,
add 360 to `xmax`. It is null for a spider without rows. The `bbox`
of a brand is the same, for the brand's features in that country.

`opening_hours_top_share` is the share of the spider's features with
`opening_hours` that have its most common value. Near 1 for a large
spider, it suggests that the spider writes the same hours everywhere,
such as those of its head office.

### Why features are missing

The `missing_*` columns say why the matcher found nothing for a
feature that ends up `missing_in_osm`:

- `missing_no_candidate`: OpenStreetMap has no feature of the same kind
  (such as a shop, for a shop) within the distance the matcher looks.
  Either OpenStreetMap lacks it, or the spider's coordinates are far
  off.
- `missing_rejected`: there are such features, but the matcher found no
  evidence that any is the same, such as the same brand, phone number
  or name. `rejected_distance_m` is a histogram of the distance to the
  nearest of them, with the bins of `distance_m`. Many rejected
  features close by hint at a matcher that misses some evidence, or at
  tags that differ between the spider and OpenStreetMap.
- `missing_taken`: the matcher accepted some, but another feature of the
  same spider took them first; see
  [`CONFLATED_PARQUET.md`](CONFLATED_PARQUET.md#match).

### Chance pairs

To see how many matches may come from chance, conflate moves about
100 features of each spider (all of a smaller spider) 2 km in a random
direction and matches them again. `chance_matched` of
`chance_sampled` still find a match, which can only be wrong. Their
share estimates the share of wrong matches among the spider's matches,
and overstates it, since in the real matching a true partner usually
takes the feature first. The sample is chosen by hashing, so it stays
the same from run to run on the same input. See
[the notes on infrastructure matching](../notes/2026-11-05-infrastructure-matching.md)
for where this comes from.

### How far tags agree

`tags` compares the tags that `match.evidence.agreeing` compares, for
the `same` rows. `atp` counts rows whose AllThePlaces feature has the
tag; these then split into `agreeing`, `differing` and `missing_in_osm`.
Tags no feature of the spider has are not listed. Phone numbers, email
addresses and websites count under `phone`, `email` and `website`
whichever key they're under, such as `contact:phone`.

### Brands

`brands` splits the spider's rows by the country of its features and
by their `brand:wikidata`, `operator:wikidata` and `network:wikidata`.
A feature with several IDs counts for each of them; a feature without
any counts under a null `wikidata_id`. So the brand counts of a spider
can add up to more than its rows.

`name` is the most common `brand`, `operator` or `network` name for
the ID in OpenStreetMap and AllThePlaces, preferring one in Latin
script; it is there so that readers needn't look up Wikidata IDs.

`osm_features` counts the OpenStreetMap features in the country with
the same ID, of the same kinds as the spider's features (such as
`shop=supermarket`), whether they matched or not. If a spider has 300
supermarkets in a country and OpenStreetMap 500, either the spider
misses some or OpenStreetMap has some that closed.
`deletion_candidates` counts the `deletion_candidate` rows with the
same ID in the country. Both are null for features without an ID.

## Example

With [DuckDB](https://duckdb.org/), the spiders with the most features
that found no match:

```sql
SELECT spider, missing_in_osm, rows
FROM 'spider-stats.parquet'
ORDER BY missing_in_osm DESC
LIMIT 10;
```

The license of each spider whose features we left out:

```sql
SELECT spider, not_for_osm, dataset_attributes['license'] AS license
FROM 'spider-stats.parquet'
WHERE not_for_osm > 0
ORDER BY not_for_osm DESC;
```

And the brands of one spider:

```sql
SELECT UNNEST(brands, recursive := true)
FROM 'spider-stats.parquet'
WHERE spider = 'denner_ch';
```
