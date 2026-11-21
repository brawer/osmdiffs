# Deletion candidates: compare the main category, not only the match mask

Investigation note behind the category check in
[`conflate/deletion.rs`](../../src/pipeline/conflate/deletion.rs) and the
shared [`matchers/category.rs`](../../src/matchers/category.rs). It came
out of the review of the production run of 2026-11-16.

## Short answer

An unmatched OSM feature with a brand became a deletion candidate if
ATP had features of that brand, in that country, whose *match mask*
intersected its own. The mask is coarse. `SHOP` covers supermarkets,
pharmacies, garden centres and car repair alike. So a spider that lists
only a brand's supermarkets made the brand's pharmacies, garden centres
and auto centres candidates too. In the same way, a fuel spider made the
shops at the stations candidates, and their chargers too.

Now the main category has to agree as well. It is either equal, or one
of the pairs that one business often has on the two sides, such as
`amenity=pharmacy` and `shop=chemist`. Those pairs are the ones the
contact matcher already uses, moved to `matchers/category.rs`. On the
2026-11-16 output, this removes **9,792 of 178,989 candidates (5.5%)**.
For **69%** of the removed ones, ATP has a feature of the same brand
within 100 m. Among the candidates that stay, it is 12%.

## Why the mask isn't enough

Walmart is the clearest case. In the 2026-11-16 run, `walmart_us` came
back after a week with no features, with 3,779 features, all
`shop=supermarket`. Walmart's deletion candidates went from 89 to 1,721:

| OSM main category | candidates |
|---|---:|
| `amenity=pharmacy` | 630 |
| `shop=supermarket` | 385 |
| `shop=garden_centre` | 375 |
| `shop=department_store` | 212 |
| `shop=car_repair` | 56 |
| `shop=optician` | 44 |

OSM maps Walmart Pharmacy, the Garden Center, Auto Care Center and
Vision Center as features of their own, next to or inside the store.
ATP doesn't list them, so they can't be matched, and they say nothing
about whether the store still exists.

## The rule

For each brand, network or operator and country, `AtpCoverage` now
keeps the distinct main categories (`main_category`, such as
`shop=supermarket`) of the ATP features, next to the union of their
masks. An OSM feature is covered if the masks intersect, as before,
*and* its main category agrees with one of those categories, by
`categories_agree`. A category agrees if it is equal or one of the
pairs in `category_pairs.tsv`. Some cases are judged by the mask alone,
as before:

- an OSM feature without a main category, such as a
  `public_transport=station` with an operator;
- a brand whose ATP features have no main category at all;
- an OSM `shop=vacant` that still has the brand. It is gone, whatever
  ATP lists, so it stays a candidate.

Street infrastructure (post boxes, bike-share stations, chargers that
are street furniture) already had its own finer kind and is unchanged.

## The category pairs

`category_pairs.tsv` lists pairs of main categories that one business
often has in ATP and OSM. It deliberately leaves out pairs that are two
features at one site, such as a fuel station and the shop at it, or a
Western Union counter inside a supermarket. That is the line both uses
need. The matcher shouldn't match a station's shop to the station by
its phone number, and the station shouldn't keep its shop from being a
candidate, nor make it one. So the table, `main_category` and
`category_factor` moved from `matchers/contact` to `matchers/category`,
and deletion uses the same function the matcher does.

### Pairs from `brand:wikidata` matches

The table so far came from contact matches on a planet run without a
category check, built on an experimental branch (see
[`2026-10-25-phone-and-email-matching.md`](2026-10-25-phone-and-email-matching.md)).
Matches by equal `brand:wikidata` ignore the category, so any
production run gives a second, unbiased source.
`scripts/infer_contact_category_pairs.py` now takes
`--evidence=brand_wikidata` for this. It counts the category pairs of
such matches up to 25 m apart (`--max-distance`):

```
uv run scripts/infer_contact_category_pairs.py conflated-20261116-aae5eb64.parquet \
    --evidence=brand_wikidata --min-name-agree 20 > new_pairs.tsv 2> dropped.txt
```

The name check filters hardly anything here. Both sides carry the
brand's name, so names agree in 91% of the matches even when the
categories are equal, and in 95% for a pharmacy matched to the
supermarket it is in. Of the 88 pairs the script kept, 23 were new
and are added, and 15 are listed in the file as removed by hand, with
the reason. The others were in the table already, or had too few
matches to judge.

