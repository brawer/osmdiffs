# One OSM feature per branch: exclusive POI matching

Until now, each AllThePlaces (ATP) feature with a `PoiMatcher` took its
best OpenStreetMap (OSM) candidate, whatever other ATP features did. When
OSM lacks one branch of a chain, the missing branch takes the nearest
OSM feature of the same brand, which is usually the shop of another
branch, a few hundred meters away. So the shop gets two matches. The
edits suggested for the far match are wrong, and the missing branch
never shows up as missing.

The 2026-11-02 planet run had a lot of these. 70,886 OSM features were
matched by more than one ATP feature of the same spider. For 35,658 ATP
features, the OSM feature they took was more than 50 m closer to another
ATP feature of their spider (median 246 m away).

Phone numbers show which match is right. These 35,658 ATP features
agreed with their OSM feature's phone in 16% of cases. Other
`brand:wikidata` matches agreed in 83–87% of cases beyond 150 m, and in
89% below 50 m. So almost all of the drop in quality at larger distances
comes from features sharing an OSM feature, not from the 400 m search
radius.

## The change

`PoiMatcher` is now exclusive (`Matcher::is_exclusive`), like
`TransitMatcher` and `InfrastructureMatcher`. Within one spider,
`pipeline::conflate::assign_exclusive` gives each OSM feature to the
best-scoring ATP feature. The others fall back to their next candidate
(up to four are kept) or stay unmatched. As before, conflicts only count
within a spider, so car dealers of several brands in one OSM building
can all match it.

Two kinds of features may still share an OSM feature that another
feature of their spider took:

- **Features close together.** This is a new `Matcher::colocated_radius`,
  which is 25 m for POIs. Spiders often list several things at one
  point, such as a car dealer's sales and service, or a supermarket and
  its pharmacy. OSM often maps these as one feature. On the planet,
  40,401 shared OSM features had all their ATP features at the same
  point. The distance is measured to the ATP feature that took the OSM
  feature first.
- **Features with a shared contact** (`Matcher::may_share`): a
  descriptive phone number, email address or website that the OSM
  feature carries too. Some spiders list one store several times, a few
  dozen meters apart, such as Staples with a feature for each print
  service. A shared contact says which place it is, whichever feature of
  the spider is closer.

An ATP feature without any candidate now gets its row right away, even
if its matcher is exclusive. That keeps the many unmatched ATP features
out of the assignment's memory.

## Evaluation

The evaluation uses the ten [eval regions](2026-11-06-eval-regions.md),
with the ATP run that production used on 2026-11-02 (2026-10-31-13-32-34,
filtered by `addr:country`) and the extracts from the baseline. It
compares main (e64a8b9) with this branch. "Lost" means matched on main
but not on the branch. "Moved" means matched to another OSM feature.
"Phone agrees" compares the last seven digits of the ATP `phone` with
the OSM `phone` or `contact:phone`, leaving out numbers that three or
more ATP features share. Only rows where both sides have a phone count.

| region | ATP rows | matched main | matched branch | lost | moved | lost: median distance | lost: phone agrees | kept, >150 m: phone agrees | OSM-only main → branch |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Switzerland | 115,500 | 64,109 | 63,948 | 161 | 35 | 215 m | 0/3 | 44/52 | 6,641 → 6,610 |
| Japan | 246,792 | 64,544 | 61,258 | 3,286 | 180 | 229 m | 0/527 | 138/198 | 22,778 → 22,615 |
| Massachusetts | 22,086 | 7,294 | 7,138 | 156 | 8 | 101 m | 0/19 | 78/92 | 1,981 → 1,974 |
| Berlin | 8,842 | 6,111 | 5,949 | 162 | 9 | 196 m | 0/5 | 23/25 | 681 → 673 |
| Brazil Sudeste | 18,974 | 5,369 | 4,893 | 476 | 34 | 208 m | 0/1 | 2/6 | 3,786 → 3,764 |
| Thailand | 19,577 | 4,318 | 4,043 | 275 | 8 | 256 m | – | 4/5 | 935 → 927 |
| Kenya | 1,414 | 481 | 455 | 26 | 2 | 208 m | – | – | 142 → 140 |
| UAE | 2,878 | 479 | 440 | 39 | 6 | 227 m | 0/1 | 4/4 | 160 → 154 |
| Karnataka | 6,184 | 1,240 | 1,203 | 37 | 4 | 256 m | 0/1 | 3/4 | 469 → 466 |
| Tamil Nadu | 4,776 | 583 | 551 | 32 | 2 | 256 m | 0/1 | – | 527 → 525 |

