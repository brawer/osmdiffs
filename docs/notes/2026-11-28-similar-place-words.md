# Address words in `similar_osm` names

## Background

OSMDiffs matches each AllThePlaces (ATP) feature with OpenStreetMap (OSM). When nothing in OSM matches
(`match.relation = missing_in_osm`), the edits step may propose adding the place to OSM: a *create* in
`edits.parquet`. A create is only useful if OSM really lacks the place, so before proposing one we look for OSM
features that may be the place under another name, without a brand, or mapped as another kind. That lookup is
`nearest_similar` in `src/pipeline/conflate/similar.rs`. It writes up to two OSM features to
`match.evidence.similar_osm` (see [CONFLATED_PARQUET.md](../outputs/CONFLATED_PARQUET.md)):

- `by = category`: the nearest OSM feature within 100 m whose category agrees with the ATP feature's, either equal
  or a known pair from `src/matchers/category_pairs.tsv`, such as `tourism=hotel` and `amenity=restaurant`. The
  edits step holds back a create if this feature is within 50 m (100 m for large sites such as hotels or
  supermarkets).
- `by = name`: the nearest OSM feature of any kind within 100 m whose names (`name`, `brand`, `official_name`,
  `alt_name`, `short_name`, `name:*`) share a *distinctive word* with the ATP feature's. That's a word of at least
  four letters that isn't a generic word like “Hotel” or “Restaurant”. This catches a hotel that OSM maps only as its
  restaurant. The edits step holds back the create whenever such a feature exists (`skipped_similar_nearby` in
  the log).

