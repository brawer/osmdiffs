# Matching places whose OSM brand is a former brand

Investigation note behind
[`matchers/former_brands.rs`](../../src/matchers/former_brands.rs), for
[#990](https://github.com/brawer/osmdiffs/issues/990).

## Short answer

When a chain is rebranded, AllThePlaces (ATP) has the new brand right
away, but OpenStreetMap (OSM) keeps the old one for years. A different
`brand:wikidata` on the two sides vetoes a match, so such places never
matched. Dropping the veto would hurt everywhere else. Instead, a small
table, kept by hand and for one country at a time, says which brands a
brand replaced. An OSM feature with such a former brand now matches,
with the rule `former_brand`.

In Japan, apollostation goes from **778 to 1,930 matched stations, of
5,641 (14% to 34%)**. One match moves to another OSM feature, and
nothing else changes. No other eval region can change, because every
entry of the table is for Japan.

## The table

| country | new brand | former brands |
|---|---|---|
| JP | apollostation `Q114731101` | Idemitsu `Q2216770`, Showa Shell Sekiyu `Q277115`, Shell `Q110716465` |
| JP | 三十三銀行 `Q106781309` | 三重銀行 `Q11358192` |

- **Kept by hand, not built from Wikidata.** Wikidata's "replaced by"
  (P1366), "replaces" (P1365) and "followed by" (P156) also record
  partial takeovers, where only some branches changed brand. None of
  the four former brands above even has such a statement pointing to
  its successor.
- **For one country.** A replaced brand can live on elsewhere. Shell is
  still Shell outside Japan, and Japanese OSM tags Showa Shell
  stations with the global Shell ID, as the name-suggestion-index entry
  `showashell-fbe2e4` says. Wikidata currently labels `Q110716465`
  "Exxon", which looks like an error there; NSI uses it for Shell
  worldwide.
- The country is that of the ATP feature's position, looked up only
  for ATP brands that are in the table.
- 第三銀行 (Daisan Bank) merged into 三十三銀行 too, but isn't in the
  table yet, for want of a checked Wikidata ID.

## The rule

In [`PoiMatcher`](../../src/matchers/poi_matcher.rs), when both sides
have `brand:wikidata` and share no ID, but the OSM ID is a former brand
of ours in this country, the pair matches with `former_brand`. The score
is 0.96 × the category factor × the distance score, the same as a
match on the brand name: a former brand says which brand a place is,
not which branch. Beyond 150 m, the street address must agree, as for
other matches below 1.0. Any other brand still vetoes.

For such a match, the POI edit suggester proposes ATP's `brand`,
`brand:wikidata` and `brand:<language>` tags. It proposes ATP's
`brand` as `name` only if the OSM `name` is just the old brand. Fuel
stations get no edit suggestions yet, as edits only run for shops; see
[#995](https://github.com/brawer/osmdiffs/issues/995). This matters
here: many stations of the Idemitsu group may still carry 出光 or
Shell signs, even though ATP lists them all as apollostation. An
edit to the brand should wait until we know which ones changed.

## Evaluation

ATP run 2026-11-07, filtered to `addr:country=JP`. OSM:
openstreetmap.fr Japan extract of 2026-11-05. Both runs were on the
whole of Japan: `main` at 508836c, and this change.

| | `main` | this change |
|---|---:|---:|
| matches (`same`) | 94,178 | 95,330 |
| `missing_in_osm` | 197,692 | 196,540 |
| deletion candidates | 7,715 | 7,715 |
| apollostation matched | 778 | 1,930 |
| 三十三銀行 matched | 13 | 13 |

The 1,153 `former_brand` matches are all `amenity=fuel`:

| OSM `brand:wikidata` | matches |
|---|---:|
| Idemitsu `Q2216770` | 733 |
| Shell `Q110716465` | 218 |
| Showa Shell Sekiyu `Q277115` | 202 |

That fits the estimate in #990 of about 1,000 ATP stations with such an
OSM station within 50 m. The distance is a median 4 m, 1,105 of
1,153 are within 25 m, 13 are beyond 50 m, and none is beyond 150 m
(the farthest is 134 m). OSM names are mostly the old brand: 出光 627,
昭和シェル 331, 昭和シェル石油 26. Another 22 are already named
`apollostation` but still have the old `brand:wikidata`.

In a random sample of 30 matches, each OSM feature is a fuel station
named for the old brand, within 47 m of the ATP point except one at
96 m. The sample was checked only against the data, not on the
ground or on imagery. The 13 matches beyond 50 m would need a closer
look before edits are suggested for them.

One ATP station moved its match from an OSM `apollostation` node 53 m
away to an OSM 出光 node 5 m away. The two nodes have consecutive IDs,
so they are likely two OSM nodes for one station, and the closer one
wins.

三十三銀行 doesn't change: no OSM bank with 三重銀行's
`brand:wikidata` is close enough to one of its ATP branches to match.
The matched OSM banks named 三重銀行 or 第三銀行 have no
`brand:wikidata` and match by phone number.

## Left out

- OSM features with only the old `brand` or `name`, such as
  `brand=出光` without `brand:wikidata`, don't match, because the table
  holds Wikidata IDs only. Former brand names could be added later.
- Edit suggestions for fuel stations: #995.
