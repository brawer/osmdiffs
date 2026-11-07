# The Swiss transit register outside Switzerland

Investigation note behind the home area of the Swiss register in
[`src/pipeline/conflate/deletion.rs`](../../src/pipeline/conflate/deletion.rs).
The [eval regions](2026-11-06-eval-regions.md) showed 96 Berlin
S-Bahn stations as deletion candidates
([`2026-11-05-deletion-candidates.md`](2026-11-05-deletion-candidates.md)).
This note finds the cause, measures it near the Swiss border, and
describes the fix.

## Short answer

The cause is the Swiss register (`opentransportdata_swiss`), and only
that spider. About a quarter of its stops lie outside Switzerland and
Liechtenstein. They are scattered across the neighbouring countries
and far from complete, yet the deletion rules treated them as full
coverage. Now the register only counts in its home area: Switzerland,
Liechtenstein, and the two enclaves served by Swiss public transport,
Büsingen am Hochrhein (Germany) and Campione d'Italia (Italy).
Outside that area, its stops still match OSM, but they no longer make
any OSM stop a deletion candidate. With the Swiss spider alone, the
OSM-only rows drop from 6,314 to 46 in Baden-Württemberg, from 752 to
0 in Austria, and from 110 to 0 in Berlin. Inside Switzerland, nothing
changes.

## Setup

- ATP run 2026-10-10, with the complete output of
  `opentransportdata_swiss` (103,881 features), and no other spider.
  The eval regions filtered ATP to each region's country by
  `addr:country`, but the pipeline itself takes the spider whole.
  Without other spiders, every OSM-only row below comes from the
  Swiss register.
- OSM extracts from Geofabrik: Switzerland (2026-10-20, with a strip
  of the neighbouring countries), Baden-Württemberg (2026-11-04),
  Austria (2026-11-05), Rhône-Alpes (2026-11-04), and Berlin
  (2026-11-04).
- `osmdiffs run`, stopped after `conflate`, on main at
  d57cb99 and with the fix. The scripts are in the project files under
  `swiss-transit-deletion/`.

## What the register lists abroad

| country | features | farther than 150 km from Switzerland |
|---|---:|---:|
| Switzerland | 75,280 | |
| Liechtenstein | 633 | |
| Germany | 14,940 | 5,668 |
| Austria | 9,399 | 3,898 |
| France | 3,021 | 113 |
| Italy | 606 | 20 |

Countries are by the features' `addr:country`; two PostBus stops at
the wrong coordinates claim Somalia. Most foreign features are stop
areas. Near the border, they cover the cross-border networks, such as
Verkehrsverbund Vorarlberg (1,890 stop areas), bodo (1,641), VHB
(922), Waldshut (805), Lörrach (575) and Geneva's TPG in France (369).
Farther away, they are a sample of stations with no evident pattern:
Niebüll, Grimmen, Wernigerode, Graz, Linz, and 119 stop areas in
Berlin. All of them carry the register's own `ch:1:sloid:` IDs, and
their `uic_ref` codes start with their own country's UIC code (11
Germany, 12 Austria, 13 Italy, 14 France). The exceptions are about
80 stops right at the border and in the two enclaves, whose codes
start with 85, as Swiss ones do.

## How it went wrong

Results on main, Swiss spider only:

| region | `wikidata` | `name` | `swiss_transit_id` | `swiss_transit_stop` | total |
|---|---:|---:|---:|---:|---:|
| Germany: Berlin | 0 | 110 | 0 | 0 | 110 |
| Germany: Baden-Württemberg | 2,676 | 3,407 | 230 | 1 | 6,314 |
| Austria | 617 | 2 | 133 | 0 | 752 |
| France: Rhône-Alpes | 5 | 0 | 62 | 5 | 72 |
| Switzerland extract | 62 | 37 | 1,010 | 146 | 1,255 |

There were three separate leaks:

1. **Foreign stops counted as coverage.** For the `wikidata` and
   `name` rules, a network or operator counts as covered in a country
   as soon as ATP has features with it there. The `wikidata` rule has
   no minimum count, and the `name` rule needs only three. Eight German
   stops with `operator=DB Netz AG` made all 110 Berlin rows: 97 S-Bahn
   stop areas with `operator=DB Netz AG;DB Station&Service AG`, plus
   a few stop positions and stations. In Austria, a single stop in
   Nauders with Verkehrsverbund Tirol's `operator:wikidata` made about
   600 Tirol stops candidates.
