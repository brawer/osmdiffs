# Deletes on the eval regions (not shipped)

**Status:** we did not merge this. The deletes and POI-tag removals
measured here were too imprecise to publish; see
[Finding closed places with better precision](2026-11-24-deletion-signals.md)
for why, and for what a next attempt should do. The code is in
[#1038](https://github.com/brawer/osmdiffs/pull/1038) (commit
`c46b223`, branch `edits-deletes`), kept for reference. This note
records what it did and how the eval went.

This was milestone 5 of the [edits.parquet plan](2026-11-23-edits-parquet.md):
deletes of places that AllThePlaces no longer lists, removal of POI
tags from buildings, the recent-edit guard, the `host_still_listed`
warning and the reference-count pass over the planet file. It was
measured on the ten [eval regions](2026-11-06-eval-regions.md) with
the AllThePlaces run of 2026-11-14, as in
[milestone 4](2026-11-23-opening-hours-eval.md). The rules were
documented in `docs/outputs/EDITS_PARQUET.md` on that branch.

## What the code did

A `deletion_candidate` from conflate became an `obsolete_poi` edit:

- A bare node, way or relation was deleted, together with its untagged
  members that nothing else used, listed in a new `deleted_members`
  column.
- A building, an address with a house number, `landuse` and the like
  kept their tags, and only the POI tags were removed.
- An element in a relation, a way with a tagged node, or a way or
  relation that would keep only its address became a `needs_review`
  row. So did a place with an unmatched feature of the same spider and
  kind within 50 m.

There was no edit when:

- the spider was unhealthy or served from an older run;
- the element was a charging station;
- the element was edited after the fetch;
- the element was checked, or added at version 1, within 180 days of
  the fetch;
- the element matched as `same`.

A new pass over the planet file found which ways and relations used
each element.

## Summary

The pipeline worked, but a hand check of named places showed that **a
place missing from a store locator is often still open**. Of the 11
sampled places whose status the web settled, 3 were gone and 8 still
open: every hotel (4 of 4) and every supermarket (3 of 3) checked was
open. The follow-up note traces most of the wrong ones to duplicates
in OSM and to failed spider runs.

