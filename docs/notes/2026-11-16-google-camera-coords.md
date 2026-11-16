# Positions copied from Google Maps URLs

A Google Maps place URL, `…/maps/place/…/@lat,lon,17z/…`, holds the
camera center of the map, not the position of the place. On a desktop
browser, Google moves the camera west so that the side panel doesn't
cover the marker. At zoom 17 that is about 0.0022° of longitude, at
any latitude, and nothing north or south: 240 m at the equator, 165 m
in Switzerland, 120 m in Oslo. The offset halves with every zoom level
up and doubles with every level down. Where the place's own position
is in the URL, it follows `!3d` and `!4d`.

Two things put camera centers into AllThePlaces (ATP):

- ATP's `url_to_coords` reads `@lat,lon` when a spider takes its
  positions from a Google Maps link. That's ATP's bug, and
  alltheplaces/alltheplaces#20570 fixes it.
- A brand's own store data has them, because whoever entered the
  stores copied `@lat,lon` from the address bar. ATP passes them on
  faithfully. No change to ATP's code can fix this; only the brand can.

For osmdiffs#966 we looked at the `same` rows of the 2026-11-09
production run, `conflated-20261109-e984a3b3.parquet`. About 42
spiders that read positions straight from a brand's data have part of
their stores, sometimes all, in this window. `apotek_1_no` has 88 of
427 matches there, `save_mart_us` 84 of 97, `wingstop_mx` 17 of 22.
Those are only the features that matched at all. A store 165 m off
often matches nothing, or another branch, so the true number is larger.
The list is in brawer/osmdiffs#971.

This note is about detecting such spiders, and about passing what we
learn through the pipeline, to the dashboard.

## Detecting it

While `import_atp` reads a spider's GeoJSON file, it probes each
feature that has a `brand:wikidata`, `operator:wikidata` or
`network:wikidata`. It looks for OSM features with one of the same
IDs in three places:

- **Exact**: within 30 m of the feature.
- **Hit**: not exact, but 0.0018–0.0030° east and less than 0.0003°
  north or south. That is where the place is if the position is a
  camera center at zoom 17. The window is that wide because the
  offset depends on the browser window of whoever copied the URL; we
  see 0.0020–0.0024° for most features, and 0.0027° for all of
  `wingstop_mx`.
- **Control**: the same window, mirrored to the west. Nothing puts a
  feature there on purpose. A spider of a dense chain, where some
  other branch is often 150 m away, has about as many controls as
  hits.

A spider is flagged as `google_camera_coords` when it has

- at least 8 hits,
- at least four times as many hits as controls plus one, and
- hits that are at least a tenth of exact plus hits.

The third rule keeps out a large, correct spider that has a few
stores entered by hand from Google Maps. Those few stay unmatched or
badly matched, but trying a second position for a hundred thousand
features to win a dozen isn't worth the cost, nor the risk of a wrong
match.

The probes only count. `import_atp` doesn't hold any spider in memory
for this, unlike the datum vote for mainland China
([`2026-11-09-china-datum.md`](2026-11-09-china-datum.md)), and it
doesn't change any feature: `alltheplaces.parquet` keeps the positions
ATP gave. Each probe is two queries to the OSM index, the same order
of cost as matching itself, and spiders are read in parallel.
Features in mainland China are probed after their datum conversion.

Code: `src/pipeline/atp/camera.rs`, called from `process_geojson` in
`src/pipeline/atp/mod.rs`.

## Passing it on

```
import_atp ──▶ alltheplaces.spider-counts.tsv ──▶ conflate ──▶ conflated.parquet
                (counts, median offset)          (second        (evidence.position,
                                                  position)      position_shift_deg)
                        │                                              │
                        └──────────────▶ spider_stats ◀────────────────┘
                                            │
                                            ▼
                                spider-stats.parquet ──▶ dashboard
```

1. **`alltheplaces.spider-counts.tsv`** gets four columns per spider:
   `exact_coords_hits`, `google_camera_coords_hits`,
   `google_camera_coords_control` and `google_camera_coords_offset`,
   the median longitude difference of the hits, ATP minus OSM, so
   negative. Older files without these columns still read, with zero
   counts. The log line
   `import_atp: positions look like Google Maps camera centres`
   names each flagged spider with its counts.
2. **`conflate`** reads the flagged spiders and their offsets from that
   file. For each feature of a flagged spider, it scores every
   candidate twice, at the position ATP gave and at that position
   moved east by the spider's median offset, and keeps the better
   score. Candidates come from the union of both search areas.
3. **`conflated.parquet`**: when the corrected position wins, the `same`
   row's `atp_geometry` is the corrected one, `match.evidence.position`
   is `google_camera_coords`, and `match.evidence.position_shift_deg`
   is the shift in degrees. `distance_m` is measured from the corrected
   position. Subtracting the shift gives ATP's position back. See
   [`CONFLATED_PARQUET.md`](../outputs/CONFLATED_PARQUET.md).
4. **`spider-stats.parquet`** copies the four counts from
   `spider-counts.tsv`, as `exact_coords_hits`,
   `google_camera_coords_hits`, `google_camera_coords_control` and
   `google_camera_coords_offset_deg`, and adds
   `same_google_camera_coords`, the `same` rows that matched at the
   corrected position. See [`SPIDER_STATS.md`](../outputs/SPIDER_STATS.md).
