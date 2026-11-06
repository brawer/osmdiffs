# Matching post boxes and defibrillators

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

## Known limits

- The radius and ref rules per spider come from these three evals.
  Thousands of spiders can't be tuned by hand; a way to derive them
  from statistics of production runs is still open.
- Only post boxes and AEDs so far. Benches, street lamps, power poles
  and others will reuse the same matcher with their own settings.
  Trees are left out: comparing species needs code of its own.
- Edits for these kinds aren't suggested yet. Most unmatched ATP
  features (6,002 Swiss Post boxes, about 66,000 Japanese AEDs) would
  be additions at import scale, which need community review.