| added | matches | why one business |
|---|---:|---|
| `shop=mobile_phone` ~ `shop=telecommunication` | 1,050 | Orange, Vodafone, SFR, Movistar |
| `shop=department_store` ~ `shop=supermarket` | 272 | Target, Walmart |
| `amenity=fast_food` ~ `shop=bakery` | 257 | Greggs, Brezelkönig |
| `shop=supermarket` ~ `shop=variety_store` | 211 | Dollar General |
| `amenity=cafe` ~ `shop=coffee` | 201 | Starbucks, Costa |
| `shop=electrical` ~ `shop=electronics` | 180 | |
| `shop=convenience` ~ `shop=newsagent` | 180 | |
| `amenity=fast_food` ~ `shop=deli` | 95 | |
| 15 more, 20–82 matches each | | `amenity=bar` ~ `amenity=pub`, `shop=locksmith` ~ `shop=shoe_repair` (Timpson), `amenity=ice_cream` ~ `shop=ice_cream`, … |

| removed by hand | matches | why not |
|---|---:|---|
| `shop=convenience` ~ `shop=yes` | 2,091 | the station shop; ATP gives the fuel station `shop=yes` |
| `amenity=fuel` ~ `shop=convenience` | 2,051 | the station shop |
| `amenity=fuel` ~ `shop=gas` | 813 | gas cylinder sales at the station |
| `amenity=fuel` ~ `shop=kiosk` | 533 | the station shop |
| `amenity=post_office` ~ `shop=copyshop` | 183 | The UPS Store and FedEx, one business; but in the contact review, postal counters inside copy shops (see Losses) |
| `amenity=charging_station` ~ `amenity=fuel` | 157 | the station's charger |
| `amenity=pharmacy` ~ `shop=supermarket` | 99 | pharmacy counter inside the supermarket |
| 8 more | | butcher counters, concessions, McCafé, … |

The new pairs also reach the matcher. A contact, brand-name or
address match between them now counts at `CATEGORY_PAIR_FACTOR`
instead of not at all. I didn't run the eval regions for this. Since
these pairs come from `brand:wikidata` matches that agree, I expect
few changes, but the next production run will show them.

## Eval

**Method.** A throwaway test, not checked in, fed the 4,288,283 ATP rows
of `conflated-20261116-aae5eb64.parquet` (every row that isn't a
deletion candidate) through `AtpCoverage::add`. These are the ATP
features that count for coverage. The test then called
`deletion_reason` on each of the 178,989 candidates of that run, with
the code of this change. Local coverage wasn't applied again: it can
only remove more, and all of these candidates had already passed it.

**Result.**

| reason | candidates | removed |
|---|---:|---:|
| `wikidata` | 165,734 | 8,709 (5.3%) |
| `name` | 11,879 | 1,083 (9.1%) |
| `swiss_transit_id`, `swiss_transit_stop` | 1,376 | 0 |
| **total** | **178,989** | **9,792 (5.5%)** |

The removed candidates by OSM main category, with examples:

| OSM main category | removed | brands |
|---|---:|---|
| `shop=convenience` | 3,914 | Shell, Phillips 66, Conoco, Kwik Trip, BP, Esso (station shops) |
| `amenity=charging_station` | 2,226 | Flying J, Repsol, Leclerc, Eni, Shell (chargers at stations) |
| `amenity=pharmacy` | 774 | Walmart, Sam's Club, Wegmans, Tops, Woolworths |
| `shop=gas` | 420 | Shell, Repsol, Carrefour, Total |
| `shop=garden_centre` | 409 | Walmart, Canadian Tire, OBI, B&Q, Castorama |
| `shop=kiosk` | 192 | Bank of America, BP, Circle K |
| `shop=car_repair` | 151 | Walmart Auto Care Center, Canadian Tire, Shell |
| `shop=butcher` | 134 | SuperBrugsen Slagter, føtex Slagter (butcher counters) |
| `amenity=post_office` | 113 | PostNL, Postnord, Hermes (counters in shops), The UPS Store |
| `leisure=playground` | 78 | IKEA |
| `shop=optician` | 65 | Walmart Vision Center, JCPenney |

**Precision check.** If a removed candidate is a department or a
station shop, the host itself should be in ATP right next to it. In a
sample of 3,000 removed `wikidata` candidates, **69.0%** have an ATP
feature with the same `brand:wikidata` within 100 m. Among 3,000
candidates that stay, it is **11.8%**. So the removed ones mostly sit
at a place of the brand that ATP does list.

**Losses.** The UPS Store is `shop=copyshop` in ATP and often
`amenity=post_office` in OSM, so about 100 such stores are no longer
candidates. Adding that pair would also let postal counters in
convenience stores and supermarkets match by phone, which the contact
review rejected. I left it out.

## Not done

- **Country in the eval.** The eval uses the same country lookup as the
  pipeline, but a single run. Week-to-week effects, for example a spider
  coming back as `walmart_us` did, will only show in the next
  production runs.
- **Matched OSM features don't count by category for local coverage.**
  `local_keys` still goes by brand and infrastructure kind. A store
  brand's matched supermarkets therefore still count as local coverage
  for its unmatched pharmacies. That only matters for candidates that
  the category check lets through, and those have an agreeing category.
