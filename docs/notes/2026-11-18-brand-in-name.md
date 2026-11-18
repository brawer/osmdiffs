# Finding the brand in an OSM name

Many OpenStreetMap (OSM) shops have neither `brand` nor
`brand:wikidata`, only a `name` that begins with the brand:
`name=セブン-イレブン 八重瀬東風平西店`, `name=Rubis Majaoni`,
`name=Pizza Hut`. `PoiMatcher` compared `brand` and `operator`, but not
`name`, so these didn't match their AllThePlaces (ATP) feature
([#989](https://github.com/brawer/osmdiffs/issues/989)).

## The rule

An OSM feature without `brand` or `brand:wikidata` matches an ATP
feature nearby when its `name` begins with one of the names of the ATP
feature's brand, operator or network (rule `brand_in_name`, with the
same factor as `brand_name`). The existing `brand_name` rule, which
compares OSM `brand` (or `operator`) with ATP, now uses the same names.

The names are compared by their words, lowercased, without accents and
punctuation, and joined without spaces, so `7-Eleven`, `7 ELEVEN` and
`7eleven` are the same. A name must end at a word boundary, so that
`Spar` isn't found in `Sparkasse` or `Mini` in `Minimalist Atelier`,
except next to a character of Unicode line break class `ID`
(ideographs, kana), `CJ` (small kana, `ー`) or `SA` (Thai, Lao, Khmer,
Myanmar, Tai scripts): scripts written without spaces between words,
which need a dictionary to segment
([UAX #14](https://www.unicode.org/reports/tr14/)). So `ローソン` is
found in `ローソン能美寺井店`, and Korean or Arabic names need a space,
as for Latin script. The property comes from ICU4X's `icu_properties`,
which we already had as a dependency of `idna`.

## Where the names come from

1. The ATP feature's own `brand`, `operator` and `network`, and their
   language variants such as `brand:ja`.
2. The [name-suggestion-index](https://nsi.guide) (NSI), by the ATP
   feature's `brand:wikidata`, `operator:wikidata` and
   `network:wikidata`: each item's `name`, `brand`, `operator`,
   `network` (with language variants) and `matchNames`.

We compared NSI with a table built from all of ATP before deciding. ATP
copies NSI's canonical tags into its features (56% carry an `nsi_id`),
but not NSI's `matchNames`, which hold the aliases, abbreviations and
former names that mappers type: `Agip` for Eni, `Vijaya Bank` for Bank
of Baroda, `Sunkus` for FamilyMart, `7/11`, `Mini Big C`, `TMB`,
`Petrobras` for BR. In a quick estimate on the eval regions, the names
from all of ATP found 3% more name pairs than each feature's own brand,
and NSI 9% more, at the same chance rate. So ATP's names are, in
practice, a subset of NSI's, and we don't build an ATP-wide table.

An NSI item only counts where its `locationSet` says, as one brand can
have other names elsewhere: Korean `matchNames` of 7-Eleven include
Ministop, a brand of its own in Japan. We resolve `locationSet` with
`country-boundaries`, not NSI's polygons:

| NSI tree | items | countries or world only | ISO subdivision | NSI's own polygons |
|---|---:|---:|---:|---:|
| brands | 19,335 | 17,538 (91%) | 177 | 1,559 |
| operators | 24,194 | 11,016 (46%) | 673 | 12,483 |
| transit | 6,029 | 2,267 (38%) | 146 | 3,616 |

Most of NSI's own polygons are named after a state, such as
`us-tx.geojson`; we take those as the state when `country-boundaries`
knows it (US, Canada, Australia and some more), else as the country.
Circles are measured, M49 Europe (`150`) is its countries, and anything
else (other M49 regions, Wikidata IDs) holds nowhere, so the item is
left out rather than used in the wrong place.

If NSI has items of the ATP feature's category for a Wikidata ID, only
those count: `Circle K` is a FamilyMart `matchName` for convenience
stores in Japan only.

NSI is a new pipeline input, as it changes every week: `fetch_inputs`
takes the latest version that jsDelivr resolves for the npm package,
downloads `dist/json/nsi.min.json` (12 MB) from jsDelivr's CDN under
that version, and checks it against the SHA-256 in jsDelivr's package
listing. The provenance BOM lists it (BSD-3-Clause).

## Results on the eval regions

Main against this change, on the [eval regions](2026-11-06-eval-regions.md),
with the ATP run of 2026-11-07 filtered by `addr:country`, NSI
8.0.20261101, and the same OSM extracts as the baseline. Chance is the
pipeline's own estimate: matched over sampled features after moving
each sampled ATP feature (`conflate.spider-diagnostics.tsv`).

| region | matched before | after | new by name | new by brand name | lost | chance before | chance after |
|---|---:|---:|---:|---:|---:|---:|---:|
| Switzerland | 67,295 | 67,833 | 441 | 114 | 0 | 39 / 6,802 | 39 / 6,813 |
| Japan | 95,361 | 108,185 | 11,639 | 1,283 | 1 | 294 / 15,388 | 304 / 15,388 |
| Massachusetts | 8,277 | 8,338 | 58 | 5 | 0 | 7 / 83,475 | 7 / 83,481 |
| Berlin | 6,315 | 6,426 | 96 | 18 | 0 | 31 / 20,296 | 32 / 20,394 |
| Brazil Sudeste | 6,649 | 7,396 | 233 | 521 | 0 | 34 / 5,280 | 34 / 5,285 |
| Thailand | 7,609 | 9,538 | 1,429 | 528 | 0 | 48 / 4,029 | 49 / 4,029 |
| Kenya | 469 | 524 | 43 | 14 | 1 | 3 / 708 | 3 / 799 |
| UAE | 570 | 704 | 112 | 23 | 0 | 10 / 2,569 | 10 / 2,626 |
| Karnataka | 1,144 | 1,326 | 184 | 5 | 0 | 4 / 5,353 | 4 / 5,353 |
| Tamil Nadu | 558 | 586 | 28 | 0 | 0 | 3 / 5,353 | 3 / 5,353 |
| **total** | **194,247** | **210,856** | | | **2** | | |

That is 16,609 more matches (+8.6%), +13% in Japan, +25% in Thailand,
+24% in the UAE, +16% in Karnataka and +11% in Brazil. The chance
estimate barely moves. "New" includes a few hundred ATP features that
now match a closer OSM feature than before. Of the new matches, 83%
are within 25 m and 0.2% beyond 150 m (where the street address must
agree too).

The most common spiders:

- Japan: 7-Eleven (2,087), FamilyMart (1,226), Lawson (1,091),
  Welcia (519), Zensho (483), Tsuruha (389), ENEOS (388), Cosmo (352),
  Skylark (343), Matsukiyo (308), Nissan (262).
- Thailand: PTT (649, OSM `PTT`, `ป.ต.ท.` or `ปตท.` against ATP's
  `ปตท.`), 7-Eleven (354), Café Amazon (270, `Cafe Amazon` against
  `คาเฟ่ อเมซอน`), Thailand Post (161), Big C (116).
- Brazil: Correios (466): ATP gives the post offices `operator=Correios`,
  which now counts. Petrobras (141), mostly NSI's `Petrobras` and
  `Posto BR` for ATP's `BR`.
- Karnataka: Bank of Baroda (69, many OSM branches still `Vijaya
  Bank`, an NSI `matchName`), Axis Bank, Jockey, HP.
- Switzerland: `maes_dkv` stations (125, `Avia`, `Ruedi Rüssel`),
  Migros (67, `Migros Restaurant`, `Migros Partner`), Stadt Zürich.
- Berlin: Sparkasse (58, `name=Sparkasse` with
  `operator=Berliner Sparkasse`), Allego.
- UAE: Emirates Post, Aster Pharmacy, Starbucks (`Starbucks` in OSM,
  `ستاربكس` in ATP), Pizza Hut.

Random samples of 18 to 30 new matches per region were all the same
place on both sides, apart from sub-formats that are arguably the same
chain: `Migros Florissimo` for a Migros florist, `Denner Satellit` for
Denner, `Daily Yamazaki` against `デイリーヤマザキ`. Many new matches
are OSM features whose name is just the brand (`Pizza Hut`, `Jockey`,
`7-Eleven`), which had no `brand` tag and so no evidence before.

Deletion candidates go up a little: 56 in Japan, 33 in Thailand, at
most 3 elsewhere. With more matches, chains look better covered
locally, so a few more of their unmatched OSM features pass the
deletion rule's coverage check; some are a branded OSM duplicate next
to an unbranded one that now takes the ATP feature.

A side effect for the integration test: Winterthur's playgrounds now
get a matcher, as their `operator` can be found by name.

## Known gaps

- Some NSI `matchNames` are short or generic, such as `bob` or
  `post`. They only count for an ATP feature of that brand nearby and
  in the same category, and the chance estimate didn't move, so we
  don't filter them yet.
- Former brands that NSI doesn't list, such as Idemitsu and Showa
  Shell for apollostation, still need the hand-kept table of
  [former_brands](../../src/matchers/former_brands.rs).
- For operators, half of NSI's items use NSI's own polygons, which we
  approximate by state or country. NSI ships them in
  `dist/json/featureCollection.json` if we need them.
