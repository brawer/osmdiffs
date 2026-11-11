# Chinese coordinate systems in AllThePlaces

In mainland China, map services give positions in GCJ-02, a coordinate
system that the law requires there, or in Baidu's BD-09, which is
GCJ-02 shifted again. Both differ from WGS-84, which OpenStreetMap
(OSM) uses, by a few hundred meters, and by a different amount in
every place. Many AllThePlaces (ATP) spiders take their positions from
such services. Their features then land on the wrong street, and
either match nothing or match another branch of the chain nearby.

In OSMDiffs' `conflated.parquet` of 2026-11-02 (OSM planet of
2026-11-02, ATP run of 2026-10-31), the 30,700 ATP features in China
had a match rate of 4%. For `starbucks_cn`, the nearest OSM Starbucks
was a median of 624 m away.

## Spiders differ

We can't convert all of China the same way. For each spider with
features in Shanghai, Beijing and Guangdong, we counted the features
that are within 30 m of an OSM feature of the same `brand:wikidata`
when we read their positions as WGS-84, GCJ-02 or BD-09:

| spider | WGS-84 | GCJ-02 | BD-09 |
|---|---:|---:|---:|
| `starbucks_cn` | 2 | 472 | 4 |
| `burger_king_cn` | 0 | 19 | 0 |
| `saizeriya` | 0 | 13 | 0 |
| `nike` | 0 | 12 | 0 |
| `marriott_hotels` | 0 | 18 | 0 |
| `shell` | 1 | 0 | 25 |
| `apple` | 10 | 0 | 0 |

So `starbucks_cn` and most others use GCJ-02, `shell` uses BD-09, and
`apple` uses WGS-84. A spider for a global brand, such as `nike` or
`marriott_hotels`, can still use GCJ-02 for China, so the brand alone
doesn't tell either.

## The rule

`import_atp` now runs after `import_osm`. While it reads a spider's
GeoJSON file, it holds back the features in mainland China. Hong Kong,
Macau and Taiwan don't count; the `country-boundaries` crate tells
them apart. Each feature with `brand:wikidata` then votes: for each of
the three systems, is there an OSM feature of its brand within 30 m
of the converted position? A system other than WGS-84 wins if it has
at least 3 votes and at least three times as many as the next best.
Then the spider's features in mainland China are converted to WGS-84:
point, shape and S2 cell. Otherwise they stay as given.

So `alltheplaces.parquet` is in WGS-84 throughout, and every later
step, from matching to OSMViews importance to the departments of
[`2026-11-09-departments.md`](2026-11-09-departments.md), sees the
corrected positions. The log line `import_atp: datum in mainland China`
gives each spider's votes and choice.

Code: `src/pipeline/atp/datum.rs`, used by `process_geojson` in
`src/pipeline/atp/mod.rs`. The conversion formulas are the usual ones
from open source libraries. They are accurate to a meter or two, and
the inverse of GCJ-02 is found by iteration.

## Alternatives considered

- **Convert all of China as GCJ-02.** This would break `apple` and
  `shell`, which use WGS-84 and BD-09.
- **A datum attribute in ATP's metadata.** That is the cleanest signal
  where a spider author knows the answer, but it needs every Chinese
  spider to be tagged and kept up to date. We can still read such an
  attribute later, and let it override the vote.
- **Convert in ATP.** ATP should report what its sources say, and the
  conversion is lossy and specific to China.
- **Vote in `conflate`, convert before matching.** This was the first
  version. It needed the OSM index, which only `conflate` had at the
  time. But `alltheplaces.parquet` then kept the shifted positions,
  and so did everything that reads it besides matching, such as
  OSMViews importance. `import_osm` doesn't need anything from
  `import_atp`, so it can simply run first.
- **Choose the system per feature, not per spider.** A store could
  then snap to another branch of its chain a few hundred meters away.
  A vote over the whole spider avoids that.
- **Datum per brand.** Brands are covered by several spiders, which
  may differ. The spider is the unit that takes its positions from one
  source.

## Eval

ATP: the run of 2026-10-31 (`2026-10-31-13-32-34`), its features with
`addr:country=CN`. OSM: openstreetmap.fr extracts of Shanghai, Beijing
and Guangdong, of 2026-11-09. Main at 8a822da against this branch,
which also has #904 and #905. Rows are those inside each region; for Guangdong,
not Hong Kong or Macau.

| region | ATP rows | `same` main → branch | median distance of `same` | within 50 m |
|---|---:|---:|---:|---:|
| Shanghai | 2,636 | 243 → 496 | 246 → 26 m | 15 → 337 |
| Beijing | 1,852 | 81 → 230 | 251 → 31 m | 7 → 155 |
| Guangdong | 2,735 | 91 → 469 | 263 → 34 m | 4 → 305 |

