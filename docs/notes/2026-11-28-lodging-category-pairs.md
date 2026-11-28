# Lodging category pairs

## Background

`src/matchers/category_pairs.tsv` lists pairs of main categories that one business often has in AllThePlaces (ATP)
and OpenStreetMap (OSM), such as `tourism=hotel` and `amenity=restaurant`. A pair is used in four places:

- the matcher accepts a match by contact, brand name, brand in name, brand domain or address between the two
  categories (`category_factor` in `src/matchers/category.rs`);
- `similar_osm` with `by = category` takes an OSM feature of one category as similar to an ATP feature of the other,
  and the edits step then holds back a create within 50 m (100 m for large sites such as hotels);
- the search for deletion candidates takes an OSM feature of one category as covered by ATP features of the other;
- the edits step treats two ATP features of agreeing categories as one site when merging and finding twins.

In the eval of [#1064](2026-11-28-similar-place-words.md), five lodging places became creates next to an OSM
feature that is very likely the same place, mapped with a category that doesn't pair with the ATP one
([#1065](https://github.com/brawer/osmdiffs/issues/1065)): hotels next to OSM motels and resorts, and a hostel
next to an OSM restaurant. The file pairs hotels with hostels, guest houses and restaurants, but pairs aren't
transitive.

## Evidence

The pairs in the file came from `scripts/infer_contact_category_pairs.py`, which needs at least 20 matches with
agreeing names per pair. The lodging pairs are rarer than that, so I looked at three sources.

**Equal `brand:wikidata`** in the production run of 2026-11-23 (the script's `--evidence=brand_wikidata`, up to
25 m, `--min-name-agree 1`). The matcher doesn't look at the category for these, so they show the pairs without
bias:

| pair | matches | names agree |
|---|---:|---:|
| `tourism=hotel` – `tourism=motel` | 198 | 198 |
| `tourism=hotel` – `leisure=resort` | 0 | 0 |
| `tourism=hostel` – `amenity=restaurant` | 0 | 0 |

Hotel and motel would pass the script's cut-off now; 183 of them are `sonesta` (Americas Best Value Inn and
other motel brands that ATP tags as hotels). OSM resorts rarely have a `brand:wikidata`, and hostels rarely
belong to a brand.

**Shared phone or email** between ATP (run of 2026-11-14, filtered to the country) and OSM features within 250 m,
in the 11 eval regions, counted like the script does but straight from the inputs, so pairs the matcher doesn't
accept yet show up too. With equal categories, names agree in 79% (4,277 of 5,434).

| pair | matches | names agree | where |
|---|---:|---:|---|
| `tourism=hotel` – `tourism=motel` | 6 | 5 | Switzerland 5/5, Massachusetts 0/1 |
| `tourism=hotel` – `leisure=resort` | 3 | 3 | Switzerland 2/2, Thailand 1/1 |
| `tourism=hostel` – `amenity=restaurant` | 21 | 21 | Switzerland |

The one that disagrees is a Red Roof Inn that shares a number with a Rodeway Inn 12 m away. The 21 hostels and
restaurants are all `discover_swiss`, mostly mountain inns and village hotels, such as `Berghuus Radons` and
`Hotel Restaurant Capricorns`. For comparison, the existing hotel–restaurant pair has 565 such matches in
Switzerland, of which 490 agree on name.

**The nearest OSM feature of each category within 100 m** of each ATP lodging feature, and whether the two share a
word of four or more letters that isn't generic (“Hotel”, “Resort”, “Restaurant” and so on), in the same regions.
This says how often a nearby feature of that kind is the same place, which is what `similar_osm` relies on:

| ATP | nearest OSM | features | share a word |
|---|---|---:|---:|
| `tourism=hotel` | `tourism=hotel` | 3,789 | 78% |
| `tourism=hotel` | `leisure=resort` | 32 | 78% |
| `tourism=hotel` | `tourism=motel` | 35 | 57% |
| `tourism=hostel` | `amenity=restaurant` | 80 | 49% |
| `tourism=hostel` | `tourism=hotel` | 68 | 85% |
| `tourism=hotel` | `amenity=restaurant` | 3,517 | 34% |
| `tourism=hotel` | `tourism=guest_house` | 381 | 25% |

A nearby resort is as often the same place as a nearby hotel, and a nearby motel or restaurant more often for a
hostel than a restaurant is for a hotel, which is already a pair.

## The change

Three pairs, in a section of their own at the end of `category_pairs.tsv`, with the counts from the phone and
email table:

- `tourism=hotel` – `tourism=motel`
- `leisure=resort` – `tourism=hotel`
- `amenity=restaurant` – `tourism=hostel`

An OSM resort that is also tagged `tourism=hotel` already agreed with hotels, since `tourism` comes before
`leisure` in the main category's key order. The pairs are for resorts tagged only `leisure=resort`.
`leisure=beach_resort` (one St. Regis in Abu Dhabi, 229 m away) is left out.

## Results in the eval regions

Each run used ATP of 2026-11-14, filtered by `addr:country`, and ran the full pipeline with osmdiffs at 3d743f1
(main) and with this change. Fallbacks for broken spiders were turned off. The extracts were the
[eval regions](2026-11-06-eval-regions.md), Switzerland, Japan, and Île-de-France (openstreetmap.fr, fetched
2026-11-28). Only ATP features inside the region's polygon count.

| region | matches | creates | modifies | rows with `similar_osm` by category |
|---|---:|---:|---:|---:|
| Switzerland | 77,811 → 77,835 (+24) | 1,390 → 1,380 | 16,271 → 16,285 | 13,461 → 13,472 |
| US: Massachusetts | 8,527 → 8,528 (+1) | 2,167 | 4,782 → 4,783 | 1,855 → 1,859 |
| United Arab Emirates | 672 | 163 → 161 | 459 | 941 → 943 |
| Thailand | 9,891 | 9,879 | 5,918 | 4,733 → 4,744 |
| Japan | 111,086 | 55,829 | 100,380 | 40,632 → 40,635 |
| Germany: Berlin | 7,289 | 171 | 2,786 | 1,442 → 1,443 |
| France: Île-de-France | 28,912 | 753 | 1,322 | 16,698 → 16,699 |
| Brazil: Sudeste | 7,692 | 3,193 | 4,124 | 3,532 |
| Kenya | 524 | 228 | 403 | 324 |
| India: Karnataka | 1,440 | 2,808 | 1,236 | 1,208 |
| India: Tamil Nadu | 906 | 2,373 | 810 | 672 |

No match was lost or moved, and the deletion candidates are the same in every region.

### New matches

All 25 are ATP features that main left as `missing_in_osm`, and all look right:

- 12 `discover_swiss` hotels on OSM motels, such as `Motel Fehraltorf`, `b_smart motel Landquart`,
  `Gasthof zum Falken` on `Motel Falken`, and `A1 Grauholz Hotel Restaurant` on `BEST WESTERN Hotel A1 Grauholz`.
  Nine are by phone or email, three by address at 0 m, such as `Hôtel Soleil` on `Motel-Restaurant Soleil`.
- 12 `discover_swiss` hostels on OSM restaurants, all by phone or email, such as `Hotel Stüa Granda` on
  `Stüa Granda`, `Mattli Antoniushaus` on `Antoniushaus Mattli` and `Hôtel Weisshorn` on `Hôtel Weisshorn`.
- In Massachusetts, `ihg_hotels` `Holiday Inn Express & Suites Pittsfield-Lenox-Berkshires` on the OSM motel
  `Holiday Inn Express & Suites Pittsfield-Lenox` (brand in name, 0 m).

The 15 new modifies are 14 of these matches in Switzerland and the one in Massachusetts. They only add or change
contact tags, such as `email=info@weisshorn.ch`. A modify doesn't change the OSM category.

No ATP hotel matched an OSM resort in any region. Resorts in OSM rarely have a phone, email or brand.

### Creates held back

Twelve creates are gone, 10 in Switzerland and 2 in the UAE. I looked up the OSM feature that now holds each back
(current tags from the OSM API).

Right, 6:
- the five from #1065: `Hotel Vezia` (OSM motel `Motel Vezia`, 15 m), `Bamboohouse Motel Riazzino`
  (`Motel Riazzino`, 6 m), `Sonnmatt Luzern` (resort `Kurhaus Sonnmatt`, 0 m), Accor's
  `SO/ RAS AL KHAIMAH ADULTS ONLY 13+` (resort `SO/ Ras Al Khaimah`, 66 m) and `Berghaus Niederhorn`
  (restaurant `Bergrestaurant Niederhorn`, 31 m);
- `Sky Motel`, which now matches the OSM motel `Sky Motel` and is a modify.

Not sure, 2:
- Accor's `Caravan Hatta by Our Habitas` is 19 m from the OSM resort `منتجع سدر تريلرز` (Sedr Trailers Resort),
  whose website is on visithatta.com. They may be the same camp under a new operator; I couldn't confirm.
- `discover_swiss` puts `Berghotel Schwarenbach` (`Gemmi Weg 1`) in the middle of Kandersteg, 6 m from the
  restaurant `Schweizerhof`. The Berghotel is up at the Gemmi pass, about 9 km away, so the create was in the wrong
  place anyway.

Wrong, 4: all are `discover_swiss` features tagged `tourism=hostel` next to a restaurant that is likely another
place: `Seven Town House Boutique Hotel` in Locarno (`Ristorante Pizzeria Svizzero`, 20 m), `capsule hotel chapter
lucerne` (`Waipo`, 14 m), `Emma's Hotel – Bed & Breakfast` in Lungern (`Bahnhöfli`, 42 m) and `Feriendorf Rustici
della Verzasca` (`Osteria Paradiso`, 6 m, which may be the village's own osteria). `discover_swiss` tags many
hotels as hostels, so these are town hotels with a restaurant nearby, the same miss the hotel–restaurant pair
already has.

So the hostel–restaurant pair adds 12 right matches and 1 right hold-back, at the cost of about 4 missed creates
in Switzerland. A missed create costs one suggestion; a wrong create would put a duplicate into OSM. That's why
I kept the pair. If the misses grow, `similar_osm` could require a shared name word for pairs, and equal categories
alone would count without one.

## Speed

The change adds three entries to a hash set, so the run times don't change. The runs of main and of the change
shared the machine, so their times aren't comparable.

