# Parcel lockers

In the AllThePlaces run of 2026-11-14, 209,741 features in 58 spiders are `amenity=parcel_locker`. No matcher took them,
so spider-stats counted them as `no_matcher`
([brawer/osmdiffs#1015](https://github.com/brawer/osmdiffs/issues/1015)). The largest spiders are
`paczkomat_inpost_pl` (27,513), `deutsche_post_de` (17,589), `inpost_gb` (16,030), `amazon_locker_us` (14,101),
`allegro_one_box_pl` (10,000), `dhl_pl` (9,682), `czech_post_cz` (9,039) and `orlen_paczka_pl` (8,937).

## How far the OSM locker is

For each ATP locker, I looked up the nearest OSM `amenity=parcel_locker` of the same brand, and for comparison the
nearest one after moving the ATP locker 2 km in a random direction ("chance"). Two lockers are of the same brand when
they share a `brand:wikidata` or a brand word, or one of them has no brand ([The rules](#the-rules)).

Most of the 10 [eval regions](2026-11-06-eval-regions.md) have no ATP lockers at all: ATP has none in Switzerland,
Brazil, Thailand, Kenya, the UAE or India. So I also took six extracts where the large spiders are: Mazowieckie,
Prague, South Moravia, Greater London, Berlin and Massachusetts (openstreetmap.fr, 2026-11-22, except Berlin and
Massachusetts, which are the eval extracts of 2026-11-04 and 2026-11-05). ATP lockers count when they are in the
region's bounding box, or within 3 km of an OSM locker for the openstreetmap.fr extracts, which have no bounding box.

Share of ATP lockers by distance to the nearest OSM locker of the same brand:

| spider, region | ATP | OSM same brand | < 5 m | 5–10 | 10–20 | 20–30 | 30–50 | 50–100 | > 100 m | chance ≤ 30 m |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| paczkomat_inpost_pl, Mazowieckie | 3,890 | 4,034 | 60% | 19% | 10% | 4% | 3% | 1% | 2% | 0% |
| allegro_one_box_pl, Mazowieckie | 1,609 | 539 | 14% | 4% | 3% | 1% | 1% | 1% | 75% | 0% |
| dhl_pl, Mazowieckie | 1,442 | 330 | 14% | 5% | 2% | 0% | 0% | 0% | 78% | 0% |
| packeta, Prague | 937 | 862 | 62% | 14% | 10% | 2% | 1% | 0% | 10% | 0% |
| packeta, South Moravia | 773 | 783 | 74% | 13% | 7% | 3% | 1% | 1% | 1% | 0% |
| czech_post_cz, Prague | 1,567 | 491 | 2% | 6% | 15% | 9% | 10% | 7% | 50% | 0% |
| inpost_gb, London | 1,272 | 219 | 5% | 4% | 2% | 1% | 1% | 2% | 85% | 0% |
| deutsche_post_de, Berlin | 1,006 | 603 | 30% | 14% | 7% | 2% | 2% | 2% | 43% | 0% |
| amazon_locker_at_cz_de, Berlin | 464 | 239 | 14% | 9% | 6% | 3% | 2% | 1% | 65% | 0% |
| myflexbox, Berlin | 202 | 85 | 17% | 4% | 1% | 2% | 1% | 0% | 74% | 0% |
| amazon_locker_us, Massachusetts | 507 | 26 | 1% | 1% | 0% | 0% | 0% | 0% | 98% | 0% |
| pudostation_jp, Japan | 7,356 | 291 | 0% | 1% | 1% | 1% | 0% | 0% | 97% | 0% |
| amazon_locker_jp, Japan | 1,668 | 325 | 1% | 1% | 1% | 1% | 0% | 0% | 96% | 0% |

Where OSM has the locker, it is almost always within 30 m: of the ATP lockers with a same-brand OSM locker within
100 m, 90–100% have it within 30 m. By chance, under 0.5% of lockers find one within 30 m. Most lockers are simply not
in OSM, which has 105,000 parcel lockers worldwide (taginfo), half as many as ATP. Japan's OSM has 609, against 9,024
ATP lockers.

`czech_post_cz` is the exception. Its lockers are the Z-BOX, AlzaBox, OX Point and Penguin Box lockers where Czech Post
parcels can be picked up, without brand or operator, and with rougher positions than the networks' own. `packeta`
has the Z-BOXes again, with their brand.

### Refs

Many networks give each locker a public code, which OSM mappers copy into `ref`. Among the nearest same-brand pairs
within 30 m that have an OSM ref:

| spider | region | close pairs with OSM ref | ref agrees | how |
|---|---|---:|---:|---|
| paczkomat_inpost_pl | Mazowieckie | 3,602 | 99% | `WAW211AP` = `WAW211AP` |
| allegro_one_box_pl | Mazowieckie | 263 | 98% | `AL010ZST` = `AL010ZST` |
| inpost_gb | London | 7 | 86% | `UK00000146` = `UK00000146` |
| packeta | Prague, South Moravia | 1,584 | 100% | `I_23111` = `ref:packeta=23111` |
| deutsche_post_de | Berlin | 420 | 93% | `1-14482182` = `182`, the Packstation number |
| pudostation_jp | Japan | 33 | 82% | `PCJ00177` = `00177` or `PCJ00177` |
| amazon_locker_* | Berlin, Massachusetts, Japan | 85 | 88% | the nickname in `Amazon Locker - Nilias` = `nilias` |
| dhl_pl | Mazowieckie | 188 | 0% | another numbering in OSM |
| myflexbox | Berlin | 27 | 81% | `DE-30` = `30`, but also `#245` and `myflexbox #209 (berzx)` |

A ref twin can be farther than 30 m: for InPost in Mazowieckie, 90% of them are within 19 m, 99% within 55 m, and 4 of
3,766 beyond 100 m.

### Networks side by side

Several networks often stand at the same supermarket. In Mazowieckie, for 0.5–3% of the InPost, Allegro and DHL
lockers a locker of another network is nearer than the nearest one of their own; for Packeta in Prague 9.5%, since a
Z-BOX and an AlzaBox are often next to each other.

## The rules

`amenity=parcel_locker` is street furniture, and the infrastructure matcher has a new kind, `parcel_locker`:

- **Brands must agree.** Two lockers may match if they share a Wikidata ID (`brand:wikidata` or `operator:wikidata`)
  or a word of their `brand` (lowercase, at least three letters, without `box`, `hub`, `locker`, `parcel`, `pickup`,
  `point` or `station`), or if they have nothing to compare. A word is enough, since OSM has both `Amazon Locker` and
  `Amazon Hub`, each with its own Wikidata ID. A brand without such words stands for itself, so `Z-Box` is `zbox`.
  Operator IDs count because ATP gives PUDO lockers the operator's ID (Packcity Japan) as `brand:wikidata`, while OSM
  has `PUDOステーション` with its own ID. The operator's name only counts for a locker without `brand`, so that an OSM
  locker with just `operator=Amazon` isn't a PUDO station. Otherwise it says little: OSM gives 283 Allegro lockers in
  Mazowieckie `operator=UPS`.
- **Lockers without brand or operator get no matcher**, since they could be of any network. That leaves out
  `czech_post_cz`'s partner lockers, and the 2,503 myflexbox lockers that `dpd_de` and `general_logistics_systems_de` list
  without brand, which `myflexbox` already has.
- **`deutsche_post_de`'s Packstations count as `DHL Packstation`** (Q1766703) operated by DHL (Q489815), since ATP
  gives them only `operator=Deutsche Post`
  ([alltheplaces#20730](https://github.com/alltheplaces/alltheplaces/issues/20730)). Without this, a Packstation in
  Berlin took an Amazon Locker 22 m away. With DHL's ID, the smaller `DHL Paketbox` (Q2046604) matches too.
- **Within 30 m**, by distance alone (rule `kind`). `pudostation_jp` gets 75 m: of the 243 OSM PUDO lockers in Japan,
  49 have the nearest ATP one 30–75 m away, while moving the 7,356 ATP lockers 2 km brings only 3 that close.
- **The same ref** (rule `ref`) scores at least 0.9 and may be up to 100 m away, for the spiders whose refs OSM uses:
  `paczkomat_inpost_pl`, `allegro_one_box_pl`, `inpost_gb`, `packeta` (`ref:packeta`), `deutsche_post_de` (last three
  characters), `pudostation_jp` (without `PCJ`), `myflexbox` (without `DE-`) and the `amazon_locker_*` spiders (the
  nickname in `name`). Where OSM keeps the prefix, as in `PCJ02291`, it is dropped there too. A different
  ref doesn't rule out a match, since `dhl_pl` and OSM number lockers differently.
- **Located in a host.** An ATP locker with `located_in` or `located_in:wikidata` that matches no OSM locker becomes
  `located_in` its host, by the existing rule. `amazon_locker_us` has these tags on a third of its lockers, `inpost_gb`
  on all of them.
- **Deletion candidates.** An unmatched OSM locker of a brand that ATP covers becomes a deletion candidate, like other
  street furniture. `allegro_one_box_pl` doesn't count for this: its one map query stops at 10,000 lockers, and in
  Mazowieckie 26% of the Allegro lockers in OSM have none of its lockers within 100 m
  ([alltheplaces#20728](https://github.com/alltheplaces/alltheplaces/issues/20728)).

## Results in the eval regions

ATP of 2026-11-14 filtered by `addr:country`, with osmdiffs at 1e09380 against this change, each a full run up to
`conflated.parquet`.

| region | ATP lockers | `same` | `located_in` | deletion candidates | other rows changed |
|---|---:|---:|---:|---:|---:|
| Germany: Berlin | 22,764 in DE | +742 | +2 | +51 | 0 |
| US: Massachusetts | 14,101 in US | +8 | +71 | +1 locker, −3 hosts | 0 |
| Japan | 9,024 | +288 | 0 | +11 | 0 |
| Switzerland, Brazil Sudeste, Thailand, Kenya, UAE, Karnataka, Tamil Nadu | 0 | 0 | 0 | 0 | 0 |

ATP covers the whole country while Berlin and Massachusetts are extracts, so most ATP lockers there are
`missing_in_osm` simply because they lie outside the extract. Every run also has 615 `sdek` lockers that are new
`missing_in_osm` rows: the spider failed in the 2026-11-14 run, so the pipeline took them from an older run, which the
country filter didn't reach.

In Berlin, 538 matches are by ref (443 Packstation numbers, 73 Amazon nicknames, 22 myflexbox numbers), and 204 by
distance alone.
In Massachusetts, OSM has only 27 lockers, but 71 Amazon lockers are now `located_in` the 7-Eleven, Speedway, Sunoco,
Whole Foods or Family Dollar named in their `located_in` tag. Three of these hosts had been deletion candidates
before, and aren't any more. In Japan, 228 PUDO and 60 Amazon lockers match, 45 of them by ref.

Nothing changed for any ATP feature that isn't a locker.

## Precision

I checked a random sample of 78 new matches by hand: 40 in Berlin, 30 in Japan and all 8 in Massachusetts. All of
them are lockers of the same network, and where both sides have a ref, it agrees, so none looks wrong. Before the
fixes to the brand rule, the runs showed three kinds of wrong or missed matches, which the rules above now handle: a
Packstation taking an Amazon Locker, a PUDO station taking an OSM locker with only `operator=Amazon`, and PUDO
stations and DHL Paketboxes that their own ATP feature didn't take.

Of the 63 deletion candidates, 49 have no unmatched ATP locker within 100 m, so ATP no longer lists a locker there.
The other 14 are doubtful: in Berlin, six Packstations and four Amazon Lockers have an unmatched ATP locker 21–84 m
away, sometimes of another network; in Japan, four PUDO lockers have one 80–90 m away. Some of these are probably the
same locker with a rough position on one side.

## What we left out

- **`czech_post_cz`.** Its partner lockers need the network's brand, or should be dropped, since the networks' own
  spiders list them ([alltheplaces#20729](https://github.com/alltheplaces/alltheplaces/issues/20729)).
- **`dhl_pl` refs.** They aren't OSM's, so position alone matches them.
- **Networks that ATP lacks.** Of the 129 locker brands in the name-suggestion-index, ATP has lockers for 40. The
  others, such as Ozon Box, Nova Poshta, AlzaBox, DPD and GLS, are in
  [alltheplaces#20731](https://github.com/alltheplaces/alltheplaces/issues/20731).
- **Old tagging.** A few OSM lockers, 4 of 5,881 in Mazowieckie, are still `amenity=vending_machine` with `vending=parcel_pickup`. They
  don't match.
