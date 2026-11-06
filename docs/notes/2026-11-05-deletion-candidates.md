# OSM-only rows for deletion candidates

`conflated.parquet` now also has rows with an OpenStreetMap feature but
no AllThePlaces feature. AllThePlaces will never cover as much as OSM,
so most unmatched OSM features say nothing; we only emit a row when a
strong signal says that ATP would list the feature if it still
existed. The rules are in `src/pipeline/conflate/deletion.rs`. This
note records how they were chosen, measured on Swiss data. The
`name` rule came later; see [Names](#names).

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

## The rules now: 2,252 rows

| signal | rule | rows |
|---|---|---:|
| `wikidata` | ATP has features with this Wikidata ID, of the same kind (match mask), in the same country, and that we have a matcher for | 1,058 |
| `swiss_transit_id` | transit stop whose Swiss IDs are all unknown to ATP (IFOPT IDs, `uic_ref`, and UIC codes derived from SLOIDs) | 1,046 |
| `swiss_transit_stop` | transit stop in Switzerland without any IFOPT ID or `uic_ref`, not an aerialway, and with no register stop within one S2 level-15 cell (about 280 m) | 148 |

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
- Countries come from the
  [country-boundaries](https://crates.io/crates/country-boundaries)
  crate, whose data is derived from OpenStreetMap (ODbL) and listed as
  an input in the provenance BOM. It gets the border cases right:
  Konstanz, Büsingen and Campione are not Swiss; Kreuzlingen and Basel
  are. An earlier version used Natural Earth's 1:10m boundary, which
  counted Konstanz south of the Rhine as Swiss and gave 225
  `swiss_transit_stop` rows instead of 148.
- A brand counts as covered only in the countries where ATP has
  features of it. The country of an ATP feature is that of its
  location, not its `addr:country`.

What the remaining rows look like:

- `wikidata`: fuel stations (SOCAR, Avia, Eni, BP, Tamoil, Shell),
  Jumbo, k kiosk, Marionnaud, Migros, Denner, Spar, car dealers. These
  are what the rule is for. Before brand coverage was per country, 26
  more were just outside Switzerland, because ATP was filtered to
  Switzerland for this eval and a brand's German shops looked
  unlisted.
- `swiss_transit_id`: mostly bus platforms and stop positions whose
  `uic_ref` the register doesn't have, such as "Goldgrubenweg"
  (8593333) or "Koppigen, Aspi" (8593259). These are stops the
  register no longer lists, or OSM features with a wrong code. 65 are
  outside Switzerland, mostly in Liechtenstein, whose stops the CH
  filter of this eval dropped from ATP.
- `swiss_transit_stop`: platforms and stop positions without IDs, far
  from any register stop.

## Liechtenstein

The Swiss register covers Liechtenstein too. The 2026-10-10 run of
`opentransportdata_swiss` has 633 features with `addr:country=LI`, all
with `ch:` IFOPT IDs and `85` UIC codes. For example, Vaduz, Alte
Rheinbrücke is `ch:1:sloid:81821` and `uic_ref=8581821`, the same
`uic_ref` as in OSM. So `swiss_transit_id` already works there, and
`swiss_transit_stop` applies in Liechtenstein as well as in
Switzerland.

The eval above filtered ATP to `addr:country=CH`, so it lacked
Liechtenstein's stops, and 19 of the `swiss_transit_id` rows were in
Liechtenstein for that reason alone. With Liechtenstein's features
added back, `swiss_transit_id` drops from 1,046 to 1,027, and
`swiss_transit_stop` stays at 148. The Geofabrik extract only covers a
strip of Liechtenstein along the Rhine, about 20 stops, so this says
little about matching there.

## Names

A fourth rule, `name`, covers features without any of the three
Wikidata IDs: it flags an unmatched OSM feature if ATP has features of
the same kind in the same country whose `brand`, `network` or
`operator` has the same name as the OSM feature's `brand`, `network`
or `operator`. Names are compared after lowercasing and collapsing
whitespace. These runs include ATP's Liechtenstein features.

| step | `name` rows |
|---|---:|
| first version | 996 |
| `PoiMatcher` matches by brand name, see [`2026-11-05-brand-name-matching.md`](2026-11-05-brand-name-matching.md) | 543 |
| ATP must have at least 3 features with the name in the country | 476 |
| `shop=car;car_repair` agrees with `shop=car_repair` when matching | 445 |
| no name rule for ski lifts in Switzerland and Liechtenstein | 380 |

- The first version mostly flagged features that ATP had nearby: of
  931 flagged features that weren't transit stops, 515 had an ATP
  feature with the same name within 50 m. The matcher only knew
  `brand:wikidata`, phones and emails, and these OSM features had
  none of them.
- Bicycle shops tag the brands they sell, such as
  `brand=Veloplus;Trek;Specialized;Cube`. ATP has a single Trek store
  in Switzerland, in Lausanne, so 43 shops were flagged. The minimum
  of 3 ATP features removed them, plus 3 Guess shops and a Coiff&Co.
  Values with more than 3 names are also ignored, since a list that
  long names what a shop sells, not its brand. Car dealers often have
  2 or 3, such as `brand=Renault;Dacia;Alpine`.
- The Swiss register lists cable cars with the names of their
  operators, such as Téléverbier, but few ski lifts, so 65 ski lift
  stations were flagged by their operator's name.
- For features with a Wikidata ID, names don't count.

The other rules stay as they were: 1,059 `wikidata` (one more, because
ATP features with only a brand name now have a matcher),
1,027 `swiss_transit_id` and 148 `swiss_transit_stop`.

Of the remaining 380 `name` rows (counted without transit stops),
308 have no unmatched ATP feature with the same name within 1 km.
Most are fuel stations (SOCAR, Avia, Mini Prix, BP) and car dealers
or garages (Toyota, Ford, Volkswagen, Mitsubishi, Honda, Renault).
For the dealers, this mostly means that ATP doesn't list them as
official dealers of that brand anymore, which a mapper should check
before changing anything. 34 rows have an unmatched ATP feature
with the same name within 50 m, mostly where OSM has the brand only
as `operator` and the categories differ, such as a florist with
`operator=Migros` next to a Migros supermarket.

## Results on the eval regions

Main on the [eval regions](2026-11-06-eval-regions.md), with the
setup described there. The Swiss transit rules don't apply outside
Switzerland and Liechtenstein.

| region | `wikidata` | `name` | `swiss_transit_id` | `swiss_transit_stop` | most common |
|---|---:|---:|---:|---:|---|
| Switzerland | 1,304 | 416 | 1,027 | 148 | Swiss Post boxes (260), SOCAR (98), Jumbo (75), Avia (66) |
| Japan | 14,018 | 1,441 | 0 | 0 | Japan Post boxes (2,593), ENEOS (829), まいばすけっと (717), 7-Eleven (697+374), Japan Post offices (579), Lawson (555) |
| US: Massachusetts | 1,850 | 36 | 0 | 0 | CVS (360), USPS boxes (55), Tesla Superchargers (48), Chase (46), Bank of America (46) |
| Germany: Berlin | 498 | 153 | 0 | 0 | DB stop areas (96), Deutsche Post boxes (64), TotalEnergies chargers (34), DHL (32) |
| Brazil: Sudeste | 4,020 | 686 | 0 | 0 | Ipiranga (1,383), Shell (750), Bradesco (681), BR (489), Santander (322) |
| Thailand | 2,882 | 368 | 0 | 0 | 7-Eleven (2,177), Thailand Post (167), PTT (122) |
| Kenya | 113 | 14 | 0 | 0 | Total and TotalEnergies (35), Shell (32), Rubis (31) |
| United Arab Emirates | 278 | 7 | 0 | 0 | McDonald's (89 in Arabic, 17 in Latin), Starbucks (22) |
| India: Karnataka | 393 | 27 | 0 | 0 | HDFC Bank (67), Axis Bank (30), Bank of Baroda (23) |
| India: Tamil Nadu | 174 | 15 | 0 | 0 | Hindustan Petroleum (27), HDFC Bank (24) |

The "most common" column counts OSM-only rows by their `brand`,
`operator` or `network` value, whatever the rule.

- **Berlin's DB stop areas are false positives.** The Swiss register
  (`opentransportdata_swiss`) lists a few stops in Germany, 8 of them
  stop areas with `operator=DB Netz AG`. That makes DB look covered
  in Germany, and the `name` rule flags 96 Berlin S-Bahn stop areas
  with `operator=DB Netz AG;DB Station&Service AG`. The name rule
  needs a minimum that a border feed can't reach, or transit kinds
  should be left out of it, as ski lifts are.
- Japan has the most rows by far. 2,593 are OSM post boxes of Japan
  Post that no ATP box matched within the 20 m radius, and most of
  the rest are convenience stores and fuel stations.
- In Brazil, fuel stations and banks make up most rows.
- None of these were checked by hand. How many are closures, and how
  many are matches that the matcher misses, is open for each region.

## Known gaps

- Within a country, a brand counts as covered everywhere, even if a
  spider only covers one region.
- `conflated.pmtiles` and `suggest_edits` skip OSM-only rows for now.
- The kinds are coarse: a florist with `operator=Migros` is a shop,
  and so is a Migros supermarket. The name rule flags such florists if
  ATP doesn't have them.
