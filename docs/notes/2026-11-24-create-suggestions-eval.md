# New places on the eval regions

Milestone 6 of the [edits.parquet plan](2026-11-23-edits-parquet.md):
creates of places that OpenStreetMap lacks, from `missing_in_osm` and
`located_in` rows, with several sources merged into one element.
Measured on the ten [eval regions](2026-11-06-eval-regions.md), with
the AllThePlaces run of 2026-11-14 split by country as in
[the milestone 3 note](2026-11-23-edits-eval.md). Milestone 5
(deletes) didn't ship, see
[2026-11-23-deletion-suggestions-eval.md](2026-11-23-deletion-suggestions-eval.md);
creates don't depend on it.

The rules are documented in
[`EDITS_PARQUET.md`](../outputs/EDITS_PARQUET.md#new-places). In short,
a missing place becomes a new node only when nothing speaks against
it: no similar OSM feature nearby, a spider whose positions are good,
no twin of the same spider, a kind that OSM maps on its own, and a
name, brand or operator.

## Two caveats of the eval setup

- **Planet spider statistics.** The position gate needs at least 20
  matched features per spider, which few spiders reach within one
  region: on regional `spider-stats.parquet`, only two spiders could
  propose creates in Kenya. As in the
  [opening-hours note](2026-11-23-opening-hours-eval.md), the counts
  below use the planet `spider-stats.parquet` of 2026-11-16, as
  production would. It lets 1,906 spiders propose creates (3,930
  don't), and 778 of all spiders have their own names (below).
- **Fallback rows.** An eval region's `conflated.parquet` also holds
  the ATP features of the country outside the region, as
  `missing_in_osm`, since the region's OSM extract doesn't cover them
  (about 330,000 rows per region). Production has the whole planet, so
  for the counts I dropped the rows without an OSM side outside the
  region's polygon before running `edits-only`.

## Counts

| region | candidates | kind | similar nearby | spider position | shared position | matched site | other | merged | creates | tenants |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Kenya | 2,080 | 1,042 | 338 | 236 | 90 | 69 | 0 | 2 | 303 | 0 |
| UAE | 4,501 | 2,084 | 788 | 1,401 | 9 | 0 | 2 | 2 | 215 | 0 |
| Thailand | 23,433 | 1 | 3,954 | 8,132 | 1,434 | 53 | 3 | 0 | 9,856 | 0 |
| Berlin | 4,265 | 3,059 | 723 | 229 | 25 | 41 | 0 | 0 | 188 | 0 |
| Switzerland | 39,284 | 29,101 | 6,204 | 1,551 | 602 | 227 | 194 | 11 | 1,394 | 3 |
| Karnataka | 6,332 | 623 | 942 | 2,325 | 12 | 0 | 4 | 0 | 2,426 | 0 |
| Tamil Nadu | 7,248 | 1,320 | 514 | 2,458 | 32 | 0 | 4 | 0 | 2,920 | 0 |
| Brazil Sudeste | 17,172 | 574 | 3,666 | 7,769 | 1,417 | 27 | 4 | 0 | 3,715 | 0 |
| Massachusetts | 18,010 | 10,870 | 1,519 | 1,785 | 301 | 16 | 10 | 69 | 3,440 | 22 |
| Japan | 280,052 | 144,214 | 39,512 | 37,241 | 3,354 | 18 | 800 | 7 | 54,906 | 6 |
| all | 402,377 | 192,888 | 58,160 | 63,127 | 7,276 | 451 | 1,021 | 91 | 79,363 | 31 |

"Other" is a coarse position without a host (46), a tenant too far
from its host (8), and no name, brand or operator after the tag rules
(967, mostly spiders with their own names and no brand). No candidate
was skipped as moved, that is for a deletion candidate of its brand
within 200 m: those within 100 m are already a similar feature by
name. 247 creates have warnings: 240 `personal_email`, 7
`sources_disagree:website`, and none `hours_outside_host`.

Japan has most of the creates, as it had most of the opening-hours
edits: OSM there lacks many chain stores, and the Japanese spiders
have good positions. The largest spiders are `seven_eleven_th`
(5,698), `seven_eleven_jp` (5,062), `ja_bank_jp` (4,658), `eneos_jp`
(4,507), `familymart_jp` (3,808), `lawson_jp` (3,193) and
`hindustan_petroleum_in` (2,491).

Few `located_in` rows become creates (31): most are postal partners,
which are services and edit their host, or charge points, which are
left out.

The edits step now takes 40 seconds for Japan, up from 25.

## Hand check

I read two samples, against the live OpenStreetMap map within 150 m
(fetched on 2026-11-24, so newer than the extract), the tags, the
spider and the source page's address:

- 80 creates of one source without a host: 5 per region from the
  first full run, and 3 per region from the final one.
- 30 creates with a host or several sources, across the regions.

As in the earlier notes, "plausible" means that nothing in the data or
on the map speaks against it; I didn't check that the place exists on
the ground.

| sample | size | plausible | wrong |
|---|---:|---:|---|
| one source, no host | 80 | 77 | a Zurich Kinderhort that `stadt_zuerich_ch` gives as `amenity=school`, next to OSM's `amenity=childcare`; a Longchamp counter in a Bongénie store named `Bongénie Sierre`, 3 m from a Manor; a Sparda-Bank branch as `Volksbank Raiffeisenbank` |
| several sources or a host | 30 | 24 | 4 wrong merges, now fixed (below); 2 doubtful: Petit Bateau corners in the Isetan and Globus department stores |

So about 96% of the creates are plausible. The tags had smaller flaws
too, fixed before the final run: names that are the spider's labels,
and `branch` values that give an address.

The merged sample is mostly `dollar_tree_ca` and `dollar_tree_us`,
which list the same Massachusetts stores, and Starbucks inside
Target stores, which OSM maps as their own nodes. The car dealer case
of the plan (Audi, Škoda and Volkswagen) has a unit test, but no
dealer merged in the eval regions: where several brands' spiders list
one dealer, OSM has the dealer already.

## Fixed along the way

Each of these came out of a hand check, and has a unit test:

- **Similar features of other kinds.** At first, conflate only looked
  for OSM features of the ATP feature's own kind, so hotels from
  `discover_swiss` that OSM maps as their restaurant 7 to 11 m away
  became new hotels. `match.evidence.similar_osm` is now a list: the
  nearest feature with an agreeing category, and the nearest of any
  category whose name shares a distinctive word (not “Hotel”,
  “Restaurant” and the like). All category keys count, so a fuel
  station that OSM tags `shop=kiosk` too agrees with ATP's
  `amenity=fuel`.
- **Tags.** Creates carry only what the place is and its names, plus
  contacts and opening hours by the rules of the modifies. Before,
  they had addresses such as `addr:city=Mount Kenya East`, and
  brand-wide `fuel:*` and `payment:*` templates.
- **Spiders' own names.** `valora` names its kiosks `kkiosk Faido EKZ
  MI` or `TKA BP Hägendorf`, `oggi_sorvetes_br` its shops `SP ITAPEVI
  CAROLINA DE ABREU`, and `maes_dkv` its stations `TAMOIL Station`.
  When less than 10% of a spider's matched names agree with OSM's, a
  create is named after the brand. Hotels (`discover_swiss` 30%,
  `radisson_hotels` 17%) stay above the line.
- **Kinds that aren't places of their own:** ATMs, post partner
  counters, street furniture such as Packstations named after their
  host (`EDEKA Markt`), transit, the virtual scooter zones of `gbfs`,
  and self-service banks such as Sparkasse “SB-Center”.
- **Parts of matched sites.** The Shell, BP and TotalEnergies spiders
  list a fuel station's shop next to the station, which OSM has as
  part of it.
- **Coarse positions.** `ptt_th` gives some stations to three
  decimals, about 100 m.
- **One spider, one place.** Two features of one spider never merge,
  not even through a third feature: BP lists some Swiss stations
  twice, and both merged with one `maes_dkv` station. Two features of
  one spider and kind within 10 m now give no create at all.
- **Little Free Library.** Its 2,335 Massachusetts creates were book
  boxes in front yards, named after their owners (`Joyce Taylor
  #19057`). The registry keeps boxes long after they are gone, and
  nobody can check them from home, so the spider is left out.
- **Standard Chartered.** `standard_chartered_bank` gives its branches
  in Kenya, India and the UAE the brand of Standard Chartered Taiwan
  (`渣打國際商業銀行`, Q62267023), so it is left out.

## Known gaps

- Creates don't carry the name-suggestion-index tags for the brand,
  which the plan names. They do carry ATP's `brand`, `brand:wikidata`
  and names, which come from the index in most spiders.
- Features in department stores that conflate doesn't see as tenants,
  such as the Longchamp and Petit Bateau corners, become creates.
- `maes_dkv` is a fuel-card network that lists other brands' stations.
  With the brand as name and without `shop=yes`, its 3 samples were
  plausible; it gives 265 creates in Switzerland.
- The similar check only finds OSM features that conflate indexes,
  so the libraries of research institutes from `worldcat` aren't
  similar to the institute that OSM has.

## Upstream

Filed from this hand check, as data problems for any consumer of
AllThePlaces:

- `standard_chartered_bank` gives every branch the brand of Standard Chartered Taiwan: [alltheplaces#20767](https://github.com/alltheplaces/alltheplaces/issues/20767).
- `stadt_zuerich_ch` tags 512 Kinderhorte `amenity=school`: [alltheplaces#20768](https://github.com/alltheplaces/alltheplaces/issues/20768).
- `volksbank_raiffeisenbank_de` gives 338 Sparda-Bank branches the Volksbank Raiffeisenbank brand: [alltheplaces#20769](https://github.com/alltheplaces/alltheplaces/issues/20769).
- `bp` lists 299 of its 602 Swiss stations twice, about 5 m apart: [alltheplaces#20770](https://github.com/alltheplaces/alltheplaces/issues/20770).
- `valora` names its shops by internal labels such as `KK Gilching EKZ`: [alltheplaces#20771](https://github.com/alltheplaces/alltheplaces/issues/20771).
- `oggi_sorvetes_br` names its shops by franchise labels such as `RJ BUZIOS CENTRO`: [alltheplaces#20772](https://github.com/alltheplaces/alltheplaces/issues/20772).
- `maes_dkv` names every station `<BRAND> Station` and tags it `shop=yes`: [alltheplaces#20773](https://github.com/alltheplaces/alltheplaces/issues/20773).
- `burger_king` puts the full address in `branch` in the US: [alltheplaces#20774](https://github.com/alltheplaces/alltheplaces/issues/20774).
- `icici_bank_in` repeats the brand in `branch`: [alltheplaces#20775](https://github.com/alltheplaces/alltheplaces/issues/20775).
- `starbucks_us` puts Target's store number in `branch`: [alltheplaces#20776](https://github.com/alltheplaces/alltheplaces/issues/20776).
- `little_free_library` names about a third of its boxes after their stewards: [alltheplaces#20777](https://github.com/alltheplaces/alltheplaces/issues/20777).

## Follow-ups

- Look at the remaining large spiders by hand once the planet run
  has creates, starting with `ja_bank_jp` and `chocozap_jp`.
