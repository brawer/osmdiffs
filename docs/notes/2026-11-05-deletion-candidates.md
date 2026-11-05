# OSM-only rows for deletion candidates

`conflated.parquet` now also has rows with an OpenStreetMap feature but
no AllThePlaces feature. AllThePlaces will never cover as much as OSM,
so most unmatched OSM features say nothing; we only emit a row when a
strong signal says that ATP would list the feature if it still
existed. The rules are in `src/pipeline/conflate/deletion.rs`. This
note records how they were chosen, measured on Swiss data.

## Setup

- OSM: Geofabrik's Switzerland extract (it reaches a few kilometers
  into the neighbouring countries).
- ATP: the 2026-10-10 run, filtered to `addr:country=CH`, including
  75,280 features of the `opentransportdata_swiss` spider (the Swiss
  public transport register). This run predates the spider fixes that
  add railway tracks.
- Each run takes about a minute; the OSM-only pass itself takes 0.2 s.

## The rules as first written: 57,000 rows

The first version flagged an unmatched OSM feature if (a) its
`brand:`, `network:` or `operator:wikidata` was anywhere in ATP, or (b)
it was a transit stop with a `ch:` IFOPT ID or a `uic_ref` starting
with `85`, or (c) it was a transit stop in Switzerland without any of
those IDs.

| signal | rows |
|---|---:|
| (a) Wikidata | 6,617 |
| (b) Swiss transit ID | 32,152 |
| (c) Swiss stop without ID | 18,209 |

Almost none of these are gone from the real world:

- 27,156 of (b) were stop positions. The register has no bus stop
  positions (and in this run no railway tracks either), so these can
  never be matched. Most other (b) rows were platforms of stops the
  register does have; OSM often maps more platforms than the register,
  and transit matching is one-to-one.
- (c) was mostly OSM bus platforms whose names differ from the
  register's ("Central" against "Zürich, Central"), plus 638 ski lift
  stations, which the register mostly lacks.
- 3,487 of (a) were Swiss Post post boxes and 394 post offices. ATP has
  them, but no matcher handles those kinds, so they never match.
  Bicycle rental and charging stations were the same.

## The rules now: 2,358 rows

| signal | rule | rows |
|---|---|---:|
| `wikidata` | ATP has features with this Wikidata ID, of the same kind (match mask), and that we have a matcher for | 1,087 |
| `swiss_transit_id` | transit stop whose Swiss IDs are all unknown to ATP (IFOPT IDs, `uic_ref`, and UIC codes derived from SLOIDs) | 1,046 |
| `swiss_transit_stop` | transit stop in Switzerland without any IFOPT ID or `uic_ref`, not an aerialway, and with no register stop within one S2 level-15 cell (about 280 m) | 225 |

Counts are by rule; the log line `conflate.deletion_candidates: done`
has the same numbers.

Other details:

- The Swiss rules only apply when ATP has at least 10,000 features with
  a Swiss IFOPT ID, so a week in which the spider fails doesn't flag
  every Swiss stop.
- For stops that a Swiss rule is about, the Wikidata rule doesn't
  apply: the register lists `network:wikidata` for its stops.
- Values separated by `;` are split, and any one of them counts.
- `uic_ref` is the right key: Swiss OSM has it on 64,568 objects.
- "In Switzerland" uses Natural Earth's 1:10m boundary, which is a few
  hundred meters off in places. Konstanz south of the Rhine counts as
  Swiss, which gave 7 of the 225 `swiss_transit_stop` rows.

What the remaining rows look like:

- `wikidata`: fuel stations (SOCAR, Avia, Eni, BP, Tamoil, Shell),
  Jumbo, k kiosk, Marionnaud, Migros, Denner, Spar, car dealers. These
  are what the rule is for. 26 of them are just outside Switzerland,
  which is an artifact of this eval: ATP was filtered to Switzerland,
  so a brand's German shops look unlisted.
- `swiss_transit_id`: mostly bus platforms and stop positions whose
  `uic_ref` the register doesn't have, such as "Goldgrubenweg"
  (8593333) or "Koppigen, Aspi" (8593259). These are stops the
  register no longer lists, or OSM features with a wrong code. 65 are
  outside Switzerland, mostly in Liechtenstein, whose stops the CH
  filter of this eval dropped from ATP.
- `swiss_transit_stop`: platforms and stop positions without IDs, far
  from any register stop.

## Known gaps

- On the planet, a brand counts as covered as soon as ATP has it
  somewhere. A chain that ATP only lists in one country will flag its
  shops in every other country. Checking coverage per country would
  fix that.
- `conflated.pmtiles` and `suggest_edits` skip OSM-only rows for now.
- Next: emit OSM-only rows for POIs whose brand, network or operator
  *name* ATP has, for features without Wikidata IDs.
