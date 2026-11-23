# Finding closed places with better precision

Ideas for telling which OpenStreetMap features describe places that no
longer exist, written after a first attempt at milestone 5 of the
[edits.parquet plan](2026-11-23-edits-parquet.md) was too imprecise to
merge ([#1038](https://github.com/brawer/osmdiffs/pull/1038); its eval is in
[Suggesting deletions of closed places: a first attempt](2026-11-23-deletion-suggestions-eval.md)). The
quick checks below used the AllThePlaces run of 2026-11-14, the ten
[eval regions](2026-11-06-eval-regions.md), and the 14 places from the
hand check whose status could be looked up on the web. Fourteen places
show which signals look promising; they don't measure precision.

## What we tried, and why it fell short

Conflate marks an OSM feature as a `deletion_candidate` when its brand
(or, for Swiss transit stops, its register ID) is covered by an
AllThePlaces spider, at least half of the brand's OSM features nearby
were matched, and no spider feature matched this one. The edit step
then deleted bare elements, removed POI tags from buildings, and sent
relation members and a few other cases to review. Guards skipped
broken or fallback spiders, charging stations, and features that had
been edited, checked or added shortly before the fetch.

On the ten regions this made about 7,900 deletes and 4,400 tag
removals. In the hand check, only 3 of the 11 places that the web
settled were really gone, and every hotel and supermarket checked was
open.

The reason is a base rate. Perhaps 2 to 5% of the shops in OSM close
in a year. A spider that misses 5% of a chain's branches produces more
candidates from its misses than from closures. Across the 14 places,
the share of the brand's OSM features in the region that no spider
feature matched was 9 to 50% (1% for Lidl in Switzerland), far above
any closure rate. `local_share` only catches spiders that miss most
branches. "Not in this week's spider output" is therefore a weak
signal, and several stronger ones are needed together.

## What explains the 14 places

| place | status on the web | AllThePlaces history | other evidence |
| --- | --- | --- | --- |
| Andaz Dubai The Palm Apartments | open | listed at 19 m in the current run, matched to the hotel's other OSM element | Overture: present, updated 2026-11 |
| Shell La Gruyère | likely open | listed at 29 m in every run, matched to the neighbouring node 10080554886 | station page live; Overture: open, 2026-11 |
| Roche Bobois, Zürich | open | listed at 12 m, matched to a second OSM node | Overture: open, 2026-11 |
| Lidl, Schaffhausen | open | listed at 57 m, matched to the building | branch page live; Overture: open, 2026-11 |
| Holiday Inn Express Chennai OMR | open | listed at about 145 m in every run, matched to another OSM way | Common Crawl: pages answer 200 in July 2026 |
| REWE, Rosenthaler Weg 74, Berlin | open | listed in every run checked except 2025-11-15 and 2026-11-14, both with a third fewer REWE features | `rewe_de` lost 29% of its requests on 2026-11-14 |
| REWE, Allee-Center, Berlin | open | listed in every run checked except 2026-11-14 | the same failed run |
| Sheraton Khalidiyah, Abu Dhabi | open | never listed within 1.4 km | Overture: present, 2026-11 |
| D'Angelo, Quincy | likely open | the spider has 53 to 104 branches and never lists Quincy | ordering page live |
| Storchengarage, Neunkirch (Renault) | likely open | listed until 2025-11, gone from the Renault list since 2026-05 | own website up; Overture: open, 2026-11 |
| Residence Inn Al Jaddaf, Dubai | listed as open, unclear | listed until 2025-11, gone since 2026-05 | Common Crawl: Marriott's page answers 404 from December 2025 |
| Walgreens, 220 Grafton St, Worcester | closed | listed until 2025-05, gone since 2025-11 | branch page announces the closing; Overture still says open |
| Sonic, 220 Main St, Wilmington | closed | in no run checked; the spider's output starts in 2026-05 | Common Crawl: store page 200 in March 2025, 404 now |
| C&A, Zentralstrasse 12, Dietikon | closed | never listed since 2024-11 | Overture's sources last updated in 2025 |

Four of the eight wrong suggestions, and the unclear Shell, are **OSM
duplicates**. The spider does list the place, but conflate matched it to a second OSM element of
the same brand nearby: a building and a node, two nodes, or a hotel
and its apartments. Two more come from **one failed run** (REWE). One
is a **gap in the spider** (Sheraton), and one a place the spider
dropped for good while it may still be open (Residence Inn). The three
right ones are a
place that **disappeared from the spider and stayed gone** (Walgreens),
and two the spider never had (Sonic, C&A).

## Ideas, by how much they would help

### 1. A matched OSM neighbour of the same brand

If the brand has another OSM element within about 150 m that a spider
matched as `same`, the candidate is most likely a duplicate or a part
of the same place. That should be a review, or a merge, never a
delete. This affects 3 to 22% of the edits per region (19% in Japan,
15% in the UAE and Massachusetts). It would have caught four of the
eight wrong suggestions above, and the unclear Shell. It needs nothing new.

### 2. Disappearance, not absence

Each week AllThePlaces publishes a whole run, and its history goes
back years. A branch that a spider listed for months, then stopped
listing for three or more runs in which the spider had no request
errors, is much stronger evidence than one the spider never had:

- The REWE pair, Sheraton and D'Angelo would not have been deleted.
- Walgreens would, correctly.
- C&A and Sonic would not, which loses true closures but costs nothing in trust.
- Residence Inn and Storchengarage still need idea 7.

Matching a place across runs works best by spider `ref`, which is
stable for most spiders, with distance as a fallback. Keeping a small
table of "first seen, last seen" per (spider, ref) would make this
cheap; the quick check read the old runs' outputs from the published
zips.

### 3. Gate on the spider's request errors

The spider stats that AllThePlaces publishes for each run show when a
run lost requests:

| spider | requests | failed | items |
| --- | ---: | ---: | ---: |
| `japan_post_jp` | 1,410 | all, HTTP 503 | 0 |
| `inditex` | 4,564 | 1,848 | 3,892 |
| `rewe_de` | 4,227 | 1,219 | 3,006 |
| `amazon_locker_at_cz_de` | 1,806 | 521 | 2,936 |
| `hilton` | 1,355 | 268 | 6,465 |
| `nissan` | 772 | 160 | 9,439 |

"Failed" counts downloader exceptions, 5xx responses and
`retry/max_reached`. Of the 527 spiders behind candidates, 32 lost
more than 0.5% of their requests. Gating on that would remove 578 of
the 7,933 deletes. That's helpful, but less than ideas 1 and 2, and
spiders that probe a grid of locations can lose results without any
error. As Sascha pointed out, a chain that leaves a whole region at
once looks like a gap too. The difference is that it is consistent
over several error-free runs, which is idea 2 again.

### 4. Coverage of the brand and region by the spider

Some spiders list only part of a brand or region. `dangelos_us` had 53
to 104 features, and never one within 60 km of the Quincy shop.
`marriott_hotels` has a single Residence Inn in the UAE extract. A spider
that never emits the candidate's `brand:wikidata` anywhere, or has no
feature within some distance in any run, should not drive deletes.
Candidates that cluster in cells where a brand's match rate suddenly
drops, as `japan_post_jp`'s did, point the same way: closures are
spread out, while gaps come in patches.

### 5. Other open place data as a second opinion

- **Overture Maps places** has a record with a source update from
  November 2026 for all six open places it lists (it lacks the Holiday
  Inn Express), and for the likely open Shell and Storchengarage. For
  the closed C&A it has only updates from 2025, and nothing for Sonic.
  It lists the closed Walgreens as open, updated 2026-09, and the
  Residence Inn with an update from 2025-09.
- **Foursquare's open places** data has `date_closed` and
  `date_refreshed`. Not checked here.

Neither licence lets us copy their data into OSM, but as evidence for a
person reviewing an edit they would help.

### 6. The web: Common Crawl and live pages

Common Crawl's columnar URL index can be queried on
`data.commoncrawl.org` with DuckDB. A query for a few URL prefixes takes
about 10 s per crawl, but rate limits made many queries fail and need
retries.

- **Freshness:** for the 14 places it had pages from 2024 to mid-2026,
  but each crawl samples the web (649 of about 8,000 Walgreens branch
  pages in one crawl), so the newest capture is often months old.
- **Removed pages:** it shines when a page that answered 200 for
  years starts answering 404. That happened for Marriott's Residence
  Inn page from December 2025 and the Sonic store page, which answered
  200 in March 2025 and 404 now.
- **Live pages:** a branch page in the OSM `website` tag, fetched
  today, answered the question directly for Lidl (live), Shell (live)
  and Walgreens (live, announcing the closing). Many chain sites block
  scripts: Marriott, Hyatt, IHG and REWE answered 403.

Common Crawl also has the page text, so closure notices ("閉店のお知らせ",
"wir schliessen", "permanently closed") near a branch's address could
be searched. That isn't tried yet; Japanese chains publish one per
store.

### 7. A brand leaving, not a place closing

Storchengarage is still a garage with its own website, but no longer in
Renault's dealer list. The Residence Inn may have left Marriott. When a
place disappears from a brand's list but its own website, Overture or
Foursquare still show it, the right edit removes `brand*` and keeps
the place. The Name Suggestion Index also marks brands that were
renamed or merged. That makes a rebrand a modify, not a delete,
which also covers the fuel stations with their own names.

### 8. Signals in OSM itself

OSM Notes within 200 m of the 14 places said nothing about them.
StreetComplete answers, `check_date` and street-level imagery
(Panoramax, Mapillary) taken after the fetch could still help, the
last one only with sign detection.

### 9. Feedback from reviews

MapRoulette records whether a mapper fixed a task, found it already
fixed, or found it wrong. Per spider and category that becomes a
measured precision, which can gate later runs the way the
opening-hours gate does.

## Infrastructure

The infrastructure edits weren't checked one by one, so "they look
better" in the hand check meant only that the sources are the
operators' own lists. The data shows where they go wrong:

- **Swiss transit (1,400 edits).** By construction, the register lacks
  the IDs of these stops, but:
  - 81 have a register stop of the same name within 300 m, which
    looks like a new ID for the same stop;
  - a sample of 16 has on-demand stops (`mybuxi`) and ski-lift
    stations, which the register may not cover at all.

  Per stop type, the register's scope needs checking before any
  deletes.
- **Bike-share docks (403 edits).** 25% have an unmatched dock of the
  same feed within 200 m, against 9% for docks that matched. Docks
  that move or get renamed are common, so a nearby unmatched dock
  should make the edit a move, not a delete. The feed's history
  (station IDs across runs) would tell moves from removals.
- **Post boxes (403 edits).** 31% have an unmatched box of the same
  operator within 200 m, but so do 27% of the matched boxes, so here
  that signal says nothing. The `ref` mismatches seen in the hand check
  (30005 against 030005) point to normalising refs before matching.
- **Transit, generally.** Unmatched register stops are so dense (64%
  of matched stops have one within 50 m) that "something unmatched
  nearby" is no signal for transit at all; matching by ID or name is.

## What AllThePlaces could do

Nothing big, but a few small things in their pipeline would help any
consumer that infers closures:

- Mark runs that lost requests. A stat such as
  `atp/incomplete: true` when `retry/max_reached` or downloader
  exceptions exceed a small share would save everyone from parsing
  the stats themselves. The fallback that `japan_post_jp` got this week
  only happens when a spider drops entirely.
- Keep `ref` stable across runs, and say which spiders don't, so
  that consumers can follow a branch over time.
- Emit places that a store locator marks as closing or closed, with
  `end_date` or a status, instead of dropping them. Walgreens' page
  announced the closing weeks ahead.
- Publish, per spider, whether it reads a complete list (a sitemap or
  an "all stores" API) or probes a grid of locations. Only the first
  kind can make absence mean much.
- Publish the brands and countries a spider is meant to cover, so
  that a sub-brand it doesn't scrape isn't taken as closed.

## Reusing the OSM index instead of a planet pass

The reference pass in #1038 read the planet file once more, about 35
minutes on 4 cores. The prune step of `import_osm` already reads every
relation and way. It keeps `relation_members`, but only for members of
relations we keep (about 0.9 million of 14.6 million), so route
relations, the cause of most Swiss reviews, are missing.

During those passes the prune step could store two flags on each
interesting feature:

- member of some relation;
- for nodes, used by some way.

That covers node candidates completely. For a way, deleting its
leftover untagged nodes needs their versions, which the index doesn't
have. If the edit leaves them out, a mapper's editor deletes them with
the way, as JOSM and iD do. No second planet pass would then be
needed.

## A next attempt

1. Duplicates first: a candidate with a matched OSM element of the
   same brand within 150 m becomes a review, or a merge suggestion.
2. Deletes only for places that a spider listed and then dropped for
   three or more runs without request errors. A place the spider never
   had gets no delete.
3. No deletes for spiders that lost requests, or that never emit the
   candidate's brand.
4. Each edit lists its evidence (last seen in AllThePlaces, the web
   page's status, Overture's last update), so the reviewer can judge
   quickly.
5. Measure precision through MapRoulette before anything goes out in
   bulk.

Until then, no `obsolete_poi` edits.