ATP rows and matches are those inside the extract's polygon; OSM-only
rows are deletion candidates, for the whole extract. No region gained
a match, since exclusivity only takes matches away or moves them.

Over all regions, none of the 558 lost matches with a phone on both
sides had the same phone as its OSM feature. Among the matches that
were kept and are more than 150 m away, 296 of 386 did (77%).

For every lost match (up to 400 per region, in six regions), the
ATP feature that kept the OSM feature was closer to it, except for one
in Massachusetts. The median distance of the winners was 7–24 m, and
of the losers 101–256 m.

Moved matches look better after the move: in Japan, 5 of 37 moved
features with a phone on both sides agree with their new OSM feature,
and none of 32 agreed with the old one.

The OSM-only rows went down by a few, since a feature that moves to its
second candidate takes an OSM feature that had been a deletion
candidate.

### What was lost

By hand, the losses are mostly what we wanted: different branches of
dense chains that took a neighbor's shop.

- **Japan:** FamilyMart (946), Lawson (681), Starbucks (196), Doutor
  (166), 7-Eleven (136). Every ATP feature has its own phone, and none
  of the 527 with a phone on both sides agreed.
- **Brazil:** mostly McDonald's (249), whose spider lists its dessert
  kiosks (`CK …` in the name) as features of their own, as well as
  Shell (78) and Petrobras (55) stations.
- **Thailand:** Café Amazon (165), which is often found at PTT
  stations, and KFC (29).
- **Berlin:** street-lamp chargers by ubitricity (94), which all took
  one OSM charging station up to 400 m away. The rest are a few
  fuel-card stations (maes_dkv) and Deutsche Post partner shops.
- **Switzerland:** k kiosk (80), often a second kiosk in the same mall
  or station, and Shell shops (18).
- **Massachusetts:** most of the 156 are Staples (74). Its spider lists
  each print and travel service of a store as a feature of its own,
  40–200 m from the store. These no longer share the store's OSM
  feature when the OSM feature has no phone, which is the case for all
  74. They now become ATP-only rows, as if OSM lacked them. That is
  better fixed in ATP, or by merging one spider's features with the same
  address and phone before matching. The other losses are Dunkin' (20)
  and Starbucks (17) branches.

## Cost

Exclusive matching keeps up to four candidates for each ATP feature with
any candidate, and sorts them once at the end of matching. In
Massachusetts, where the pipeline matches all 1.2 million American ATP
features, only 7,400 had candidates. Anonymous memory at the end of
`conflate` was 1,132 MiB on main and 1,118 MiB on the branch. In Japan,
with 65k features with candidates, it was 1,475 and 1,514 MiB. On the
planet, about 1.5 million ATP features have candidates. The assignment
should add a few hundred MiB of anonymous memory at most, well within
the 8 GB the [production setup](../PRODUCTION.md) recommends. Run times
didn't change measurably.

## Planet estimate

On the 2026-11-02 planet output, the same rule would remove about 36k
matches. Those are the ATP features more than 50 m further from their
OSM feature than another ATP feature of their spider, and only 16% of
them agree on the phone. Most will become ATP-only rows, meaning the
branch is missing in OSM.