Matches on main were mostly wrong: at a median of 250 m, an ATP
Starbucks matched whichever OSM Starbucks was within reach, often
another branch. Phone numbers that agree went from 9 to 19 in
Shanghai, and from 2 to 10 in Guangdong. Most gains are from
`starbucks_cn` (201 → 363 in Shanghai, 73 → 299 in Guangdong),
`burger_king_cn`, `saizeriya`, `nike`, `adidas`, the hotel spiders and,
in Guangdong, `shell` (1 → 24, BD-09).

Each region votes only with its own OSM data, and the choices agree
where a spider has enough votes. With 2 or 3 votes, some spiders stay
at WGS-84 in one region and convert in another, such as `ihg_hotels`
and `accor`. On the whole of China, the votes will be larger.
`hm` had 3 votes for GCJ-02 and 4 for BD-09 in Guangdong, and stayed
WGS-84; that's the intended outcome when the vote is unclear.

Deletion candidates went from 186 to 92 in Shanghai, but from 0 to 34
in Beijing and 8 to 82 in Guangdong. With more matches, ATP now covers
these brands locally, so the rule of
[`2026-11-09-local-deletion-coverage.md`](2026-11-09-local-deletion-coverage.md)
lets candidates through: mostly Starbucks, Burger King, Nike and
Adidas. Some will be closed stores. Others may be OSM features that
were themselves mapped from a GCJ-02 map, which this change doesn't
detect.

Cost: the vote queries the OSM index three times for each ATP feature
in mainland China, about 30,000 features in the whole ATP run, against
3.5 million features that `conflate` matches.

## Unclear votes and deletion candidates

There is no separate state for an unclear vote. A spider without a
clear winner stays at WGS-84, and its features in mainland China go
into conflation as given, like any others. Deletion candidates don't
look at the datum either. Their only safeguard is the rule of
[`2026-11-09-local-deletion-coverage.md`](2026-11-09-local-deletion-coverage.md):
at least half of the brand's OSM features nearby must have been
matched. But features that are still shifted can match another branch
a few hundred meters away. Such wrong matches count as matched, and
then the OSM feature of the right branch can become a candidate.

We checked how often this happens in the first production run with
this change: `osmdiffs:v0.8.13`, `conflated-20261102-a4a70f0f.parquet`
(OSM planet of 2026-11-02), with the votes from the log line
`import_atp: datum in mainland China`. Of the 113 spiders with
features in mainland China, 12 chose GCJ-02, 2 chose BD-09, and 99
stayed at WGS-84, most of them with no votes at all.

Mainland China had 383 deletion candidates. For 369, at least one
spider of the brand had a clear vote; most are Starbucks, Burger King,
Adidas and Nike, all GCJ-02. The other 14 come from spiders that
stayed at WGS-84 on almost no votes:

| brand | candidates | spider | votes WGS-84 / GCJ-02 / BD-09 | median distance of `same` |
|---|---:|---|---|---:|
| Skechers | 4 | `skechers` | 0 / 1 / 0 | 85 m |
| Vans | 2 | `vans` | 0 / 1 / 0 | 127 m |
| Omega | 2 | `omegawatches` | 0 / 1 / 0 | 237 m |
| Burberry | 1 | `burberry` | 0 / 1 / 0 | 145 m |
| Tiffany | 1 | `tiffany` | 0 / 2 / 0 | 122 m |
| Porsche | 1 | `porsche` | 0 / 3 / 2 | 11 m |
| Hugo Boss | 1 | `hugo_boss` | 0 / 0 / 0 | 13 m |
| CoCo Ichibanya | 1 | `coco_ichibanya` | 0 / 0 / 0 | 20 m |
| FamilyMart | 1 | `familymart_tw` | 0 / 0 / 0 | — |

For `starbucks_cn`, converted to WGS-84, the median distance of `same`
matches is 24 m. So Skechers, Vans, Omega, Burberry and Tiffany are
probably still shifted, and their candidates are doubtful. `skechers`
alone has 3,169 features in mainland China, but OSM has few Skechers
stores with `brand:wikidata` there, so its vote is tiny. The distances
are over all of each spider's `same` rows in a box around China, so
they include some outside the mainland.

A vote can also be unclear because two shifted systems compete:
`audi` had 0 / 10 / 24 votes and `hm` 0 / 7 / 4. Both are clearly not
WGS-84, but neither shifted system won by a factor of three, so both
stayed at WGS-84. Neither had a deletion candidate in mainland China.

We could skip deletion candidates in mainland China whose brand has
only spiders with an unclear vote. For 14 of 383 candidates, that is
not worth the code for now.
