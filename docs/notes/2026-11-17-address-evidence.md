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
and their names don't contradict it. Across 11 eval regions this
adds about 1,600 matches and moves 11 to a better OSM feature. No
match is lost. In a labelled sample, 98% of the new matches are
right.

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

ATP run of 2026-11-07, Geofabrik extracts (Bretagne from
openstreetmap.fr), `main` against this change.

| region | matches before → after | gain |
|---|---:|---:|
| US: Massachusetts | 7,501 → 7,660 | +159 |
| Germany: Berlin | 6,108 → 6,157 | +49 |
| Switzerland | 66,126 → 66,858 | +732 |
| France: Bretagne | 5,535 → 6,141 | +606 |
| Brazil: Sudeste | 6,304 → 6,362 | +58 |
| Thailand | 7,558 → 7,564 | +6 |
| Japan | 74,827 → 74,832 | +5 |
| India: Tamil Nadu | 557 → 558 | +1 |
| UAE, Kenya, Karnataka | unchanged | 0 |

The `missing_in_osm` rows go down by the same amount, and deletion
candidates barely change (Berlin +2, Switzerland +1).

Switzerland gains most from `discover_swiss` hotels, which have no
brand at all, and car dealers. Massachusetts gains libraries
(`worldcat`), post offices, dealers and fuel stations whose OSM
feature has no brand. Bretagne is nearly all schools
(`enseignement_premier_second_degre_fr`, 576 of 608). Japan gains
little, because Japanese OSM rarely has `addr:street`. The run time
of the conflate step didn't change measurably.

The order of street and number doesn't matter: `71 rue DE METZ`
matches `Rue de Metz` + `71`, because the house number and the street
are looked up in the ATP address separately.

### Features with only an address

A feature with no brand and no contact had no matcher before, so it
got no row in `conflated.parquet`. With an address matcher, such a
feature that didn't match would have become `missing_in_osm`: in
Bretagne, 57k more rows, mostly schools. An address can confirm a
match, but failing to match on it doesn't say the place is missing,
so these features still get no row
([`Matcher::reports_missing`](../../src/matchers/mod.rs)). They don't
count for deletion coverage either.

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

Making `library` a generic word would remove the Minuteman match, but
also 4 right ones, such as `American Textile History Museum Library`
→ `Osborne Library`, where "library" is the only shared word.

### Schools in Bretagne

French schools carry their national ID on both sides (`ref:FR:UAI`
in ATP, `ref:UAI` in OSM), which gives a check. Of the 608 address
matches, 508 have the same ID, 42 a different one, and 58 have no ID
in OSM. Most of the 42 are a part of the same school with an ID of its
own, such as the SEGPA section of a collège, or the lycée professionnel
of a lycée polyvalent. About 15 are really another school at the same
address, mostly the école maternelle next to the école élémentaire
(`Ecole élémentaire publique Guyenne` → `École maternelle publique
Guyenne`). So for schools, about 97% are the same place.

Making school words (`école`, `primaire`, `publique`, `collège`…)
generic would remove about 10 of these, but also about 25 right
matches, because many schools are named after their commune or street,
which don't count. The UAI itself would be much stronger evidence than
the address; that is a separate idea.
