# Matching by shared official IDs

Investigation note behind
[`matchers/official_id.rs`](../../src/matchers/official_id.rs), for
[#985](https://github.com/brawer/osmdiffs/issues/985).

## Short answer

Some AllThePlaces (ATP) features carry an official ID that
OpenStreetMap (OSM) also has, such as the national ID of a French
school or the National Register number of a US historic place. When
the ID is the same on both sides, the two are the same place, whatever
else differs. When both sides have an ID of the same kind and they
differ, they are different places, whatever else agrees.

Which keys hold such IDs is a small table of (ATP key, OSM key)
pairs, not code per country:

| ATP key | OSM key | what |
|---|---|---|
| `ref:FR:UAI` | `ref:UAI` | French school (Unité administrative immatriculée) |
| `ref:FR:UAI` | `ref:FR:UAI` | the same, as some OSM schools tag it |
| `ref:nrhp` | `ref:nrhp` | US National Register of Historic Places |

In Bretagne this adds 2,001 matches (+36%), nearly all schools, and
moves 50 school matches to a better OSM feature. In Massachusetts it
adds 164 historic places. No other eval region changes.

## The rule

- In [`PoiMatcher`](../../src/matchers/poi_matcher.rs), the official
  ID is compared before all other evidence. The same ID gives the rule
  `official_id`. A different ID of the same kind vetoes the match,
  like a different `brand:wikidata` does. A school with only its UAI
  now gets a matcher too.
- ATP features with an ID but no other matcher, such as historic
  places, get an `OfficialIdMatcher`, which matches only on the same
  ID.
- A candidate with the same ID scores at least 0.9, plus up to 0.1 by
  distance. Distance is measured to the nearest point of the OSM shape,
  up to 1 km: a historic district or a school campus can be large, and
  National Register points are often off by a few hundred meters.
- Values are compared without regard to case or spaces, and may list
  several IDs separated by `;`. OSM sometimes maps a lycée polyvalent
  and its lycée professionnel as one school with both IDs; then both
  ATP features may share it.
- A new match mask bit, `OFFICIAL_ID`, is set on both sides for any key
  in the table. Without it, the historic places would have no category
  at all, so the ATP import dropped them, and OSM features with
  nothing but `ref:nrhp` (often just a `building`) weren't indexed.
- As with the street address, an ATP feature that has an ID but finds
  no match isn't reported as `missing_in_osm`, and doesn't count for
  deletion coverage: OSM often has the place, just without the ID.

Swiss transit stops share IFOPT IDs too (`ref:IFOPT` ↔ `ref:sloid`),
but those stay in the transit matcher, which also knows how the IDs
of a stop and its platforms relate.

### National Register license

The ATP spider `national_register_of_historic_places` is marked with
license `Q60671452`, a work of the US federal government, and lineage
`S_ATP_GOVERNMENTS`. The pipeline didn't know this license, so it
skipped the spider as not usable for OSM. US federal works are in the
public domain, so the license is now accepted. In the 2026-11-07 ATP
run, no other spider has this license.

## Results

ATP run of 2026-11-07. Bretagne from openstreetmap.fr (2026-11-17),
the other regions as in
[2026-11-06-eval-regions.md](2026-11-06-eval-regions.md). `main` at
d93b369 against this change.

| region | matches before → after | gain |
|---|---:|---:|
| France: Bretagne | 6,141 → 8,142 | +2,001, 50 moved |
| US: Massachusetts | 7,660 → 7,824 | +164 |
| Switzerland | 66,858 → 66,858 | 0 |

In the other eval regions, no ATP feature has a key from the table,
and no OSM feature gains a match through it, so nothing can change
there. `missing_in_osm` rows and deletion candidates are the same in
all three regions. The conflate step takes as long as before.

### Bretagne

All 2,565 `official_id` matches are schools
(`enseignement_premier_second_degre_fr`). 2,160 are within 50 m and
only 10 beyond 400 m; those 10 all have the same name on both sides.
The issue counted 2,576 OSM features whose UAI is also in ATP, on an
earlier extract.

Of the 608 schools that matched by street address before, 509 had the
same UAI in OSM and 42 a different one:

- 29 of the 42 now match the OSM feature with their own UAI, such as
  the école maternelle next to the école élémentaire that took it
  before.
- 8 no longer match. All are a part of the OSM school with an ID of
  its own: 4 SEGPA sections of a collège, 3 lycées professionnels or
  sections of a lycée polyvalent, and an école primaire matched to
  another école of the same name. Most of them are not the same place
  as their host, but `located_in` it; that is left for later.
- 4 have their UAI among several in the OSM feature, and keep their
  match, now by `official_id`.
- 1 moved by address to another OSM feature without a UAI.

The other 50 − 29 = 21 moved matches went from an OSM feature without
a UAI, or from a duplicate, to the one with the school's UAI.

15 OSM schools are shared by two ATP schools, because the OSM school
lists both IDs, such as `Lycée La Fontaine des Eaux` with the IDs of
the lycée polyvalent and its lycée professionnel.

5 matches come from `ref:FR:UAI` in OSM, the rest from `ref:UAI`.

### Massachusetts

164 historic places match, of the 172 National Register numbers that
the issue found in both. On the OSM side, 149 are ways, mostly
buildings (some tagged `historic=building`, `lighthouse` or
`maritime`), 14 nodes and one relation. 126 are within 50 m of the
OSM centroid, and 13 beyond 400 m, such as the Arlington Reservoir and
its water tower, or Bancroft Tower in a park. Names agree exactly in
only a third, as the Register writes names its own way
(`Wilson, Capt. John, House and Bates Ship Chandlery` for OSM's
`Maritime Museum - Samuel Bates Ship Chandlery`), which is exactly
where an ID helps.

## Other IDs looked at

From the issue: `ref:INSEE` in `la_poste_fr` is a commune code, not
an ID of the post office. `ref:FR:SIRET` (`france_travail_fr`) had
only 2 hits in Bretagne. OSM has `ref:FR:FINESS` and
`ref:FR:prix-carburants`, but no French ATP spider sets them yet. A
new pair is one row in `OFFICIAL_IDS`, once a spider has the key.
