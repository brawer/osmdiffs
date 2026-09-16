# Running `osmdiffs` in production

`osmdiffs` doesn’t run anywhere permanent yet — see
[`TECHNICAL_DESIGN.md`](TECHNICAL_DESIGN.md)’s “Status” section. This
document isn’t a description of an existing deployment; it’s the
operational knowledge gathered from testing on Hetzner Cloud
([`scripts/test-on-hetzner`](../scripts/test-on-hetzner/README.md),
[#711](https://github.com/brawer/osmdiffs/issues/711),
[#722](https://github.com/brawer/osmdiffs/issues/722)), written
down now so whoever sets up the actual deployment doesn’t have to
re-establish it from scratch.

## Hardware sizing

**CPU**: every number below comes from a fixed `--cpus=6` (a Hetzner
`cpx42`) — CPU count itself hasn’t been swept independently, so “how
many CPUs” is still an open question, not a documented recommendation.

**Memory**: a `--mem-limit` sweep of the released `v0.8.2` container
against the full OpenStreetMap planet, same CPU count throughout,
gave:

| `--mem-limit` | `import_osm` | `conflate` | Total |
|---|---|---|---|
| 12g | 2h24m12s | 10m37s | 2h43m2s |
| 8g | 2h24m24s | 10m18s | 2h43m20s |
| 6g | 2h27m53s | 10m17s | 2h46m28s |
| 4g | 4h02m46s | 10m36s | 4h21m36s |

**6GB is the measured floor** on this CPU count: 8g and 6g are
indistinguishable from a comfortable 12g baseline; 4g still completes
correctly (identical row counts, all `validate` hard checks pass — the
memory pressure costs wall-clock time, not correctness) but takes
~1h40m longer, entirely inside `import_osm`’s node-coordinate
resolution step, not `conflate` — see
[`TECHNICAL_DESIGN.md`](TECHNICAL_DESIGN.md#why-conflate-doesnt-need-its-own-cache)
for why those two steps behave so differently under memory pressure.

**Don’t provision at the measured floor.** 6GB is where the sweep
happened to land this time; it isn’t a target with margin built in, and
the planet only grows over time — a floor measured today gets tighter,
never looser. **8GB is the recommended minimum** in production: the
first config in the sweep with no measurable difference from a
generous limit.

**Disk**: peak usage during a full-planet run was **~172GB**, settling
to **~143GB** once `import_osm` finishes and its temporary files are
cleaned up — consistent across two independent runs on different
hardware (this sweep, and the earlier
[#665](https://github.com/brawer/osmdiffs/issues/665) shakedown,
which saw ~174GB peak / ~148GB settled). **220–250GB** gives reasonable
headroom over that peak without being wildly oversized; the 400GB
default `scripts/test-on-hetzner/cloud_test.py` uses for testing is
itself generous margin, not a sizing recommendation.

## Container invocation

The published image (`ghcr.io/brawer/osmdiffs:vX.Y.Z` — pin an
exact tag; see
[the releases page](https://github.com/brawer/osmdiffs/releases)
for the current version and its notes) runs as:

```sh
podman run --rm --read-only \
  --memory=8g --cpus=6 \
  -v /path/to/workdir:/workdir \
  --env-file s3.env \
  ghcr.io/brawer/osmdiffs:vX.Y.Z \
  run --workdir /workdir --run_id "$RUN_ID"
```

- `--read-only`: the image ships nothing that needs a writable root
  filesystem — see
  [`SUPPLY_CHAIN_SECURITY.md`](SUPPLY_CHAIN_SECURITY.md#minimal-containers).
  Everything the pipeline writes goes to `/workdir`, the one mounted
  volume.
- `--memory`/`--cpus`: the actual cgroup limits this document’s sizing
  guidance is about — without them, `pipeline.log`’s own `cgroup_*`
  memstats fields read `None` (see
  [`LOGGING.md`](LOGGING.md)), and nothing here has been validated
  running unconstrained.
- The image runs as **UID 1000** (see `USER 1000` in the
  [`Containerfile`](../Containerfile)), not root — a `scratch` base
  with no `/etc/passwd` entry to name it. `/workdir` must be writable
  by that UID on the host: `chown 1000:1000 /path/to/workdir` (or an
  equivalent, e.g. rootless podman's uid mapping) before the first
  run, or `import_osm` fails immediately on its first write.
- `--run_id`: whatever identifier the scheduling system assigns this
  run (a Kubernetes Job name, a cron invocation ID, …). It becomes
  `formulation[].workflows[].uid` in the output’s embedded provenance
  BOM (see
  [`outputs/CONFLATED_PARQUET.md`](outputs/CONFLATED_PARQUET.md)), keys
  the run’s archived `pipeline.log`, and pins the workdir against a
  mismatched restart. Anything outside `[A-Za-z0-9._-]` is normalised
  for those uses (see [`LOGGING.md`](LOGGING.md)); pass a clean value
  and it’s used verbatim. Optional; empty if omitted.

## Required configuration

Uploads go to **two** independent S3 destinations, each with its own
five environment variables, read once at startup (see
[`src/pipeline/upload.rs`](../src/pipeline/upload.rs)):

| Prefix | Holds | Intended backing store |
|---|---|---|
| `PUBLIC_S3_*` | `conflated.parquet`, the PMTiles archives — the public downloads | a Bunny.net S3 instance, so objects replicate to the CDN edge |
| `INTERNAL_S3_*` | `logs/<run-id>.log`, and future intermediates | in-datacenter object storage (e.g. Hetzner Object Storage); never needs CDN replication |

For each prefix:

- `<prefix>_ENDPOINT` — also that bucket's on/off switch: unset it
  entirely to disable those uploads (e.g. a local/dry-run invocation, or
  a run that only needs one of the two buckets) rather than passing
  empty values.
- `<prefix>_BUCKET`, `<prefix>_REGION`, `<prefix>_ACCESS_KEY_ID`,
  `<prefix>_ACCESS_KEY_SECRET` — required once `<prefix>_ENDPOINT` is
  set; genuine S3-compatible buckets for the actual output, never the
  ephemeral per-run test buckets `scripts/test-on-hetzner` creates for
  its own testing.

`*_ACCESS_KEY_ID`/`*_ACCESS_KEY_SECRET` are live credentials — handle
them as secrets, not plain configuration. Keep them out of a command
line (visible to anyone who can `ps` the host, and easy to leak into
shell history or CI logs) and out of any manifest committed to a repo.
The `podman run --env-file` invocation above already does the
minimum right thing for a manual/systemd invocation — the file’s
contents never appear as process arguments. Whatever eventually
schedules this in production should do the equivalent for its own
environment: a Kubernetes `Secret` referenced via `secretKeyRef` (not
a literal value in the `Job`/`CronJob` manifest), or the analogous
mechanism for whatever scheduler ends up being used.

## Scheduling

Nothing here runs on a schedule yet. The design intent
([`TECHNICAL_DESIGN.md`](TECHNICAL_DESIGN.md#objective)) is a weekly
cadence, matching how often AllThePlaces itself publishes a fresh dump
— whatever wires this up (a Kubernetes `CronJob`, a plain cron job
somewhere, a scheduled GitHub Actions workflow) is future work, not
something this repository provides today.

## A first real run: Infomaniak Kubernetes (2026-09)

On 2026-09-15/16, [`brawer/production`](https://github.com/brawer/production)
(`infomaniak/` + `infomaniak-k8s/`, see that repo's README for the full
OpenTofu setup) ran `osmdiffs:v0.8.5` end-to-end as a Kubernetes `CronJob`
on Infomaniak Public Cloud, against the real planet — not a permanent
deployment (the cluster was torn down again right after, per that repo's
own cost/scope decisions), but a genuine first production-shaped run, worth
recording here since several things differed from the Hetzner numbers above
or aren't covered by them at all.

**Real total runtime: 5h55m** (a `--cpus=6`/`--mem-limit=8g`-equivalent
config, matching this document's own recommended minimum), against this
document's ~2h43m Hetzner figure — more than double. Three plausible,
**unconfirmed** contributors, most to least likely:

1. The node's flavor (`a8-ram16-disk20-perf1`) is Infomaniak's entry-level
   compute tier — the project has no access to their higher performance
   tiers (`perf2`/`perf3`/`perf4`, checked via their API). The physical CPU
   itself is a modern AMD EPYC-Genoa (`/proc/cpuinfo`, read via `kubectl
   debug node/... -- chroot /host`) — not an old/slow chip — so the gap is
   more likely oversubscription/shared-core scheduling at that tier than
   raw silicon age.
2. The workdir volume was Infomaniak's network-attached Cinder block
   storage (`csi-cinder-sc-delete`), not necessarily comparable to whatever
   local/attached storage the Hetzner sweep used - and this pipeline's
   design leans on fast storage for its page-cache-as-scratch-space
   pattern (see "What to watch" below).
3. Compounding (2): the same 8GB config that's "indistinguishable from a
   generous limit" on Hetzner's hardware sat at 97-100% of the cgroup limit
   for multiple steps on this run (see below) - if that pushed more of the
   working set through the (slower, per #2) volume instead of page cache,
   it would compound with #1 rather than being an independent cause.

No CPU/memory/disk sweep was repeated on this hardware - these are
plausible explanations for a single data point, not measurements. Whoever
next benchmarks on non-Hetzner hardware: capturing the same `--mem-limit`
sweep this document's Hardware Sizing section did, on the target hardware,
would turn this from a hypothesis into a number.

**The "page-cache design holding up" signal (see "What to watch") held**,
and is worth citing as a concrete example: `pipeline.log` repeatedly warned
`memory usage at 97-100% of the cgroup limit -- at risk of being
OOM-killed` (at `extract_conflated_layers`, `render_conflated_overview`,
and `render_conflated_detail`), yet `rss_anon_bytes` stayed near-constant
at ~22MB throughout all of them - the pressure was entirely reclaimable
file-backed/page-cache memory, not heap growth, exactly as designed. Ended
up not needed here, but worth knowing before assuming a scary-looking
warning means real trouble: it doesn't distinguish the two, so check
`rss_anon_bytes` vs `rss_bytes`/`cgroup_current_bytes` before reacting to
it.

**`render_conflated_detail`** (zoom 13-16 vector tiles via `tippecanoe`,
~83 minutes on this run) and **`join_conflated_tiles`** are not swept or
discussed elsewhere in this document. Both completed correctly and within
what this deployment considered a reasonable timeframe; neither needed any
tuning or investigation beyond confirming that. Treat them the same way
this document treats `conflate` versus `import_osm` in the Hardware Sizing
sweep - as long as a step finishes without crashing in reasonable time, its
internals aren't this document's concern.

**Kubernetes-specific gotchas, not covered by the podman invocation above**
(all specific to running under an orchestrator with its own volume
provisioning, not to `osmdiffs` itself):

- The `chown 1000:1000 /path/to/workdir` guidance above assumes a
  bind-mounted host directory. A Kubernetes-provisioned volume (an
  ephemeral CSI PVC, in this deployment) mounts owned by root regardless -
  the orchestrator-level equivalent is a pod-level
  `securityContext.fsGroup` (any fixed value; it doesn't need to match the
  image's actual GID, which this deployment never determined - only the
  `UID 1000` this document already documents was checked). Omitting it
  reproduces the same "fails immediately on its first write" failure mode
  this document already describes for a wrong host-directory `chown`, just
  from a different cause.
- `active_deadline_seconds` (Kubernetes' own job-level timeout, no
  equivalent in the bare `podman run` invocation above) needs real margin,
  not a Hetzner-derived guess: a first attempt at 6h (Hetzner's ~2h43m
  headlined figure plus what looked like generous buffer) killed a run
  that was still legitimately working - actively consuming CPU and memory,
  no crash, no error - deep into `join_conflated_tiles`. Unlike a container
  crash, this failure mode gives no `backoff_limit` retry and no
  indication of how much more time was actually needed: the whole Job
  fails outright and the pod is deleted. Size this deadline off this
  document's real 5h55m figure (on non-Hetzner-`cpx42` hardware) with
  real margin, not the €/hour-focused Hetzner timing this document
  otherwise emphasizes.
- The AWS SDK (used for both `PUBLIC_S3_*`/`INTERNAL_S3_*` destinations,
  see [`upload.rs`](../src/pipeline/upload.rs)) validates `*_REGION`
  client-side and rejects any value with an uppercase character:
  `invalid config: region must contain only lowercase ASCII letters,
  digits, or '-'`. Neither example backing store this document names
  (Bunny.net, Hetzner Object Storage) - nor, for that matter, most
  S3-compatible object storage generally - has a real AWS-style region, so
  it's an easy mistake to pass whatever region code or placeholder string
  the backing store's own docs use verbatim, uppercase and all. Caught
  only at `upload_conflated`/`upload_internal`, i.e. only once a run has
  otherwise fully completed - budget a whole run's wall-clock time to
  discover this the hard way if it's wrong, since nothing validates it
  earlier.

**Cost shape differs from the Hetzner model above, not just the number.**
This deployment's managed-Kubernetes node pool needed `min_instances = 1`
(the provider/API rejected `0`, an unrelated bug specific to that
platform) - one node running continuously, billed by the hour regardless
of whether a job is active, rather than Hetzner's per-run
create-then-destroy model this document's Cost section prices. Whoever
picks the actual production infrastructure should treat "billed
continuously" vs. "billed per run" as a real design axis, not just compare
hourly rates.

## Serving the outputs (CDN)

Nothing serves the pipeline’s outputs to the public yet. The intent is
to put them behind the same [Bunny](https://bunny.net) CDN that
[`osmviews`](https://github.com/brawer/osmviews) uses, sharing one
account and one OpenTofu config
([`brawer/production`](https://github.com/brawer/production), `bunny/`).
The CDN and its edge-cache rules for `osmdiffs` are **already
provisioned** there, against `osmdiffs.brawer.ch` — so this section is
about the shape the pipeline’s uploads must take, not infra still to
build.

### The `/data/` contract

Bunny serves objects straight from the storage bucket and cannot attach
per-object response headers, so caching is driven entirely by URL
patterns in
[`bunny/cdn.tf`](https://github.com/brawer/production/blob/main/bunny/cdn.tf).
Two rules apply under `https://<host>/data/`:

- **`data/datapackage.json` — 60 s TTL.** The one object allowed to
  change at a stable URL: a
  [Frictionless Data Package](https://datapackage.org/) descriptor,
  overwritten in place every run, listing that run’s files with byte
  sizes and SHA-256 hashes. It is the update-check target — a client
  GETs ~1 KB and compares `version`. 60 s (a little more across cache
  tiers) is then the whole propagation delay of a new run; **the
  pipeline never purges the CDN** and never needs a Bunny account
  credential.

- **everything else under `data/` — long TTL**
  (`local.immutable_max_age` in `cdn.tf`: 600 s today, moving to 30
  days). Matched as a *negative* rule — “under `data/` and not
  `datapackage.json`” — so new file types need no config change.

That negative match is the load-bearing invariant:

> **`data/datapackage.json` is the only object the pipeline may
> overwrite. Every other published file must carry an immutable URL** —
> a date *and* a content hash in the name.

Reusing a name for changed bytes would serve stale data from cache for
up to the long TTL, recoverable only with a manual purge. Upload the
dated files first and `datapackage.json` last, so “the manifest lists
file X” always implies X is already there.

Pruning old runs to save storage is fine — a deleted dated object just
starts returning 404, and the CDN caches that 404 correctly. Replacing
a published object is not.

### What the pipeline publishes

The `PUBLIC_S3_*` bucket, under `data/`:

| Key | Notes |
|---|---|
| `data/conflated-<YYYYMMDD>-<hash8>.parquet` | the data product |
| `data/conflated-<YYYYMMDD>-<hash8>.cdx.json` | its CycloneDX provenance BOM as a standalone sidecar (`<hash8>` is the Parquet's, so the two names pair) |
| `data/conflated-<YYYYMMDD>-<hash8>.pmtiles` | visualization of the above; debugging aid |
| `data/edits-<YYYYMMDD>-<hash8>.pmtiles` | visualization of suggested edits; debugging aid |
| `data/datapackage.json` | the manifest, overwritten each run, uploaded last |

`<YYYYMMDD>` is `max(AllThePlaces run start, OSM planet replication)` —
the same input-anchored date the provenance BOM uses (see
[`TECHNICAL_DESIGN.md`](TECHNICAL_DESIGN.md#reproducibility-and-restarts)),
so a rebuild from the same inputs produces the same names. `<hash8>` is
the first 8 hex characters of the file's SHA-256: an identical rebuild
re-`PUT`s an identical key (a no-op); a *different* dataset the same day
lands on a different key rather than clobbering the first. The pipeline
logs a `WARN` if it's about to replace an existing dated object — if
that isn't a retry of the day's run, prune that day's `data/` objects
first (pruning is explicitly fine; replacing is not).

`datapackage.json`'s `version` (the anchor date) and its `conflated` /
`bom` resource entries are reproducible; the two PMTiles resource
entries carry per-run hashes (tippecanoe output isn't bit-stable) and so
is the manifest as a whole — acceptable, since the PMTiles are a
debugging aid, not a data product.

**First production run:** do it by hand and check
`https://<host>/data/datapackage.json` on the CDN looks right before
wiring up a cron — nothing here has run against a real public bucket +
CDN yet.

### Still open

- CORS headers on `/data/*`, needed if a browser reads the outputs
  directly —
  [brawer/production#10](https://github.com/brawer/production/issues/10).
- A `--prune` helper for old `data/` runs — today pruning is a manual
  `aws s3 rm`.

## What to watch

Everything below comes straight out of `pipeline.log` (see
[`LOGGING.md`](LOGGING.md)) — no separate monitoring system needed to
get started:

- **OOM**: exit code 137, or an OOM-kill signature in `dmesg`/
  `journalctl -k` — the definitive failure signal. A killed step’s own
  “end” log record is simply missing, not an error entry, so don’t
  rely on `pipeline.log` alone to notice this; check the container’s
  own exit status too.
- **Approaching the limit**: a `WARN` fires automatically once
  `cgroup_current_bytes` crosses 85% of `cgroup_max_bytes`, at any
  step — an early signal ahead of an actual kill.
- **The page-cache design holding up**: during `conflate.match`
  specifically, `rss_file_bytes` should dominate `rss_bytes` (reclaimable
  cache, not heap) — if that ratio drops, something’s changed about the
  access pattern this design depends on.
- **Step timings drifting**: `import_osm`’s own sub-steps
  (`import_osm.fetch`/`.open`/`.prune`/`.assemble`/`.index`, logged
  individually as of
  [#761](https://github.com/brawer/osmdiffs/pull/761)) are worth
  watching over time as the planet grows — a slow drift is expected;
  a sudden jump on an otherwise-unchanged config is worth investigating
  the way this document’s own `--mem-limit` sweep did.

## Cost

Real Hetzner Cloud pricing as of 2026-08 (`cpx42`, `fsn1`/`hel1`,
excluding VAT): **€0.1114/hour**. A weekly full-planet run at the
recommended 8GB config takes ~2h43m, so:

- **~€0.30 per run** in compute, plus a few cents of volume cost for
  its few-hour lifetime (volumes bill per GB-month;
  `€0.0572`/GB-month, negligible unless kept around persistently
  between runs).
- **~€1.30/month** at a weekly cadence — compute only. Not included:
  S3-compatible storage for the actual output, egress/traffic, or any
  larger instance chosen for genuine margin beyond this document’s
  bare sizing numbers.

This is Hetzner-specific pricing, from the same provider
`scripts/test-on-hetzner` tests against — an actual production
deployment might land on different infrastructure entirely; treat the
euro figures as an order-of-magnitude anchor, not a quote.

**Infomaniak Public Cloud, one real run (2026-09-16)**: the single clean
full-planet run this document’s “A first real run” section describes above
— one data point, not a sweep, but the only one available so far. In CHF
(tax included), at rates read directly from the account’s own API:

- Compute: `a8-ram16-disk20-perf1` (8 vCPU / 16GB RAM — the smallest
  flavor covering this document’s recommended 6 vCPU / 8GB pod request
  with headroom), `CHF 0.0374`/hour, for the run’s actual 5h55m: **≈ CHF
  0.22**.
- Storage: the 250GB ephemeral scratch volume (Ceph, `perf1` tier — 500
  IOPS / 200MB/s), `CHF 0.00012`/GB/hour, for the same 5h55m: **≈ CHF
  0.18**.
- **≈ CHF 0.40 per run**, compute and scratch storage together — the same
  order of magnitude as the Hetzner figure above.

Not included: the Kubernetes cluster this ran on has a free control plane
at Infomaniak, but needs at least one node running permanently regardless
of whether a job is active — an unavoidable baseline cost this per-run
figure doesn’t capture, and not quantified here since it depends on how
the cluster ends up shared/scheduled, not on this pipeline.
