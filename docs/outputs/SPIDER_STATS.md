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

## Change from last week

The file's key-value metadata `osmdiffs:previous` names the
`spider-stats.parquet` published before it, such as
`spider-stats-20261109-a1b2c3d4.parquet`. The name resolves next to
the file itself, in the same `data/` directory, and carries that
file's date; usually it is a week earlier, but more if a weekly run
failed. The very first file names none. To show how a spider changed,
fetch both files and compare their counts, for example `atp_features`,
`dump_features`, `atp_errors`, `rows`, or the match rate
(`same + located_in`, divided by `rows`). A spider whose feature count
halves, or whose match rate drops, most likely broke at its source or
in its conversion. A spider missing from the previous file is new.

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
held_back                UINT64   nullable — left out: spider held back until AllThePlaces fixes it
no_geometry              UINT64   nullable — left out: AllThePlaces gives no geometry
bad_geometry             UINT64   nullable — left out: geometry we can't use, see below
no_kind                  UINT64   nullable — left out: no tag says what kind of place it is
no_matcher               UINT64   nullable — no matcher handles their kind
in_china                 UINT64   nullable — features in mainland China
datum                    UTF8     nullable — "wgs84", "gcj02" or "bd09", for in_china
dataset_attributes       MAP<UTF8, UTF8> nullable — the spider's dataset attributes, such as its license
exact_coords_hits        UINT64   nullable — features right at an OSM feature with the same Wikidata ID
google_camera_coords_hits UINT64  nullable — features where a Google Maps camera center would be, see below
google_camera_coords_control UINT64 nullable — ... the same, mirrored: the control
google_camera_coords_offset_deg FLOAT nullable — median offset of those hits, in degrees of longitude
rows                     UINT64   non-null — rows in conflated.parquet
same                     UINT64   non-null — rows with match.relation = "same"
located_in               UINT64   non-null — rows with match.relation = "located_in"
missing_in_osm           UINT64   non-null — rows with match.relation = "missing_in_osm"
same_google_camera_coords UINT64  non-null — same rows matched at the corrected position
shared_osm_features      UINT64   non-null — same rows whose OSM feature another feature of the spider also matched
shared_positions         UINT64   non-null — features at the same position as another feature of the spider
shared_osm_features_same_kind UINT64 non-null — shared_osm_features, only among features of the same kind and brand
shared_positions_same_kind UINT64 non-null — shared_positions, only among features of the same kind and brand
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
opening_hours_top_value  UTF8     nullable — ... and that value
rules                    LIST<STRUCT> non-null
  rule                   UTF8     non-null — match.evidence.rule
  rows                   UINT64   non-null
tags                     LIST<STRUCT> non-null
  tag                    UTF8     non-null — "name", "phone", "opening_hours", ...
  atp                    UINT64   non-null — same rows whose ATP feature has the tag
  agreeing               UINT64   non-null — ... and OSM agrees
  differing              UINT64   non-null — ... and OSM has a different value
  missing_in_osm         UINT64   non-null — ... and OSM has none
  kinds                  LIST<STRUCT> non-null — how they agree or differ, see below
    kind                 UTF8     non-null — "same_schedule", "small", ...
    rows                 UINT64   non-null
values                   LIST<STRUCT> non-null — value quality over all rows, see below
  key                    UTF8     non-null — "phone", "start_date", ...
  atp                    UINT64   non-null — rows whose ATP feature has the key
  unparsed               UINT64   nullable — ... whose value we can't read
  top_value_atp          UINT64   non-null — ... with the most common value
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
  deletion_candidates_by_name UINT64 nullable — deletion candidates without an ID, by name, see below
  offset_east_m          FLOAT    nullable — like offset_east_m above, for this brand and country
  offset_north_m         FLOAT    nullable
  bbox                   STRUCT   nullable — like bbox above, for this brand and country
