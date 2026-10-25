#!/usr/bin/env python3
"""Compares contact-matching variants of osmdiffs, at planet scale.

Usage:
  python3 -I eval_contact.py RUNS OUTDIR

RUNS holds one directory per variant (as written by run_contact_planet.sh),
each with conflated.parquet, pipeline.log and wallclock.txt. Needs duckdb,
shapely and numpy. Writes OUTDIR/summary.md, OUTDIR/metrics.json and
OUTDIR/sample_*.csv (hand-label the `label` column: ok / same-site / wrong / unsure).
"""

import csv
import json
import math
import os
import random
import re
import sys

import duckdb
import shapely

RUNS, OUT = sys.argv[1], sys.argv[2]
os.makedirs(OUT, exist_ok=True)
db = duckdb.connect(os.path.join(OUT, "eval.duckdb"))
db.execute("PRAGMA threads=8")

CAT = """coalesce(
  'shop=' || nullif(t['shop'], 'no'), 'amenity=' || t['amenity'], 'tourism=' || t['tourism'],
  'leisure=' || t['leisure'], 'healthcare=' || t['healthcare'], 'craft=' || t['craft'],
  'office=' || t['office'])"""


def cat(prefix):
    return CAT.replace("t[", prefix + "[")


def load(v):
    path = os.path.join(RUNS, v, "conflated.parquet")
    if not os.path.exists(path):
        return False
    db.execute(f"""
      CREATE OR REPLACE TABLE "{v}" AS
      SELECT md5(atp.fetched.spider || to_json(atp.tags)::VARCHAR || hex(atp_geometry::BLOB)) AS k,
             atp.fetched.spider AS spider,
             map_from_entries(map_entries(atp.tags)) AS at,
             CASE WHEN osm IS NULL THEN NULL ELSE osm.type || '/' || osm.id END AS o,
             CASE WHEN osm IS NULL THEN NULL ELSE map_from_entries(map_entries(osm.tags)) END AS ot,
             atp_geometry::BLOB AS ag, osm_geometry::BLOB AS og
      FROM read_parquet('{path}')""")
    return True


def one(sql):
    return db.execute(sql).fetchone()


