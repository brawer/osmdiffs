# Matching post boxes, defibrillators and benches

Investigation note behind `InfrastructureMatcher`
([`src/matchers/infrastructure_matcher.rs`](../../src/matchers/infrastructure_matcher.rs)).
Before this change, no matcher handled street infrastructure, so the
13,623 Swiss Post boxes in AllThePlaces (ATP) matched nothing in
OpenStreetMap (OSM). The deletion-candidate rules had to leave them
out for that reason
([`2026-11-05-deletion-candidates.md`](2026-11-05-deletion-candidates.md)).

## Short answer

Post boxes and AEDs have no brand and rarely a name, and many stand
close together. So the match rests on position, one to one within a
spider, like `TransitMatcher`. How far apart the right pairs are
depends more on the source than on the kind: Japan Post's positions
are within a few meters, Deutsche Post's often tens of meters off. So
each kind has a default search radius, and a spider can override it,
together with how its refs compare to OSM's. Matching tags such as
`collection_times` can raise a score, but differing ones never lower
it, since they are what an edit should fix.

## How the evals worked

For Switzerland, Germany and Japan, with the 2026-10-10 ATP run and
Geofabrik extracts of early November 2026 (OSM nodes only), we paired
ATP and OSM features one to one, closest first, up to 200 m. To see
how many pairs come from chance, we repeated the pairing with every
ATP feature moved 2 km in a random direction ("chance pairs"). That
overstates false pairs, since in the real pairing a true partner
usually takes a feature first. Where both sides had a comparable tag,
we also counted how often it agreed, per distance band.

| distance | CH post boxes, `collection_times` equal | DE post boxes | DE chance pairs | DE: OSM `ref` names the ATP street | JP post boxes | JP chance pairs | JP: same `ref` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0–2 m | 53% | 1,721 | 2 | 97% | 1,964 | 1 | 59% |
| 2–5 m | 52% | 6,270 | 1 | 97% | 4,226 | 6 | 52% |
| 5–10 m | 49% | 13,013 | 9 | 97% | 2,684 | 22 | 56% |
| 10–20 m | 52% | 20,461 | 67 | 97% | 1,281 | 74 | 44% |
| 20–50 m | 46% | 20,941 | 471 | 96% | 624 | 500 | 33% |
| 50–100 m | 48% | 7,989 | 1,548 | 95% | 398 | 1,409 | 16% |
| 100–150 m | 30% | 2,858 | 2,161 | 94% | 384 | 1,691 | 0% |

Random pairs of Swiss post boxes have equal `collection_times` only
5% of the time, so the flat 50% out to 100 m means those far pairs
are mostly right. The other half disagree because the times changed
and OSM still has the old ones.

## Post boxes

- **Switzerland** (`swiss_post_ch`, 13,623 boxes): radius 100 m.
  Refs don't help: ATP has `003BE_00335986`, OSM `3-8716-25024`.
- **Germany** (`deutsche_post_de`, 106,786 boxes against 83,564 in
  OSM): radius 100 m. Most pairs are 5–50 m apart. German OSM puts the
  box location in `ref` (`Lütticher Str. 76`), so an OSM `ref` that
  contains the ATP street confirms a match. ATP's own ref
  (`6-209474`) never appears in OSM.
- **Japan** (`japan_post_jp`, 95,455 boxes against 19,745 in OSM):
  radius 20 m, since chance pairs catch up at 20–50 m. Japan Post's
  6-digit `ref` is a real ID, equal on 55% of close pairs where OSM
  has a ref. It still only counts within the radius: 456 of the 1,201
  OSM boxes that share a ref with a Japan Post box are more than
  200 m away. `collection_times` rarely agree, because OSM groups the
  days differently (`Sa,Su,PH` against `Sa …; Su,PH …`), which would
  need an `opening_hours` parser to compare.
- Other post box spiders get 50 m.

Different `operator:wikidata` on both sides rule out a match: a post
box of another operator is another box.

## Defibrillators

