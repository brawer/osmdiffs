# Matching waste baskets, fountains and toilets

Follow-up to
[`2026-11-06-hydrants-bookcases-bicycle-parking.md`](2026-11-06-hydrants-bookcases-bicycle-parking.md),
with the same method. This note adds three more kinds to
`InfrastructureMatcher`.

## Short answer

| kind | default radius | per spider |
| --- | ---: | --- |
| `amenity=waste_basket` | 15 m | Copenhagen 10 m, Glasgow 20 m |
| `amenity=fountain`, `amenity=drinking_water` | 15 m | Zürich: 10 m, ref without `wvz-` |
| `amenity=toilets` | 30 m | Zürich 20 m, Changing Places 100 m |

Fountains and drinking water points are one kind, since OSM tags
Zürich's fountains mostly as `amenity=drinking_water`. No tag confirms
a match for these kinds. `amenity=toilets` now gets the
street-furniture match mask; before, it had none.

## How the evals worked

As in the earlier notes: ATP run of 2026-10-10 and Geofabrik extracts
of early November 2026 (Scotland, Greater London, Denmark) or earlier
(South Yorkshire, Australia, New Zealand, Washington, Switzerland),
with OSM nodes and ways. ATP features are paired one to one with OSM
features of the same kind, closest first, up to 200 m. Chance pairs
come from ATP features moved 2 km in a random direction.
`inf_osm2.py` is `inf_osm.py` with these kinds. For fountains,
`inf_eval.py` reads the OSM kinds to compare from `OSM_KV`, such as
`amenity=fountain|amenity=drinking_water`.

## Waste baskets

| spider | ATP | OSM | 0–2 m | 2–5 m | 5–10 m | 10–20 m | 20–50 m | chance 0–10 m / 10–20 m / 20–50 m |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `glasgow_city_council_waste_baskets_gb` | 5,605 | 702 | 70 | 160 | 135 | 110 | 60 | 7 / 23 / 125 |
| `sheffield_city_council_litter_bins_gb` | 3,002 | 388 | 118 | 84 | 44 | 22 | 17 | 2 / 7 / 47 |
| `copenhagen_municipality_waste_baskets_dk` | 4,913 | 2,024 | 607 | 364 | 158 | 93 | 111 | 23 / 76 / 372 |
| `auckland_council_furniture_nz` | 7,566 | 1,583 | 662 | 285 | 140 | 79 | 94 | 5 / 12 / 84 |
| `kingston_city_council_waste_baskets_au` | 834 | 157 | 75 | 36 | 16 | 4 | 7 | 1 / 0 / 4 |
| `hobsons_bay_city_council_waste_baskets_au` | 793 | 63 | 13 | 22 | 5 | 10 | 3 | 0 / 0 / 3 |

- Most right pairs are within 10 m. In Copenhagen, chance pairs nearly
  catch up at 10–20 m (76 against 93), so it gets 10 m. Glasgow's
  positions are rougher, with 110 pairs at 10–20 m against 23 by
  chance, so it gets 20 m.
- OSM has far fewer waste baskets than the cities list: 702 near
  Glasgow's 5,605.
- Tags don't help. Copenhagen's are all `waste=trash`. Besides refs,
  which OSM doesn't carry, the other spiders have only `volume`
  (Glasgow, Hobsons Bay) and `material` (Hobsons Bay), and OSM rarely
  has either on these baskets.

## Fountains and drinking water

| spider | ATP | OSM | 0–2 m | 2–5 m | 5–10 m | 10–20 m | 20–50 m | chance 0–10 m / 10–20 m / 20–50 m |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `stadt_zuerich_ch` | 1,287 | 1,209 | 838 | 143 | 36 | 15 | 11 | 1 / 12 / 92 |
| `auckland_council_furniture_nz` | 508 | 369 | 119 | 85 | 24 | 17 | 5 | 0 / 0 / 0 |
| `melbourne_city_council_drinking_fountains_au` | 338 | 398 | 102 | 81 | 54 | 15 | 6 | 4 / 2 / 19 |
| `seattle_parks_and_recreation_drinking_fountains_us` | 219 | 145 | 39 | 39 | 13 | 7 | 3 | 0 / 0 / 0 |

OSM counts here include `amenity=fountain`, `amenity=drinking_water`
and, for the last three, `man_made=water_tap` and
`man_made=drinking_fountain`.

