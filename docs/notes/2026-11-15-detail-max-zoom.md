# Stopping the conflated.pmtiles detail at z14

This note records why the `conflated.pmtiles` detail pass now stops
at z14 instead of z16, and the measurements behind it. The numbers
come from two planet runs: `conflated-20261102-0e8c8fa5.pmtiles`,
built before #918, and `conflated-20261109-5d446a7d.pmtiles`, built
with osmdiffs 0.8.17, after #918.

Showing H3 hexagons instead of thinned points at coarse zooms, which
came up in the same discussion, is a separate proposal in
[#957](https://github.com/brawer/osmdiffs/issues/957).

## File size

Bytes per zoom in the 2026-11-02 `conflated.pmtiles` (4.06 GB), from
its PMTiles directory:

| Zooms | Tiles | Size | Share |
|---|---:|---:|---:|
| z0–12 | 380,672 | 350 MB | 8.6% |
| z13 | 359,672 | 622 MB | 15.3% |
| z14 | 595,144 | 793 MB | 19.5% |
| z15 | 949,481 | 1,012 MB | 24.9% |
| z16 | 1,463,329 | 1,281 MB | 31.6% |

### This archive predates #918

The 2026-11-02 archive was built before #918 switched the detail pass
to `-r 1`. Counting the AllThePlaces points inside one z13 tile and
inside its descendants confirms the loss #918 describes:

| Area | z13 | z14 | z15 | z16 |
|---|---:|---:|---:|---:|
| Central London | 669 | 803 | 964 | 1,157 |
| Zurich | 1,953 | 2,345 | 2,817 | 3,379 |
| Manhattan | 802 | 957 | 1,161 | 1,375 |
| Dubbo, NSW | 25 | 28 | 37 | 44 |

So z13 holds about 58% of the points, z14 69% and z15 83%, even in a
small town where no tile is anywhere near the size limit. Only z16 is
complete. From #918 on, every detail zoom holds every feature.

### After #918

The 2026-11-09 archive (4.45 GB), built with osmdiffs 0.8.17, holds the
same number of AllThePlaces points at every detail zoom: 826 in central
London, 3,379 in Zurich, 1,375 in Manhattan and 44 in Dubbo, in the
same tiles as above. Its bytes per zoom:

| Zooms | Tiles | Size | Share | Largest tile |
|---|---:|---:|---:|---:|
| z0–12 | 382,853 | 397 MB | 8.9% | 83 KB |
| z13 | 363,199 | 750 MB | 16.9% | 243 KB |
| z14 | 602,540 | 897 MB | 20.1% | 131 KB |
| z15 | 964,184 | 1,085 MB | 24.4% | 60 KB |
| z16 | 1,492,835 | 1,324 MB | 29.7% | 33 KB |

### Stopping at z14

With every feature at every detail zoom, z15 and z16 only repeat z14 in
smaller tiles. Together they take 2.41 GB, so stopping at z14 should
bring the 2026-11-09 archive from 4.45 GB to about 2.05 GB, a cut of
54%. The z13 and z14 tiles shouldn't change, since nothing is dropped
from them.

An earlier estimate scaled each zoom of the 2026-11-02 archive by its
missing points and predicted about 5.1 GB before and 2.6 GB after. It
was too high: the points #918 added cost fewer bytes than the average,
so z13 grew by 21%, not by the 72% the estimate assumed.

Why z14 is enough:

- **Precision.** A z14 tile is 4,096 units across, so positions resolve
  to about 0.6 m at the equator and 0.4 m in London. Connector lines are
  only drawn from 5 m (`MIN_CONNECTOR_LENGTH_METERS`).
- **Overzoom.** MapLibre takes the source's max zoom from the PMTiles
  header and overzooms beyond it. The app's detail layers only set
  `minzoom: 13`, and fitting the map to a spider already stops at z14.
- **Lookups.** The app's fallback for shared links
  (osmdiffs-app#18) reads the deepest zoom from the header, so it
  follows the change on its own.
- **Tile size.** In the 2026-11-09 archive, the largest z13 tile is
  243 KB and the largest z14 tile 131 KB, well under tippecanoe's
  500 KB limit.

`DETAIL_MAX_ZOOM` is therefore now 14.

## Method

The per-zoom sizes come from
walking the PMTiles directory over HTTP range requests with the
`pmtiles` package. The point counts per tile come from decoding tiles
with `mapbox-vector-tile`, counting only `part: "atp"` points inside
the tile, not in its buffer.
