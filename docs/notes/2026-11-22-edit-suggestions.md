# Edit suggestions: a Parquet file, one map, better edits

This note is a plan, not a record of finished work. It covers three
things that came up together: storing suggested edits in a Parquet
file, showing them on the same map as the conflation, and deciding
which edits to suggest in the first place. Nothing here is measured
yet; the parts that need numbers say so.

## Where we are

`suggest_edits` ([`src/pipeline/edits.rs`](../../src/pipeline/edits.rs))
reads `conflated.parquet`, keeps the `same` rows, and asks
`PoiEditSuggester` what to change. It writes GeoJSON Lines straight
to `shops.jsonl`; the `infrastructure` and `trees` layers are always
empty. `render_tiles` turns them into `diffed-places.pmtiles`, which
is the only form in which the edits leave the pipeline.

Each edit carries the new tag values, `osm_version`, the OSM
feature's centroid and the row's `importance`. It doesn't carry the
old OSM value, the spider, the match rule, or which row of
`conflated.parquet` it came from. When several AllThePlaces features
match one OSM feature, `write_edits` sorts by `osm_id` and keeps the
first, so which one wins is arbitrary.

`PoiEditSuggester` suggests `email`, `end_date`, `fax`,
`opening_hours`, `phone`, `start_date` and `website` wherever ATP's
value differs from OSM's or OSM has none, plus brand tags for former
brands (#990). Fuel stations get no edits (#995), and `chargepoint`
is held back at ingestion until alltheplaces#20717 is fixed.

## `edits.parquet`

`suggest_edits` writes `edits.parquet` instead of GeoJSON Lines. It
isn't public for now: it isn't listed in `datapackage.json`, and once
a consumer such as MapRoulette (#696) needs it, it gets a page in
`docs/outputs/` and goes public.

It does go to the internal bucket, like the run's log, as
`edits/<YYYYMMDD>.parquet`, named by the release date. That way, tuning a suggester means
comparing this week's file with last week's in DuckDB, without a
planet run of our own, and the stability rule below can read last
week's edits.

One row per OSM feature:

```
osm_type        UTF8      "node" | "way" | "relation"
osm_id          UINT64
osm_version     UINT32    the version the edit was computed against
kind            UTF8      "add" | "change" | "rebrand", see below
category        UTF8      "shops" | "infrastructure" | "trees"
changes         LIST<STRUCT<
  key           UTF8
  old           UTF8      nullable, OSM's value
  new           UTF8
  sources       LIST<UINT32>  indexes into `sources` that propose `new`
>>
conflicts       LIST<STRUCT<
  key           UTF8
  values        LIST<STRUCT<value UTF8, sources LIST<UINT32>>>
>>                        keys left out of `changes`, see below
sources         LIST<STRUCT<
  spider        UTF8
  conflated_row UINT64    ordinal in conflated.parquet, like `fid`
  relation      UTF8      "same" | "located_in"
  rule          UTF8      the row's match.evidence.rule
>>
importance      FLOAT     the highest of its sources'
geometry        BINARY    WKB point, the OSM centroid (GeoParquet)
```

The category becomes a column, so the three channels and three
external sorts in `write_edits` go away.

### Several matches on one OSM feature

One OSM feature often has several AllThePlaces features on it, and
they should become one edit, not compete for it:

- **Several spiders describe the same place.** A car dealership can
  appear in the spiders of each brand it sells, and a fuel station
  in both the oil company's spider and the convenience store's.
- **Services inside a host.** Western Union counters, ATMs, parcel
  lockers and postal services offered by a shop are `located_in`
  rows whose OSM feature is the host.

`suggest_edits` therefore collects every row for an OSM feature,
asks each one's suggester for changes, and merges them per key:

- If all sources that propose a key agree, it becomes one change
  listing all of them in `sources`. "Agree" means the same value as
  the key's own comparison sees it, such as `same_schedule` for
  `opening_hours`, so `Mo-Su 00:00-24:00` and `24/7` merge.
- If they disagree, the key goes to `conflicts` with each value and
  who proposed it, and stays out of `changes`. A dealership's brand
  spiders may give sales hours and service hours under the same key;
  picking one would be a guess. A reviewer can still see the
  conflict, and once the rules below exist, a key that only one
  trusted source proposes wins over sources that aren't trusted for
  it.
- A few keys take a list rather than one value, such as `brand` and
  `brand:wikidata` for a dealership of several brands. Those merge
  into a `;`-separated value in a fixed order. Which keys these are
  is an allowlist, starting empty.

Rows of the second kind need their own suggester, since they don't
copy tags from AllThePlaces onto the host: a Western Union counter
in a shop suggests something like `money_transfer=western_union` on
the shop, a postal counter `post_office=post_partner` with its
`post_office:brand`. Edits come only from `same` rows today, so this
is new work, and each such mapping should be checked with the OSM
tagging that is actually in use before it is suggested.

The file is sorted by S2 cell, like `conflated.parquet`, for spatial
chunking. Merging needs the rows by OSM feature first, so this is two
passes: the existing external sort by `osm_id` (by `osm_type` too,
since a node and a way can share an id), with the merge done while
reading it back, then a sort by cell. At ~20 seconds for the
whole step today, a second sort shouldn't matter, but measure it.

## One PMTiles file

The web app should show matched OSM features in a different colour
when there is an edit for them. Joining two PMTiles archives in the
browser can't do that well: the app would have to read every loaded
tile of one archive and set feature-state on the other, again on
every tile load. At coarse zooms the two archives also drop different
features, so an edit would often have no matched feature to colour.

So the join happens in the pipeline, where both sides come from the
same rows of `conflated.parquet` anyway:

1. `suggest_edits` runs before `extract_conflated_layers` and writes
   `edits.parquet`.
2. `extract_conflated_layers` reads the edited `(osm_type, osm_id)`
   pairs into a set. Even a few hundred thousand edits fit easily in
   memory.
3. Overview features of `matched` rows get `edit: true` when the set
   holds their OSM feature, and nothing else. A two-valued attribute
   dedups in the vector tile and costs almost nothing at z0; edit
   tags there would repeat what happened with full tags and `fid`
   (see `extract_conflated_layers` in
   [`TECHNICAL_DESIGN.md`](../TECHNICAL_DESIGN.md)).
4. Detail features of the OSM side get `edit:kind` and one
   `edit:<key>` property per changed key, holding the new value, so a
   click shows what would change. Keys in conflict are listed in
   `edit:conflicts`, `;`-separated, with the values left to
   `edits.parquet`.

`render_tiles` for `diffed-places.pmtiles`, and `upload_tiles`, then
go away. The cost is that the conflated tile steps wait for
`suggest_edits`, and the overview grows by one attribute.

## Fewer OSM tags in the tiles

The detail layers carry the full `osm:*` tag set twice for each
matched row: on the OSM shape and again on the connector line. The
web app could fetch OSM tags when a feature is clicked, from
`/api/0.6/{type}/{id}` (or `/{type}/{id}/full` for a way's nodes).
That is one request of a few hundred milliseconds per click, which
the OSM API usage policy allows for interactive use. It also shows
the feature as it is now: if the current version is newer than
`osm_version`, someone has edited it since, and the app can say that
the suggestion may be stale.

The plan:

- Keep on the OSM side only what the map style needs without a
  request: `osm:type`, `osm:id`, `osm:version`, `osm:name` and the
  key that says what kind of place it is.
- Keep all `atp:*` tags; AllThePlaces has no per-feature API to
  fetch them from later.
- Drop all tags from connector lines; `fid` already groups them with
  their two ends.
- Keep the `edit:*` properties from the previous section.

Whether this is worth doing depends on how much of the ~1 GB detail
archive the OSM tags take up, which we don't know. Before changing
anything, build the detail pass once with and once without `osm:*`
tags on a planet `conflated.parquet` and compare sizes.

## Which edits to suggest

Today's rule is "ATP's value for an allowlisted key differs from
OSM's". That treats every spider as equally right about every key.
The changes below are about *when* to trust ATP more than about
*which* keys to touch.

### Trust per spider and key

`spider-stats.parquet` already counts, per spider and tag, how many
`same` rows agree and differ where both sides have a value (`tags`,
see [`SPIDER_STATS.md`](../outputs/SPIDER_STATS.md)). A spider that
agrees with OSM on `opening_hours` 95% of the time is probably right
where it differs; one that agrees 40% of the time probably has bad
data. So `suggest_edits` suggests key K for spider S only when S's
agreement on K is above a threshold, with enough rows to tell. The
threshold needs a look at the actual distribution; start by listing
it per key.

For `opening_hours`, the `kinds` breakdown helps further: a spider
whose disagreements are mostly `small` is rounding, not wrong, and
shouldn't produce `change` edits.

Spiders on the held-back list get no edits, and a spider with an open
bug report in this project should be easy to block too.

### Three kinds of edit

- **`add`**: OSM has no value for the key. Low risk, quick to review.
- **`change`**: OSM has a different value. Needs a reviewer to decide
  who is right.
- **`rebrand`**: the brand tags from former brands, as today.

An edit with both an addition and a change counts as `change`.
MapRoulette challenges would split along these lines, so that an
`add` challenge can move quickly.

### Freshness and stability

- No `change` over an OSM value that was edited, or got a
  `check_date`, after the spider collected its data. Both
  `osm.modified` and `atp.fetched` are in `conflated.parquet`.
- Suggest a value only when the spider reported it in two weekly runs
  in a row. This reads last week's `edits.parquet` from the internal
  bucket, found the same way as everything else from last week:
  `previous_release` reads `version` from the public
  `datapackage.json`, and the internal key follows from it. That is
  why the key carries the date but no hash. There is only one place
  that says which run came before, and the manifest goes up last, so a
  run it lists has uploaded its edits too. If the file is missing
  anyway, the rule is skipped for that week, and a warning logged.

### Keys

Worth adding:

- **`brand`, `brand:wikidata`, `brand:<lang>`** when the match came
  from a phone number, website, email or address and OSM has no
  brand at all. High value, and easy to check.
- **Addresses**, building on
  [the address evidence](2026-11-17-address-evidence.md), as `add`
  only.
- **Branch websites**: suggest `website` only when ATP's URL is
  specific to the branch, not the brand's home page.

Worth dropping or guarding:

- **`fax`**: little value to OSM editors.
- **`opening_hours`** stays, but never replaces a value with richer
  syntax than ATP writes (holidays, exceptions); `has_rich_syntax`
  already exists for this.
- **`phone`, `email`, `website`** are never added when OSM has the
  same key under `contact:`.
- **`name`** is left alone outside rebrands.

Deletion candidates stay a separate output: removing a feature is a
different kind of review from changing its tags.

## Order of work

1. `edits.parquet`, with the schema above and the merge of `same`
   rows, replacing the GeoJSON Lines and uploaded to the internal
   bucket; `render_tiles` reads a layer extracted from it, so the map
   is unchanged.
2. `edit: true` and `edit:*` in `conflated.pmtiles`; drop
   `diffed-places.pmtiles`.
3. Measure the OSM tags' share of the detail archive, then decide.
4. Edit kinds and per-spider trust, with a before/after diff of
   `edits.parquet` on a planet run for each.
5. Freshness, stability and the new keys, one at a time.
6. Edits on hosts from `located_in` rows, one kind of service at a
   time.
