# Less noise in deletion candidates: local ATP coverage

This change is about the OSM-only rows of `conflated.parquet`, the
deletion candidates: OpenStreetMap (OSM) features that no AllThePlaces
(ATP) feature matched, and that we think may be gone. Matching itself
doesn't change. Every ATP row gets the same OSM feature as before; in
the eval below, the matched rows are identical in all ten regions. Only
fewer OSM-only rows come out.

## The problem

A feature with `brand:wikidata` (or `operator:`, `network:`, or the
plain names) became a candidate when ATP had features of that brand, of
the same kind, *anywhere in the same country*. See
[`2026-11-05-deletion-candidates.md`](2026-11-05-deletion-candidates.md).
But a spider often lists only part of a brand: one region, or a few
hundred stores out of thousands. All the other OSM features of the brand
in that country then look like closures. The 2026-11-02 planet run had
331,542 such candidates. Some examples:

- ATP has 31 Swiss Post post boxes. That made 3,939 Swiss Post boxes
  in OSM deletion candidates.
- ATP's `rewe_de` has 1,551 REWE stores in Germany, where REWE has
  more than twice as many. 79 of them are in Berlin. In Berlin, 98 of
  the 160 REWE stores in OSM were candidates.
- `cvs_us` has no store in Massachusetts that matches OSM, so all 361
  CVS pharmacies there were candidates. Burger King, whose only
  features in the US come from the Czech and Romanian spiders, made 95.

None of these say anything about the stores. They only show that ATP
doesn't cover the brand there.

## The rule

A candidate by `brand:wikidata`, `operator:wikidata`,
`network:wikidata` or by name now also needs *local* coverage. Of the
OSM features of its brand within a radius, at least half must have been
matched by ATP. The candidates themselves count as unmatched. If a
candidate has several brands that ATP covers, the best one counts. The
Swiss transit rules don't change: the Swiss register lists every stop
in its home area.

The radius depends on the brand. ATP lists a Burger King every few
kilometers, but a car dealer of some brand only every hundred. For each
spider and brand, we compute the median distance from each ATP feature
to its nearest neighbour. The radius is five times that, between 25 km
and 150 km. A spider with only one feature of a brand gets 150 km. A
brand often comes from several spiders, often one per country, and one
spider may emit several brands. So spacing is computed per spider and
brand, and for a brand in a country, the radius is the largest among
the spiders that have the brand there.

Street furniture counts by its kind, as before: Swiss Post's post boxes
and its post offices are counted apart. Points of interest of one brand
count together, whatever their category. A brand's fuel stations aren't
candidates when ATP only has its supermarkets, so they never enter the
counts.

Code: `src/pipeline/conflate/local_coverage.rs`, used by
`send_deletion_candidates` in `src/pipeline/conflate/mod.rs`.

## Why the general density of an area doesn't matter

The rule doesn't look at how many features an area has overall. In the
Nevada desert, one gas station for many kilometers is normal; in a city
center, it isn't. But the share compares two counts of the same thing:
OSM features of one brand. In a city, a candidate may have 40 Shell
stations nearby, 30 of them matched: share 0.75, kept. In the desert,
the window may hold 3, with 2 matched: share 0.67, kept too. In both
cases, the evidence is that ATP covers the brand there. Density cancels
out.

Density does affect sample size. In an empty area, the window may hold
only one or two features, and the share gets noisy:

- No matched neighbour at all: share 0, so the candidate is dropped.
  That is the safe direction. A missed deletion costs little, while a
  false one costs a mapper's time.
- One matched, one candidate: share 0.5, so the candidate is kept on
  thin evidence. The log line counts these as `thin_local_evidence`:
  kept candidates with fewer than three other OSM features of the brand
  in the window. In the eval, they are 3% (Berlin) to 26% (UAE) of the
  kept candidates. If that turns out to be a problem, we can
  require a minimum count.

## Alternatives considered

- **A fixed radius.** On the planet run, the share rule with 25 km left
  120,972 candidates; the adaptive radius leaves 129,242. A fixed
  radius is too small for sparse brands like car dealers, which then
  never see a matched neighbour.
- **The k nearest OSM features of the brand**, which adapts to local
  density. With k = 10 it left 153,565 candidates on the planet: more,
  because it reaches far for sparse brands. It is also harder to build,
  with a spatial index per brand, so we kept the simpler rule.
- **Spacing per brand, not per spider and brand.** One radius per
  Wikidata ID mixes feeds of different density: McDonald's in Germany
  is much denser than in Australia, and each has its own spider.
- **Spacing per spider only.** It comes out too small for spiders that
  emit several brands, such as one spider for both KFC and Taco Bell,
  since their points interleave.
