# Street address as matching evidence

Investigation note behind the `address` rule in
[`matchers/poi_matcher.rs`](../../src/matchers/poi_matcher.rs). It
builds on the address comparison described in
[2026-11-17-street-addresses.md](2026-11-17-street-addresses.md).

## Short answer

Until now, an AllThePlaces (ATP) feature matched an OpenStreetMap
(OSM) feature only on `brand:wikidata`, a shared phone number, email
or website, the brand name, or the brand's web domain. Many OSM
features have none of these: a car dealer mapped as `shop=car` with
just an address, a library, a hotel, a fuel station without a brand.
They now match when their street address agrees with the ATP address
and their names don't contradict it. Across the 10 eval regions this
adds 988 matches and moves 9 to a better OSM feature. No match is
lost. In a labelled sample, 98% of the new matches are right.

This is deliberately not a geocoder. The rule only compares two
addresses that are already within 150 m of each other, with the
normalizations from #980 and #982, and never parses or looks up an
address on its own.

## The rule

The address rule is the last kind of evidence, tried only when none of
the others applies. A candidate matches when all of these hold:

- The OSM `addr:street` and `addr:housenumber` appear in the ATP
  address (the existing check in
  [`matchers/address.rs`](../../src/matchers/address.rs)).
- The main categories are equal or a known pair, as for the other
  kinds of evidence.
- The OSM feature has no `brand:wikidata` and no brand name of its
  own. A Honda dealer doesn't become a Toyota dealer because it shares
  an address.
- The names fit (see below).
- The OSM feature is within 150 m, or within 30 m if it is a shop
  without a name. Distances are measured to the nearest point of the
  OSM shape, so a big dealer lot still counts.

The factor is 0.9, below brand name (0.96) and contact (0.98), so a
stronger kind of evidence wins when there is one.

### Names

Names are split into words, without accents and case. Words that are
part of either side's address (the city, the street) are dropped.
"Telling" words have at least 4 letters and aren't generic, like
`bank`, `hotel`, `garage`, `pizza` or `station`. An OSM name fits if:

- the OSM feature has no name;
- or it has no telling words, and equals the ATP name or brand;
- or it shares a telling word with the part of the ATP name that isn't
  the brand;
- or the ATP name is just the brand, and the OSM name shares a brand
  word;
- or all words of the OSM name are brand words.

Without the name check, about half of the new matches in malls and
shopping streets were wrong. Without dropping address words, `Berlin`,
`Boston` or `Phuket` made unrelated shops look alike.

### House numbers and street prefixes

While at it, the evidence comparison now treats `7 a`, `7-a`, `7/a`
and `7a` as the same house number (`3-5` stays a range), and splits a
house number like `1 a` off the street. The street comparison drops a
leading Polish `ul.` or `ulica`. These didn't change any match in the
eval regions, but make more addresses agree in `match.evidence`.

## Results

ATP run of 2026-11-07, Geofabrik extracts, same binary with the rule
off and on.

| region | matches off → on | new | moved |
|---|---:|---:|---:|
| US: Massachusetts | 7,501 → 7,660 | +153 | |
| Germany: Berlin | 6,108 → 6,157 | +44 | 4 |
| Switzerland | 66,126 → 66,855 | +723 | 5 |
| Brazil: Sudeste | 6,304 → 6,362 | +56 | |
| Thailand | 7,558 → 7,564 | +6 | |
| Japan | 74,827 → 74,832 | +5 | |
| India: Tamil Nadu | 557 → 558 | +1 | |
| UAE, Kenya, Karnataka | unchanged | 0 | |

Switzerland gains most from `discover_swiss` hotels, which have no
brand at all, and car dealers. Massachusetts gains libraries
(`worldcat`), post offices, dealers and fuel stations whose OSM
feature has no brand. Japan gains little, because Japanese OSM rarely
has `addr:street`. The run time of the conflate step didn't change
measurably.

## Precision

A random sample of 192 new matches, 40 per large region (60 in
Switzerland) and all of them in the small regions, had 188 right
(98%). All 9 moved matches are right, for example a Steinecke bakery
moving from an OSM node 381 m away to the bakery at the same address.

Two of the four wrong ones are fixed in the final rule: a Toyota
dealer matching another Toyota dealer at the same address, and a
Diesel store matching an unnamed clothes shop in a Geneva mall. The
30 m limit for unnamed shops removed all 11 matches into that mall,
at the cost of a few right matches to unnamed dealers and fuel
stations 45–90 m away. Two remain:

- `Minuteman Library Network` → `Goodnow Public Library`. They share
  only the generic-ish word "library", at the same address.
- `kkiosk … EKZ Coop` → the `Coop` supermarket it sits in. This is a
  kiosk inside its host, which should rather be `located_in`.