2. **Unmatched parts of stops the register has.** Transit matching is
   one to one: a register stop area matches one OSM object, and the
   platforms and stop positions next to it stay unmatched. In
   Switzerland, the Swiss rules spare these, because they look at the
   stop's IDs and at nearby register stops. Abroad, only the
   `wikidata` and `name` rules looked at them. Of the 6,314 rows in
   Baden-Württemberg, 5,209 (83%) lie within 150 m of a register stop.
   In Austria, 723 of 752 (96%) do.
3. **Old Swiss codes abroad.** The `swiss_transit_id` rule ran
   everywhere. Austrian and German stops near the border still carry
   old `uic_ref` codes starting with 85, such as `uic_ref=8581987`
   in Nauders, while the register has since given them their own
   country's codes. That made 133 rows in Austria and 230 in
   Baden-Württemberg.

In the Switzerland extract, the 130 rows that go away all lie in its
strip outside the country, in places like Lörrach, Grenzach-Wyhlen,
Konstanz, Saint-Louis, Annemasse and Gaggiolo.

## The fix

The Swiss register now has a home area. It is a list of countries
(Switzerland and Liechtenstein) plus enclaves, each a small polygon
in the source code: the OSM boundary relations of Büsingen am
Hochrhein (2785126) and Campione d'Italia (46664), widened by about
50 m so that stops on the border count, and simplified to about
25 m. Büsingen's polygon has 54 points and Campione's has 23.

- The `swiss_transit_id` and `swiss_transit_stop` rules only apply in
  the home area. A stop abroad with an old Swiss code is no longer a
  candidate for that reason, but the `wikidata` and `name` rules still
  apply to it.
- Register features outside Switzerland and Liechtenstein don't count
  for the `wikidata` and `name` rules. That includes the enclaves,
  since those rules go by country: the register's Büsingen stops
  carry VHB's Wikidata ID, and in a first version they made 411 VHB
  stops across Konstanz and the Hegau candidates.
- Register features everywhere still match OSM features, and their
  IDs and positions still count when deciding whether a stop in the
  home area is in the register. A Swiss stop at the border stays
  protected by a register stop just across it.

The table of registers holds only the Swiss one for now. When ATP
gets other national feeds, such as Germany's DELFI or France's
national feed, each gets its own home area, and the Swiss-specific
rules can become rules for each register.

## Results with the fix

Swiss spider only:

| region | `wikidata` | `name` | `swiss_transit_id` | `swiss_transit_stop` | total | before |
|---|---:|---:|---:|---:|---:|---:|
| Germany: Berlin | 0 | 0 | 0 | 0 | 0 | 110 |
| Germany: Baden-Württemberg | 0 | 0 | 45 | 1 | 46 | 6,314 |
| Austria | 0 | 0 | 0 | 0 | 0 | 752 |
| France: Rhône-Alpes | 0 | 0 | 0 | 5 | 5 | 72 |
| Switzerland extract | 9 | 0 | 970 | 146 | 1,125 | 1,255 |

- No OSM feature became a candidate that wasn't one before.
- Of the 46 rows left in the Baden-Württemberg extract, 20 are in
  Büsingen, where the Swiss rules now apply as intended. The others
  lie in the extract's strip of Switzerland: Basel Badischer Bahnhof
  (which DB runs), Riehen, and a few places along the Rhine.
- The 5 in Rhône-Alpes also lie in the extract's strip of Switzerland,
  in Meyrin and at Emosson.
- Matches don't change: 1,060 register features match in
  Baden-Württemberg, with and without the fix.

## Known gaps

- The `wikidata` rule has no minimum count, unlike the `name` rule.
  A single feature of a spider that covers a few places abroad is
  enough to make its brand look covered in the whole country. This
  fix only closes that gap for the Swiss register.
- Other enclaves will matter for other registers. Jungholz and the
  Kleinwalsertal belong to Austria but are reachable by road only from
  Germany. Whether they belong to DELFI's home area needs checking
  once ATP has DELFI.
- The enclave polygons are fixed in the source code. If a boundary
  ever moves, they need to be redrawn from OSM.
