# Memory of `make_edits`

The weekly run of 2026-11-25 was OOM-killed in `make_edits`, the first
time the step ran on the planet (#1059). The step kept every matched
row and every create candidate in the heap, so its memory grew with
the number of rows. Now it spills them to disk and keeps only the
statistics that the rules need over the whole planet; see the module
doc of `src/pipeline/edits/mod.rs`.

## Same output

On the ten [eval regions](2026-11-06-eval-regions.md), with the
AllThePlaces run of 2026-11-14, `edits.parquet` and
`edits-spiders.json` are the same as before, byte for byte in content
and metadata.

## Max RSS

`/usr/bin/time -v` of `make_edits` alone, on 4 cores. "×20" repeats the
rows of a region's `conflated.parquet` twenty times, with OSM IDs made
distinct, to come near the planet: Japan ×20 has 2.87 million matched
rows and 2.65 million edits, about half of the planet's 5.15 million
conflated rows.

| region | main | this change |
|---|---:|---:|
| Kenya | 0.57 GB | 0.27 GB |
| Switzerland | 0.84 GB | 0.46 GB |
| Berlin | 1.25 GB | 0.59 GB |
| Japan | 1.93 GB | 0.75 GB |
| Massachusetts | 3.58 GB | 0.67 GB |
| Berlin ×20 (13.5M rows) | killed at 12.7 GB | 0.58 GB |
| Japan ×20 (13.3M rows) | killed at 13.9 GB | 0.69 GB |

The machine had 15 GB, so main was killed there. Most of what remains
is fixed: the NSI names, the nondescript tables, the spill buffers
(128 MiB in all, `SPILL_BYTES`), and the zstd compressor of the writer.
What still grows with the planet is the statistics: `Positions` (one
entry per create candidate), `MatchedSites` (one per `same` row) and
`DeletionCandidates`, now about 30 to 40 bytes per point.

The step now logs memory after each part (`make_edits: first pass`,
`shared values`, `decisions`, `creates`, `modifies`), so a production
log shows where the heap goes.

## How to repeat

Upscale a kept `conflated.parquet` with a script like this, then run
`make_edits` on the workdir under `/usr/bin/time -v`:

```python
import sys, pyarrow as pa, pyarrow.parquet as pq, pyarrow.compute as pc
src, dst, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
f, w = pq.ParquetFile(src), None
for k in range(n):
    for i in range(f.num_row_groups):
        t = f.read_row_group(i)
        osm = t.column("osm").combine_chunks()
        fields = [osm.field(j) for j in range(osm.type.num_fields)]
        j = [osm.type.field(j).name for j in range(osm.type.num_fields)].index("id")
        fields[j] = pc.add(fields[j], pa.scalar(k * 10**13, pa.uint64()))
        osm = pa.StructArray.from_arrays(fields, fields=list(osm.type), mask=pc.is_null(osm))
        t = t.set_column(t.schema.get_field_index("osm"), t.schema.field("osm"), osm)
        w = w or pq.ParquetWriter(dst, t.schema, compression="zstd")
        w.write_table(t)
w.close()
```

The copies sit on the same points, so they become twins or shared
positions and give no creates; the candidates still go through all of
the step.
