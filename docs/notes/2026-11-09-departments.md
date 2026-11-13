# Departments and `located_in`: keeping stores off their host

This change is about AllThePlaces (ATP) features that are inside
another business. Such a feature must not match the OpenStreetMap
(OSM) feature of its host, or the edits for it would put the pharmacy's
opening hours on the supermarket. When OSM has no feature of its own
for it, its row in `conflated.parquet` now gets the new
`match.relation = located_in`, with the host's OSM feature in `osm`.
That tells mappers where to add it, and no edits are made from it.

## What we mean by "department"

A department is a service that a large store has inside, under its own
brand. ATP often lists it as a separate feature, from the same spider
and at about the same spot as the store:

- **Pharmacies in supermarkets.** The `publix` spider emits each Publix
  supermarket, its Publix Pharmacy and its Publix Liquors. The
  `albertsons` spider does the same for Safeway, Shaw's, Star Market
  and other chains. In Massachusetts, OSM has a Shaw's supermarket at
  41.562° N, 70.594° W (`node/4365977048`), and ATP has both the
  supermarket and Shaw's Pharmacy, 21 m apart.
- **Restaurants and bistros in furniture stores.** The `ikea` spider
  lists IKEA Restaurant and IKEA Bistro next to each store.
- Also travel agencies, car rentals, opticians, cafés and bakeries in
  supermarkets, hypermarkets and department stores.

The second kind is a feature that ATP itself marks with `located_in`
or `located_in:wikidata`, such as `starbucks_us` for its cafés in
Target stores, `eathappy` for its sushi counters in REWE and EDEKA,
`ptt_th` for its Jiffy shops at PTT fuel stations, or `petit_bateau`
for its corners in Manor and Isetan.

Not departments: the post box outside a post office (a separate
object), the shop of a fuel station or the workshop of a car dealer
(facets of one site, where OSM often has one feature), and synonyms
such as `amenity=pharmacy` against `shop=chemist`. Those stay `same`.

## How it works

Code: `src/pipeline/conflate/located_in.rs`, used by `produce_rows`
in `src/pipeline/conflate/mod.rs`.