- **Computing spacing while reading the GeoJSON.** There, each spider's
  features come separately, which would make it cheap. But the
  per-spider positions take only a few MB even for large regions (see
  below). Computing them in `conflate`, from the ATP features it reads
  anyway, needs no extra file on the side, nor the code to rebuild that
  file when `alltheplaces.parquet` is reused.
- **A memory-mapped file sorted by S2 cell**, instead of a hash map,
  for the local counts. Not needed so far: the map is small (see
  below). If memory gets tight, it can still be sorted by (cell, brand)
  and mapped.

## Eval

Same ten regions as in
[`2026-11-06-eval-regions.md`](2026-11-06-eval-regions.md): main
(0adf9ef) against this branch. The counts come from the log line
`conflate.deletion_candidates: done`. The OSM-only rows are those
inside the region's polygon, and the Swiss transit rows don't change.

| region | `wikidata` | `name` | OSM-only rows | thin | matched rows main / branch |
|---|---:|---:|---:|---:|---:|
| Switzerland | 4,340 → 588 | 867 → 204 | 6,608 → 2,193 | 71 | 63,950 / 63,950 |
| Berlin | 630 → 358 | 43 → 8 | 673 → 366 | 10 | 5,951 / 5,951 |
| Massachusetts | 1,941 → 727 | 33 → 6 | 1,974 → 733 | 71 | 7,150 / 7,150 |
| Japan | 21,321 → 5,683 | 1,294 → 434 | 22,615 → 6,117 | 312 | 61,258 / 61,258 |
| Brazil Sudeste | 3,087 → 1,104 | 677 → 124 | 3,764 → 1,228 | 96 | 4,895 / 4,895 |
| Karnataka | 423 → 249 | 43 → 16 | 466 → 265 | 18 | 1,203 / 1,203 |
| Tamil Nadu | 491 → 78 | 34 → 5 | 525 → 83 | 21 | 551 / 551 |
| Thailand | 752 → 414 | 175 → 52 | 927 → 466 | 92 | 4,043 / 4,043 |
| Kenya | 118 → 86 | 22 → 5 | 140 → 91 | 13 | 455 / 455 |
| UAE | 147 → 72 | 7 → 1 | 154 → 73 | 19 | 440 / 440 |

No OSM-only row appears that main didn't have.

What was dropped, checked by hand for the biggest brands:

- **Switzerland:** 3,939 Swiss Post post boxes (ATP has 31). 144 SOCAR
  fuel stations, which ATP only has from the `maes_dkv` fuel card
  spider (15 stations). 9 Coiff&Co hairdressers, from a single ATP
  feature. Medbase went from 32 candidates to 1: ATP has 53 Medbase
  locations, 29 of them matched. Brands that ATP covers well keep their
  candidates: Migros 31 → 31, Denner 26 → 26, Avia 43 → 43, k kiosk 41
  → 41.
- **Berlin:** all 98 REWE (ATP has 79 of about 160 in Berlin, 62
  matched), 22 Commerzbank (8 matched), and 31 of 32 DHL Express. Kept:
  Netto 56 → 56, Deutsche Post 77 → 77 (2,131 matched).
- **Massachusetts:** all 361 CVS, 95 Burger King and 89 Wendy's, none of
  which match anywhere in the state. Walgreens went from 98 to 14
  (89 matched). Kept: Bank of America 45 → 45, Dunkin' 43 → 43.

Planet estimate, from the 2026-11-02 output: 331,542 → about 129,000
`brand:wikidata` candidates. Royal Mail went from 16,226 to 1,336,
Canada Post from 10,433 to 734, and Burger King US from 6,138 to 16.
Dollar General (3,056 → 2,458) and Subway (2,582 → 2,121) barely
changed: ATP covers them well.

## Memory and time

Production logs the sizes, in bytes, so we can see how they grow:

- `conflate.coverage: done`: `spacing_bytes`, the ATP positions by
  spider and brand, freed once the radii are built; `coverage_bytes`,
  all of `AtpCoverage` after that; and `radii`.
- `conflate.deletion_candidates: done`: `local_coverage_bytes` and
  `local_coverage_entries`, the counts by brand and S2 level-9 cell
  (about 18 km).
- `conflate.match: done`: `exclusive_bytes`, the candidates kept for
  exclusive matching.

In Massachusetts, where conflate reads all US ATP features, the
positions take 12.8 MB and the local counts 147 KB. For the planet, the
2026-11-02 output has about 1.2 million distinct pairs of brand and
level-9 cell, matched or not. The local counts only hold the brands of
candidates, so that is an upper bound: at 41 bytes per slot, at most
about 90 MB. The positions should take 50 to 100 MB on the planet, and
they are freed once the first matching pass ends. The deletion pass
takes under 0.6 s in every eval region, about 10% more than before.