Place names counted as distinctive words. Many names include the town or street, so an unrelated OSM feature
nearby with the same town in its name held back the create. In the [offices eval](2026-11-24-offices.md),
`Orpi Anma Gestion Immo Crosne`, an estate agent's `official_name`, held back a BNP Paribas in Crosne. The word
“Berlin” did the same throughout Berlin ([#1050](https://github.com/brawer/osmdiffs/issues/1050)).

## The change

A shared word now counts only if it is in neither feature's address: `addr:city`, `addr:street`, `addr:suburb`,
`addr:place`, and ATP's `addr:street_address` or `addr:full`. The address is split into words the same way as the
names (`name_words`), so a future change to word splitting, such as CJK segmentation
([#953](https://github.com/brawer/osmdiffs/issues/953)), applies to both. `PoiMatcher::name_fits` already ignores
address words in the same way.

## Results in the eval regions

Each run used ATP of 2026-11-14, filtered by `addr:country`, and ran the full pipeline with osmdiffs at 3d90d81
(main) and with this change. Fallbacks for broken spiders were turned off. The extracts were the
[eval regions](2026-11-06-eval-regions.md), Switzerland, Japan, and Île-de-France (openstreetmap.fr, 2026-11-27).
Only ATP features inside the region's polygon count.

| region | ATP rows with `similar_osm` by name | creates | new creates | lost creates |
|---|---:|---:|---:|---:|
| France: Île-de-France | 17,184 → 14,580 (−15%) | 2,371 → 2,546 | 176 | 1 |
| Switzerland | 15,070 → 8,170 (−46%) | 1,318 → 1,390 | 72 | 0 |
| Brazil: Sudeste | 1,141 → 919 (−19%) | 3,096 → 3,193 | 97 | 0 |
| US: Massachusetts | 1,249 → 946 (−24%) | 2,130 → 2,167 | 37 | 0 |
| Thailand | 856 → 689 (−20%) | 9,862 → 9,879 | 17 | 0 |
| Japan | 14,977 → 14,567 (−3%) | 55,815 → 55,829 | 14 | 0 |
| Germany: Berlin | 1,207 → 907 (−25%) | 165 → 171 | 6 | 0 |
| United Arab Emirates | 438 → 275 (−37%) | 161 → 163 | 2 | 0 |
| Kenya | 80 → 71 | 228 → 228 | 0 | 0 |
| India: Karnataka | 77 → 45 | 2,808 → 2,808 | 0 | 0 |
| India: Tamil Nadu | 45 → 15 | 2,373 → 2,373 | 0 | 0 |

No modify changed in any region, since `similar_osm` only matters for creates.

Of the 11,191 rows that lost their `by = name` feature, only 421 became creates. 17% still have a `by = category`
feature, and most of the rest are held back by other checks: a spider whose positions aren't good enough
(`spider_position`), a position that several of the spider's features share (`shared_position`), or a kind we
never create. In Switzerland, for example, `similar_nearby` fell by 341, creates
rose by 72, `spider_position` by 187 and `shared_position` by 63 (counts for the whole extract).

The new creates come mostly from banks, opticians, insurers, post offices and car dealers. In Île-de-France
they include `bnp_paribas_fr` 27, `optic2000` 21, `lcl` 20 and `atol` 19. In Switzerland, `swiss_post_ch` 23 and
`discover_swiss` 19. In Brazil, `oggi_sorvetes_br` 75. In Massachusetts, `santander_us` 12 and `citizens_us` 10.

The one lost create is correct. `gan_fr_mc` puts its La Garenne-Colombes agency 2.7 m from its Levallois agency.
Main held back the Levallois agency by name. Now that agency is a candidate too, so the two count as *twins* (two
features of one spider within 10 m), and neither becomes a create.

### Japan and Thailand

Japanese names have no spaces, so `name_words` takes `ａｕショップ イオンモール羽生` as two words, and an address
word rarely equals a whole name word. That's why Japan's rows lose only 3%. Once
[#953](https://github.com/brawer/osmdiffs/issues/953) segments CJK text, town names like `羽生` will become words
of their own on both sides, and this change will matter much more there. Thai is also written without spaces, but
ATP's Thai `addr:full` puts spaces between its parts (`ตำบล ท่ามะขาม อำเภอ เมืองกาญจนบุรี`), and many names end in
the province (`BigC อ่างทอง`), so Thailand loses 20%.

### Precision

For every new create outside Île-de-France, Brazil and Massachusetts, and for a random sample of 30 in
Île-de-France, 15 in Brazil and 15 in Massachusetts, I looked up the OSM feature that had held it back on main
(current tags from the OSM API). That's 171 of the 421. About 156 look right:

- Most were held back by unrelated places that only shared the town or street: `Orpi Renaissance Immo
  Chambourcy` for Carrefour Express Chambourcy, `Weston Fire Department` for Citizens Weston, `Coop Matzendorf`
  for the Matzendorf post office, `Redo XXL Berlin` for a Telekom shop.
- Many others were held back by the shopping mall or station the place is in, such as `Emmen Center` for its post
  office, `イオンモール羽生` for the au shop inside it (12 of Japan's 14), or `Big C สมุทรสงคราม` for the Pure
  pharmacy inside it. A shop in a mall is a place of its own, so the create is right, but it would be better as
  `located_in` the mall ([#1067](https://github.com/brawer/osmdiffs/issues/1067)). Among the new creates, at least
  33 are within 35 m of an OSM mall or department store, and 30 of them are inside one.

About 15 look like the same place under another kind, now proposed as a duplicate. All are in Switzerland,
Thailand and the United Arab Emirates. The 60 random samples from Île-de-France, Brazil and Massachusetts had
none.

- **Lodging categories that don't agree** ([#1065](https://github.com/brawer/osmdiffs/issues/1065)):
  `discover_swiss` `Hotel Vezia` (`tourism=hotel`) next to OSM `Motel Vezia` (`tourism=motel`, 15 m),
  `Bamboohouse Motel Riazzino` next to `Motel Riazzino` (6 m), `Sonnmatt Luzern` next to `Kurhaus Sonnmatt`
  (`leisure=resort`, 0 m), Accor's `SO/ RAS AL KHAIMAH ADULTS ONLY 13+` next to `SO/ Ras Al Khaimah`
  (`leisure=resort`, 66 m), and `Berghaus Niederhorn` (`tourism=hostel`) next to `Bergrestaurant Niederhorn`
  (`amenity=restaurant`, 31 m). `category_pairs.tsv` pairs hotels with hostels, guest houses and restaurants, but
  not with motels or resorts. Pairs aren't transitive, so hostels and restaurants don't agree either.
- **Zürich after-school care tagged as schools** ([#1066](https://github.com/brawer/osmdiffs/issues/1066)): 7
  `stadt_zuerich_ch` features such as `Höfliweg`, 3–10 m from OSM `Mittag-/Abendhort Höfliweg`
  (`amenity=childcare`). These are the 518 Kinderhorte from the city's `poi_kinderhort_view`, which the ATP run of
  2026-11-14 tags `amenity=school` + `after_school=yes`, so only 8 of them match OSM. The spider on ATP's `master`
  now returns `amenity=childcare`. The `amenity=childcare`–`amenity=school` pair was left out of
  `category_pairs.tsv` with the comment “after-school care named after its school (Zurich)”, which may need
  another look.
- **Big C malls in Thailand:** `BigC ปทุมธานี` next to OSM `Big C Pathum Thani -บิ๊กซี ปทุมธานี` (0 m) and
  `BigC อ่างทอง` next to `บิ๊กซี อ่างทอง` (81 m). Spelling isn't the problem: ATP gives `brand:th=บิ๊กซี`, and the
  `brand_in_name` rule matches 101 Big C stores in Thailand. The problem is that OSM maps the whole Big C building
  as `shop=mall`, while ATP's feature is the hypermarket inside it (`shop=supermarket`). Pairing supermarkets with
  malls would be wrong, since most malls contain one. Under one-feature-per-thing tagging, these creates are
  arguably right, and they belong to [#1067](https://github.com/brawer/osmdiffs/issues/1067).

In each case, the place is named after its location, and its brand or kind doesn't tie the two sides together.

I tried one alternative rule: keep the address words whenever leaving them out leaves either side with no name
words. On the 407 new creates outside Japan, it would have held back 115. Most of them are unrelated places
(`LCL MAUREPAS` by the restaurant `Le Maurepas`, `Hotel Uzwil` by the parking `Bahnhof Uzwil`), and it would still
have created the Big C ones. So the change keeps the simple rule, and the duplicates are better fixed by the
issues above.

### ATP data problems found

- [alltheplaces#20876](https://github.com/alltheplaces/alltheplaces/issues/20876): `aster_pharmacy_ae` puts the
  store's name in `addr:city` for 43 of its 260 stores (`Aster 134 - Al Barsha`), because the store API copies
  `title` into `city`. The word “Aster” was then left out as an address word, so the Aster Pharmacy no longer
  counts as similar to the Aster Clinic 25 m away. That one happens to be right, since they're different places.
  The spider also gives `brand=Aster Pharmacy` to about 80 stores of other pharmacy chains in the API, such as
  Medicom and Beta Pharmacy.
- An `atol` store in Bailly-Romainvilliers has the placeholder `xxxxx` as its street address. (Not filed.)

## Speed

Address words are computed once per ATP feature. For an OSM feature, they are only computed when the names already
share a word, which is rare. The conflate step took 69.8 s → 69.3 s in Berlin, 72.9 s → 72.6 s in Île-de-France,
38.4 s → 39.0 s in Switzerland, 126.8 s → 124.5 s in Japan and 291.5 s → 303.4 s in Massachusetts, each from a
single run.

`nearest_similar` still splits the names of each nearby OSM feature into words again for every unmatched ATP
feature near it, and the match mask doesn't narrow the search, since any kind of feature counts. These timings
don't show it to be a problem. If it ever is, caching the words per OSM feature would fix it.
