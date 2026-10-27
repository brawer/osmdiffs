# Merging ranks into one importance score

Design note behind the `importance` column of `conflated.parquet`
([#713](https://github.com/brawer/osmdiffs/issues/713)), computed in
[`src/pipeline/importance.rs`](../../src/pipeline/importance.rs). The
goal was one number from 0 to 1, so that people can work through the
file in priority order, and tiles can show the most important places
first at coarse zooms.

## Short answer

Three signals go in. Each is turned into a value from 0 to 1 that
means the same thing: the share of comparable places that rank
below this one. Their weighted mean is the place's score. The
published `importance` is the score's quantile among all rows.

| Signal | Source | Weight |
|---|---|---|
| Location | OSMViews rank of the place's location | 0.4 |
| Brand | QRank of its `brand:`, `operator:` or `network:wikidata` item | 0.3 |
| Kind of place | `MatchMask::importance()` | 0.3 |

A place without a brand drops the brand term, and the other two
weights are scaled up to fill its share.

## Why quantiles, not raw values

The raw signals live on very different scales, so averaging them
directly would let one of them decide everything:

- **OSMViews** `rank()` runs from 0 to 1, but inhabited places bunch
  up in a narrow band. In Switzerland, 80% of ATP locations lie
  between 0.40 and 0.61. Averaged as is, location would barely move
  the score.
- **QRank** is a pageview count with a heavy tail: a few global brands
  have millions of views, and most have a few hundred.

Quantiles fix both. Each signal becomes "what share of the reference
population ranks below this", which spans 0 to 1 evenly. Ties use the
mid-rank, `(less + same / 2) / n`, so many equal values share one
quantile in the middle of their run, not at its bottom.

The reference population matters as much as the quantile:

- **Locations** are ranked against every AllThePlaces location
  (`compute_osmviews_ranks`). Ranking them against all map pixels,
  most of which are ocean or desert, would put every shop near the top.
- **Brands** are ranked against every `brand:`, `operator:` and
  `network:wikidata` ID on ATP and OSM features (`collect_wikidata_ids`,
  `compute_qrank_quantiles`). Plain `wikidata` is left out, since on
  OSM it mostly points at cities and rivers.

IDs with no pageviews are left out of the brand population and count
as no brand. Many Name Suggestion Index items have no Wikipedia
article. The Shell item used on fuel stations, Q110716465, is one of
them, and before this rule it pushed every Shell station to the bottom
of the list.

## Why a missing brand drops out

Most places that matter have no Wikidata brand: a local hospital, an
independent bakery, a village pharmacy. Scoring a missing brand as 0
would rank them below every chain store. So the brand weight drops
out instead, and the score is the weighted mean of location and kind
of place alone.

## Kind of place

Location and brand cannot tell an AED from a flower bed when neither
has a brand and both sit on the same street. So the third signal is a
small hand-made score per category, derived from the `MatchMask` bits
the matcher already computes:

- 1.0 for anything with the IMPORTANT bit, such as AEDs, pharmacies,
  hospitals and police;
- 0.8 for schools and transit;
- 0.7 for shops, restaurants and lodging;
- 0.6 for fuel;
- 0.4 for parking;
- 0.3 when no category is known;
- 0.2 for street furniture;
- 0.1 for shrubbery.

We first tried deriving category scores from data: the median QRank
quantile of the brands in each `shop=`/`amenity=` value. That ranked
car dealers at 0.98 and pharmacies at the floor, because it measures
how famous a category's chains are, not how much the places matter.
It also could not help categories without brands at all, such as AEDs.
A short hand-made table, kept in code next to the matching
categories, turned out simpler and easier to reason about. It also
needs no separate data file in the repository.

## Weights

The weights were chosen with pair tests (`test_score_pairs`), each
saying which of two places should rank higher. Examples are "AED beats
flower bed at the same location", "famous brand beats obscure brand",
and "busy street beats quiet street". Our first try was 0.5 location,
0.3 brand and 0.2 kind. With those weights, a bench on a busy street
beat an AED on a quiet one. Moving 0.1 from location to kind of place
fixed that without breaking the other pairs.

## Why the final quantile

The weighted mean is itself unevenly spread. Taking its quantile
among all rows makes `importance >= 0.9` always mean "the top tenth".
Tiles rely on this: z0 shows the top 1%, and each zoom after that
shows four times as many. The price is that importance is relative:
the same place can move from week to week as other places come and go.

## Left out

- **OSM version count.** Frequently edited features may matter more,
  but unmatched ATP rows don't have a version, and the count also
  grows with bot edits and tag cleanups that say little about the
  place.
- **Evidence columns.** `conflated.parquet` carries only the final
  `importance`, not the three inputs, so the weights can change
  without a schema change.

## Results on Switzerland

On the Swiss data (21,808 rows), the top is downtown pharmacies,
followed by McDonald's and Starbucks. The bottom is rural fuel stations
and kiosks. Mean importance by kind:

| Kind | Mean importance |
|---|---|
| police | 0.87 |
| school | 0.83 |
| pharmacy | 0.78 |
| fast_food | 0.74 |
| supermarket | 0.52 |
| fuel | 0.41 |
| hotel | 0.38 |
| convenience | 0.25 |
| restaurant | 0.23 |

## How to evaluate changes

- Extend the pair tests whenever a ranking looks wrong.
- Read the top and bottom 50 rows of a country you know.
- Compare with signals that weren't used as inputs, such as OSM
  version counts or other place datasets' rankings.
- Check week-to-week stability: a place shouldn't jump by much unless
  its inputs changed.
- Once reviewers act on suggested edits, check whether acceptance rate
  rises with importance.