5. **The dashboard** can show the flag for each spider, the share
   `google_camera_coords_hits / (exact_coords_hits + google_camera_coords_hits)`,
   the offset, and `same_google_camera_coords`. That needs a change in
   brawer/osmdiffs-app; it is filed there. The flag itself isn't a
   column: the dashboard can apply the rule above to the counts, or we
   add a boolean column once the rule has settled.

## How accurate the corrected position is

We correct by the spider's median offset, not by a fixed 0.0022°,
because spiders differ: `wingstop_mx` at 0.0027° would come out 50 m
off with a fixed correction.

Within a spider, the offsets still spread, because every store was
copied from a different browser window. For `c_and_a` in the 2026-11-09
production run, the hits in units of 0.0001° of longitude were −22:
197, −21: 127, −23: 107, −24: 61, −20: 58, −19: 31. In Switzerland,
0.0001° is 7.5 m. So the median takes away the systematic part, and
leaves 60% of the stores within 8 m in longitude; a hit is at most
0.0008°, about 60 m, from the median, because that's where the window
ends.

Measured against OSM in the Swiss eval below, matched C&A stores end
up a median of 17 m from their OSM feature, and 80% within 42 m. That
is no worse than Swiss chains whose positions aren't shifted:
`denner_ch` 20 m, `hm` 19 m, `valora` 24 m. What remains is the usual
disagreement between a brand and OSM about where a store is, such as
the middle of a mall against its entrance.

That is good enough to pick the right OSM feature, which is what
conflation needs. It isn't the position of the store: the OSM feature
is that. This is why we correct only what we can check:

- **`same` rows won at the corrected position** get the corrected
  `atp_geometry`. Its error is bounded by the match itself, and the
  OSM feature carries the real position.
- **Unmatched features** (`missing_in_osm`) stay where ATP put them.
  Nothing tells us that this particular store was copied from Google
  Maps, rather than entered correctly, as part of each flagged spider
  is. A corrected position could be 165 m off where the original was
  right. Such features can still be proposed for OSM, but a flagged
  spider's features should be treated with care; the dashboard flag is
  for that.
- **`alltheplaces.parquet`** keeps ATP's positions, so everything that
  isn't matching sees the data as published.

## Alternatives considered

- **Correct every feature of a flagged spider.** This moves the stores
  that were right by 165 m. In the production run, `save_mart_us` had
  84 of 97 matches in the window, but `apotek_1_no` only 88 of 427.
- **Correct per feature in `import_atp`.** A feature whose camera
  window has an OSM feature of the same brand is probably shifted, so
  we could move just that one. But that is matching, done early and
  without the scoring of `conflate`, and it fails for stores that OSM
  doesn't have yet, which are the interesting ones.
- **Try several zoom levels.** Zoom 16 doubles the shift, to 330 m in
  Switzerland; zoom 18 halves it. Among the matches we saw, nearly all
  shifted features are at zoom 17, which is what Google Maps uses for
  a place. Each extra position widens the search and adds wrong
  matches to a dense chain. If a spider's hits show another zoom, the
  median offset already follows it.
- **Denylist flagged spiders**, so their stores never reach OSM. That
  drops the stores that are correct, which are most of them for many
  spiders, and we can still do it from the flag later.
- **Leave it to ATP.** ATP can flag a spider, but it can't tell from
  the brand's data alone which stores are shifted; it has no OSM data.

## Eval

ATP: the run of 2026-10-10 (`2026-10-10-13-32-41`), its 323,910
features with `addr:country=CH`. OSM: Geofabrik's Swiss extract. Main
at ee471e9 against this branch.

`import_atp` flagged one spider, `c_and_a`: 46 hits, 0 controls, 0
exact, median offset −0.0022116°. So every C&A store in Switzerland
is a camera center. The control did its job on dense data:
`stadt_zuerich_ch` had 516 hits but 505 controls (trees, benches and
waste baskets every few meters), `opentransportdata_swiss` 66 and 55,
`swiss_post_ch` 38 and 37. None was flagged. The next spider with
more hits than controls was `valora`: 9 hits against 3 controls and
373 exact, so it fails both the ratio to the control and the share.

| `c_and_a` | main | branch |
|---|---:|---:|
| `same` | 64 | 64 |
| `missing_in_osm` | 21 | 21 |
| median distance of `same` | 166 m | 18 m |
| `same` rows with `position = google_camera_coords` | — | 60 |

The number of matches doesn't change: a C&A store has no other C&A
within reach, so main matched it 165 m away too. But the distances
are now usable, and two stores moved to the right OSM feature: in
Basel (Freie Strasse 56), from a C&A node 152 m away to one 4 m away,
and in Yverdon-les-Bains from 133 m to 12 m. Four rows matched better
at the original position, 54–139 m away; all four agree on brand and
postcode, and three on the city. No row of any other spider changed,
and the totals (64,765 `same`, 58,374 `missing_in_osm`, 2,270 deletion
candidates) are the same.

Cost: `import_atp` took 8.8 s instead of 6.2 s for the Swiss
features, about 40% more, for two queries of the OSM index per
feature with a Wikidata ID. `conflate` took 21.6 s instead of 20.0 s;
only the 90 features of flagged spiders are scored twice.

In this eval there was only one flagged spider. Larger chains of the
production list, such as `aspen_dental_us` or `the_works`, can have
another branch of the same brand within 165 m, and the second position
may then pick a different store than the first. That is the point of
it, but we'll check the first production run for spiders whose matches
moved a lot.
