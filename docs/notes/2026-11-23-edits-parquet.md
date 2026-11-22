# Plan: edits.parquet

Status: plan, agreed on 2026-11-23. Implementation follows the milestones below, one PR each.

## Goals and non-goals

We replace today's edits step with a new step that reads `conflated.parquet` and writes `edits.parquet`: one GeoParquet
row per proposed OpenStreetMap edit, carrying everything needed to turn that row into an OSMChange element or a
MapRoulette task without going back to the OSM planet.

In scope for the first version:

- **Create** a missing feature, from `missing_in_osm` and `located_in` rows.
- **Delete** an obsolete feature, from `deletion_candidate` rows.
- **Modify tags** on an existing feature, from `same` rows, for `email`, `phone`, `fax`, `website` and `opening_hours`,
  the `:wikidata` IDs of brand, operator, network and `post_office:brand`, and the service tags of postal partners.
- Merge several `conflated.parquet` rows into one edit when they are about the same place, such as several car brands
  listing one dealership.
- Treat minor notation differences as agreement, so we never propose an edit that only reformats a value or throws away
  OSM detail (public holidays, seasons, comments).
- Publish the file with a documented format and a prominent "experimental, may change without notice" disclaimer.

Out of scope: tiles and any visualization, writing OSMChange or MapRoulette files ourselves (we document how instead),
other tags (names, addresses, brand tags, `start_date`/`end_date`), and moving features whose position differs. The
current `suggest_edits` step and its tiles are deleted.

## Current state

`conflated.parquet` already has most of what an edit needs: the OSM element's type, ID, version, full tags,
`way_members` (node refs in order) and `relation_members` (type, ID, role), plus `match.relation`, the deciding rule,
`agreeing_tags`/`disagreeing_tags`, `local_share` and `importance`. A node's `osm_geometry` is a float64 WKB point taken
straight from the planet file, so printed with 7 decimals it gives back OSM's coordinates exactly. One thing is missing
for edits: a link from each ATP row to the store page it came from (ATP's `@source_uri`, which `import_atp` drops today;
see milestone 2).

The code to delete is small and self-contained:

