# Shops inside shopping malls

## Background

When nothing in OpenStreetMap (OSM) matches an AllThePlaces (ATP) feature, its row in `conflated.parquet` is
`missing_in_osm`, and the edits step may propose a *create*. Some unmatched features are `located_in` another OSM
feature instead: a department in its store, a Starbucks in a Target, a postal counter in a kiosk, a charge point in
its station (`src/pipeline/conflate/located_in.rs`). Such a row has a host, which tells mappers where to add the place,
and the edits step puts the create inside the host.

Many ATP shops are in a shopping mall that OSM maps as `shop=mall`, often with the mall's name in the shop's name or
address (`ａｕショップ イオンモール羽生`, `NEW YORKER | Lyssach Center`). In the eval of
[#1050](2026-11-28-similar-place-words.md), the mall's name held these back as `similar_nearby`, or, once address
words stopped counting, let them through as plain creates. [#1067](https://github.com/brawer/osmdiffs/issues/1067)
asked for a `located_in` kind for malls.

## The change

A new `located_in` rule, `mall`. An ATP feature that nothing matched and that has no other host in OSM is `located_in`
the OSM `shop=mall` whose area contains its point, or a mall node within 30 m. The rule is only tried for a row that
would otherwise be `missing_in_osm`, so matching doesn't change at all. A mall's own outline must contain the point,
however large it is. The OSM index only knows an area by its centroid, so we look for mall centroids up to 500 m away.

These are left out:

- Features that aren't shops, restaurants, hotels or schools (by `MatchMask`): post boxes, parcel lockers, bike-share
  docks and transit stops, which often stand in a mall's car park or at its door. In the first run, 80 Swiss Post
  boxes, 33 `gbfs` docks and 5 bus stops in Switzerland alone became `located_in` a mall.
- Postal partners, whose host gets their `post_office` tags in the edits.
- A mall with the feature's `brand:wikidata`, or whose `name` or `brand` (or `name:*`, `brand:*`) begins with the
  feature's name or brand (`same_business_name`), so a store can't be in itself. OSM sometimes tags the store as the
  mall: the Migros VOI in Kappel SO is OSM `shop=mall` `name=VOI` with the store's phone and website, and Thailand's
  Big C buildings are malls named `Big C Supercenter` or `Big C Pathum Thani -บิ๊กซี ปทุมธานี`.
- A mall with another category that agrees with the feature's: in Massachusetts, OSM tags the Westford and Forge Village
  post offices `amenity=post_office` *and* `shop=mall`. Without this, the USPS features became creates next to them.

The mall is left out of `similar_osm`, as for any host, so its name no longer holds back the shop. A mall with tenants
is never a deletion candidate, but it doesn't count as matched for local coverage either, since no ATP feature of its
brand matched it. The first run marked hosted malls as matched like other hosts. In Japan, this made 14 unmatched AEON
features nearby deletion candidates, because the AEON-branded malls with tenants raised AEON's local share.

### Strict or containment only

The issue proposed two rules: containment only, or containment plus a distinctive word that the ATP `name`,
`addr:street_address`, `addr:full` or `addr:place` shares with the mall's names, not counting words like “Centre”,
“Shopping” or “Mall”. Over all 11 regions, 13,501 rows are `located_in` a mall by containment, and 6,069 (45%) would
also pass the strict rule (counted by `cmp1067.py`, which also looks for a mall name inside Thai and Japanese
addresses). The strict rule misses most chains whose name and address don't mention the mall: Sunglass Hut, Pandora,
Starbucks, Foot Locker, Pull & Bear.

I read 8 random rows that fail the strict rule in each region, 88 in all. Nearly all are shops that are in the mall:
`Pull & Bear` in Alexa, `MAC Cosmetics` in Sihlcity, `Bershka` in The Dubai Mall, `Zara Home` in centralwOrld,
`Tesla` in Les 4 Temps. A few are doubtful or wrong:

- `GiFi` in a building that OSM calls `Ochsner Sport / Let's Go Fitness` and tags `shop=mall`, which is arguably a small
  retail building with several shops;
- a Kenyan `MoneyGram` 0 m from a node `Naturalmente Pane` tagged `shop=mall`, likely a bakery;
- an `Adidas` whose ATP address says “Mantri Square mall” inside Bengaluru's `Nexus Vega City`, a position error in
  ATP.

76% of the rows (10,262) have a mall polygon with a `building` tag, so the point is inside the mall's building. Large
malls without one, such as Shopping Parque Dom Pedro (440,000 m², 18 rows), map the whole site, and the shops there
are still at that mall. So the change uses containment only.

## Results in the eval regions

Each run used ATP of 2026-11-14, filtered by `addr:country` (Switzerland with Liechtenstein), and ran the full
pipeline with osmdiffs at ea34b8f (main) and with this change. Fallbacks for broken spiders were turned off. The
extracts were the [eval regions](2026-11-06-eval-regions.md), Switzerland, Japan, and Île-de-France
(openstreetmap.fr, 2026-11-28). Only ATP features inside the region's polygon count.

| region | `missing_in_osm` rows | `located_in` mall | of which strict | malls | creates | new creates | lost creates | creates that got a host |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Japan | 204,011 → 196,417 | 7,594 | 3,590 | 1,558 | 55,829 → 55,892 | 64 | 1 | 3,104 |
| Thailand | 23,372 → 21,983 | 1,389 | 635 | 258 | 9,879 → 9,881 | 2 | 0 | 147 |
| Brazil: Sudeste | 16,226 → 14,824 | 1,402 | 419 | 364 | 3,193 → 3,198 | 5 | 0 | 21 |
| France: Île-de-France | 40,998 → 40,017 | 981 | 424 | 123 | 753 → 753 | 0 | 0 | 12 |
| United Arab Emirates | 4,449 → 3,804 | 645 | 406 | 103 | 161 → 161 | 0 | 0 | 25 |
| Switzerland | 73,768 → 73,187 | 581 | 192 | 210 | 1,380 → 1,382 | 2 | 0 | 48 |
| US: Massachusetts | 18,100 → 17,675 | 425 | 133 | 30 | 2,167 → 2,167 | 0 | 0 | 21 |
| Germany: Berlin | 5,319 → 5,139 | 180 | 76 | 55 | 171 → 172 | 1 | 0 | 5 |
| India: Karnataka | 6,176 → 6,004 | 172 | 118 | 44 | 2,808 → 2,808 | 0 | 0 | 21 |
| India: Tamil Nadu | 6,961 → 6,876 | 85 | 46 | 22 | 2,373 → 2,373 | 0 | 0 | 7 |
| Kenya | 2,078 → 2,031 | 47 | 30 | 28 | 228 → 228 | 0 | 0 | 0 |

No match, modify or deletion candidate changed in any region. All examples in the issue are now `located_in` their
mall, except Big C, which stays `missing_in_osm` because its mall is named after it (see above), and the Sparkasse
in the Allee-Center, which has three ATP features there: two matched OSM's Sparkasse nodes, and the third, the
`ImmobilienCenter`, is now `located_in` the mall.

Most `located_in` rows don't change the edits, since their create was held back anyway: by the spider's positions,
by a shared position, or by a nearby OSM feature of the same kind. Where a create was already made, it now has the mall
as its `host` (3,411 in all), and the create stays where ATP puts it inside the mall. The 74 new creates were held back
by the mall's name before: 56 au shops in Japanese malls (`イオンモール水戸内原`, `アリオ川口`), 5 Matsumoto Kiyoshi, 4
Cobasi pet shops in Brazil, two NEW YORKER in Switzerland.

The one lost create is right. `taito` lists two arcades in イオン旭川永山店, 0 m apart. Main held one back by the
mall's name, and the other became a create. Now both are candidates, so they count as twins, and neither is created.

### Île-de-France and the spider health gate

Île-de-France's edits look unchanged only because the comparison leaves out 11 spiders: Abercrombie & Fitch, Adopt,
Body Minute, Franck Provost, Hollister, Inditex, La Boutique du Coiffeur, Pandora, Vans, Victoria's Secret and Wycon.
The edits step leaves out a spider whose match rate dropped since the last run, and an eval run compares a
country-filtered ATP with last week's planet-wide spider stats, which many spiders fail. Since `located_in` rows count
as matched, these mall-heavy spiders passed the gate with the change and failed it on main, which gave 98 creates and
96 modifies that have nothing to do with malls. In production, the change will raise these spiders' match rate once,
which never fires the gate.

### Tenants of mall relations

In Japan, 11 creates in malls that OSM maps as a relation with an area and other members, such as `エールエールA館`
(relation/18458744), were first lost as `host_far`: `tenant_position` in the edits step only took a polygon or a
point as the host's geometry. It now takes the areas of a geometry collection too.

## Speed

The conflate step took 70.0 s → 70.4 s in Berlin, 38.1 s → 41.5 s in Switzerland, 303.8 s → 310.0 s in Massachusetts
and 115.2 s → 120.6 s in Japan, each from a single run. Each ATP feature that would be missing makes one more index
query, for shops within 500 m, and only decodes the geometry of malls.

## Files

The eval scripts and the comparison per region (`cmp-*.json`, with every `located_in` mall row) are in the project
folder `mall-tenants-1067`.
