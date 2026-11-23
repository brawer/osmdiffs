# Suggesting deletions of closed places: a first attempt and its evaluation

**Status:** we did not merge this. The edits it made were too
imprecise to publish. The follow-up note,
[Finding closed places with better precision](2026-11-24-deletion-signals.md),
explains why and what a next attempt should do. The code is in
[#1038](https://github.com/brawer/osmdiffs/pull/1038) (commit
`c46b223` on branch `edits-deletes`), kept for reference. This note
records what that code did and how it fared, so that a future attempt
can start from the numbers instead of repeating the work.

## Background

osmdiffs compares OpenStreetMap (OSM) with
[AllThePlaces](https://www.alltheplaces.xyz/) (ATP), a project that
runs a scraper, called a *spider*, for each chain's store locator once
a week. Each spider emits that chain's branches as point features. The
`conflate` step pairs ATP features with OSM features and writes one
row per pair, or per feature left unpaired, to
[`conflated.parquet`](../outputs/CONFLATED_PARQUET.md). The `edits`
step turns those rows into suggested changes to OSM in
[`edits.parquet`](../outputs/EDITS_PARQUET.md). A person is meant to
review those changes, for example in MapRoulette, before they reach
OSM.

Milestones 1 to 4 of the [edits.parquet plan](2026-11-23-edits-parquet.md)
suggested only additions and corrections: a phone number the spider
knows and OSM lacks, or opening hours that changed. Milestone 5, the
subject of this note, was the first to suggest *removing* things from
OSM, for places that seem to have closed.

### Where the candidates come from

Conflate already marks some OSM features as a `deletion_candidate`.
That is an OSM feature whose brand a spider covers, but which no
spider feature matched. Brands are recognised by their Wikidata ID
(`brand:wikidata`), by their name, or, for Swiss public transport
stops, by their ID in the national register. To guard against spiders
that only cover part of a country, conflate keeps a candidate only
when at least half of the brand's other OSM features nearby were
matched. That share is stored as `local_share`.

The idea behind the milestone: if a chain's own store locator no
longer lists a branch that OSM still has, the branch has probably
closed.

## What the code did

For each candidate, it first checked a list of guards. If any guard
applied, it made no edit:

| guard | why |
| --- | --- |
| the spider looked broken this week | it lost features since last week, or its match rate with OSM dropped (the same check the earlier milestones use) |
| ATP served the spider's output from an older run (a *fallback*) | an old run can miss branches, as Japan Post's did; see below |
| the element is a charging station | charging networks list their own and their partners' chargers unevenly |
| someone edited the OSM element after the spider ran | they probably saw the place |
| `check_date` or `survey:date` is within 180 days of the spider run | a mapper confirmed the place recently |
| the element is at version 1 and was added within 180 days of the run | the place may be newer than the spider's list |
| some ATP feature matched the element as the same place | it isn't missing at all |

A candidate that passed every guard got one of three kinds of edit,
all marked with the topic `obsolete_poi`:

- **Delete.** A node, way or relation with nothing but tags about the
  place (shop, name, brand, opening hours and so on) was deleted. Way
  nodes and relation members that had no tags and that nothing else
  used were deleted with it, and listed in a new `deleted_members`
  column so that an OSMChange file could be written from the row.
- **Strip** (a `modify` with reason `obsolete`). Some elements hold
  more than the place: a building outline, a land-use area, an
  entrance, or an address with a house number. For those, only the
  tags about the place were removed and the rest was kept. A
  street name and city alone, `level`, `ele`, `height` or `indoor=yes`
  didn't count as worth keeping.
- **Review** (`needs_review`, with no change to the tags). Some cases
  need a person's judgement, and each review row carried a warning
  saying why:

  | warning | case |
  | --- | --- |
  | `relation_member` | another relation, such as a bus route, uses the element; deleting it would break that relation |
  | `tagged_member_node` | a way has a node with its own tags, such as an entrance |
  | `address_only` | only the address would remain |
  | `has_subrelation` | a relation has other relations as members |
  | `unmatched_nearby:<spider>` | the same spider lists a feature of the same kind within 50 m that matched nothing; usually a move, a rebrand or a missed match |

Two more pieces were needed:

- **A pass over the planet file.** An OSM node doesn't record which
  ways use it, nor does a way record which relations use it. So a new
  pass read the ways, relations and nodes of the whole planet file,
  and recorded who uses each candidate and its members. It also
  recorded the version numbers that an OSMChange delete needs.
- **A change in conflate.** Conflate links small ATP features such as
  parcel lockers to the OSM building they are in (`located_in`).
  Before, such a building was treated as matched, so it could never
  be a candidate. The branch dropped that, because a supermarket can
  close while the locker in it stays. A delete of such a building
  carried the warning `host_still_listed:<spider>`.

## Results

Counts per region, with the ATP run of 2026-11-14 on the ten
[eval regions](2026-11-06-eval-regions.md). The columns are:

- **edits:** the `obsolete_poi` rows, split into the delete, strip and
  review columns;
- **fallback**, **recently checked**, **recently added** and
  **charging station:** candidates skipped by those guards;
- **deleted members:** the untagged nodes and ways deleted along with
  a way or relation.

| region | candidates | edits | delete | strip | review | fallback | recently checked | recently added | charging station | `unmatched_nearby` | `host_still_listed` | deleted members |
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

In all, about 7,900 deletes, 4,400 strips and 1,240 reviews.

- **Guards that never fired.** The broken-spider, "edited after the
  run" and "matched as the same place" guards skipped nothing here.
  No OSM element in the regional extracts was edited after the ATP
  run. They were meant as backstops for production, where
  the planet file can be newer than the ATP run.
- **Swiss reviews.** In Switzerland, at least 523 of the 754 reviews
  are `relation_member`, mostly bus and tram stops in route relations.
- **The conflate change** added 32 candidates in Berlin and 7 in
  Massachusetts; the 30 `host_still_listed` warnings came from these.

### Japan Post, and why fallbacks are skipped

The fallback column is mostly Japan. The `japan_post_jp` spider failed
on 2026-11-14: the post office's site answered with HTTP 503 errors,
and the spider gave up on 469 requests. ATP then served the spider's
output from 2026-10-17 instead. That older output was itself
incomplete: 1,449 of the 2,080 Japanese post offices in OSM became
candidates, and offices such as 京都向島郵便局 are still open. The
gaps came in patches, 20 to 30% of the offices in some 1° cells
against 2% in others, which is how a failed scrape looks, not how
closures look.

The fallback guard was added for this. It skipped 4,214 candidates,
3,770 of them in Japan. ATP already has an issue for the failure:
[alltheplaces#20613](https://github.com/alltheplaces/alltheplaces/issues/20613).

### Why `unmatched_nearby` was added

A first version had no `unmatched_nearby` warning. In it, about 7% of
the edits had a feature of the same spider within 50 m that matched
nothing in OSM. Some were different places, such as a post box next
to a post office. Of the same kind, though, they were mostly the same
place under a changed name or ID:

- a post box with `ref` 30005 in OSM and 030005 in ATP;
- Swiss stops whose register entry has a new ID;
- rebrands: Price Chopper becoming Market 32, Agip becoming Aral, and
  a Petco now named "Petco Unleashed".

These became reviews with the warning (592 rows).

## Hand check

We drew three random samples, five rows per region each: 50 deletes,
50 strips and 15 reviews. For each row we read the OSM tags, the
spiders, the matching rule, the tags that would stay and the members
that would go. Where a place had a name and an address, we also
looked it up on the web. In the table, "delete 6" means row 6 of the
delete sample.

### Places looked up on the web

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

Of the 11 places whose status the web settled, **3 were gone and 8
still open**. Every hotel (4 of 4) and every supermarket (3 of 3)
checked was open. Outside the samples, the Swissôtel Al Murooj in
Dubai and the M.C.F. Motor Company, still a Toyota dealer, were wrong
too. The follow-up note traces most of the wrong ones: some are OSM
duplicates of a place the spider does list, and some come from spider
runs that lost part of their results.

### The rest of the samples

- **Deletes.** None of the 50 had a house number, so none could be
  looked up by address. Most were nodes of chains: 7-Eleven in
  Thailand, Indian banks, Kenyan fuel stations, Uniqlo, au. Such an
  edit is right if the place is gone, which only someone on the
  ground can tell.
  - A Bluebikes dock, Deutsche Post and Swiss Post boxes and a Swiss
    bus platform came from the operators' own lists. That looked more
    trustworthy, but the follow-up note found failure modes there too.
  - Two Brazilian fuel stations had their own names ("Auto Posto
    Parque Cidade Nova", "Posto Metano 2"). Such stations often change
    their fuel brand rather than close, so these are doubtful.
  - Where a way went, the untagged nodes that went with it (4 and 8)
    were right to go.
- **Strips.** The tags kept were right in 49 of 50: the building, the
  address with a house number, `landuse`, and `level` while something
  else stayed. A Packstation node kept only `indoor=yes` and `level=0`,
  which mean nothing alone. The branch was then changed so that
  `indoor=yes` alone keeps nothing; a room, `indoor=room`, still does.
  Two more Brazilian fuel stations were doubtful for the same reason as
  above.
- **Reviews.** All 15 carried the warning they should
  (`unmatched_nearby` 7, `relation_member` 4, `address_only` 3,
  `tagged_member_node` 1). Each needed a person, for example the Berlin
  Agip that is now an Aral, or the Swiss stops in route relations.

## Why precision was low

A deletion candidate is a branch that the spider doesn't list. Perhaps
2 to 5% of shops close in a year. If a spider misses even 5% of a
chain's branches, the branches it missed outnumber the ones that
closed, and most candidates are mistakes. The `local_share` filter
only catches spiders that miss most branches. Missing from one week's
spider output is therefore weak evidence of closure, and several
stronger signals are needed together. The
[follow-up note](2026-11-24-deletion-signals.md) tests some of them.

## Run time

The planet pass read the ways (twice, when there were members to look
at), the relations and the nodes. On Japan, the `edits` step took 55
to 74 s, against about 25 s in milestone 4. The pass therefore cost
about 50 s for a 2 GB file on 4 cores. The planet file is about 40
times larger, which makes about 35 minutes on 4 cores. For comparison,
`import_osm` takes 78 minutes. The pass kept only what it needed, so
memory stayed small. The follow-up note describes how the prune step
of `import_osm` could record the same facts and make the pass
unnecessary.

## Left undone

- When a deleted way's node was also in some relation, the node simply
  stayed; no edit to that relation was made.
- A node shared by two candidate ways that were both deleted stayed.
- The plan's rule for a POI node inside a building with the same
  address.
- Spider completeness, OSM duplicates and history across ATP runs.
  These are in the follow-up note.