```

### What became of the features

The counts add up like this:

```
dump_features  = not_for_osm + held_back + no_geometry + bad_geometry + no_kind + no_matcher + rows
rows           = same + located_in + missing_in_osm
missing_in_osm = missing_no_candidate + missing_rejected + missing_taken
```

`held_back` counts the features of a spider whose data we leave out
until AllThePlaces fixes it, because it would mislead mappers. The
spiders and their AllThePlaces issues are listed in
`src/pipeline/held_back.rs`; today it is `chargepoint`
([alltheplaces#20717](https://github.com/alltheplaces/alltheplaces/issues/20717)).

`no_geometry` counts features that AllThePlaces gives without a
geometry (`"geometry": null`, or none at all). `bad_geometry` counts
features whose geometry is there but unusable: it doesn't parse, or its
coordinates are out of range. A feature line that isn't valid GeoJSON
counts as `bad_geometry` too.

`no_kind` counts features that have none of the keys that say what
kind of place something is: the primary keys of the OpenStreetMap
wiki's [Map features](https://wiki.openstreetmap.org/wiki/Map_features),
plus `club`, `police` and `advertising`. Most are address lists, such
as council datasets that give only `ref` and `addr:*`, or brand-only
features. There is nothing in them to match.

`no_matcher` counts features of a kind that we don't conflate yet,
such as `amenity=atm`, `amenity=parcel_locker` or `office=insurance`.
Up to version 0.8.19, most of these were counted as `invalid`, which
`no_geometry`, `bad_geometry` and `no_kind` replace.

`atp_features` and `atp_errors` come from AllThePlaces' own statistics
for the run. `atp_features` normally equals `dump_features`. Where it
doesn't, something went wrong between the spider and the dump. A
spider that crashed often has errors and no features at all.

`in_china` and `datum` concern features in mainland China. Many
Chinese sources give coordinates in the GCJ-02 or BD-09 datum, which
are hundreds of meters off. We find out which datum a spider uses by
comparing its features with OpenStreetMap, and convert them to WGS-84
before matching.

### Google Maps camera centers

Some brands give positions that someone copied from a Google Maps
place URL, `…/@lat,lon,17z`. That is the camera center of the map,
which Google moves west so that the side panel doesn't cover the
marker: about 0.0022° of longitude, 120–300 m depending on the
latitude. When `import_atp` reads a spider, it compares each feature
that has a `brand:wikidata`, `operator:wikidata` or `network:wikidata`
with the OpenStreetMap features of the same ID:

- `exact_coords_hits` counts features within 30 m of one.
- `google_camera_coords_hits` counts features that aren't, but have
  one 0.0018–0.0030° east, and less than 0.0003° north or south: where
  the place would be if the position is a camera center.
- `google_camera_coords_control` counts the same in the mirrored
  window to the west, where no copied position puts a feature. A
  spider whose hits don't clearly exceed its control is just dense.
- `google_camera_coords_offset_deg` is the median offset of the hits,
  the AllThePlaces position minus OpenStreetMap's, so negative.

A spider is flagged when it has at least 8 hits, at least four times
as many as its control plus one, and the hits are at least a tenth of
`exact_coords_hits + google_camera_coords_hits`. For a flagged spider,
matching also tries each feature at its position corrected by the
median offset, and `same_google_camera_coords` counts the `same` rows
that matched better there. The share
`google_camera_coords_hits / (exact_coords_hits + google_camera_coords_hits)`
says how much of the spider is affected; a dashboard can show it
next to the flag. See
[`2026-11-16-google-camera-coords.md`](../notes/2026-11-16-google-camera-coords.md).

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

Car dealers and banks make both counts high without anything being
wrong: a dealer lists sales, service and parts, and often several
brands such as Renault and Dacia, as separate features at one point
that all match one OpenStreetMap feature; a bank lists its branch and
its ATM. So `shared_osm_features_same_kind` and
`shared_positions_same_kind` count the same, but only among features
of the same kind: the same main category, such as `shop=car` (the
first of `shop`, `amenity`, `tourism`, `leisure`, `healthcare`, `craft`
and `office` that the feature has), and the same brand, operator and
network Wikidata IDs. A spider that puts features it can't place at a
default position still shows there, since those features are of one
kind. Files from before brawer/osmdiffs#975 lack these columns.

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
such as those of its head office. `opening_hours_top_value` is that
value, as the spider wrote it, so that a reader can tell a chain of
stores that really are all open around the clock (`24/7`, but also
`Mo-Su 00:00-24:00`) from one that writes `Mo-Fr 08:00-17:00`
everywhere. Of several equally common values, it is the smallest. Files
from before brawer/osmdiffs#975 lack it.

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

`tags` compares the tags that `match.evidence.agreeing_tags` compares, for
the `same` rows. `atp` counts rows whose AllThePlaces feature has the
tag; these then split into `agreeing`, `differing` and `missing_in_osm`.
Tags no feature of the spider has are not listed. Phone numbers, email
addresses and websites count under `phone`, `email` and `website`
whichever key they're under, such as `contact:phone`.

`kinds` says how they agree or differ, for tags where we tell kinds
apart; for the others, it is empty. For now, that is `opening_hours`,
compared as weekly schedules:

- `same_schedule`: the values differ as text but describe the same
  week, such as `24/7` and `Mo-Su 00:00-24:00`. These rows are among
  `agreeing`; the other agreeing rows have the same text.
- `small`: open on the same days, and on each day, the minutes open
  in only one of the two add up to 60 or less, such as
  `Mo-Fr 08:00-18:00` and `Mo-Fr 08:30-18:30`.
- `over_60_min`: open on the same days, but on some day by more than
  60 minutes differently.
- `other_days`: open on some day in one value and closed in the other.
- `unparsed`: one of the values uses syntax we don't read, such as
  months or comments.

The last four add up to `differing`. Files from before
brawer/osmdiffs#1000 lack `kinds`.

### Value quality

`values` looks at the values of the spider's features themselves, in
all rows, whether they matched or not: for each key of `name`,
`brand`, `phone`, `email`, `website`, `opening_hours`, `addr:street`,
`addr:postcode`, `addr:city`, `start_date` and `end_date` that any of
them has. Keys whose values are unique to a feature by nature, such
as `addr:housenumber` or `ref`, are left out. Unlike
`tags`, it goes by the key as written, so `contact:phone` doesn't
count under `phone`.

- `unparsed` counts values we can't read: `opening_hours` beyond the
  simple syntax that AllThePlaces writes, phone numbers not in
  international format (`+41 44 417 77 77`), email addresses and
  websites that don't look like any, and dates other than `YYYY`,
  `YYYY-MM` or `YYYY-MM-DD`, such as the timestamps that some spiders
  write into `start_date`. For keys without a reader, such as `name`,
  it is null.
- `top_value_atp` counts the rows with the most common value, so that
  `top_value_atp / atp` is its share, as `opening_hours_top_share` is
  for `opening_hours`. A share near 1 is expected for `name` or
  `brand`, but for `phone` or `opening_hours` it suggests that the
  spider writes the same value, such as its head office's, for every
  feature.

Files from before brawer/osmdiffs#1000 lack `values`.

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

`deletion_candidates_by_name` counts the `deletion_candidate` rows
with rule `name`: OpenStreetMap features without `brand:wikidata`,
`operator:wikidata` or `network:wikidata` whose `brand`, `operator` or
`network` is a name that ATP has in the country. Such a candidate
counts for a brand if any of the spider's features of that brand in
the country carries the same name, compared without regard to case
and spacing, so it also counts in the row with a null `wikidata_id`.
Like `deletion_candidates`, a candidate shared by several spiders
counts for each, and once per brand, even if it matches the brand by
more than one name. The field is null if none of the brand's features
has a name. The rows that the two fields count are disjoint, so their
sum is all deletion candidates of the brand; but the kind of feature
is not compared, so a spider with restaurants of a brand also gets
the brand's shops that are candidates by name.

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
