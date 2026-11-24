# After a weekly run

What to check after each weekly production run, in this order, and
what to do about it. It takes about half an hour when nothing is
wrong. Most of it reads `pipeline.log`, which every run uploads to
`logs/<run-id>.log` in the internal bucket (see
[`LOGGING.md`](LOGGING.md)); the rest reads the published
[`spider-stats.parquet`](outputs/SPIDER_STATS.md) and
[`edits.parquet`](outputs/EDITS_PARQUET.md).

The commands below assume the log as `pipeline.log` and the files of
this week and last week as `spider-stats.parquet` and
`spider-stats.last.parquet`, with [jq](https://jqlang.org/) and
[DuckDB](https://duckdb.org/).

## 1. Did the run finish?

- The container exited with status 0. Status 137, or an OOM kill in
  `dmesg`, means it ran out of memory; see
  [What to watch](PRODUCTION.md#what-to-watch). A killed step has no
  `end` record, so the log alone doesn't tell.
- `data/datapackage.json` on the CDN carries this week's date and
  lists `conflated`, `edits` and `spider-stats`.
- No `ERROR` records:

  ```sh
  jq -c 'select(.level == "ERROR")' pipeline.log
  ```

- Step timings are close to last week's. A step that takes twice as
  long on unchanged code is worth a look:

  ```sh
  jq -c 'select(.fields.phase == "end") | [.fields.step, .fields.elapsed_seconds]' pipeline.log
  ```

## 2. Read the warnings

All warnings, grouped by message:

```sh
jq -r 'select(.level == "WARN") | .message' pipeline.log | sort | uniq -c | sort -rn
```

Then the ones below, with their fields:

```sh
jq -c 'select(.level == "WARN" and (.message | startswith("spider_"))) | [.message, .fields]' pipeline.log
```

| message | likely cause | what to do |
|---|---|---|
| `spider_churn: spider lost features since last week` | the spider broke at its source, or AllThePlaces fell back to an older run | Nothing for the pipeline: the edits step already leaves out such spiders (`make_edits: leaving out unhealthy spiders`). If it lasts several weeks, check whether AllThePlaces has an issue for it. |
| `spider_churn: spider's match rate dropped since last week` | positions or kinds changed in the spider | As above. Look at a few of its `missing_in_osm` rows on the map. |
| `spider_creates: brand ID that OSM doesn't use in these countries` | the spider gives its features another brand's Wikidata ID, as `standard_chartered_bank` gave its Kenyan branches the ID of Standard Chartered Taiwan | Look up the ID on Wikidata. If it is wrong, file an issue at AllThePlaces, and leave the spider out of creates (`excluded_spider` in `src/pipeline/edits/create.rs`) until it is fixed. Some are fine: a brand that OpenStreetMap tags with another ID, or one that is new in a country. |
| `spider_creates: spider lists places twice` | the spider writes some features twice, a few meters apart, as `bp` did in Switzerland | Its doubles are already left out of creates (`twin`). File an issue at AllThePlaces if there is none. |
| `spider_creates: spider's creates jumped since last week` | the spider gained features, changed its kinds, or its positions got good enough; or conflate stopped matching them | Look at five of its new places (step 4). If conflate stopped matching, the spider's `same` count in `spider-stats.parquet` dropped too. |
| `spider_creates: spider's names changed since last week` | the spider switched between real names and internal labels | Look at a few of its names in `conflated.parquet`. |

The `spider_creates: review` record sums them up, with `compared_with_last_week`
false if last week's file had no create counts.

## 3. Compare the edits with last week

The edits step logs its totals in `make_edits: creates` and
`make_edits: done`:

```sh
jq -c 'select(.message | startswith("make_edits")) | [.message, .fields]' pipeline.log
```

Compare `edits`, the topic counts (`contact`, `wikidata`,
`opening_hours`, `post_partner`, `new_poi`) and the `skipped_*` counts
with last week's log. They change by a few percent from week to week.
A change by a quarter or more, on any of them, needs an explanation:
a code change, a new spider, or a spider that broke.

The spiders whose new places changed most:

```sql
SELECT n.spider, o.creates.creates AS last_week, n.creates.creates AS this_week
FROM 'spider-stats.parquet' n
LEFT JOIN 'spider-stats.last.parquet' o USING (spider)
WHERE n.creates IS NOT NULL
ORDER BY abs(coalesce(n.creates.creates, 0) - coalesce(o.creates.creates, 0)) DESC
LIMIT 20;
```

## 4. Look at a sample

Precision matters more than the number of edits, so look at some by
hand every week, against the live OpenStreetMap map:

- Ten new places, from the spiders whose creates jumped or that are
  new this week, else at random:

  ```sql
  INSTALL spatial; LOAD spatial;
  SELECT id, sources[1].spider AS spider,
         ST_AsText(ST_GeomFromWKB(geometry)) AS point, changes
  FROM (SELECT * FROM 'edits.parquet' WHERE action = 'create')
  USING SAMPLE 10;
  ```

- Ten modifies, at random across the topics.

A new place is wrong if OpenStreetMap has it already (under another
kind or name), if it is part of another feature (the shop of a fuel
station), if its kind or name is wrong, or if it is somewhere else.
When one is wrong, find the cause before counting it as noise: most
come from one spider, and a rule or an upstream fix handles all of
them.

## 5. Spiders that conflate matches badly

`creates.similar_name_30m` counts a spider's candidates that have an
OpenStreetMap feature within 30 m with a similar name. Many of them
mean that the spider's kinds differ from OpenStreetMap's, so conflate
doesn't match them:

```sql
SELECT spider, creates.candidates, creates.similar_name_30m
FROM 'spider-stats.parquet'
WHERE creates.similar_name_30m >= 50
ORDER BY creates.similar_name_30m DESC
LIMIT 20;
```

Look at a few rows of the top spiders. If AllThePlaces tags them
wrongly, file an issue there; if OpenStreetMap maps such places with
another kind that should agree, it belongs in `category.rs`.

## 6. File what you found

- Data problems of a spider go to
  [AllThePlaces](https://github.com/alltheplaces/alltheplaces/issues),
  framed as problems for any consumer of its data, with a few example
  features and their source pages. Until a fix ships, hold the spider
  back (`src/pipeline/held_back.rs`) if it harms all its rows, or
  leave it out of creates if only its new places are wrong.
- Problems of the pipeline go to
  [brawer/osmdiffs](https://github.com/brawer/osmdiffs/issues).
- A check that should have caught a problem, but didn't, belongs in
  this document, and if it can be automated, in `spider_churn.rs` or
  `spider_creates.rs`.
