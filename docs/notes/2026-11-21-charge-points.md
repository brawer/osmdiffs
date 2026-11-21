# Charge points

AllThePlaces has about 520,000 charge points (`man_made=charge_point`), one per charging device. Until now, no matcher
took them, so all of them were `no_matcher`. Most come from `chargepoint` (496,732), and the rest from
`connected_kerb_gb` (7,800), `electrify_america_us` (6,289), `flo_ca_us` (5,498), `charge_place_scotland_gb` (1,731) and
`electrify_canada_ca` (276).

## What OSM has near them

For each ATP charge point, I looked up the nearest OSM `man_made=charge_point` and `amenity=charging_station` in the
openstreetmap.fr extracts of 2026-11-21, against the ATP run of 2026-11-14. "Station ≤ 30 m" includes the charge points
inside a station's outline.

| spider, region | charge points | OSM charge point ≤ 15 m | station ≤ 30 m | 30–50 m | 50–100 m | nothing ≤ 100 m |
|---|---:|---:|---:|---:|---:|---:|
| chargepoint, Netherlands | 79,383 | 1% | 17% | 3% | 7% | 73% |
| chargepoint, Denmark | 26,883 | 12% | 11% | 3% | 5% | 69% |
| chargepoint, Luxembourg | 1,311 | 1% | 9% | 5% | 10% | 76% |
| chargepoint, Colorado | 2,173 | 1% | 14% | 2% | 2% | 80% |
| electrify_america_us, Colorado | 241 | 0% | 82% | 7% | 2% | 7% |
| flo_ca_us, Quebec | 1,709 | 1% | 10% | 3% | 6% | 80% |
| electrify_canada_ca, Quebec | 56 | 0% | 21% | 7% | 14% | 57% |
| charge_place_scotland_gb, UK | 1,731 | 2% | 22% | 3% | 7% | 66% |
| connected_kerb_gb, UK | 7,800 | 3% | 6% | 1% | 1% | 88% |

The ATP positions are precise: 99% of `chargepoint`'s have four or more decimals. So most charge points are simply not
in OSM, which maps far fewer: the Netherlands extract has 23,679 charging stations and 1,763 charge points, against
79,383 ChargePoint devices there. Electrify America's fast chargers are the exception. Connected Kerb's are mostly
kerbside chargers, which OSM rarely maps.

## The rules

- **`same`, rule `kind`:** an OSM `man_made=charge_point` within 15 m, through the infrastructure matcher. Each OSM charge
  point takes one ATP charge point. The operator doesn't have to agree, since a charge point's network often isn't its
  operator.
- **`located_in`, rule `charging_station`:** otherwise, the OSM charging station whose outline contains the charge
  point, or else the one nearest to it within 50 m. Its operator doesn't have to agree either. Up to 50 m, the nearest
  station is mostly the right one. Beyond that, the extra 5–10% would more often land in the wrong station.

`electrify_america_us` and `flo_ca_us` also emit the station, and give each charge point the station's position: all
6,289 Electrify America charge points are at their station's point. So the OSM station that their ATP station
matched is the one the rule above finds anyway, and we don't need to look it up by `ref`.

## What we left out

- **Deletion signals.** An unmatched OSM charge point isn't a deletion candidate, even if ATP has the operator. Since
  spiders put every charge point of a station at one point, the OSM charge points spread around a station that no ATP
  charge point took within 15 m aren't gone. For the same reason, ATP charge points don't yet count as coverage for OSM
  charging stations. Today, that would only matter for `charge_place_scotland_gb`, whose stations aren't in ATP.
- **`chargepoint`.** 89% of its devices belong to roaming partners, but all of them say `brand=ChargePoint`
  ([alltheplaces#20717](https://github.com/alltheplaces/alltheplaces/issues/20717)). `import_atp` holds the spider back
  until that is fixed, and spider-stats counts its features as `held_back`. See `src/pipeline/held_back.rs`.
- **Suggested stations.** About 70% of charge points have no OSM station near them. Grouping them into suggested
  stations would help mappers. But only `chargepoint` and `charge_place_scotland_gb` emit charge points without their
  station, so it's better done in AllThePlaces (point 4 of alltheplaces#20717).

## Expected effect

Without `chargepoint`, about 20,000 charge points get a matcher. By the table, about 2% of them should become `same`,
roughly 15–30% `located_in` (more for Electrify America, fewer for Connected Kerb), and the rest `missing_in_osm`. The
next production run will give the real numbers.