def wall(v):
    try:
        xs = sorted(float(x) for x in open(os.path.join(RUNS, v, "wallclock.txt")).read().split())
        return xs[len(xs) // 2]
    except Exception:
        return None


def log_fields(v, message):
    try:
        for line in open(os.path.join(RUNS, v, "pipeline.log")):
            if message in line:
                return json.loads(line)["fields"]
    except Exception:
        pass
    return {}


VARIANTS = ["main", "main-wide", "brand150-wide", "c-any-wide", "c-exact-wide", "c-group-wide",
            "c-group-wide-phone", "c-group-wide-email", "c-group-wide-lib", "c-group-wide-liball"]
present = [v for v in VARIANTS + ["only-any", "only-exact", "only-group", "c-group-wide-lib-stats"] if load(v)]
metrics = {}
lines = ["# Contact matching, planet", ""]

lines += ["| variant | rows | matched | on main's rows | new rows | matched new rows | lost vs main | moved vs main | run (s) | match step (s) |",
          "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
nmain = one('SELECT count(*) FROM "main"')[0]
for v in [v for v in VARIANTS if v in present]:
    rows, matched = one(f'SELECT count(*), count(o) FROM "{v}"')
    on_main = one(f'SELECT count(v.o) FROM "{v}" v JOIN "main" m USING (k)')[0]
    new_rows, new_matched = one(f'SELECT count(*), count(v.o) FROM "{v}" v ANTI JOIN "main" m USING (k)')
    lost = one(f'SELECT count(*) FROM "main" m LEFT JOIN "{v}" v USING (k) WHERE m.o IS NOT NULL AND v.o IS NULL')[0]
    moved = one(f'SELECT count(*) FROM "main" m JOIN "{v}" v USING (k) WHERE m.o IS NOT NULL AND v.o IS NOT NULL AND m.o <> v.o')[0]
    step = log_fields(v, "conflate.match: done").get("elapsed_seconds")
    metrics[v] = dict(rows=rows, matched=matched, on_main=on_main, new_rows=new_rows, new_matched=new_matched,
                      lost=lost, moved=moved, wall=wall(v), match_step=step,
                      phonenumber=log_fields(v, "contact: phonenumber crate"))
    lines.append(f"| {v} | {rows:,} | {matched:,} | {on_main:,} ({100*on_main/nmain:.1f}%) | {new_rows:,} | {new_matched:,} | {lost:,} | {moved:,} | {wall(v) or 0:.0f} | {step or 0:.0f} |")

for v in ["c-group-wide-lib", "c-group-wide-liball"]:
    if v in metrics:
        lines += ["", f"phonenumber crate in `{v}`: `{json.dumps(metrics[v]['phonenumber'])}`"]

# Per-country share of OSM phone values not in + form.
st = log_fields("c-group-wide-lib-stats", "contact: OSM phone values").get("by_country")
if st:
    rows = json.loads(st)
    metrics["phone_values_by_country"] = rows
    lines += ["", "## OSM phone values seen by contact matchers, by ATP country (top 40)", "",
              "| country | `+` values | other values | other share | other parsed by crate |", "|---|---:|---:|---:|---:|"]
    for r in rows[:40]:
        tot = r["plus"] + r["other"]
        lines.append(f"| {r['country']} | {r['plus']:,} | {r['other']:,} | {100*r['other']/max(tot,1):.1f}% | {100*r['other_parsed']/max(r['other'],1):.0f}% |")

# Gains by country and spider (best variant vs brand150-wide).
if "c-group-wide" in present and "brand150-wide" in present:
    q = '''FROM "c-group-wide" c LEFT JOIN "brand150-wide" b USING (k) WHERE c.o IS NOT NULL AND b.o IS NULL'''
    lines += ["", "## New matches from contact evidence (c-group-wide vs brand150-wide)", "",
              f"Total: {one('SELECT count(*) ' + q)[0]:,}", "", "| country | new matches |", "|---|---:|"]
    for c, n in db.execute(f"SELECT c.at['addr:country'] AS cc, count(*) n {q} GROUP BY cc ORDER BY n DESC LIMIT 25").fetchall():
        lines.append(f"| {c} | {n:,} |")
    lines += ["", "| spider | new matches |", "|---|---:|"]
    for s, n in db.execute(f"SELECT c.spider, count(*) n {q} GROUP BY 1 ORDER BY n DESC LIMIT 30").fetchall():
        lines.append(f"| {s} | {n:,} |")

# Hide-the-wikidata test.
lines += ["", "## Hide-the-wikidata test", "",
          "Main's matches whose ATP row got a contact matcher with brand tags ignored on both sides.", "",
          "| variant | eligible | same OSM | other OSM | none | precision |", "|---|---:|---:|---:|---:|---:|"]
for v in ["only-any", "only-exact", "only-group"]:
    if v not in present:
        continue
    el, same, other = one(f'''SELECT count(*), count(*) FILTER (WHERE v.o = m.o),
        count(*) FILTER (WHERE v.o IS NOT NULL AND v.o <> m.o)
        FROM "main" m JOIN "{v}" v USING (k) WHERE m.o IS NOT NULL''')
    metrics[v] = dict(eligible=el, same=same, other=other)
    lines.append(f"| {v} | {el:,} | {same:,} | {other:,} | {el-same-other:,} | {100*same/max(same+other,1):.1f}% |")


def dist(ag, og):
    a = shapely.centroid(shapely.from_wkb(ag))
    o = shapely.from_wkb(og)
    p, q = shapely.get_coordinates(shapely.shortest_line(o, a))
    x = math.radians(q[0] - p[0]) * math.cos(math.radians(p[1]))
    return 6371000 * math.hypot(x, math.radians(q[1] - p[1]))


def addr(t):
    return (t.get("addr:street_address") or " ".join(x for x in (t.get("addr:street"), t.get("addr:housenumber")) if x)
            or (t.get("addr:full") or ""))[:60]


# Samples for hand labels.
random.seed(42)
if "c-group-wide" in present and "brand150-wide" in present:
    rows = db.execute(f'''SELECT * FROM (SELECT c.spider, c.at, c.ot, c.ag, c.og,
          {cat("c.at")} AS ac, {cat("c.ot")} AS oc,
          coalesce(c.at['brand:wikidata'], '') <> '' AS has_qid
        FROM "c-group-wide" c LEFT JOIN "brand150-wide" b USING (k)
        WHERE c.o IS NOT NULL AND b.o IS NULL)
        USING SAMPLE reservoir(20000 ROWS) REPEATABLE (42)''').fetchall()
    strata = {}
    for sp, at, ot, ag, og, ac, oc, has_qid in rows:
        d = dist(ag, og)
        key = ("brand" if has_qid else "noqid", "samecat" if ac == oc else "pair", "far" if d > 150 else "near")
        strata.setdefault(key, []).append((sp, at, ot, ac, oc, d))
    for key, L in sorted(strata.items()):
        name = "sample_" + "_".join(key) + ".csv"
        with open(os.path.join(OUT, name), "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["label", "distance_m", "spider", "country", "atp_name", "atp_category", "atp_address",
                        "osm_name", "osm_category", "osm_address", "atp_phone", "osm_phone"])
            for sp, at, ot, ac, oc, d in random.sample(L, min(40, len(L))):
                w.writerow(["", round(d), sp, at.get("addr:country"), at.get("name"), ac, addr(at),
                            ot.get("name"), oc, addr(ot), at.get("phone"), ot.get("phone") or ot.get("contact:phone")])
        lines.append(f"\nSample `{name}`: {min(40, len(L))} of {len(L)} sampled rows in this stratum.")

open(os.path.join(OUT, "summary.md"), "w").write("\n".join(lines) + "\n")
json.dump(metrics, open(os.path.join(OUT, "metrics.json"), "w"), indent=1, default=str)
print("\n".join(lines))
