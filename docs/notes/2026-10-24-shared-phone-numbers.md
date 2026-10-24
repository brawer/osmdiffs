# Shared phone numbers in AllThePlaces, and nondescript phones

Investigation note behind `alltheplaces.nondescript-phones`
([`src/pipeline/atp/nondescript_phones.rs`](../../src/pipeline/atp/nondescript_phones.rs)),
written for a planned phone/fax matching signal. The question was how
often several ATP features share one number, and which kind of sharing
makes a number *nondescript*: it doesn't describe one place, so a match
on it should add little or no confidence. It is no reason against a
match, though.

## Method

Data: the worldwide ATP run `2026-10-10-13-32-41` (5,640 spiders, 41.1M
features). Tags: `phone`, `fax`, `contact:phone`, `contact:fax`
(`mobile` and `contact:mobile` don't occur). Values were split at `;`,
trimmed, parsed with libphonenumber (region from `addr:country`), and
kept only if valid, then compared as E.164. Phone and fax were merged:
3,531 numbers show up as a phone on one feature and a fax on another.
"Spread" is the distance between features carrying the same number.

This analysis covered every spider. The pipeline only sees spiders
usable for OSM, so its own numbers (at the end) are smaller.

## How common is sharing?

2.57M features (6.3%) have at least one valid number; 1.7% of raw
values fail libphonenumber (`1111111111`, `9350-0`, `+81 None`).

| | Features | Share of features with a number |
|---|---:|---:|
| Share a number with another feature of the **same spider** | 604,982 | 23.5% |
| … where the number also appears ≥ 1 km away | 276,605 | 10.7% |
| Share a number with a feature of **another spider** | 224,897 | 8.7% |
| … where the number also appears ≥ 1 km away | 22,952 | 0.9% |
| Number appears ≥ 1 km apart, any spider | **287,788** | **11.2%** |

1,859 of the 3,940 spiders with numbers have at least one shared number.
Most shared numbers are shared by only two features (111k of 160k).

## Two very different kinds of sharing

**Same site (75% of shared numbers have every feature within 100 m).**
One place, several features: car dealers listed once per brand or per
sales and service (Renault: 37,451 of 37,833 features share a number;
Volkswagen, BMW, Ford, Toyota and Audi look the same), a shop plus its
ATM or fuel station, Staples, Intermarché. The number is still a
correct local identifier. It points to the right place; it just can't
tell the co-located features apart, which is a job for brand matching.
**These numbers must not be marked nondescript**, or the phone signal would be lost
for almost a quarter of all features with a number.

**Hotlines and switchboards (spread ≥ 1 km).** A few numbers dominate:
the top 30 cover 111k features.

- `hdfc_bank_in`: all 25,933 branches share `+91 1800 1601`
- `gbfs`: 15,419 bike-share stations share `+81 50-3821-8282`
- `fedex`: 14,611 locations share `+1 800-463-3339`
- 41 spiders with ≥ 100 numbered features have one single number for all of them
- Regional switchboards with smaller spread: `sparkasse_de` (`+49 341 9860` × 158),
  `church_of_england_gb` (parish offices, × 36), `helsinki_servicemap_fi`
  (city switchboard × 751)
- Switzerland: BP `+41 848 848 490` × 142 (and × 30 in `shell`),
  H&M × 76, MediaMarkt × 44, Stadt Zürich `+41 44 417 77 77` × 179

These say nothing about which place a feature is, so they are nondescript.

## Across spiders

81,867 numbers appear in two or more spiders. For 91% of them, some
features from different spiders are within 1 km of each other (82%
within 100 m): mostly the same place. Typical cases:

- Multi-brand dealerships: one number in `audi`, `seat`, `skoda` and `volkswagen`.
- Shop-in-shop: Western Union desks inside Walgreens, Publix or 7-Eleven;
  a Tokyo department store number in 12 luxury-brand spiders.
- Overlapping spiders: `nandos_za` data repeated in 8 `nandos_*` country
  spiders, `bank_austria_at` vs `bankaustria_at`, `nissan` vs `nissan_eu`.

Only 3,059 cross-spider numbers have no pair within 1 km, for example a
Citi hotline that also appears once in `phillips_66_conoco_76`. Grouping
by number alone, not by (spider, number), catches those with the same
rule, so cross-spider sharing needs no separate treatment.

## The rule, and what it does in the pipeline

A number is nondescript when the bounding rectangle of all places carrying
it, across all spiders, has a diagonal of at least 1 km. The diagonal
is between 1 and about 1.4 times the largest pairwise distance, which
is precise enough for this threshold, and it doesn't depend on the order
in which places arrive. `import_atp` computes it with one external sort
of (number, S2 cell) pairs and one streaming pass, while it writes
`alltheplaces.parquet`. The pipeline reads only numbers in `+`
international format, which is how ATP writes almost all of them.

On the same ATP run, the pipeline marks 20,011 numbers as nondescript. That affects
181,698 of the 1,749,943 imported features with a number (10.4%), close
to the 11.2% above. On the Swiss subset, it marks 55 numbers (5.6% of
features with a number). `import_atp` took the same time with and
without this step (about 105 s worldwide).

## Caveats

- Some placeholders pass libphonenumber, for example `+33 1 01 01 01 01`
  (in `axa_fr`, `carrefour_fr`, `franprix_fr`). The spread rule catches
  this one, but a placeholder used at a single site would not be caught.
- Regional switchboards with a spread of only a few km (a city's central
  number, a bank's regional office) are nondescript too. That is intended:
  within a few km, the number no longer points to one place.
- Hotlines in spiders the pipeline drops for licensing reasons don't
  contribute. An OSM object tagged with such a hotline wouldn't be
  recognized as nondescript on that basis.
