# Matching fire hydrants, public bookcases and bicycle parking

Follow-up to
[`2026-11-05-infrastructure-matching.md`](2026-11-05-infrastructure-matching.md),
which added `InfrastructureMatcher` for post boxes, AEDs and benches.
This note adds three more kinds to it. We chose them because many
spiders or countries have them, OSM maps all three, and they need no
code beyond what benches use, except that bicycle parking may share.

## Short answer

| kind | default radius | per spider | confirms a match |
| --- | ---: | --- | --- |
| `emergency=fire_hydrant` | 10 m | EPCOR: same `ref` | `fire_hydrant:type` |
| `amenity=public_bookcase` | 30 m | Little Free Library: 50 m, same `ref` | |
| `amenity=bicycle_parking` | 15 m | Zürich, Cambridge (US): 10 m; San Francisco: 20 m | `capacity` |

Bicycle parking that OSM maps as one feature with a larger `capacity`
may take several ATP features, since some cities list each rack on
its own. Public bookcases are never deletion candidates.

## How the evals worked

As in the earlier note: ATP run of 2026-10-10, Geofabrik extracts of
2026-11-05 (Alberta, Massachusetts, Northern California, Oregon) or
earlier (Australia, Washington, Switzerland), greedy one-to-one
pairing up to 200 m, and "chance pairs" with every ATP feature moved
2 km in a random direction. This time OSM ways count too, at the mean
of their nodes, since bicycle parking is often an area (411 of the
2,762 near Zürich's). OSM features count only within 300 m of an ATP
feature of the spider, and ATP features only within 5 km of an OSM
one, so that a city's spider isn't compared against an empty area.
Tag agreement compares values without spaces and case.

The two scripts aren't checked in: `inf_osm.py` extracts the three
kinds from a PBF file with pyosmium, and
`inf_eval.py <geojson> <osm.jsonl> <key=value> <tags to compare>
[ATP filters]` prints the tables below.

## Fire hydrants

| spider | ATP | OSM | 0–2 m | 2–5 m | 5–10 m | 10–20 m | 20–50 m | chance 0–10 m / 10–20 m / 20–50 m |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `barwon_water_potable_fire_hydrants_au` | 18,236 | 177 | 105 | 41 | 7 | 4 | 9 | 2 / 13 / 44 |
| `epcor_fire_hydrants_ca` (Edmonton) | 20,559 | 460 | 344 | 54 | 13 | 10 | 10 | 10 / 15 / 90 |
| `bend_city_council_fire_hydrants_us` | 5,803 | 175 | 160 | 6 | 2 | 0 | 0 | 3 / 15 / 48 |
| `cambridge_city_council_fire_hydrants_us` | 1,954 | 706 | 238 | 203 | 67 | 34 | 30 | 13 / 32 / 126 |

ATP counts are those within 5 km of an OSM hydrant; Barwon Water has
30,307 in all.

- Hydrants are precise and sparse: the median distance to the next
  ATP hydrant is 28–88 m, and right pairs are mostly within 5 m. At
  10–20 m, chance pairs already catch up (Cambridge 34 against 32), so
  the radius is 10 m.
- OSM has few of them. Against 18,236 Barwon Water hydrants, OSM has
  177 nearby. Most ATP hydrants will stay unmatched; they would be
  additions at import scale.
- `fire_hydrant:type` (`underground`, `pillar`) agrees on all 157 close
  pairs of Barwon Water, so it confirms a match.
- In Edmonton, OSM has EPCOR's IDs in `ref`: 98% equal within 2 m.
  Other spiders' refs (`WFH004918` in Bend, `PH6000` in Cambridge)
  don't occur in OSM.

## Public bookcases

Little Free Library (`little_free_library`, 92,980 bookcases, 86,122
in the US and 4,007 in Canada) is the one big source. It has
`brand:wikidata`, but `PoiMatcher` leaves street furniture alone, so
it matched nothing before. We compared it state by state:

| state | ATP | OSM | 0–5 m | 5–10 m | 10–20 m | 20–50 m | 50–100 m | chance 0–20 m / 20–50 m / 50–100 m |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Washington | 2,392 | 843 | 8 | 29 | 243 | 142 | 43 | 1 / 14 / 34 |
| Northern California | 2,881 | 534 | 12 | 21 | 127 | 74 | 25 | 0 / 5 / 20 |
| Oregon | 1,316 | 174 | 1 | 5 | 56 | 26 | 9 | 3 / 1 / 4 |
| Massachusetts | 1,075 | 148 | 6 | 14 | 35 | 27 | 10 | 0 / 2 / 5 |
| Alberta | 409 | 43 | 4 | 2 | 14 | 7 | 1 | 0 / 0 / 0 |

- Positions are offset: most right pairs are 10–50 m apart, probably
  because the registry places a library at its address while OSM
  maps it at the curb. Chance pairs stay rare up to 50 m, so the
  radius is 50 m. Other bookcase spiders get 30 m.
- Where OSM has a `ref`, it is the charter number, equal on 96% of
  pairs within 50 m. A few OSM libraries use `ref:lfl` instead (6 in
  Massachusetts); we only compare `ref`.
- `name` never agrees: ATP's names hold the steward, charter number
  and town (`Paul Downey #51931 Kalispell MT`), OSM's are
  `Little Free Library` or a nickname.

Public bookcases are never deletion candidates. With the matcher, the
deletion rules first flagged four Swiss bookcases with
`brand=Little Free Library`. One was 88 m from an ATP library, beyond
the radius, and one carried a charter number that ATP doesn't list in
Switzerland. OSM uses the brand for libraries that aren't in the
registry, and the registry's positions are too rough, so a missing
ATP library says little.

## Bicycle parking

| spider | ATP | OSM | 0–2 m | 2–5 m | 5–10 m | 10–20 m | 20–50 m | chance 0–10 m / 10–20 m / 20–50 m |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `stadt_zuerich_ch` | 2,865 | 2,762 | 922 | 474 | 206 | 73 | 113 | 24 / 88 / 354 |
| `san_francisco_mta_bicycle_parking_us` | 5,964 | 1,580 | 28 | 62 | 144 | 358 | 397 | 35 / 80 / 341 |
| `cambridge_city_council_bicycle_parking_us` | 2,684 | 1,523 | 216 | 161 | 105 | 91 | 206 | 28 / 52 / 181 |
| `bend_city_council_bicycle_parking_us` | 154 | 135 | 4 | 9 | 7 | 6 | 7 | 0 / 1 / 1 |
| `greater_geelong_city_council_bicycle_parking_au` | 104 | 65 | 0 | 6 | 18 | 8 | 9 | 0 / 0 / 0 |

- Zürich is precise, and its chance pairs pass the right ones at
  10–20 m, so it gets 10 m, as do Cambridge (91 against 52 at
  10–20 m, with many racks close together). San Francisco's right
  pairs are mostly 5–50 m apart, and chance pairs stay well below them
  up to 20 m, so it gets 20 m. Other spiders get 15 m.
- `capacity` agrees on about 60% of close pairs in San Francisco and
  Cambridge, against 25% and 33% for random pairs, since most racks
  hold 2 bicycles. It confirms a match weakly, as a bonus that never
  rules one out. Zürich has no capacities in ATP.
- `bicycle_parking` (the type) agrees on 0–4% of close pairs: the
  spiders say `rack` for everything, where OSM says `stands`,
  `wall_loops` or `bollard`. It is not compared.
- Cambridge lists each rack on its own: 711 of its 2,684 racks share
  a position with another, and almost all have `capacity=2`. OSM often
  maps a group of racks as one node with the total capacity. In a
  one-to-one pairing within 10 m, 610 more Cambridge racks could join
  159 OSM features with a larger capacity, and for half of those, the
  ATP capacities add up to OSM's exactly. So an OSM bicycle parking
  with a larger `capacity` than the ATP feature may be shared
  (`Matcher::may_share`). San Francisco has few such groups (9 within
  10 m).

## Results on Switzerland

Swiss OSM extract and ATP 2026-10-10 filtered to `addr:country=CH`,
against main with the bench matcher:

| measure | before | after |
| --- | ---: | ---: |
| Zürich bicycle parking matched | 0 | 1,571 of 2,865 (55%) |
| matched distance, median / 90th percentile | | 1.6 m / 5.7 m |
| Little Free Libraries matched | 0 | 4 of 16 |
| deletion candidates `wikidata` / `name` | 1,303 / 416 | 1,303 / 416 |

Switzerland has no hydrants in ATP. No other row changed. No OSM
bicycle parking in Zürich took several ATP features, since Zürich's
ATP data has no capacities.

## Results on the eval regions

On the [eval regions](2026-11-06-eval-regions.md), only bookcases
and bicycle parking show up; the table for all kinds is in
[`2026-11-05-infrastructure-matching.md`](2026-11-05-infrastructure-matching.md#results-on-the-eval-regions).

- Little Free Libraries: 4 of 16 matched in Switzerland, 81 of 2,670
  in Massachusetts (median 15 m), and none of 48 in Japan or 39 in
  Brazil's Sudeste.
- Bicycle parking: 1,571 of 2,865 in Zürich, as above, and 146 of 702
  in Japan, from parking operators such as `cyca_parking_jp` and
  `tobu_park_jp`.
- No eval region has fire hydrants in ATP. Cambridge's hydrants and
  bicycle parking, measured above, have no license in ATP's
  metadata, so the pipeline skips them and they aren't in the
  Massachusetts run.

## Known limits

- OSM has few hydrants where ATP has many, so this kind is mostly
  about additions, which need community review before any import.
- Shared bicycle parking is only checked by capacity. Whether the
  shared ATP racks add up to OSM's capacity, and whether the right
  edit is to merge them or to split OSM's node, is for the edit
  suggester.
- Niagara Falls hydrants weren't evaluated: the Ontario extract is
  too large for a quick check. They get the 10 m default.
