# Address words in `similar_osm` names

`nearest_similar` in `src/pipeline/conflate/similar.rs` holds back a create when an OSM feature of any kind within
100 m has a name that shares a distinctive word with the AllThePlaces (ATP) feature (`similar_osm` with `by = name`).
Place names counted as distinctive words, so the town in an `official_name` such as `Orpi Anma Gestion Immo Crosne`
held back a BNP Paribas in Crosne
([brawer/osmdiffs#1050](https://github.com/brawer/osmdiffs/issues/1050), found in the
[offices eval](2026-11-24-offices.md)).

## The change

A shared word now counts only if it is in neither feature's address: `addr:city`, `addr:street`, `addr:suburb`,
`addr:place`, and ATP's `addr:street_address` or `addr:full`. The address is split into words the same way as the
names (`name_words`), so a future change to word splitting, such as CJK segmentation
([#953](https://github.com/brawer/osmdiffs/issues/953)), applies to both.

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

No modify changed in any region. Most rows that lose `similar_osm` don't become creates, because another check
holds them back too, most often a similar feature by category.

The new creates come mostly from banks, opticians, insurers, post offices and car dealers: in Île-de-France
`bnp_paribas_fr` 27, `optic2000` 21, `lcl` 20 and `atol` 19; in Switzerland `swiss_post_ch` 23 and `discover_swiss`
19; in Brazil `oggi_sorvetes_br` 75; in Massachusetts `santander_us` 12 and `citizens_us` 10.

The one lost create is correct. `gan_fr_mc` puts its La Garenne-Colombes agency 2.7 m from its Levallois
agency. Main held back the Levallois agency by name, and now that agency counts too, so the two are twins and
neither becomes a create.

### Japan and Thailand

Japanese names have no spaces, so `name_words` takes `ａｕショップ イオンモール羽生` as two words, and an address
word rarely equals a whole name word. That's why Japan's rows lose only 3%. Once
[#953](https://github.com/brawer/osmdiffs/issues/953) segments CJK text, town names like `羽生` will become words
of their own on both sides, and this change will matter much more there. Thai is also written without spaces, but
ATP's Thai `addr:full` puts spaces between its parts (`ตำบล ท่ามะขาม อำเภอ เมืองกาญจนบุรี`), and many names end in
the province (`BigC อ่างทอง`), so Thailand loses 20%.

Twelve of Japan's 14 new creates are au shops in shopping malls (`ａｕショップ イオンモール羽生` next to
`イオンモール羽生`). The mall's name is in the shop's `addr:street_address`, so it no longer counts. A shop in a mall
is a place of its own, so these creates are fine.

### Precision

For every new create outside Île-de-France, Brazil and Massachusetts, and for a random sample of 30 in
Île-de-France, 15 in Brazil and 15 in Massachusetts, I looked up the OSM feature that held it back on main (current
tags from the OSM API): 171 of the 421. Most are unrelated places that only shared the town or street with the ATP
feature: `Orpi Renaissance Immo Chambourcy` for Carrefour Express Chambourcy, `Weston Fire Department` for Citizens
Weston, `Coop Matzendorf` for the Matzendorf post office. Many others are the mall or station the place is in
(`Emmen Center` for its post office, `Big C สมุทรสงคราม` for the Pure pharmacy inside it), which is fine for a
create.

About 15 look like the same place under another kind, now proposed as a duplicate:

- Switzerland: `discover_swiss` hotels next to an OSM feature of another kind with the same place name:
  `Hotel Vezia` (OSM `Motel Vezia`, 15 m), `Bamboohouse Motel Riazzino` (`Motel Riazzino`, 6 m), `Berghaus
  Niederhorn` (`Bergrestaurant Niederhorn`, 31 m), `Sonnmatt Luzern` (`Kurhaus Sonnmatt`, 0 m).
- Switzerland: 7 `stadt_zuerich_ch` features tagged `amenity=school` and named after their street, such as
  `Höfliweg`, 3–10 m from an OSM `amenity=childcare` named `Mittag-/Abendhort Höfliweg`. The name is all there is
  to tie them, and it's the street.
- Thailand: `BigC ปทุมธานี` (OSM mall `Big C Pathum Thani -บิ๊กซี ปทุมธานี`, 0 m) and `BigC อ่างทอง` (`บิ๊กซี อ่างทอง`,
  81 m). The brand is spelled `BigC` in one and `บิ๊กซี` in the other, so only the province ties them.
- United Arab Emirates: Accor's `SO/ RAS AL KHAIMAH ADULTS ONLY 13+` (OSM resort `SO/ Ras Al Khaimah`, 66 m).

In each of these, the place is named after its location, and the brand or kind doesn't tie the two sides. I tried
one alternative rule: keep the address words when leaving them out would leave a side with no name words. On the
407 new creates outside Japan, it would have held back 115, most of them the unrelated places listed above (`LCL MAUREPAS` by
the restaurant `Le Maurepas`, `Hotel Uzwil` by the parking `Bahnhof Uzwil`), and it would still have created the
Big C ones. So the change keeps the simple rule.

## Speed

Address words are computed once per ATP feature. For an OSM feature, they are only computed when the names already
share a word. The conflate step took 69.8 s → 69.3 s in Berlin, 72.9 s → 72.6 s in Île-de-France, 38.4 s → 39.0 s in
Switzerland, 126.8 s → 124.5 s in Japan and 291.5 s → 303.4 s in Massachusetts, each from a single run.
