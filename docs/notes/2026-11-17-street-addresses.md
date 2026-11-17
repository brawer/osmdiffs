# Comparing street addresses

Investigation note behind the address normalization in
[`pipeline/conflate/evidence.rs`](../../src/pipeline/conflate/evidence.rs)
(brawer/osmdiffs#967, #980) and the street abbreviations in
[`matchers/street.rs`](../../src/matchers/street.rs) (#981, #982).

Addresses play two parts in osmdiffs. They are evidence: for every
match, `match.evidence.agreeing` in `conflated.parquet` lists the tags
on which AllThePlaces (ATP) and OpenStreetMap (OSM) agree, and
`spider-stats.parquet` and the dashboard count agreeing and differing
tags per spider. And they gate matches: a match on a phone number,
email, brand name or brand domain beyond 150 m needs the OSM street
address to appear in the ATP address
([`matchers/address.rs`](../../src/matchers/address.rs)). Addresses
are never suggested as edits.

## Short answer

Before these changes, a third of matched streets agreed, mostly
because the two sides write the same address in different ways. They
now agree for 71% of them. Nearly all of the gain comes from two
things: splitting a house number off the street, and allowing for
abbreviations such as `Str.` for `Straße`. The precision looks
perfect: in a sample of 150 streets that newly agree, all 150 are
the same street. Matching changes very little, because only a small
group of matches looks at addresses at all.

## Postcodes and house numbers (#967)

In the production run of 2026-11-15, the biggest "differing" counts
were formatting:

- US ZIP+4: `46901-4131` in ATP, `46901` in OSM. `wendys` had 1,059
  agreeing and 9,194 differing postcodes, `usps` 977 and 8,683.
- Spaces: `7591 BG` and `7591BG` in the Netherlands, where
  `aldi_nord_nl` agreed on none.
- A house number in `addr:street`: `maes_dkv` has 509 agreeing and
  19,846 differing streets, because it writes `Darmstädter Str. 236`
  and no `addr:housenumber`. 17 spiders do this for at least 300
  features each.

Postcodes are now compared by their letters and digits only, with
full-width digits made ASCII, and a nine-digit result (ZIP+4) cut to
five. When a feature has `addr:street` but no `addr:housenumber`, a
number at the end (`7`, `12a`, `3-5`) or start (`123 Main St`) of the
street is taken as the house number. A street that ends in a number,
like `Route 66`, gets split too; that only costs a little agreement.

### Japan

The 175 Japanese spiders in the ATP run of 2026-11-07 have 205k
postcodes, 123k written `060-0001` and 81k `0600001`, plus a few with
`〒`, `‐` (U+2010) or `ｰ`. Before #980, the plain seven-digit ones
could never agree with OSM. They now all compare equal. None of these
spiders sets `addr:street`, so the house-number split doesn't touch
them. They use `addr:full`, `addr:street_address` and
`addr:province` instead.

## Street abbreviations (#981)

After the house-number split, the remaining differences were mostly
abbreviations (`Str.`/`Straße`, `Rd`/`Road`, `Av.`/`Avenida`,
`N`/`North`) and compounds (`Nussdorferplatz`/`Nußdorfer Platz`).

[libpostal](https://github.com/openvenues/libpostal) knows these
well, but its Rust crates bind the C library, which needs about 2 GB
of model data and a slow start, too much for a statistics column.
Its per-language dictionaries, however, are plain text under the MIT
license: one line per word, the canonical form first, then its
variants, such as `straße|str.|strasse` or `road|rd|ro|r|roa|raod`.
`scripts/generate_street_abbreviations.py` copies five of them, for
all of libpostal's languages, into
[`src/matchers/street_abbreviations.txt`](../../src/matchers/street_abbreviations.txt)
(93 KB): street types, directionals, personal titles, qualifiers, and
separable suffixes.

Two street names are the same if they have the same number of words,
and each pair of words is equal or appears on one dictionary line.
Words are lowercased, without accents, with `ß` as `ss`, and split at
anything not a letter or digit. A separable suffix such as `platz`,
`straße` or `weg` is split off a longer word, so `Nussdorferplatz`
gives `nussdorfer`, `platz`. Sharing a line isn't transitive: `st` is
the same as `street` and as `saint`, but `street` and `saint` are not
the same, so mixing all languages gives few false agreements.

For the address gate, the OSM street must appear word by word, with
the same rule, somewhere in the ATP address text, next to the house
number check the gate already did. The older rules (the compact
street contained in the text, or a 5+ letter prefix like
`Zürcherstr.`) still apply.

## Evaluation

### Tag agreement, planet

Data: `conflated-20261109-e984a3b3.parquet`, the 58,416 `same` rows
where both sides have `addr:street`. These numbers come from a Python
prototype of the same rules; the regional runs below confirm them
for the Rust code.

| comparison | agreeing | share |
|---|---:|---:|
| production (before #980) | 20,666 | 35.4% |
| house number split off (#980) | 28,559 | 48.9% |
| plus folding accents, `ß` and punctuation only | 30,280 | 51.8% |
| plus libpostal street types | 40,665 | 69.6% |
| plus all five dictionaries and compound split (#982) | 41,546 | 71.1% |

That is 12,987 more agreeing streets than after #980. By country, the
biggest gains are Germany (7.6k to 14.7k), Austria (1.1k to 2.3k), the
US (0.9k to 2.3k) and South Africa (0.2k to 0.9k). By spider:
`maes_dkv` 5.1k to 12.3k, `deutsche_post_de` 0.45k to 1.7k, `engen`
42 to 670, `culvers_us` 253 to 762, `billa` 250 to 729.

In a random sample of 150 rows that newly agree, all 150 are the same
street. Of a sample of 150 that still differ, few are abbreviations.
The rest are:

- Polish streets named after a person, written without the first name
  or title (`Sikorskiego`, `Generała Władysława Sikorskiego`).
- Road numbers and kilometre posts (`N-332 PK: 63`).
- Corner sites, where each side names a different street.
- House numbers with a space (`Ruhlebener Str. 1 a`).
- A `ul.` prefix in Polish.
- Cyrillic against Latin script, and plain typos.

### Matching, regional runs

The same binary was run on four regions, once with the dictionaries
and once without, with ATP 2026-11-07. Every address check whose
outcome the dictionaries changed was logged.

| region | matches without → with | address checks changed | agreeing `addr:street` |
|---|---:|---:|---:|
| US: Massachusetts | 7,499 → 7,501 | 2 | 19 → 75 |
| Germany: Berlin | 6,108 → 6,108 | 0 | 302 → 477 |
| Switzerland | 66,124 → 66,126 | 2 | 658 → 851 |
| Brazil: Sudeste | 6,302 → 6,304 | 5 | 121 → 173 |

All six new matches are right: `280 S. Washington St.` and `South
Washington Street`, `112-116 Bank Rd` and `Bank Road`, `Ch Grély 18`
and `Chemin Grély`, `Av Ayrton Senna, 3243` and `Avenida Ayrton
Senna`. No match was lost or moved. The conflate step took as long as
before (Massachusetts 145 s and 144 s, Berlin 26 s and 29 s,
Switzerland 17 s both times).

The effect on matching is small because the gate only applies to
matches without a shared `brand:wikidata` that are more than 150 m
apart, and most matches share a `brand:wikidata`. Counting an agreeing
street address as evidence for a match, rather than only as a gate,
would change more. That is a separate design question.

## Left open

- `addr:street_address`, which many US spiders use instead of
  `addr:street` and `addr:housenumber`, isn't compared as evidence.
- House numbers written with a space (`7 a` and `7a`) and ranges
  (`286-290 King Street`) at the start of a street.
- Polish names: OSM spells out the person (`Tadeusza Kościuszki`),
  shops often write only the surname (`ul. Kościuszki`).
