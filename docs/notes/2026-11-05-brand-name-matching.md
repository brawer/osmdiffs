# Matching by brand name

`PoiMatcher` ([`src/matchers/poi_matcher.rs`](../../src/matchers/poi_matcher.rs))
used to match an AllThePlaces (ATP) feature to an OpenStreetMap (OSM)
feature only on an equal `brand:wikidata`, or on a shared phone number
or email address. Many OSM features name their brand without
`brand:wikidata`, such as a fuel station tagged `brand=Avia`, so ATP
features next to them stayed unmatched. This came up while building the
name rule for deletion candidates
([`2026-11-05-deletion-candidates.md`](2026-11-05-deletion-candidates.md)):
of 931 OSM features that the rule flagged, 515 had an ATP feature of the
same brand within 50 m.

This is a small step towards
[#708](https://github.com/brawer/osmdiffs/issues/708). It compares
whole brand names, not names in general, and needs no word segmenter.

## The rule

An equal brand name counts as evidence, with factor 0.96 times the
category factor times the distance score:

- On the ATP side, the values of `brand`.
- On the OSM side, the values of `brand`, or of `operator` if the
  feature has no `brand`. Many OSM fuel stations name their brand only
  as `operator=Tamoil`.
- Values separated by `;` are split, and any one shared value counts.
  Names are compared after lowercasing and collapsing whitespace, so
  "AVIA" and "Avia" are equal.
- The main categories must be equal or a known pair, as for phone and
  email matches. Beyond 150 m, the street address must agree too.
- A `brand:wikidata` conflict still vetoes the match: if both sides
  have `brand:wikidata` and share no ID, the name does not matter.

The factor is below the one for phones and emails (0.98), since a
chain's name is shared by all its branches, while a phone number
usually belongs to one.

An ATP feature with a `brand` but without `brand:wikidata` or contacts
now gets a matcher too.

The category factor now also splits values: `shop=car;car_repair` on
OSM agrees with `shop=car_repair` on ATP. This helps contact matches as
well.

## Results on Swiss data

Same setup as the deletion-candidates note: Geofabrik's Switzerland
extract and the 2026-10-10 ATP run, filtered to Switzerland and
Liechtenstein.

| step | matched pairs | new | name rule rows |
|---|---:|---:|---:|
| before | 47,570 | | 996 |
| `brand` on both sides | 48,146 | 592 | 637 |
| OSM `operator` if no `brand` | 48,264 | 121 | 543 |
| split category values | 48,328 | 71 | 445 |

The last column also changed for reasons in the deletion-candidates
note (minimum feature count, long brand lists, ski lifts); with those
it is 380.

In all, 781 ATP features got a new or different match, for a net gain
of 758 pairs. For 23 of them, the ATP feature had matched another OSM
feature before and now matches a closer one with the same brand, such
as a Denner 342 m away before and 21 m away now.

| distance | new matches |
|---|---:|
| under 25 m | 575 |
| 25 to 50 m | 120 |
| 50 to 150 m | 83 |
| 150 m or more | 3 |

The most common brands were BP (85), Avia (61), Toyota (with Lexus,
about 120), Tamoil (44), Shell (32), Ruedi Rüssel (27), Renault and
Dacia, Coop Vitality, and Rotpunkt Apotheke. A random sample of 25 new
matches were all the same place on both sides. The three beyond 150 m
are two Caffè Spettacolo cafés and a Nissan dealer, all with agreeing
street addresses.

## Results on the eval regions

Main on the [eval regions](2026-11-06-eval-regions.md), with the
setup described there. These are matched points of interest whose
`brand` (or OSM `operator`) is equal while no `brand:wikidata`,
phone or email agrees, so roughly the matches that this rule adds.

| region | brand name matches | share of matches | under 25 m | 25 to 50 m | 50 to 150 m | 150 m or more | most common spiders |
|---|---:|---:|---:|---:|---:|---:|---|
| Switzerland | 774 | 7% | 570 | 120 | 81 | 3 | `maes_dkv` (222), Toyota (126), Renault (68), BP (44) |
| Japan | 21,516 | 22% | 18,929 | 1,741 | 846 | 0 | `japan_post_jp` (18,431), ENEOS (552), Lawson (406) |
| US: Massachusetts | 92 | 1% | 83 | 7 | 2 | 0 | Red E chargers (72), Shell (5) |
| Germany: Berlin | 115 | 3% | 59 | 15 | 36 | 5 | ubitricity (38), Deutsche Post (22), car dealers |
| Brazil: Sudeste | 410 | 6% | 275 | 72 | 62 | 1 | Petrobras (170), Shell (94), Ipiranga (76) |
| Thailand | 554 | 6% | 311 | 150 | 93 | 0 | Isuzu (141), 7-Eleven (107), Shell (74), Ford (58) |
| Kenya | 26 | 6% | 18 | 6 | 2 | 0 | Shell (18), Rubis (7) |
| United Arab Emirates | 31 | 4% | 13 | 6 | 12 | 0 | hotels, Aster pharmacies, car dealers |
| India: Karnataka | 78 | 6% | 53 | 18 | 7 | 0 | Hindustan Petroleum (50) |
| India: Tamil Nadu | 33 | 3% | 24 | 7 | 2 | 0 | Hindustan Petroleum (19) |

- Japan gains most: Japanese OSM tags most post offices with
  `operator=日本郵便` but no Wikidata, and ATP's Japan Post spider has
  `brand=日本郵便`. Samples were all the same post office on both
  sides, mostly within 15 m.
- Fuel stations are the common case elsewhere, as in Switzerland,
  with car dealers in Thailand and Berlin, and charging stations
  (ubitricity in Berlin, Red E in Massachusetts) where OSM has the
  operator but no Wikidata.
- Names in other scripts only count when both sides write them the
  same way. Thai OSM tags PTT stations `brand=ป.ต.ท.`, while ATP has
  `brand=PTT`; most of them share `brand:wikidata` and match anyway.

## Known gaps

- Only the brand is compared. ATP's `name`, such as "Garage Bussard
  SA", would often match OSM's `name` as well, but that needs fuzzy
  matching and a segmenter for CJK, see #708.
- An OSM car dealer tagged `brand=Toyota;Lexus` matches both the ATP
  Toyota and the ATP Lexus feature at that address. This is right, but
  it means several ATP features can match one OSM feature.
