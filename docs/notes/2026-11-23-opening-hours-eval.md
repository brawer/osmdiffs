# Opening-hours edits on the eval regions

Milestone 4 of the [edits.parquet plan](2026-11-23-edits-parquet.md):
`opening_hours` modifies, with the equivalence table as tests and a
per-spider gate, measured on the ten [eval regions](2026-11-06-eval-regions.md)
with the AllThePlaces run of 2026-11-14, split by country, as in
[the milestone 3 note](2026-11-23-edits-eval.md).

## The gate, regional and planet-wide

The gate reads each spider's opening-hours agreement from
`spider-stats.parquet`. In production that file covers the planet; in
an eval run it covers only the region, so most spiders have fewer than
10 pairs there and make no edits at all (none in Kenya, Karnataka or
Tamil Nadu). To see what production would do, the counts below use a
`spider-stats.parquet` built from the per-spider opening-hours counts
and top-value shares of the 2026-11-02 planet run. That file lets
through 1,249 spiders; 471 agree with OSM too rarely, 123 give one
schedule for nearly every branch, and 746 have too few pairs.

Spiders that the gate blocks on the planet include `wendys` (9% of
pairs agree), `taco_bell_us` (37%), `total_energies` (35%),
`dunkin_us` (59%) and `starbucks_us` (50%), which mostly give the
hours of the drive-through or of another service. The template rule
blocks `maes_dkv` (32,267 fuel stations, all `24/7`, agreeing 86%) and
`shell` (31,114, all `24/7`): before it, they made 413 and 64 of the
additions in Switzerland, and a Shell station in Isiolo, Kenya, that
OSM has as `07:00-22:00` was to become `24/7`.

Decided 2026-11-23: evals normally run on regional statistics. Planet
counts are only worth building when a rule depends on a per-spider
threshold, as here, and only to estimate what production will do.

## Counts

Counts are of `opening_hours` changes, by reason, with the planet gate.
"Disagree" counts features whose sources gave different hours, which
get the warning `sources_disagree:opening_hours` and no change.

| region | changes | `missing_in_osm` | `small` | `over_60_min` | `other_days` | `needs_review` | disagree |
|---|---:|---:|---:|---:|---:|---:|---:|
| Kenya | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| UAE | 150 | 135 | 5 | 8 | 1 | 1 | 1 |
| Thailand | 57 | 55 | 2 | 0 | 0 | 0 | 0 |
| Berlin | 649 | 217 | 227 | 153 | 41 | 11 | 1 |
| Switzerland | 2,755 | 1,478 | 694 | 397 | 157 | 29 | 10 |
| Karnataka | 108 | 95 | 9 | 3 | 0 | 1 | 1 |
| Tamil Nadu | 47 | 42 | 5 | 0 | 0 | 0 | 0 |
| Brazil Sudeste | 122 | 105 | 5 | 4 | 5 | 3 | 0 |
| Massachusetts | 2,338 | 1,869 | 269 | 149 | 44 | 7 | 4 |
| Japan | 29,835 | 27,787 | 1,045 | 758 | 168 | 77 | 5 |

No change in the ten regions has the reason `public_holidays`: few
spiders give a `PH` rule that differs from their Sunday. The other
changes of the file (contact, Wikidata, postal partners) are the same
as without this milestone, change for change, in all ten regions.

Japan is large for the same reason as in milestone 3: OSM there has
few `opening_hours`, and `lawson_jp` (6,912), `familymart_jp` (4,659),
`eneos_jp` (4,100) and `zensho_jp` (2,518) add most of them. With the
regional gate instead, the regions get 0 to 1,130 changes (Japan
40,033), and Brazil Sudeste gets 757 additions from
`banco_do_brasil_br`, which isn't in the 2026-11-02 planet counts.

The step stays at 1 to 5 seconds per region, 25 for Japan.

## Hand check

I read a sample stratified by region: 3 changes per region and reason,
107 in all. For each, I compared the change with the OSM element's
tags, the spider, the rule and the distance. As in milestone 3, I did
not open the store pages, so "plausible" means that nothing in the
data speaks against it.

| reason | sample | plausible | doubtful |
|---|---:|---:|---|
| `missing_in_osm` | 27 | 27 | |
| `small` | 26 | 26 | |
| `over_60_min` | 21 | 18 | Cotton On in Dubai at `09:00-17:00`, from a page of the Australian site; Amplifon Belgaum at `10:00-14:00`; a Coop Vitality matched by `brand_name` whose hours differ on every day |
| `other_days` | 16 | 14 | Jacques' Wein-Depot open `Mo 00:00-24:00` (a spider bug, below); a Postbank closed on Thursday |
| `needs_review` | 17 | 17 | |

The 17 `needs_review` changes are all OSM values that a person should
look at: open ends (`00:00-24:00+`), comments, `PH -1 day`, `||`
fallbacks, and typos such as `Su 08:00=18:00`, `Mo-Su 11:00,21:00` or
`Mon-Sun 7:30-0:00`, where AllThePlaces has the value right.

## Fixed along the way

The first regions showed changes that only rewrote OSM's notation, or
lost what OSM meant. These rules now prevent them, each with a unit
test:

- OSM's additional rules after a comma, `Mo-Fr 09:00-20:00, Sa
  09:00-18:00`, add to their days, as the specification says. Before,
  such values went to `needs_review`: 31 of 44 in Berlin.
- A holiday rule that OSM writes together with weekdays, `Su,PH
  10:00-16:00` or `Mo-Su,PH 10:00-22:00`, follows those days when
  AllThePlaces changes them, rather than keeping the old times: a
  Cotton On was to get `Mo-Su 09:00-17:00; PH 10:00-23:00`.
- Weeks are compared, and holidays followed, with times past midnight
  joined to the day they start on: 28 Zensho restaurants in Japan with
  `Mo-Su,PH 07:00-01:00` were to get `PH 00:00-01:00,07:00-24:00`.
- Such a holiday rule is written together with the week's last rule
  when their times agree, `Su,PH 08:00-19:00`, rather than as `Su
  08:00-19:00; PH 08:00-19:00`.
- An AllThePlaces value that is closed every day is not an edit (22
  Lidl stores in Germany are `Mo-Su closed`); that's for deletes.

## Upstream

- `lidl` skips days with special hours and so writes them as closed:
  138 stores in Saxony are `Mo-Tu,Th-Sa`, after Buß- und Bettag on
  Wednesday. Filed as
  [alltheplaces#20747](https://github.com/alltheplaces/alltheplaces/issues/20747).
  The gate doesn't catch it, since most Lidl stores agree with OSM.
- `jacques_wein_depot_de` gives `Mo 00:00-24:00` for 108 of its 369
  stores, most likely a closed Monday. Filed as
  [alltheplaces#20748](https://github.com/alltheplaces/alltheplaces/issues/20748).

Both turn into `other_days` changes that close a shop on a weekday or
open it around the clock. A review can start with `small` and
`over_60_min`, and look harder at `other_days`.

## Open questions

- **`other_days` from spiders with holes.** Should a week in which
  AllThePlaces closes a weekday between two open ones, which OSM has
  open, get a warning? It would catch the Lidl case, but also shops
  that close on Wednesdays.