- Zürich's spider emits `amenity=fountain` for all 1,287 fountains
  (1,248 of them with `drinking_water=yes`). In OSM, 86% of the
  features within 2 m are `amenity=drinking_water`. Compared with OSM
  `amenity=fountain` alone, only 115 pairs are within 2 m. So the
  matcher treats both tags as one kind. The spider might better emit
  `amenity=drinking_water` for drinking fountains, as the other
  spiders do.
- OSM has Zürich's fountain numbers in `ref`, without ATP's `wvz-`
  prefix (ATP `wvz-22`, OSM `22`). Within 10 m, 755 of 763 OSM refs
  are equal. Only 19 of the 784 OSM fountains that share a number with
  an ATP fountain are more than 20 m away. A new ref rule,
  `StripPrefix`, compares them.
- Fountains are sparse, with a median of 97–255 m to the next one, and
  positions are precise. Zürich gets 10 m, since chance pairs catch up
  at 10–20 m (12 against 15); the others get 15 m.

## Toilets

| spider | ATP | OSM | 0–5 m | 5–10 m | 10–20 m | 20–50 m | 50–100 m | 100–200 m | chance 0–20 m / 20–100 m / 100–200 m |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `stadt_zuerich_ch` | 91 | 238 | 51 | 20 | 15 | 3 | 0 | 0 | 0 / 8 / 15 |
| `changing_places_gb` (Scotland) | 229 | 252 | 6 | 3 | 11 | 23 | 25 | 24 | 0 / 0 / 1 |
| `changing_places_gb` (Greater London) | 186 | 251 | 2 | 2 | 5 | 23 | 22 | 26 | 0 / 3 / 4 |
| `changing_places_gb` (South Yorkshire) | 63 | 60 | 0 | 0 | 4 | 3 | 6 | 9 | 0 / 0 / 2 |
| `glen_eira_city_council_toilets_au` | 117 | 38 | 24 | 10 | 1 | 1 | 1 | 0 | 1 / 3 / 1 |
| `melbourne_city_council_toilets_au` | 70 | 102 | 52 | 8 | 3 | 3 | 0 | 1 | 1 / 7 / 3 |
| `frankston_city_council_toilets_au` | 84 | 47 | 2 | 3 | 6 | 16 | 5 | 4 | 0 / 0 / 2 |

- Toilets of Zürich, Melbourne and Glen Eira are mostly within 20 m;
  Frankston's are more often 20–50 m off. Zürich gets 20 m, other
  spiders 30 m.
- Changing Places lists accessible toilets inside leisure centres,
  shopping centres and stations, placed at the building's address.
  Right pairs spread out to 200 m, and chance pairs stay rare even
  there, since public toilets are sparse. It gets 100 m.
- `wheelchair` agrees on 77–86% of close pairs in Melbourne, but most
  of both sides say `yes`, so it isn't compared. Zürich's `name`
  (`Landiwiese (rollstuhlgängig)`) never equals OSM's.
- Glen Eira lists several toilets per site: the median distance to the
  next one is 2 m. With one-to-one matching, the others stay unmatched.

## Results on Switzerland

Swiss OSM extract and ATP 2026-10-10 filtered to `addr:country=CH`,
against main with the matchers of the hydrant note:

| measure | before | after |
| --- | ---: | ---: |
| Zürich fountains matched | 0 | 1,018 of 1,287 (79%) |
| of those, with the same ref | | 761 |
| matched fountain distance, median / 90th percentile | | 0.7 m / 3.0 m |
| Zürich toilets matched | 0 | 86 of 91 |
| matched toilet distance, median / 90th percentile | | 2.9 m / 11.4 m |
| deletion candidates `wikidata` / `name` | 1,303 / 416 | 1,303 / 416 |

Of the 1,018 fountain matches, 874 are OSM `amenity=drinking_water`.
The Swiss ATP extract has no waste baskets. No other row changed.

## Known limits

- Sites that list several toilets or several baskets at one spot
  match only one of them.
- Changing Places toilets are rooms inside buildings, which OSM maps
  as nodes, as areas, or not at all, so a match 100 m away may be
  another toilet of the same building.
- Most unmatched ATP waste baskets would be additions at import
  scale, which need community review.
