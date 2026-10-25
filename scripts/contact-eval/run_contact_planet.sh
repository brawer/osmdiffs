#!/usr/bin/env bash
# Planet run of the phone/email (contact) matching variants of osmdiffs.
#
#   bash run_contact_planet.sh REPO WORKDIR OUTDIR
#
# REPO     a clone of brawer/osmdiffs on branch `contact-matching`. The
#          script builds it, and rebuilds it once after regenerating
#          src/matchers/contact_category_pairs.tsv from planet data.
# WORKDIR  a workdir for this branch. It must not hold an OSM or ATP
#          import from another branch, because this branch changes the
#          match masks that the import bakes in. To skip the downloads,
#          put (or symlink) planet-latest.osm.pbf, its .meta.json,
#          alltheplaces.zip and alltheplaces.meta.json from an earlier
#          workdir into an empty WORKDIR.
# OUTDIR   where each variant's conflated.parquet, logs and timings go,
#          plus OUTDIR/eval/summary.md at the end.
#
# Needs protoc, a Rust toolchain, and Python 3 with duckdb, shapely and
# numpy (set PY=/path/to/venv/bin/python3). The first variant imports
# OSM and ATP (about 2.5 h, 220-250 GB free disk, 8 GB+ RAM). Every later
# variant only reruns the conflate step. Variants already done are
# skipped, so the script can be restarted with the same command.
# Uploads and tiles are never run.
set -euo pipefail

REPO="$1"; WORKDIR="$2"; OUT="$3"
PY="${PY:-python3}"
HERE="$(cd "$(dirname "$0")" && pwd)"
OSMDIFFS="$REPO/target/release/osmdiffs"
# Minimum number of contact matches with agreeing names for a category
# pair to be kept. 3 was used for Switzerland; the planet has far more data.
MIN_NAME_AGREE="${MIN_NAME_AGREE:-20}"
# Runs per variant for the variants used to compare run time.
TIMING_REPS="${TIMING_REPS:-2}"

B150="OSMDIFFS_BRAND_MATCH=exact-multi OSMDIFFS_BRAND_REQUIRE_ATP_WIKIDATA=1 OSMDIFFS_ADDR_CHECK_BEYOND_M=150"
C="$B150 OSMDIFFS_POI_MASKS=wide OSMDIFFS_CONTACT_MATCH=both"
declare -A ENV=(
  [main]="OSMDIFFS_BRAND_MATCH=wikidata"
  [main-wide]="OSMDIFFS_BRAND_MATCH=wikidata OSMDIFFS_POI_MASKS=wide"
  [brand150-wide]="$B150 OSMDIFFS_POI_MASKS=wide"
  [c-any-wide]="$C OSMDIFFS_CONTACT_CATEGORY=any"
  [c-exact-wide]="$C OSMDIFFS_CONTACT_CATEGORY=exact"
  [c-group-wide]="$C OSMDIFFS_CONTACT_CATEGORY=group"
  [c-group-wide-phone]="$B150 OSMDIFFS_POI_MASKS=wide OSMDIFFS_CONTACT_MATCH=phone OSMDIFFS_CONTACT_CATEGORY=group"
  [c-group-wide-email]="$B150 OSMDIFFS_POI_MASKS=wide OSMDIFFS_CONTACT_MATCH=email OSMDIFFS_CONTACT_CATEGORY=group"
  [c-group-wide-lib]="$C OSMDIFFS_CONTACT_CATEGORY=group OSMDIFFS_CONTACT_PHONE_PARSE=lib"
  [c-group-wide-liball]="$C OSMDIFFS_CONTACT_CATEGORY=group OSMDIFFS_CONTACT_PHONE_PARSE=lib-all"
  [c-group-wide-lib-stats]="$C OSMDIFFS_CONTACT_CATEGORY=group OSMDIFFS_CONTACT_PHONE_PARSE=lib OSMDIFFS_CONTACT_PHONE_STATS=1"
  [only-any]="OSMDIFFS_BRAND_MATCH=wikidata OSMDIFFS_CONTACT_ONLY=1 OSMDIFFS_CONTACT_MATCH=both OSMDIFFS_CONTACT_CATEGORY=any"
  [only-exact]="OSMDIFFS_BRAND_MATCH=wikidata OSMDIFFS_CONTACT_ONLY=1 OSMDIFFS_CONTACT_MATCH=both OSMDIFFS_CONTACT_CATEGORY=exact"
  [only-group]="OSMDIFFS_BRAND_MATCH=wikidata OSMDIFFS_CONTACT_ONLY=1 OSMDIFFS_CONTACT_MATCH=both OSMDIFFS_CONTACT_CATEGORY=group"
)
TIMED=" brand150-wide c-group-wide c-group-wide-lib c-group-wide-liball "

run() {
  local v="$1" reps=1
  if [ -s "$OUT/$v/conflated.parquet" ]; then echo "== $v: done, skipping"; return; fi
  [[ "$TIMED" == *" $v "* ]] && reps="$TIMING_REPS"
  echo "== $v: ${ENV[$v]}"
  mkdir -p "$OUT/$v"; : > "$OUT/$v/wallclock.txt"
  for _ in $(seq 1 "$reps"); do
    rm -f "$WORKDIR/conflated.parquet" "$WORKDIR/pipeline.log"
    local start; start=$(date +%s)
    env ${ENV[$v]} OSMDIFFS_STOP_AFTER_CONFLATE=1 \
      "$OSMDIFFS" run --workdir "$WORKDIR" 2> "$OUT/$v/stderr.log"
    echo "$(( $(date +%s) - start ))" >> "$OUT/$v/wallclock.txt"
  done
  cp "$WORKDIR/conflated.parquet" "$WORKDIR/pipeline.log" "$OUT/$v/"
  echo "${ENV[$v]}" > "$OUT/$v/env.txt"
}

unset PUBLIC_S3_ENDPOINT INTERNAL_S3_ENDPOINT
mkdir -p "$OUT" "$WORKDIR"
(cd "$REPO" && git rev-parse HEAD > "$OUT/commit.txt" && cargo build --release)

# 1. Import (first run) and the reference variants.
run main
run c-any-wide

# 2. Category pairs from planet contact matches, then rebuild with them.
PAIRS="$REPO/src/matchers/contact_category_pairs.tsv"
if [ ! -s "$OUT/contact_category_pairs.tsv" ]; then
  cp "$PAIRS" "$OUT/contact_category_pairs.ch.tsv"
  "$PY" -I "$REPO/scripts/infer_contact_category_pairs.py" "$OUT/c-any-wide/conflated.parquet" \
    --min-name-agree "$MIN_NAME_AGREE" > "$OUT/contact_category_pairs.tsv" 2> "$OUT/contact_category_pairs.dropped.txt"
fi
cp "$OUT/contact_category_pairs.tsv" "$PAIRS"
(cd "$REPO" && cargo build --release)

# 3. Everything else.
for v in main-wide brand150-wide c-exact-wide c-group-wide c-group-wide-phone c-group-wide-email \
         c-group-wide-lib c-group-wide-liball c-group-wide-lib-stats only-any only-exact only-group; do
  run "$v"
done

"$PY" -I "$HERE/eval_contact.py" "$OUT" "$OUT/eval"
echo "Report: $OUT/eval/summary.md"
echo "Category pairs: $OUT/contact_category_pairs.tsv (dropped: $OUT/contact_category_pairs.dropped.txt)"