The underlying reason is a base rate. A deletion candidate is an OSM
feature of a brand that no spider feature matched. If a spider misses
5% of a chain's branches and 2% of the branches in OSM have closed,
most candidates are branches the spider missed. `local_share` (at
least half of the brand's OSM features nearby matched) catches spiders
that miss most branches, not ones that miss a few.

## Counts

Counts per region. "Edits" is the number of `obsolete_poi` rows:
deletes, POI-tag removals (`modify` with reason `obsolete`) and
`needs_review` rows. Skips are candidates that got no edit.

| region | candidates | edits | delete | strip | review | fallback skip | recently checked | recently added | charging station | `unmatched_nearby` | `host_still_listed` | deleted members |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Kenya | 90 | 88 | 75 | 13 | 0 | 1 | 0 | 1 | 0 | 0 | 0 | 13 |
| UAE | 92 | 61 | 51 | 10 | 0 | 27 | 1 | 3 | 0 | 0 | 0 | 39 |
| Thailand | 1,587 | 1,414 | 987 | 423 | 4 | 111 | 2 | 60 | 0 | 2 | 0 | 254 |
| Berlin | 400 | 321 | 159 | 149 | 13 | 11 | 40 | 2 | 26 | 10 | 26 | 6 |
| Switzerland | 2,502 | 2,350 | 1,138 | 458 | 754 | 98 | 16 | 15 | 23 | 231 | 0 | 101 |
| Karnataka | 261 | 237 | 207 | 27 | 3 | 18 | 4 | 2 | 0 | 2 | 0 | 31 |
| Tamil Nadu | 110 | 92 | 82 | 9 | 1 | 9 | 0 | 9 | 0 | 0 | 0 | 30 |
| Brazil Sudeste | 1,453 | 1,320 | 472 | 800 | 48 | 108 | 3 | 22 | 0 | 6 | 0 | 390 |
| Massachusetts | 890 | 807 | 297 | 452 | 58 | 61 | 8 | 14 | 0 | 21 | 4 | 17 |
| Japan | 10,789 | 6,909 | 4,465 | 2,084 | 360 | 3,770 | 43 | 67 | 0 | 320 | 0 | 833 |

"Deleted members" counts the nodes and ways deleted along with a way
or relation. The other guards (no rule, no spider, broken spider,
`local_share` below 0.5, matched as `same`, edited after the fetch)
skipped nothing here: conflate already drops candidates with a low
`local_share`, and no OSM element in these extracts was edited after
the fetch. They were meant as backstops for production, where the
planet file can be newer than the AllThePlaces run.

In Switzerland, at least 523 of the 754 reviews are `relation_member`,
mostly transit stops in route relations; deleting them would break the
routes, as Sascha pointed out.

## What changed in conflate (on the branch only)

A building with a parcel locker inside used to be marked as matched
when a `located_in` row pointed at it, so it could never be a deletion
candidate. The branch let it be one: the supermarket may have closed while the
locker stayed. This added 32 Berlin and 7 Massachusetts candidates, and
the 30 `host_still_listed` warnings came from these.

## Japan Post and fallback spiders

`japan_post_jp` failed on 2026-11-14 and AllThePlaces served its
2026-10-17 output instead (`fallback_reason` `dropped` in
`spider_stats`). 1,449 of 2,080 Japanese post offices in OSM became
candidates, with gaps that come in patches (20 to 30% of offices in
some 1° cells, 2% in others), and offices such as 京都向島郵便局 are
still open. The same run logged 469 `Gave up retrying` errors after
HTTP 503. So no spider that fell back to an older run makes deletes;
this skips 3,770 candidates in Japan, 4,214 in all. The upstream issue
is [alltheplaces#20613](https://github.com/alltheplaces/alltheplaces/issues/20613).

## Places near an unmatched feature

About 7% of the first-cut edits had an unmatched AllThePlaces feature
of the same spider within 50 m. Some are different places (a post box
next to a post office), but of the same kind they are mostly missed
matches or rebrands: a post box with `ref` 30005 in OSM and 030005 in
AllThePlaces, Swiss stops whose register entry has a new sloid, Price
Chopper becoming Market 32, Agip becoming Aral, a Petco now named
"Petco Unleashed". Those became `needs_review` with the warning
`unmatched_nearby:<spider>` (592 rows).

## Hand check

Samples of 50 deletes, 50 POI-tag removals and 15 reviews, five per
region (seeds 31, 32, 33). For each, the OSM tags, the spiders, the
rule, the tags kept and the members were read; places with a name and
an address were also looked up on the web.

### Web-checked

| sample | place | edit | on the web | edit is |
| --- | --- | --- | --- | --- |
| delete 6 | Andaz Dubai The Palm Apartments | delete | open | wrong |
| delete 23 | Storchengarage AG (Renault) | delete | nothing found | unclear |
| delete 24 | Shell La Gruyère | delete | likely the Shell at Pont-en-Ogoz | unclear |
| strip 6 | Residence Inn by Marriott Al Jaddaf | strip | open | wrong |
| strip 10 | Sheraton Khalidiyah, Abu Dhabi | strip | open | wrong |
| strip 16 | REWE, Rosenthaler Weg 74, Berlin | strip | on REWE's own store page | wrong |
| strip 20 | REWE, Allee-Center, Berlin | strip | listings current | likely wrong |
| strip 21 | C&A, Zentralstrasse 12, Dietikon | strip | not in C&A's store list | right |
| strip 23 | Roche Bobois, Sihlporte 3, Zürich | strip | on Roche Bobois' showroom page | wrong |
| strip 24 | Lidl, Im Majorenacker 16, Schaffhausen | strip | Lidl chargers and jobs there | likely wrong |
| strip 32 | Holiday Inn Express Chennai OMR | strip | open | wrong |
| strip 41 | Sonic, 220 Main Street, Wilmington | strip | reported closed | right |
| strip 43 | Walgreens, 220 Grafton St, Worcester | strip | Walgreens' page says it closes | right |
| strip 44 | D'Angelo, 270 Hancock St, Quincy | strip | listed, undated | unclear |

Outside the sample, Swissôtel Al Murooj in Dubai (which `accor`
misses) and the M.C.F. Motor Company, still a Toyota dealer, were
also wrong.

### The rest of the sample

- **Deletes (50).** All 50 lack a house number, so none of them could
  be looked up by address. Most are nodes of chains (7-Eleven in
  Thailand, Indian banks, Kenyan fuel stations, Uniqlo, au): the edit
  is right if the place is gone, which only someone on the ground can
  tell. The Bluebikes dock comes from GBFS, the operator's live
  feed, and the Deutsche Post and Swiss Post boxes from the operator's
  own lists: likely right. The Swiss bus platform is gone from the
  register: likely right. Two Brazilian ways are fuel stations with
  their own names ("Auto Posto Parque Cidade Nova", "Posto Metano 2"),
  which in Brazil often change their flag rather than close: doubtful.
  The members (4 and 8 untagged nodes) were right to go with the way.
- **POI-tag removals (50).** The tags kept were right in 49: the
  building, the address with a house number, `landuse`, and `level`
  while something else stays. A shop on `level=-1` in a mall keeps
  its address. A Packstation node kept only `indoor=yes` and
  `level=0`, which say nothing on their own; the branch was then
  changed so that `indoor=yes` alone keeps nothing (a room,
  `indoor=room`, still does). Two more
  Brazilian fuel stations with own names are doubtful for the same
  reason as above.
- **Reviews (15).** All carry the warning they should
  (`unmatched_nearby` 7, `relation_member` 4, `address_only` 3,
  `tagged_member_node` 1), and in each case a person is needed, for
  example for the Berlin Agip that is now an Aral, or the Swiss stop
  positions in route relations.

### Precision

Too few places could be settled to give a number per category.
Hotels and supermarkets did badly. Operator feeds (bike share, post
boxes, transit registers) looked better at first sight, but weren't
checked one by one. The follow-up note found moved bike docks and
stops outside the register's scope among them.

## Run time

The reference pass reads the ways (twice, when there are members to
look at), the relations and the nodes of the planet file.
`make_edits` on Japan took 55 to 74 s against about 25 s in milestone 4,
so the pass costs about 50 s for a 2 GB file on 4 cores. The planet
file is about 40 times larger, which makes about 35 minutes on 4
cores, less with more; `import_osm` takes 78 minutes. The pass keeps
only the wanted elements, so memory stays small.

## Not done

- A modify of a relation when one of its members is deleted: members
  of other relations are never deleted, so this only matters for
  nodes of deleted ways, which are deleted only when nothing else uses
  them.
- A node shared by two candidate ways that are both deleted stays.
- The plan's "enclosing building with the same address" rule.
- Per-spider completeness, duplicates in OSM and history across runs:
  see the follow-up note.
