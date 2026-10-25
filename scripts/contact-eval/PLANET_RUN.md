# Planet run of contact (phone/email) matching

## What you need

- 220–250 GB free disk (peak about 172 GB), 8 GB+ RAM, more cores help.
- Network access as for a normal pipeline run (planet, ATP, Wikidata), unless you reuse downloads (below).
- protoc and a Rust toolchain.
- Python 3 with a venv: `python3 -m venv ~/venv-eval && ~/venv-eval/bin/pip install duckdb pyarrow shapely numpy`.

## Steps

```sh
git clone -b contact-matching https://github.com/brawer/osmdiffs ~/osmdiffs-contact
mkdir -p ~/osmdiffs-contact-planet
# Optional, to skip the downloads: reuse the inputs of an earlier planet workdir.
# Copy or symlink only these four files, never the derived imports (this branch
# changes the match masks that the imports bake in):
#   planet-latest.osm.pbf  planet-latest.osm.pbf.meta.json
#   alltheplaces.zip       alltheplaces.meta.json
PY=~/venv-eval/bin/python3 bash run_contact_planet.sh \
  ~/osmdiffs-contact ~/osmdiffs-contact-planet ~/osmdiffs-contact-eval
```

`run_contact_planet.sh` and `eval_contact.py` must sit in the same directory.

The first variant (`main`) imports OSM and ATP, which takes about 2.5 hours. Then the
script runs `c-any-wide` (contact matching with no category check), infers the category
pairs from it with at least 20 name-agreeing matches per pair (`MIN_NAME_AGREE`), writes
them into the clone's `src/matchers/contact_category_pairs.tsv` and rebuilds. After that
come 12 more variants, each a conflate-only rerun of roughly 10 minutes; four of them
run twice for timing (`TIMING_REPS`). In all, plan for about 5–6 hours. Variants already
done are skipped, so an interrupted run restarts with the same command.

## What comes out

In `~/osmdiffs-contact-eval`:

- `eval/summary.md`: match rates per variant, lost and moved matches, run times,
  phonenumber crate cost, the share of OSM phone values not in `+` form per country,
  new matches by country and spider, and the hide-the-wikidata test.
- `eval/metrics.json`: the same numbers.
- `eval/sample_*.csv`: up to 40 new contact matches per stratum (brand spider or not ×
  same category or category pair × within or beyond 150 m), to label `ok`, `same-site`,
  `wrong` or `unsure`.
- `contact_category_pairs.tsv` (planet pairs), `contact_category_pairs.dropped.txt`, and
  `contact_category_pairs.ch.tsv` (the Swiss pairs from before).
- Per variant: `conflated.parquet`, `pipeline.log`, `wallclock.txt`.

To hand the results back, `eval/` and the three `contact_category_pairs*` files are
enough (a few MB); the parquet files are large.
