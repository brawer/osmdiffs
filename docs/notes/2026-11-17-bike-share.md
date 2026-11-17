# Matching bike-share stations

AllThePlaces (ATP) has about 214,000 bike-share stations from GBFS
feeds (spider `gbfs`), 22,000 of them in Japan. Before this change no
matcher took them, so they never matched an OpenStreetMap (OSM)
feature. They are now a kind of street infrastructure in
[`InfrastructureMatcher`](../../src/matchers/infrastructure_matcher.rs).

## Short answer

- **GBFS ID.** ATP writes each station's ID as
  `ref:gbfs=hellocycling:1234` (system ID, colon, station ID). OSM has
  the same ID on about 26,000 stations, written in one of three ways:
  `ref:gbfs` like ATP (6,376 features, mostly Canada, Britain and the
  US), `gbfs:system_id` plus `gbfs:station_id` (about 20,000, nearly
  all in Japan), or `ref:gbfs:<system>` (1). The same ID matches up to
  200 m away. Another ID of the same system rules a match out.
- **Station number.** Many feeds give the number that riders see as
  GBFS `short_name`, which OSM has as `ref`: Bluebikes `D32022`,
  nextbike `80285`. It agrees on 251 of 261 matched Bluebikes pairs
  where both sides have one. The same number counts like the same ID.
- **Position.** Otherwise a station matches the closest one within
  20 m, or within 50 m when the names share a word of at least four
  letters.
- **Brand.** A different `brand:wikidata`, or brand, network and
  operator names that share nothing, rule a match out. Without this,
  Voi and Dott parking zones took PubliBike and nextbike stations.
  Rental shops (`bicycle_rental=shop`) are never bike-share stations.

| region | matches before → after | gain | by ID or number | new deletion candidates |
|---|---:|---:|---:|---:|
| Japan | 74,858 → 94,204 | +19,346 (+26%) | 19,322 | 337 |
| Switzerland | 66,878 → 67,295 | +417 | 45 | 30 |
| US: Massachusetts | 7,868 → 8,276 | +408 | 267 | 34 |
| Brazil: Sudeste | 6,294 → 6,581 | +287 | 0 | 0 |
| Germany: Berlin | 6,278 → 6,315 | +37 | 14 | 3 |
| United Arab Emirates | 532 → 538 | +6 | 0 | 0 |

No match was lost or moved in any region.

## How OSM tags bike-share stations

From taginfo, 2026-11-17: `gbfs:station_id` 20,131, `gbfs:system_id`
19,826 (`hellocycling` 13,996, `docomo-cycle` 5,799, a few `bcycle_*`),
`ref:gbfs` 6,376 (`Bixi_MTL:3`, `bike_share_toronto:7202`,
`beryl_greater_manchester:4055`, `lyft_nyc:61be9a4a-…`). The values of
`ref:gbfs` have exactly ATP's form, so they compare directly.
System IDs are compared without regard to case, station IDs exactly.
Station IDs may contain colons (`lyft_nyc` uses UUIDs, Voi
`VOI:Station:…`), so only the first colon separates.

An ID of another system says nothing about the station, since systems
get renamed: ATP has `docomo-cycle-tokyo`, where OSM may still say
`docomo-cycle`. It still counts as a brand name, so a `docomo-cycle`
station doesn't match a `hellocycling` one by position.

## How the evals worked

ATP run of 2026-11-07, filtered by `addr:country`, and the eval-region
extracts of [2026-11-06-eval-regions.md](2026-11-06-eval-regions.md),
with `main` at 2542aa1 against this change. Before the rules were
settled, ATP stations were paired one to one with OSM
`amenity=bicycle_rental` nodes, closest first, with "chance pairs"
from moving each ATP station 2 km in a random direction, as in
[2026-11-05-infrastructure-matching.md](2026-11-05-infrastructure-matching.md).

In Japan, 12,183 of 13,145 pairs within 20 m are within 2 m (an
import), against 71 chance pairs within 20 m and 419 at 20–50 m.
16,359 of the 16,735 OSM stations with a GBFS ID have one that ATP
also has, and only 24 of those are more than 100 m apart.

`capacity` agrees on only 30% of the pairs within 2 m in Japan, as
docks get added and removed, so it doesn't confirm a match.

### Brand names

The first full runs matched by position and `brand:wikidata` alone.
In Switzerland, 130 of 822 new matches were Voi parking zones
(`voi_ch`) on PubliBike stations, and in Berlin, Dott zones and
Call a Bike stations took nextbike ones. Neither side had
`brand:wikidata`, but the names differ, so brand, network and
operator names are now compared too: lowercased, with nothing but
letters and digits, and one must contain the other, so that ATP's
`Nextbike` agrees with OSM's `Deezer nextbike`. For ATP, the system
ID in `ref:gbfs` counts as a name too, since the aggregator
`sharedmobility.ch` names itself as `brand` but has `velospot` or
`nextbike_ch` in the system ID. The Swiss gain went from 822 to 417,
with 6 Voi matches left.

### No datum shift

Nextbike stations in Switzerland were often 20–60 m from OSM's. Paired
by name, the offsets have no direction: median +2.9 m east and −0.1 m
north for `nextbike_ch` (43 pairs), −1.4 m and 0.0 m for Berlin's
`nextbike_bn` (30 pairs), with even spreads around zero. So this is
placement noise, not a datum or Google camera problem, and a shared
name word widens the radius to 50 m instead.

### Station numbers

In Massachusetts, the first runs flagged 49 Bluebikes stations in OSM
as deletion candidates. 28 of them have a `ref` that equals the
`short_name` of an ATP station 14–170 m away: the station was moved,
or OSM's position is rough. With the station number, Massachusetts
gains 267 matches by number and has 34 new deletion candidates
instead of 49.

## Results

- **Japan:** 13,664 of 15,681 HELLO CYCLING stations match (87%) and
  5,666 of 6,272 docomo ones (90%), all but 24 by GBFS ID. Of the
  337 new deletion candidates, 310 have a GBFS ID that the feed no
  longer has, so the station is most likely gone; 10 have no ID.
- **Switzerland:** PubliBike stations from the `sharedmobility.ch`
  aggregator (306) and nextbike (59 plus 17 in Graubünden). 30 OSM
  nextbike stations become deletion candidates; most have no name,
  and an ATP station of the same name lies 20–60 m away.
- **Massachusetts:** Bluebikes, 408 of 624 (65%).
- **Brazil:** Bike Itaú in São Paulo (166) and Rio (120), most at the
  same spot as ATP.
- Spot checks of 12 random new matches in Switzerland and Brazil
  were all right except one: `sharedmobility.ch` lists Mobility's car
  sharing as `amenity=bicycle_rental`, and one such feature took an
  unnamed bike-rental node.

ATP stations that find no match are now `missing_in_osm` rows, as for
the other kinds of street infrastructure: +16,893 in Switzerland,
+2,634 in Japan, +763 in Brazil. Most are free-floating parking zones
(Voi, Dott, Bird) or stations that OSM doesn't have. The counts for
Berlin and Massachusetts include stations of the whole country.

## Known limits

- 6,641 of ATP's 214,000 GBFS stations have no `amenity` and get no
  matcher, among them all of Tokyo's `docomo-cycle-tokyo` and Paris'
  Vélib' (filed as
  [alltheplaces#20604](https://github.com/alltheplaces/alltheplaces/issues/20604)).
- Car sharing (`amenity=car_sharing`) and kick-scooter rental from the
  same feeds have no match mask, so they are still dropped at import.
- Free-floating systems publish parking zones rather than docks. They
  rarely match, and inflate `missing_in_osm`.