1. **Find departments in ATP.** An ATP feature in a department
   category (pharmacy, alcohol, cafe, fast food, restaurant, travel
   agency, car rental, optician, pet, outpost, bakery) is a department
   if an ATP feature with the same `brand:wikidata` in a host category
   (supermarket, hypermarket, department store, furniture, wholesale)
   is within 75 m. Spiders often put a department at the entrance and
   the store at its center, so the distance has to allow for large
   buildings. It is also a department if its position lies inside an
   OSM feature of a host category and of its own `brand:wikidata`,
   however far the ATP store is. In the production run of 2026-11-10,
   six pharmacies of `albertsons` and `safeway` were 77 to 235 m from
   their ATP supermarket but inside its OSM building, and matched the
   supermarket ([#930](https://github.com/brawer/osmdiffs/issues/930)).
2. **Keep it off the host.** A department doesn't match OSM features in
   a host category, nor those in another department category: the
   Publix pharmacy must not take the Publix Liquors either. Café, fast
   food, restaurant and bakery count as one, because OSM often tags
   IKEA Bistro as `amenity=restaurant`. A feature with ATP's
   `located_in` doesn't match its host.
3. **Match its own feature.** Matching otherwise runs as before, so a
   pharmacy matches the OSM pharmacy of its brand if there is one.
4. **Point to the host.** If nothing matched, the row gets
   `match.relation = located_in` and the nearest host within 300 m:
   for a department, the OSM feature of the host's category and brand;
   for `located_in`, a feature with the `located_in:wikidata` brand,
   or one with a matching `brand` or `name` and a store category,
   such as supermarket, convenience store, mall, fuel station or
   pharmacy (`NAMED_HOST_CATEGORIES`). Never a feature of the ATP
   feature's own main category. (Until #933, the radius was 150 m, and
   a store with another `brand:wikidata` was no host; see below.)
   `match.evidence.rule` is `department` or `located_in`.

Names in `located_in` often include the merchant, such as "REWE Ahmed"
or "EDEKA Brehm", while OSM calls the store "REWE". So a name matches
if it is the other name followed by more words. Matching names
exactly, the first version found 83 hosts for ATP features in Berlin;
with this rule, 121.

At first, a host found by name could be of any category. In the
planet run of 2026-11-10, 189 of the 16,653 `located_in` rows had a
host that was clearly no store, such as "Tesco Customer Toilets"
(`amenity=toilets`) or "Waitrose car park" (`amenity=parking`) for a
Timpson; see [#929](https://github.com/brawer/osmdiffs/issues/929).
48 of the car parks had exactly the store's name, so a stricter name
rule wouldn't have helped. Now such a host must be a store. A host
found by `located_in:wikidata` may still be of any category, since a
brand's Wikidata ID rarely sits on a car park.

## Alternatives considered

- **A penalty for a different category.** In OSMDiffs'
  `conflated.parquet` of 2026-11-02 (OSM planet of 2026-11-02, ATP run
  of 2026-10-31), 118,931 matched pairs share `brand:wikidata` but
  differ in main category, against 986,410 that agree. Most of them
  are vague ATP categories (`shop=yes` from fuel card spiders), facets
  of one site or synonyms. Only a few thousand are departments. A
  penalty would have demoted about 100,000 good matches to fix them.
- **A list of department spiders or brands.** It would need upkeep, and
  the same spider emits both the department and the host. Finding the
  pair in ATP, by brand and distance, needs no list and works for new
  spiders.
- **A department of any category next to a host of the same brand.**
  This would catch synonym and facet pairs too, such as a car dealer
  and its workshop, which OSM often maps as one feature. The two short
  category lists keep the rule to cases we have seen.
- **Excluding every other department category.** The first version did
  this, and IKEA Bistro (`amenity=fast_food` in ATP) lost its match to
  OSM's IKEA Restaurant (`amenity=restaurant`) in Switzerland, Japan
  and the UAE. Food categories now count as one.
- **Name signals for stockists and agents**, such as Western Union in
  other shops, or `soeder_ch` products in hotels. They are too noisy to
  decide on their own (see the store-in-store analysis), so they are
  left for later.
- **Distance to the host by Hausdorff distance.** From a point to a
  building, that is the distance to its farthest corner, so a counter
  in a big store would seem far away. We use the distance between the
  ATP point and the host's centroid for picking the host, and
  `match.evidence.distance_m` stays the shortest distance, 0 inside the
  building.

## Eval

Same ten regions as in
[`2026-11-06-eval-regions.md`](2026-11-06-eval-regions.md): main
(8a822da) against this branch. ATP: the run of 2026-10-31
(`2026-10-31-13-32-34`). OSM: extracts of 2026-11-04 or 2026-11-05
(Geofabrik for Berlin, Thailand and Massachusetts; openstreetmap.fr
for the others), and for Switzerland the Geofabrik extract of
2026-10-20. Rows are those inside the region's polygon; conflate reads
all ATP features of the country, so departments are counted
countrywide.

| region | departments | `located_in` rows: department / ATP tag | `same` → `located_in` | `same` main / branch |
|---|---:|---:|---:|---:|
| Switzerland | 21 | 15 / 5 | 0 | 63,948 / 63,948 |
| Berlin | 142 | 2 / 121 | 0 | 5,949 / 5,949 |
| Massachusetts | 3,897 | 16 / 23 | 16 | 7,138 / 7,122 |
| Japan | 23 | 20 / 11 | 0 | 61,258 / 61,258 |
| Brazil Sudeste | 16 | 0 / 0 | 0 | 4,893 / 4,893 |
| Karnataka | 8 | 2 / 0 | 0 | 1,203 / 1,203 |
| Tamil Nadu | 8 | 0 / 0 | 0 | 551 / 551 |
| Thailand | 12 | 10 / 33 | 0 | 4,043 / 4,043 |
| Kenya | 0 | 0 / 0 | 0 | 455 / 455 |
| UAE | 7 | 1 / 4 | 0 | 440 / 440 |

All other `located_in` rows were `missing_in_osm` on main. No `same`
row changed in any region except these in Massachusetts:

- 16 Shaw's and Star Market pharmacies (`albertsons`) matched the
  supermarket on main. They are now `located_in` that supermarket.
- One Star Market Pharmacy now matches its own OSM pharmacy,
  `node/12291481546`, 31 m away, instead of the supermarket.

Checked by hand:

- **Berlin:** 104 `eathappy` sushi counters in REWE, EDEKA and HIT
  supermarkets, 11 `yuzu_de` counters in Kaufland, and 3 `sushi_daily`.
  Three `wolle_rodel_de` yarn corners, such as in idee. craft stores. IKEA
  Restaurant and Bistro in Spandau.
- **Massachusetts:** 21 Starbucks in Target, a Peet's Coffee at the
  Massachusetts College of Art and Design, and a See's Candies pop-up
  in the Natick Mall.
- **Thailand:** 33 Jiffy shops at PTT fuel stations, whether OSM calls
  them "PTT", "ป.ต.ท." or "ปตท".
- **Switzerland and Japan:** Petit Bateau in Manor, Globus, Isetan,
  Takashimaya and Daimaru, and IKEA restaurants and bistros.

In most regions, few ATP features change, because the brands with many
departments are in the US South and West (Publix, Albertsons, Safeway).
In OSMDiffs' `conflated.parquet` of 2026-11-02, 7,587 ATP features
matched their host's OSM feature instead of their own, such as 1,294
Publix pharmacies, and 15,500 of the 19,958 ATP features with
`located_in` were unmatched. Those are the rows this change affects.

## Hosts not found (2026-11-13)

In the planet run of 2026-11-10 (v0.8.13), 3,830 ATP features with
`located_in` found no host within 150 m and stayed `missing_in_osm`
([#933](https://github.com/brawer/osmdiffs/issues/933)). To find out
why, we re-implemented the host rules (with the #929 rule above) in
Python and ran them on the ten spiders with the most such features.
ATP was the run of 2026-10-31. OSM was openstreetmap.fr extracts of
2026-11-13 for Denmark, Austria, South East England, South Australia,
Victoria, California, Colorado, Île-de-France and Lombardia. We
looked 1 km around each feature without a host. Four causes stood
out:

1. **ATP and OSM disagree on the host's Wikidata ID.** For 126 of
   the 187 `starbucks_us` features without a host, a supermarket
   named "Safeway" stood within 150 m. ATP gives Safeway Canada
   (`Q17111901`) for all of its 626 US Safeway locations, while OSM
   and NSI use Safeway USA (`Q1508234`). Timpson is `located_in`
   Tesco (`Q487494`), but OSM tags many of the stores Tesco Express
   (`Q98456772`), which NSI keeps as its own brand. Because the OSM
   store had a `brand:wikidata`, the name was never compared.
2. **The host is mapped as a pharmacy.** For 183 of the 329 `fedex`
   features without a host, the host was a Walgreens tagged
   `amenity=pharmacy`, which wasn't a named-host category.
3. **The host is more than 150 m away.** Large stores have their OSM
   centroid far from the spot that ATP gives. In a sample of 50 per
   spider, 16 of 23 far hosts of `sushi_daily` lay within 300 m, 11
   of 13 for `starbucks_us`, and 13 of 25 for `timpson_group`. The
   far hosts of `eathappy` had a median of 417 m. The ones we looked
   at were the right chain at a geocoded address, or another chain.
4. **`located_in` isn't a name that OSM uses.** `dao_dk` gives the
   merchant ("Brugsen Hornum", which OSM calls "Dagli'Brugsen").
   `sushi_daily` sometimes gives "Deliveroo", or "Finiper" for Iper.
   `sees_candies` gives mall names that OSM often doesn't have, and
   `shell_au` puts OTR shops in "Shell" at stations that OSM brands
   OTR. These features are right to stay `missing_in_osm`.

So now:

- A store found by name is a host even if its `brand:wikidata`
  differs from `located_in:wikidata`. It must still be of a named-host
  category, so the #929 car parks stay out. We considered keeping a
  list of equivalent Wikidata IDs, but it would need upkeep for each
  brand family, such as Tesco and Tesco Express, or Aldi Süd and Aldi
  Suisse. The name already says what the ID would.
- `amenity=pharmacy` is a named-host category.
- The host may be up to 300 m away, for departments too. The nearest
  host still wins, so a second branch farther away doesn't matter.
- A feature of the ATP feature's own main category is never its host.
  `tesco_gb` sets `located_in` Tesco on its own pharmacies and fuel
  stations, which must still match their OSM features, and not be
  kept off them as their host.

On the same extracts, the features without a host went from 1,757 to
1,226:

| spider | without host, before | after |
|---|---:|---:|
| starbucks_us | 187 | 20 |
| fedex | 329 | 143 |
| timpson_group | 187 | 104 |
| sushi_daily | 110 | 76 |
| eathappy | 180 | 141 |
| dao_dk | 624 | 613 |
| sees_candies | 64 | 59 |
| shell_au | 58 | 52 |
| cinnabon_us | 14 | 13 |

The script, the counts and a sample of 50 features per spider are in
the project's `located-in-hosts` folder, not in this repository.

## Not yet

- The map tiles still draw `located_in` rows as matched.
- ATP features in stockist and agent spiders, such as `western_union`,
  have no `located_in` and are not departments. They stay
  `missing_in_osm` for now.