| Path | What it is |
| --- | --- |
| `src/pipeline/edits.rs` (489 lines) | `suggest_edits` step, writes GeoJSON tile layers |
| `src/edit_suggesters/` (730 lines, with `post_partner.rs` from [#1031](https://github.com/brawer/osmdiffs/issues/1031)) | `EditSuggester` trait and `PoiEditSuggester` |
| `src/pipeline/tiles.rs` (518 lines) | `render_tiles`, only used for `diffed-places.pmtiles` |
| `EDITS_TILES` in `src/pipeline/upload.rs` | the `edits-tiles` resource in `datapackage.json` |
| mentions in `docs/outputs/CONFLATED_TILES.md` | links to `edits.pmtiles` |

The comparison helpers we want to keep live elsewhere and stay: `utils/opening_hours.rs` (`same_schedule`, `difference`,
`has_rich_syntax`) and `matchers/contact/` (phone, email and URL normalization, `Nondescript` for shared hotlines and
central addresses). The value-agreement logic in `edit_suggesters/poi.rs` moves into the new step before that module is
deleted.

One doc conflict: `docs/TECHNICAL_DESIGN.md` says the project "doesn't apply bulk edits to OpenStreetMap, and has no
plans to". That can stay true (we produce data, humans run imports), but the wording should mention that the file is
meant to feed imports that follow the OSM Import Guidelines.

## Data model

One row is one edit to one OSM element, or one new element. Several `conflated.parquet` rows can feed one edit; they are
listed in `sources`. The proposed schema reuses the `osm` struct of `conflated.parquet` so the two files read alike:

```text
id            UTF8              non-null, stable across weeks (see below)
action        UTF8              non-null: "create" | "modify" | "delete"
topics        LIST<UTF8>        non-null: "contact", "opening_hours", "new_poi", "obsolete_poi", ...

osm           STRUCT            null exactly when action = "create"
  type, id, tags, modified{timestamp, version},
  way_members, relation_members          same as conflated.parquet

changes       LIST<STRUCT>      non-null; for create, every tag of the new element
  key         UTF8              non-null
  old         UTF8              nullable, null = OSM has no such tag
  new         UTF8              nullable, null = remove the tag
  reason      UTF8              non-null: "missing_in_osm" | "differs" | "obsolete" | ...

sources       LIST<STRUCT>      non-null, at least one
  spider      UTF8
  fetched     TIMESTAMP(ms)
  atp_ref     UTF8              nullable, ATP's ref for the feature
  source_url  UTF8              nullable, a page a human can check: website or
                                contact:website, else @source_uri
  relation    UTF8              match.relation of the conflated row
  rule        UTF8              match.evidence.rule
  score       FLOAT             nullable
  distance_m  FLOAT             nullable

geometry      BINARY (WKB)      the new point for create; the OSM shape otherwise
atp_geometry  BINARY (WKB)      nullable, ATP's point(s), for display
host          STRUCT            nullable, for creates inside another place
  type, id, name                the host OSM element, and its name for reviewers
warnings      LIST<UTF8>        non-null, may be empty: checks that failed, for reviewers
importance    FLOAT             non-null, 0.0 to 1.0, max over the sources
```

Design choices behind it:

- **Changes as a diff, not as the final tag set.** The resulting tags are `osm.tags` with `changes` applied, which a
  consumer computes in one line. Keeping `old` next to `new` lets a reviewer, or a MapRoulette task, show exactly what
  changes, and lets a tool refuse to apply an edit when the live OSM value no longer equals `old`.
- **Member refs, not member geometry.** An OSMChange `modify` of a way needs its `<nd ref>` list and version, but not
  its nodes' coordinates; a relation needs its members. Both are already in `conflated.parquet`. Only nodes need
  coordinates, and their `geometry` point already holds them exactly (float64, printed with 7 decimals).
- **Stable `id`.** MapRoulette challenges are rebuilt weekly, and a task that keeps its ID keeps its status. For modify
  and delete the ID derives from `action`, `osm.type`, `osm.id` and the sorted keys in `changes`. For create it derives
  from the spiders and their `atp_ref`s, so a new store keeps its ID while it is still missing.
- **Combining sources** (generalized from [#1031](https://github.com/brawer/osmdiffs/issues/1031)). Set-valued keys add
  up across sources, such as `post_office:brand=DHL;DPD` for a kiosk that is a partner of both, or
  `brand=Audi;Volkswagen` for one dealer. Their `:wikidata` keys stay aligned value by value, and are made to line up
  where OSM's don't (see Modify: Wikidata IDs). Single-valued keys whose sources disagree are dropped. Sources are
  combined in a fixed order (spider, then ATP ref), so the same input always gives the same file;
  [#1031](https://github.com/brawer/osmdiffs/issues/1031) found that main gave different tags for 6 Berlin features
  between two runs. Each source may only edit the keys its kind allows: contact and opening hours for a business matched
  as `same`, service tags for a service.
- **One edit per element per week.** All tag changes to one OSM element are one row, even when they come from different
  spiders. A delete and a modify of the same element never both appear; delete wins only if no source matched the
  element as `same`.
- **Sorting** along the Hilbert curve, as in `conflated.parquet`, for better compression and so that area-based
  challenges read contiguous row groups.

## Edit kinds and their rules

Every edit passes three common gates first: the dataset may be used for OSM, which `import_atp` already guarantees
(`is_usable_for_osm` in `src/pipeline/atp/mod.rs` drops a spider unless `use:openstreetmap` says yes, its
`license:wikidata` is OSM-compatible, or it is first-party brand data, so no other row reaches `conflated.parquet`); the
spider's run this week looks healthy (feature count not collapsed against last week, per `spider_churn`); and the value
isn't one that `Nondescript` flags as shared across many places.

### Create

- **From** `missing_in_osm` rows, and `located_in` rows (a pharmacy inside a supermarket, a charge point at a station
  that OSM doesn't map on its own).
- **Merge** rows into one new element when they lie within about 30 m, have compatible main categories
  (`matchers/category.rs`), and share a phone or email address, so they are one business; proximity or a shared street
  address alone is not enough. This is the car dealership case: Audi, VW and Skoda spiders listing one dealer become one
  `shop=car` with `brand=Audi;Škoda;Volkswagen` and the dealer's own `name`. Conflicting values for the same key drop
  that key rather than picking one.
- **Tags** are ATP's tags minus ATP-internal ones (`@spider`, `@source_uri`, ...), normalized the way we would write
  them in a modify (below), plus the name-suggestion-index tags for the brand. We drop the spider's ref (a store number
  of the chain, see [#908](https://github.com/brawer/osmdiffs/issues/908)) and any `opening_hours` that doesn't parse.
- **Position** is ATP's point (after the camera-coordinate correction). For `located_in`, see the next section.
- **Skip** spiders whose matched features are often far from their OSM match (a per-spider position-quality threshold
  from `spider-stats`), and any create where an OSM feature of the same category lies within 50 m, since that is more
  likely a missed match than a missing place.

### Located in another place

`located_in` rows (12,909 in the 2026-11-16 run, in 11,844 hosts, plus 5,733 `department` rows in 5,094 hosts) become
creates of a new node, never edits of the host.

- **Position:** measured on the 2026-11-16 run's 18,642 rows. Inside the host polygon (7,926, 43%): ATP's point. Outside
  but within 100 m (2,100): moved along the line from ATP's point toward the polygon's point-on-surface (not its
  centroid, which can lie outside an L-shaped mall), to 2 m inside the edge. Host is a node (7,977, 43%): ATP's point if
  within 30 m (5,167), else 2 m from the host node toward ATP's point if within 100 m (1,981). Farther than 100 m (639
  polygons, 829 nodes): no edit, since the host or ATP's position is likely wrong.
- **Tags:** the tenant's own tags only. We don't add `level`, which ATP rarely knows, nor any `located_in` tag.
- **Host:** a new nullable `host` column (type, id) names the OSM feature it sits in, so a reviewer sees both and a tool
  can check the host still exists.
- **Merging:** two ATP rows for the same tenant in one host (a pharmacy listed by two spiders) become one create, as for
  other creates.
- **Charge points** (the `charging_station` rule) are left out of the first version: ATP has about 520,000 devices, OSM
  maps few individual charge points, and adding one node per device to every station would change mapping practice more
  than any other edit here. Decided: no charge point edits for now.

### Services offered by a host

Some ATP kinds are a service inside another business, which OSM usually records as a tag on that business rather than as
a feature of its own. For these, the edit is a **modify of the host** that adds the tag, and never a create. The kinds
and their tags form a hand-made table:

| ATP kind | Tag on the host | Separate feature instead when |
| --- | --- | --- |
| `amenity=money_transfer` (Western Union, MoneyGram agents) | `money_transfer=western_union` / `moneygram` | never |
| `amenity=atm` in a bank of the same brand | `atm=yes` | OSM has an `amenity=atm` of that brand or operator within 30 m |
| Postal partner (`post_office=post_partner`, matched since [#1031](https://github.com/brawer/osmdiffs/issues/1031)) | `post_office=post_partner`, `post_office:brand` and `post_office:brand:wikidata` | OSM maps the counter as an `amenity=post_office` of the network (then the partner is `same` as that office) |

- **No edit** when the host already has the tag, or OSM has a separate feature for the service nearby (the right-hand
  column).
- **Service or tenant.** Which way a `located_in` row goes depends on the ATP feature's kind, not on the rule: a service
  (a postal partner today) modifies its host, a tenant (a pharmacy, a Starbucks) becomes a new node. A `same` row of a
  service only edits its service tags; it never suggests the host's opening hours, phone or website, which may be the
  counter's.
- **Hours check.** When a service or tenant is open on some day at a time its host's OSM `opening_hours` say the host is
  closed, the edit still goes out, with the warning `hours_outside_host`. Either the host is wrong, or OSM's host hours
  are out of date, or ATP's are; a human, perhaps on the ground, decides. Values that don't parse skip the check. A
  pharmacy with its own street entrance can legitimately open longer, so this is a warning, not a filter.
- **Brand values** compare by their distinctive words, so `Die Post` and `Die Post AG` agree and only a missing Wikidata
  ID gets added. Such ID-only changes get their own reason (`wikidata_only`), since they are 347 of the 954 Swiss
  partner edits and a reviewer may want them separately. An operator such as `Empresa Brasileira de Correios e
  Telégrafos` is never suggested as a brand.
- **No host found, no edit.** In the 2026-11-16 run, 122,624 money transfer agents (79,153 MoneyGram, 43,422 Western
  Union) are `missing_in_osm`, and the plan's create rule would have turned each into a new node. These kinds are
  excluded from creates.
- **Missing hosts.** A service without a host in OSM gives no tag edit, but it can give a create of the host, in two
  cases. If ATP also lists the host and it is `missing_in_osm` (a FedEx OnSite whose Walgreens ATP has), the host's own
  create carries the service tags too, an ordinary create. Otherwise, when ATP gives the host's name, we propose a new
  node with that name and the service tags but no `shop` or `amenity` tag, which we can't know; it gets reason
  `needs_review` and the warning `host_kind_unknown`, for humans only (a survey task: which kind of business is this?),
  never for imports. Unnamed services, such as FedEx's 4,708 ShipCenters and agents named after the network, get
  nothing. Whether this is too noisy is measured per kind before it ships; it comes after host-service modifies.
- **Needs matching work first.** Postal partners have hosts since
  [#1031](https://github.com/brawer/osmdiffs/issues/1031). The other kinds don't yet: ATMs get no rows at all, and the
  agents are `missing_in_osm`. `conflate` needs a host rule for them, such as the nearest business whose name or address
  agrees with the agent's. That makes host services their own milestone, after creates.

### Delete

- **From** `deletion_candidate` rows, never from rows where any spider matched the element.
- **What gets deleted** depends on the element. A node whose tags are all POI tags is deleted outright (OSMChange needs
  only type, ID and version), but only if no way or relation uses it; a node that is a way vertex loses its POI tags
  instead, and one in a relation also needs a modify of that relation. A way or relation that also carries `building=*`,
  `area=*` or similar non-POI tags gets a **modify** instead that removes the POI tags (`shop`, `amenity`, `name`,
  `brand*`, contact tags, `opening_hours`, ...) and keeps the building. A bare POI way is deleted together with those of
  its nodes that are untagged and used by no other way or relation, as the reference-count pass tells; its shared or
  tagged nodes stay, and if the way is a relation member, that relation gets a modify too.
- **The reference-count pass.** An OSM node doesn't say which ways or relations use it, so the rules above can't be
  decided from the deletion candidate alone. The edits step first collects the IDs it may delete: the candidate nodes,
  the candidate ways, and the nodes of those ways. It then reads every way and relation of the planet file once, and for
  each collected ID counts how many other ways and relations refer to it, remembering which relations those are, since
  they need a modify. Whether a node has tags of its own is noted in the node pass `import_osm` already makes, so nodes
  aren't read again. The result is a small lookup table (ID, used by ways, used by relations, tagged) that the delete
  rules consult. Estimated cost: 10 to 15 minutes per weekly planet run, about 3 to 4% of the pipeline.
- **Addresses stay.** `addr:*` describes the place, not the business, so removing POI tags never removes them. A POI
  node with an address becomes an address node rather than being deleted, unless an enclosing building already has the
  same address.
- **Tagged member nodes.** A bare way with any tagged node (an entrance, a door) is not deleted: it loses its POI tags
  and gets `reason = needs_review`, so a person decides whether it is a building. We don't add `building=yes` ourselves,
  since mall units (`indoor=room`) have doors too. In the Swiss extract, 19 of 138 bare candidate ways have a tagged
  node, 5 of them an entrance or door.
- **Hosts still in use.** Since [#1031](https://github.com/brawer/osmdiffs/issues/1031), `conflate` counts a
  `located_in` host as matched, so a REWE that DHL still lists a counter in is no deletion candidate at all (13 such
  hosts in Berlin, 4 Walgreens in Massachusetts, 6 in Switzerland, 8 in Thailand). We change that: `conflate` keeps the
  deletion candidate and records which spiders still place something in it, and the edit goes out with the warning
  `host_still_listed`, naming those spiders, so a reviewer looks before deleting. The same holds for tenants, such as a
  Starbucks still listed in a Target that Target no longer lists.
- **Guards:** skip if OSM was edited after the spider's fetch (someone likely surveyed it), or if `local_share` is below
  the current 0.5. Later: require the element to be a candidate in two consecutive weekly runs, read from last week's
  `edits.parquet`.

### Modify: contact tags

- **Keys:** `phone`, `fax`, `email`, `website`. If OSM uses the `contact:*` form for a key family, we write there
  instead and never add a second, parallel key.
- **Add** a missing value; **replace** a value only when it disagrees under the normalized comparison and the ATP value
  is store-specific (not a brand hotline, central address or brand homepage replacing a store page). A generic OSM
  website, such as the brand's homepage or one that Nondescript flags as shared by many places, gets replaced by ATP's
  store-specific page, for human reviewers.
- **Multiple values** (`;`-separated) compare as sets; we add ATP's value to OSM's set rather than replacing the set
  when OSM has more than one.

### Modify: Wikidata IDs

For `brand`, `operator`, `network` and `post_office:brand`, we add or fix the matching `:wikidata` key so its IDs line
up with the names, value by value. We never change the names themselves.

- **Missing ID:** OSM has the name but no ID, and an ATP source gives that name (after folding) with an ID. In the
  2026-11-16 file, about 172,000 `same` rows have no OSM `brand:wikidata` where ATP has one: 61,068 by `brand_in_name`,
  48,815 by `brand_name`, 35,836 by `contact`, 11,718 by `address`.
- **Lists that don't line up:** 436 OSM features in the file have a different number of `brand` and `brand:wikidata`
  values. We rebuild the ID list in the order of the names, but only when every name resolves to an ID (from the ATP
  sources, the name-suggestion-index for the country, or the former-brands table). An OSM ID that matches none of the
  names is not removed; the row gets the warning `wikidata_unmatched` instead.
- **`brand_in_name` rows** have no `brand` at all in OSM, and an ID without a name is poor tagging. These get `brand`
  plus `brand:wikidata`, which is a name edit after all, so they wait for the per-rule precision check before going out.
- Changes that only touch IDs carry the reason `wikidata_only`, as for postal partners.

### Modify: opening hours

- Split OSM's value into its weekday rules and its other rules. Rules for holidays (`PH`, `SH`) and fixed dates (`Dec 24
  off`, `easter off`) are exceptions on top of the week, so we keep them verbatim and edit the weekday part: OSM `Tu,PH
  09:00-11:00` with ATP closed on Tuesday becomes ATP's week plus `PH 09:00-11:00`. Only rules that change the week
  itself (seasons like `Apr-Oct`, week numbers, `sunrise`, comments) block a machine-made value; those rows get `reason
  = needs_review` and no `new` value, for a human task instead of an automatic fix.
- The new value is ATP's weekly schedule plus OSM's existing `PH` and `SH` rules, unless ATP has its own. Details in the
  next section.
- Per spider, only when its opening hours agree with OSM often enough on matched pairs (from `spider-stats` kinds),
  which screens out spiders that publish one template schedule for every branch.

## Recognizing minor differences

The rule is: no edit when the values mean the same, and an edit never removes detail OSM has and ATP lacks. For opening
hours, that means comparing weekly schedules and handling holiday rules separately.

| Case | OSM | ATP | Result |
| --- | --- | --- | --- |
| Notation only | `Mo-Fr 08:00-18:00; Sa 08:00-18:00` | `Mo-Sa 08:00-18:00` | no edit |
| Always open | `24/7` | `Mo-Su 00:00-24:00` | no edit |
| OSM has PH, ATP doesn't | `Mo-Sa 08:00-20:00; Su,PH 10:00-16:00` | `Mo-Sa 08:00-20:00; Su 10:00-16:00` | no edit |
| ATP folds PH into Sunday | `Mo-Sa 08:00-20:00; Su 10:00-16:00; PH off` | `Mo-Sa 08:00-20:00; Su,PH 10:00-16:00` | no edit: ATP's PH equals its Sunday, so we read it as a summary and keep OSM's |
| Week differs, OSM has PH | `Mo-Fr 08:00-18:00; PH off` | `Mo-Fr 07:30-18:30` | `Mo-Fr 07:30-18:30; PH off` |
| ATP has its own PH rule | `Mo-Fr 08:00-18:00` | `Mo-Fr 08:00-18:00; PH 10:00-14:00` | add the PH rule |
| OSM richer than parser | `Mo-Fr 08:00-18:00; Dec 24 off` | anything | no edit |
| Phone format | `044 123 45 67` | `+41 44 123 45 67` | no edit |
| Phone missing | (none) | `+41441234567` | add `+41 44 123 45 67` (international, grouped) |
| Website noise | `http://www.example.ch/` | `https://example.ch/?utm_source=x` | no edit |
| Email case | `Info@Example.ch` | `info@example.ch` | no edit |

The "ATP folds PH into Sunday" rule is deliberately narrow: ATP's PH rule counts as its own only if it differs from
ATP's Sunday rule. When OSM has no PH rule at all and ATP's PH equals Sunday, we add nothing for PH either.

Each opening-hours change records its `difference` class (`small`, `over_60_min`, `other_days`) in `changes.reason`, so
a challenge can start with the big differences and leave the 15-minute ones for later. All of these cases become unit
tests before the rules are written.

## Generating OSMChange and MapRoulette from the file

Each row maps to one element in either format without any other input. The table below only checks that the schema
carries enough; we write no examples or converters now. Integration with MapRoulette, StreetComplete and the like, or
editing in our own webapp, is a later step that we build ourselves.

| Action | OSMChange | MapRoulette |
| --- | --- | --- |
| create | `<create><node id="-N" lat lon>` with the `new` values of `changes` | cooperative challenge, change-file task with that OSMChange embedded |
| modify | `<modify>` of the whole element: `osm.version`, `osm.tags` with `changes` applied, plus `lat`/`lon` (node), `<nd ref>` from `way_members` (way) or `<member>` from `relation_members` (relation) | cooperative challenge, tag-fix task: `setTags` for each `new`, `unsetTags` for each null `new` |
| delete | `<delete><node id version>` | cooperative challenge, change-file task |

A tool applying a modify should first check that the live element's version still equals `osm.version`, or at least that
each live tag still equals `old`; the docs will say so. The exact MapRoulette cooperative-work JSON gets checked when we
build that integration. Changeset `source` and import-tagging conventions are left to whoever runs an import.

**Armchair or on the ground.** We add no column for this, since it follows from fields we already have, and the docs
spell it out. A modify, or a create, with a `sources.source_url` can be checked from home against that store page (plus
imagery for where to put a new node). A delete needs someone on the ground or street-level imagery, because a store
missing from a locator is weaker evidence than a page that lists it. A modify without a `source_url` falls in between:
the value comes from a brand-wide feed with no page to check it against.

## Publishing and documentation

- **Resource:** a new `edits` entry in `datapackage.json`, `edits-<date>-<hash8>.parquet`, uploaded the same way as
  `conflated`, with its CycloneDX BOM embedded in the Parquet metadata. It replaces the `edits-tiles` resource.
- **License:** the file carries OSM tags and IDs, so it is ODbL like `conflated.parquet`; the per-spider ATP licenses
  travel via `sources.spider` joined to `spider-stats.parquet`.
- **Docs:** `docs/outputs/EDITS_PARQUET.md`, opening with a boxed notice: "Experimental. The format isn't settled and
  may change at any time, without notice. Don't build anything you can't easily rewrite." The same sentence goes into
  the resource's `description` in `datapackage.json` and into a `format_status: experimental` key in the Parquet
  metadata, so tools that never read our docs still see it.
- **Contents of the docs page:** the schema, the meaning of each `reason` and the equivalence table above.

## Milestones

Each step is one PR, measured on all 10 eval regions ([2026-11-06-eval-regions.md](2026-11-06-eval-regions.md)) and on
the planet once it holds up.

1. Delete `suggest_edits`, `edit_suggesters/`, `tiles.rs` and the `edits-tiles` resource. Pipeline output otherwise
   unchanged. The postal-partner rules and their tests from `edit_suggesters/post_partner.rs` move, unchanged, into a
   module of the new edits code, so nothing is lost; milestone 3 wires them into the new step. Until then no partner
   edits are produced, which only affects the unused edits tiles.
2. Add a column `atp.fetched.source_uri` to `conflated.parquet` with ATP's `@source_uri`, documented. It stays out of
   `atp.tags`, which holds no pipeline-injected `@` tags (today `import_atp` drops it: 0 of 4.29 million ATP rows in the
   2026-11-16 file have it, while 2.30 million have a website). The edits step then picks `source_url`: the feature's
   `website` or `contact:website` first, since `@source_uri` is often a JSON or XML API, or a Shapefile the spider
   parses; `@source_uri` only as a last resort.
3. New `edits` step and writer with the schema above, emitting contact-tag and Wikidata-ID modifies and postal-partner
   service tags (using the rules kept in milestone 1), with the source-combining rules above. Schema doc and disclaimer
   land in the same PR, published as the `edits` resource.
4. Opening-hours modifies, with the equivalence table as tests and the per-spider agreement gate.
5. Deletes: node deletes and POI-tag removal on buildings, with the recent-edit guard, and the `host_still_listed`
   evidence from conflate.
6. Creates, including merging several sources into one element.
7. Week-over-week persistence for deletes, reading last week's `edits.parquet`.
8. Host-service modifies for ATMs and money transfer, once matchers for them exist (postal partners come in milestone
   3). Until then, creates and deletes leave these kinds out (the 2026-11-16 run has about 140 such deletion candidates,
   such as 71 `money_transfer` nodes, which are unmatched only for lack of a matcher).

For each milestone we sample 50 edits per kind across the eval regions, check them by hand against the store pages and
OSM, and record the precision in a `docs/notes/` entry, as with the matching rules.

## Decisions

- **Car dealers with several brands:** merge ATP rows into one new feature with all their brands only when they share a
  phone or email, and never merge into a site OSM maps per brand. Practice varies by country: several brands in one tag
  is common in Switzerland (169) and Berlin (26), while per-brand features dominate in Japan (24 against 138 sites),
  Brazil Sudeste (10 against 52), Thailand (4 against 18) and India (0 against 10).
- **Bare POI ways and shared nodes:** run the reference-count pass and delete a bare way with its untagged, unused nodes
  (see Delete). Candidate nodes used by a way or relation are 33% in Switzerland (transit stops), 5% in Massachusetts,
  3% in Brazil Sudeste, 1% or less elsewhere; bare candidate ways sharing a node are common everywhere (66 of 250 in
  Japan, 54 of 145 in Brazil). Estimated cost: 10 to 15 minutes per planet run.
- **Websites:** propose replacing a generic or nondescript OSM website with ATP's store-specific page, for human
  reviewers.
- **Low-score matches:** settled by measurement. Every `same` rule produces modifies at first; each milestone's
  hand-checked sample is drawn per rule, and a rule whose edits fall short is turned off.
- **Geometry:** modify and delete rows use the OSM element's own geometry, as `conflated.parquet` does.
- **Charge points:** no edits for now.
