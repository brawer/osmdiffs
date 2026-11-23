# `edits.parquet`

> **Experimental.** The format isn't settled and may change at any
> time, without notice. Don't build anything you can't easily rewrite.

*Looking to download this file rather than read about its format? See
[downloading the current output](README.md#downloading-the-current-output).
The resource is called `edits`.*

`edits.parquet` lists edits to [OpenStreetMap](https://www.openstreetmap.org/)
that [AllThePlaces](https://alltheplaces.xyz/) suggests, one row per
OpenStreetMap element. Each row carries everything needed to turn it
into an [OSMChange](https://wiki.openstreetmap.org/wiki/OsmChange)
element or a [MapRoulette](https://maproulette.org/) task, without
going back to the OpenStreetMap planet.

The edits are suggestions for people to review. They are meant for
human review, or for imports that follow the
[OSM Import Guidelines](https://wiki.openstreetmap.org/wiki/Import/Guidelines),
never for unreviewed bulk uploads.

Today the file holds only tag changes on existing elements, of three
topics:

- `contact`: `phone`, `fax`, `email` and `website`, and
  `heritage:website` for the page of an official register, such as
  the US National Register of Historic Places, which is a record about
  the place rather than its own website.
- `wikidata`: the Wikidata IDs of `brand`, `operator` and `network`.
- `post_partner`: the service tags of a shop that has a postal
  counter, such as `post_office=post_partner` and `post_office:brand`.

Creates of missing places, deletes of closed ones and opening hours
will follow.

Like [`conflated.parquet`](CONFLATED_PARQUET.md), this is a
[GeoParquet 2.0-rc.1](https://github.com/opengeospatial/geoparquet/blob/v2.0.0-rc.1/format-specs/geoparquet.md)
file, so GeoParquet-aware tools such as QGIS or GeoPandas can open it
directly. Rows are sorted along a Hilbert curve, so nearby edits are
in the same row group. The key-value metadata says
`format_status: experimental`, and `format_notice` repeats the notice
above, for tools that never read this page.

## Schema

```text
id            UTF8                 non-null, stable across weeks (see below)
action        UTF8                 non-null: "modify" (later also "create", "delete")
topics        LIST<UTF8>           non-null: "contact", "wikidata", "post_partner"

osm           STRUCT               nullable; null only for creates, which don't exist yet
  type        UTF8                 non-null: "node" | "way" | "relation"
  id          UINT64               non-null
  tags        MAP<UTF8, UTF8>      non-null, the element's tags today
  modified    STRUCT               non-null
    timestamp TIMESTAMP(ms, UTC)   non-null
    version   UINT32               non-null
  way_members LIST<UINT64>         nullable, node IDs in order, for ways
  relation_members LIST<STRUCT>    nullable, for relations
    type      UTF8                 non-null
    id        UINT64               non-null
    role      UTF8                 nullable

changes       LIST<STRUCT>         non-null, sorted by key
  key         UTF8                 non-null
  old         UTF8                 nullable: null means OSM has no such tag
  new         UTF8                 nullable: null means remove the tag
  reason      UTF8                 non-null, see below

sources       LIST<STRUCT>         non-null, at least one
  spider      UTF8                 non-null, the AllThePlaces spider
  fetched     TIMESTAMP(ms, UTC)   non-null, when the spider fetched the feature
  atp_ref     UTF8                 nullable, the spider's ref for the feature
  source_url  UTF8                 nullable, a page a person can check
  relation    UTF8                 non-null, match.relation in conflated.parquet
  rule        UTF8                 nullable, match.evidence.rule in conflated.parquet
  score       FLOAT                nullable
  distance_m  FLOAT                nullable

geometry      BINARY (WKB)         non-null, the OSM shape
atp_geometry  BINARY (WKB)         nullable, the AllThePlaces point(s), for display
host          STRUCT               nullable, always null for now
  type, id, name                   the OSM element a new place sits in
warnings      LIST<UTF8>           non-null, may be empty, see below
importance    FLOAT                non-null, 0.0 to 1.0, the highest of the sources
```

The tags after the edit are `osm.tags` with `changes` applied. The
`osm` struct reads exactly like the one in `conflated.parquet`. A
node's `geometry` is a float64 point taken straight from the planet
file, so printed with 7 decimals it gives back OSM's coordinates.

`source_url` is the feature's `website` or `contact:website` in
AllThePlaces if it has one, else the page the spider read, which is
sometimes an API rather than a page for people.

### Stable IDs

`id` stays the same from week to week as long as the edit touches the
same keys of the same element, so a MapRoulette challenge rebuilt every
week keeps the status of its tasks. For a modify, it is made from the
element's type and ID and the sorted keys in `changes`.

## Reasons

| `reason` | Meaning |
| --- | --- |
| `missing_in_osm` | OSM has no value; AllThePlaces has one. |
| `differs` | OSM's value differs from AllThePlaces', even after normalizing notation. |
| `generic_value` | OSM's value says nothing about this one place, such as a brand's home page or a hotline shared by many branches; AllThePlaces has a value for this place. |
| `added_value` | OSM lists several values; we add AllThePlaces' value to the list rather than replacing it. |
| `misaligned` | OSM's `brand:wikidata` (or `operator:`, `network:`) doesn't line up value by value with `brand`; we rebuild the list in the order of the names, never dropping one of OSM's IDs. |
| `wikidata_only` | Every change of the edit is a `:wikidata` key. A reviewer may want to handle these separately, since they don't change what a map shows. |

## When we suggest nothing

- The values mean the same: `044 123 45 67` against `+41 44 123 45 67`,
  `http://www.example.ch/` against `https://example.ch/?utm_source=x`,
  or `Info@Example.ch` against `info@example.ch`.
- The AllThePlaces value is one that the spider lists for places far
  apart, such as a central hotline, or looks like a placeholder, such
  as a phone number ending in six equal digits.
- The AllThePlaces value is a brand's home page, and OSM has a
  website already; or OSM's website is the home page of another site
  than AllThePlaces' page, most likely the place's own.
- OSM's website is another page of the same site, or its email
  another address at the same domain, such as an English page where
  AllThePlaces has the Arabic one. OSM's value is most likely just as
  good. But when OSM's value says nothing about the one place (the
  site's home page, or a value that several OSM features carry), we
  replace it.
- The AllThePlaces phone or fax number isn't a valid number. A number
  with a trunk prefix after the country code, such as OSM's
  `+971 02 6120860`, is the same as `+971 2 612 0860`.
- The spider's run looks broken this week: it lost most of its
  features, or its match rate fell sharply, against last week.
- The AllThePlaces feature is a postal counter, ATM or similar service
  in a shop. It only ever edits its service tags, never the shop's
  phone or website, which may be the counter's.
- We can't tell every Wikidata ID of a list, for example
  `brand=Audi;Volkswagen` where only Audi's ID is known.
- Charging stations, for now.

New phone and fax numbers are written in international format, grouped
as is usual in the country, such as `+41 44 123 45 67`. Where OSM keeps
contacts as `contact:phone` and so on, new values go there too.

## Sources

When several AllThePlaces features match one OSM element, they make
one edit together, listed in `sources` in a fixed order (spider, then
ref, then fetch time). A key whose sources disagree is left out, with
a warning. Postal partners add up: a kiosk that is a counter of two
networks gets `post_office:brand=DHL;DPD`.

## Warnings

| `warning` | Meaning |
| --- | --- |
| `sources_disagree:<family>` | Sources gave different values for `phone`, `fax`, `email`, `website` or `heritage:website`, so that key isn't edited. |
| `replaces_email` | The edit replaces an email address that isn't generic. Both may be right, such as a hotel's own address and its chain's; look before applying. |
| `personal_email` | A new email address looks like a person's, such as `first.last@example.com`. The store published it, but look before it goes into OSM. Some store addresses get this warning too. |
| `wikidata_unmatched` | Lining up the Wikidata IDs would drop one of OSM's IDs, so they aren't edited. |

Warnings appear on an edit whose other changes still go out.

## Turning a row into OSMChange or MapRoulette

| Action | OSMChange | MapRoulette |
| --- | --- | --- |
| modify | `<modify>` of the whole element: `osm.modified.version`, `osm.tags` with `changes` applied, plus `lat`/`lon` from `geometry` (node), `<nd ref>` from `way_members` (way) or `<member>` from `relation_members` (relation) | tag-fix task: `setTags` for each `new`, `unsetTags` for each null `new` |

A tool applying a modify should first check that the live element's
version still equals `osm.modified.version`, or at least that each
live tag still equals `old`. Changeset tags such as `source` are up to
whoever runs the import.

An edit with a `source_url` can be checked from home against that
page. One without needs other evidence, since its value comes from a
brand-wide feed with no page for the one place.

## License

The file carries OpenStreetMap tags and IDs, so it is under the
[ODbL](https://opendatacommons.org/licenses/odbl/), like
`conflated.parquet`. The license of each AllThePlaces source is in the
`dataset_attributes` of [`spider-stats.parquet`](SPIDER_STATS.md), by
`sources.spider`.

## Data provenance

The file's key-value metadata `org.cyclonedx.bom` holds a
[CycloneDX](https://cyclonedx.org/) bill of materials, as in
[`conflated.parquet`](CONFLATED_PARQUET.md#data-provenance), naming
the inputs and the code that made the file.