- **Germany** (`steiger_stiftung_aed`, 4,075 AEDs against 21,377 in
  OSM): about 1,750 were imported into OSM at the same spot, with
  `operator=Björn Steiger Stiftung` and the ATP ref without its prefix
  (ATP `AEDR 1786`, OSM `ref=1786`). 2,050 of the 2,223 Steiger AEDs
  in OSM share the number with an ATP one; 68 of them are more than
  200 m apart, because the same number recurs under different
  prefixes (`AEDS`, `AEDR`, `AEDX`). Radius 50 m, where chance pairs
  are still rare.
- **Japan** (`aed_jp`, 67,907 AEDs against 3,729 in OSM): radius 20 m.
  ATP's AEDs are dense: 2,792 share their position with another, and
  30% have another within 100 m.
- Other AED spiders get 30 m.

Operators don't rule out a match for AEDs: Steiger is the donor, and
OSM sometimes names the town or shop that runs the device.

Two issues are for the spiders rather than the matcher: Steiger's
`name` holds what OSM tags as `defibrillator:location` ("Oberrhein
Gymnasium - Foyer EG"), and about 98 of its AEDs are carried on
vehicles ("Feuerwehr - auf dem Einsatzwagen"), with no fixed place.

## Benches

ATP has about 15,000 benches from city registries: Zürich
(`stadt_zuerich_ch`), Winterthur (`winterthur_ch`), Seattle, Sheffield
and Glen Eira (Melbourne). Each spider was paired with OSM benches
within 300 m of its own, as above; Winterthur from the 2026-09-26 run,
since the spider has delivered nothing since 2026-10-03 (ATP's crawler
times out connecting to `stadtplan.winterthur.ch`).

| spider | ATP | OSM | pairs 0–5 m | 5–10 m | 10–20 m | chance pairs 0–10 m / 10–20 m |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Zürich | 7,267 | 8,398 | 3,727 | 240 | 149 | 126 / 237 |
| Winterthur | 2,203 | 1,715 | 910 | 107 | 95 | 15 / 25 |
| Seattle | 3,131 | 2,260 | 1,020 | 159 | 78 | 8 / 14 |
| Sheffield | 1,493 | 1,157 | 332 | 83 | 34 | 1 / 6 |
| Glen Eira | 1,062 | 246 | 129 | 18 | 7 | 0 / 0 |

The registries are precise: most right pairs are within 5 m. Benches
are also dense: half of the benches in Zürich and Winterthur are less
than 5 m from the next one, so one-to-one matching matters more than
for post boxes. Benches get 15 m by default, and Zürich 10 m, where
its chance pairs already outnumber the right ones at 10–20 m.

- `backrest` agrees on 98% of close pairs in Zürich, so it confirms a
  match. `material` doesn't: ATP has `metal;wood` where OSM has
  `wood`, and it agrees on only 19% of close pairs in Seattle.
- Seattle's asset IDs are in OSM, under `ref:US-WA:DPR:AMWO_ID`
  rather than `ref`; all 55 of them agree. So a spider can name the
  OSM key that holds its refs.
- Many close pairs (752 of Winterthur's within 2 m) come from earlier
  imports of the same data.

## Results on Switzerland

Swiss OSM extract and ATP 2026-10-10 filtered to `addr:country=CH`,
as for the deletion-candidate note:

| measure | before | after |
| --- | ---: | ---: |
| Swiss Post boxes matched | 0 | 7,621 (56%) |
| Swiss Post boxes without OSM match | 13,623 | 6,002 |
| matched post box distance, median / 90th percentile | | 8 m / 35 m |
| deletion candidates `wikidata` | 1,059 | 1,303 |
| deletion candidates `name` | 380 | 416 |

The 280 new deletion candidates are unmatched OSM post boxes of Swiss
Post (264 with `operator=Die Post`), which the rules now consider
because ATP's post boxes have a matcher. Half of them have no ATP box
within 215 m; 54 none within 500 m. 56 have an ATP box within 100 m
that matched another OSM box: boxes that OSM maps twice, or two
boxes that Swiss Post lists as one, which we haven't checked by hand. All other rows stayed the same.

At first, four more were flagged: three cameras and a parking ticket
machine with `operator=Die Post`. The match mask lumps all street
furniture together, so ATP's post boxes made Swiss Post look covered
for any of it. For street furniture, the deletion rules now compare
the finer kind (`post_box`, `defibrillator`) instead, and street
furniture without a matcher is never flagged.

The 5 Swiss AEDs in this ATP extract match nothing.

With benches added, against the post box matcher as a baseline, 4,026
of the 7,267 Zürich benches match (55%), at a median distance of
0.8 m and a 90th percentile of 3.7 m. Deletion candidates stay the
same (1,303 `wikidata`, 416 `name`), and so does everything else. The
Swiss ATP extract of 2026-10-10 has no Winterthur benches, because the
spider failed.

## Results on the eval regions

Main on the [eval regions](2026-11-06-eval-regions.md), with the
setup described there, for all kinds that `InfrastructureMatcher`
handles. Regions and spiders with fewer than 10 ATP features are left
out.

| region | spider | kind | ATP | matched | median / 90th percentile distance |
|---|---|---|---:|---:|---:|
| Switzerland | `swiss_post_ch` | post box | 13,621 | 7,621 (56%) | 8.4 m / 35.3 m |
| Switzerland | `stadt_zuerich_ch` | bench | 7,267 | 4,026 (55%) | 0.8 m / 3.8 m |
| Switzerland | `stadt_zuerich_ch` | bicycle parking | 2,865 | 1,571 (55%) | 1.8 m / 6.5 m |
| Switzerland | `stadt_zuerich_ch` | fountain | 1,287 | 1,018 (79%) | 0.7 m / 3.0 m |
| Switzerland | `stadt_zuerich_ch` | toilets | 91 | 86 (95%) | 2.9 m / 11.3 m |
| Switzerland | `little_free_library` | bookcase | 16 | 4 | |
| Japan | `japan_post_jp` | post box | 95,455 | 10,088 (11%) | 4.0 m / 11.2 m |
| Japan | `aed_jp` | AED | 67,907 | 336 (0.5%) | 7.5 m / 16.2 m |
| Japan | parking operators | bicycle parking | 702 | 146 (21%) | 6.7 m / 13.7 m |
| Japan | `little_free_library` | bookcase | 48 | 0 | |
| US: Massachusetts | `usps_collection_boxes` | post box | 7,094 | 767 (11%) | 4.6 m / 26.2 m (with FedEx) |
| US: Massachusetts | `fedex` | post box | 764 | 35 (5%) | |
| US: Massachusetts | `little_free_library` | bookcase | 2,670 | 81 (3%) | 15.4 m / 32.0 m |
| Germany: Berlin | `deutsche_post_de` | post box | 2,377 | 2,177 (92%) | 15.1 m / 42.7 m |
| Germany: Berlin | `steiger_stiftung_aed` and others | AED | 19 | 3 | |
| Brazil: Sudeste | `little_free_library` | bookcase | 39 | 0 | |

- Berlin's post boxes match far better than Switzerland's or
  Japan's, at larger distances, as the German eval above showed for
  Deutsche Post.
- Japan has about five times as many boxes in ATP as in OSM, so most
  stay unmatched, and those that match are close. 2,593 Japan
  Post boxes in OSM have no ATP box within 20 m and become deletion
  candidates
  ([`2026-11-05-deletion-candidates.md`](2026-11-05-deletion-candidates.md)).
- Thailand, Kenya, the UAE and India have no street infrastructure in
  ATP beyond a handful of Little Free Libraries.
- Cambridge's hydrants, bicycle parking and other city data, which
  the later notes measured, have no license in ATP's metadata. The
  pipeline skips such spiders, so the Massachusetts run has none of
  them, and their per-spider settings have no effect in production
  until ATP records a license.

## Known limits

- The radius and ref rules per spider come from these three evals.
  Thousands of spiders can't be tuned by hand; a way to derive them
  from statistics of production runs is still open.
- Only post boxes, AEDs and benches here. Fire hydrants, public
  bookcases and bicycle parking followed
  ([`2026-11-06-hydrants-bookcases-bicycle-parking.md`](2026-11-06-hydrants-bookcases-bicycle-parking.md)).
  Street lamps, power poles and others will reuse the same matcher
  with their own settings.
  Trees are left out: comparing species needs code of its own.
- Edits for these kinds aren't suggested yet. Most unmatched ATP
  features (6,002 Swiss Post boxes, about 66,000 Japanese AEDs, 3,241
  Zürich benches) would
  be additions at import scale, which need community review.
