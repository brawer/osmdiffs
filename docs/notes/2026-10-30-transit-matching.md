# Matching public transport

Investigation note behind `TransitMatcher`
([`src/matchers/transit_matcher.rs`](../../src/matchers/transit_matcher.rs)).
Before this change, no matcher handled public transport, so none of
the 111,764 features of the Swiss spider `opentransportdata_swiss`
matched anything in OpenStreetMap (OSM).

## Short answer

Public transport has no `brand:wikidata`, but it has registry IDs.
`ref:IFOPT` (in Switzerland also `ref:sloid` or `railway:ref:sloid`)
names a stop, an area or a single quay, and `uic_ref` names a station.
In Switzerland, OSM has the exact quay ID on only about 1% of
platforms, but `uic_ref` on nearly every part of a stop. So most
matches go by the stop's ID, and `local_ref` (track or platform
number) and distance pick the right platform within the stop. Kinds
must fit: a stop never takes one of its platforms, and a track never
takes a platform.

## Results on Switzerland

Swiss OSM extract and ATP from 2026-10, with the spider changes from
[alltheplaces#20092–#20116](https://github.com/alltheplaces/alltheplaces/pull/20116).
Counts are for features in Switzerland. Stops abroad hardly match,
because the extract covers only Switzerland.

| ATP feature      | count  | matched | note |
|------------------|-------:|--------:|------|
| `platform`       | 54,380 | 84%     | 10,969 of them to a bus stop mapped as one node on the road |
| `stop_position` (railway track) | 1,864 | 94% | 1,591 with equal `local_ref` |
| `station`        | 2,500  | 78%     | 472 to a `stop_area` relation |
| `stop_area`      | 24,417 | 21%     | OSM rarely has a stop area relation for bus stops |

All 461 ATP features whose exact ID is on an OSM feature of the same
kind matched that feature.

To check matches that don't rest on an exact ID, we removed the ID from
those ATP features and gave them only their stop's `uic_ref`. Of the
469 platforms and tracks:

- 424 (90%) still matched the same OSM feature;
- 28 (6%) matched a neighbouring platform of the same stop, mostly
  the bus stop for the opposite direction when neither side has a
  `local_ref`;
- 13 (3%) matched another OSM feature without an ID, often the same
  stop mapped twice;
- 4 stayed unmatched.

## Rules worth knowing

- **Disagreeing IDs veto a match**, like `brand:wikidata` in
  `PoiMatcher`: different `uic_ref`, a different stop, or a different
  quay of the same stop. IDs from different countries (`ch:` and `de:`)
  aren't compared, since a stop near the border can have both.
- **An exact ID always beats the same stop**: it scores at least 0.9,
  a same-stop match at most 0.8, a name-only match at most 0.4.
- **Bus stops mapped as one node** (`public_transport=stop_position`
  with `highway=bus_stop`) count as platforms, slightly below a real
  platform with the same evidence.
- **Modes must be compatible**: a train platform doesn't match a bus
  stop. A trolleybus is a bus, and light rail fits both trains and
  trams.
- **Names only count without IDs to compare**, within 100 m, because
  names repeat ("Bahnhof").

## Known limits

- **Many to one.** Each ATP feature takes its best OSM candidate, and
  nothing stops two ATP features from taking the same one. For 37% of
  Swiss platforms, another ATP platform matched the same OSM feature,
  mostly where OSM maps both directions of a bus stop as one node. A
  one-to-one assignment would need a change in `pipeline::conflate`.
- **Zürich HB tracks 31–34 and 41–44**: OSM gives them the UIC codes
  of the former stations Löwenstrasse and Museumstrasse, the Swiss
  registry the code of Zürich HB, so they don't match.
- **Other countries**: IFOPT handling follows EN 28701 (stop, area,
  quay), with Switzerland's extra `sloid` field. The unit tests have
  German cases; other countries need real examples in the test table
  before we rely on them.
