# Cutting a release

This document is the practical how-to for cutting a release of
`osmdiffs`. For the concepts behind *why* the process looks like this
(SBOM, attestations, immutable releases, ...), see
[`SUPPLY_CHAIN_SECURITY.md`](SUPPLY_CHAIN_SECURITY.md).

## Quick start

[`release-please`](https://github.com/googleapis/release-please) keeps a
"release PR" continuously open against `main`, titled something like
"chore(main): release 0.8.4", with `Cargo.toml`/`Cargo.lock`'s version
bump and the new `CHANGELOG.md` entries already prepared from merged PR
titles. To cut a release:

1. Open that PR, sanity-check the version number it proposes against
   “Choosing the version number” below, and merge it like any other PR
   (it goes through the normal required checks and merge queue).
2. Once merged, `release-please` tags that commit and publishes the
   GitHub Release (with the same notes) on its own — nothing further to
   run.
3. Optionally, verify the release actually came out right (build,
   SBOM, attestations):
   ```sh
   ./scripts/verify-release.sh vX.Y.Z
   ```

See “What happens automatically” below for the full sequence — but read
“Choosing the version number” first, it’s the one part of this that
actually needs a careful decision, not just clicking merge.

## Choosing the version number

This is the one genuine judgment call in the whole process; everything
else is mechanical. We follow [SemVer](https://semver.org/), driven by
the pipeline’s **output schema** — not by how much code changed.

This is a different question than what [SemVer](https://semver.org/)
usually answers. Programmers normally think of SemVer in terms of API
compatibility for code that *links against* a library. Nobody links
against `osmdiffs`:
downstream clients only ever consume the *data* it produces. So the
question to ask isn’t “did the code change in a breaking way,” it’s “does
this change what a client reading our output has to handle differently”:

- **major** — the output schema changed in a way that breaks existing
  clients (a field was removed or renamed, a type changed, ...)
- **minor** — the output schema evolved in a backward-compatible way
  (e.g. a new optional field was added)
- **patch** — bugfixes only, no schema changes

A release that touches a lot of internal code but doesn’t change what
clients read is still a patch release. A release that changes the output
schema in a breaking way is a major release even if the code diff is
tiny.

**Before 1.0.0, a schema-breaking release bumps *minor*, not *major*.**
SemVer’s own spec is explicit that this is fine: [§4](https://semver.org/#spec-item-4)
says a `0.y.z` major version is for initial development, where “anything
MAY change at any time” and the public interface “SHOULD NOT be
considered stable” — SemVer deliberately leaves how `0.y.z` itself
increments up to the project. We use the common convention of treating
`0.MINOR.PATCH` the way `MAJOR.MINOR.PATCH` works post-1.0: a
schema-breaking release bumps `MINOR` (not `MAJOR`, which stays `0`),
anything else bumps `PATCH`. This isn’t just a convention we picked —
it’s the same rule `cargo`/crates.io itself uses for `0.x` dependency
resolution (a `^0.2.0` requirement excludes `0.3.0`, treating that
minor-version bump as the breaking one), so it’s already how our own
build tooling reasons about pre-1.0 versions. We’ll move to `MAJOR`
bumps for breaking changes once there’s an actual 1.0.0 to break
compatibility with — i.e. once this pipeline has real downstream
consumers depending on schema stability, not before.

### How release-please's version bumps map to this rule

`release-please` derives version bumps from Conventional-Commits PR
titles, not from a schema diff — so the mapping in
[`release-please-config.json`](../release-please-config.json) has to
reproduce the two-tier, pre-1.0 rule above using only the commit types
and the `!` marker:

- `"bump-minor-pre-major": true` — a `!`-marked (breaking, i.e.
  *output-schema*-breaking per
  [`CONTRIBUTING.md`](CONTRIBUTING.md#pr-titles-conventional-commits))
  commit bumps **minor** instead of major, matching “before 1.0.0, a
  schema-breaking release bumps minor” above.
- `"bump-patch-for-minor-pre-major": true` — a non-breaking `feat:`
  commit bumps **patch** instead of minor. This isn't a compromise: it's
  the same “anything else bumps patch” rule above, exactly. Pre-1.0.0
  there's no separate slot left for “schema evolved compatibly” once
  breaking has claimed minor — that's the whole point of the two-tier
  collapse, not a gap release-please forces on us.

Net effect while we're at `0.y.z`: a `!` commit → minor, everything else
(`feat:`, `fix:`, `chore:`, ...) → patch. This is exactly the existing
convention, just executed by config instead of by the old
`cut-release.sh` script's best-effort PR-title scan (that script is
gone; `release-please` reads the same `!` marker directly from commit
history instead).

This still leans entirely on contributors choosing PR titles correctly —
`release-please` only ever sees the type, never the schema diff. Before
merging the release PR, skim the commits it's based on (linked from the
PR body) against “Choosing the version number” above; if a title was
wrong, edit `Cargo.toml`'s version and the `CHANGELOG.md` entry directly
in the release PR before merging — `release-please` treats that edit as
authoritative on its next run. Once this pipeline has real downstream
consumers depending on schema stability (i.e. we cut an actual 1.0.0),
both flags should be revisited: an ordinary `feat:` would then bump
minor again, and something more deliberate (a `Release-As:` footer, or a
third `!`-only category) would be needed to keep pre-1.0's collapsed
“anything else” tier from silently becoming three real tiers.

## What happens automatically, step by step

1. **Every merge to `main` updates the release PR.**
   [`.github/workflows/release-please.yml`](../.github/workflows/release-please.yml)
   runs [`release-please`](https://github.com/googleapis/release-please)
   on every push to `main`. It parses Conventional-Commits PR titles
   since the last release, and keeps one PR open (creating it if it
   doesn't exist yet) with `Cargo.toml`/`Cargo.lock`'s version bumped and
   the new `CHANGELOG.md` entries drafted — see “How release-please's
   version bumps map to this rule” above for exactly how it picks the
   version.
2. **You merge that PR when you're ready to release** — like any other
   PR: it needs the required check
   (`Execute unit and integration tests`) and goes through the merge
   queue. `main` is protected, so this can't be pushed directly.
3. **The merge itself publishes the release.** The next
   `release-please.yml` run (triggered by that merge landing on `main`)
   recognizes its own release commit, tags it, and creates a real GitHub
   Release there with the same notes as `CHANGELOG.md`'s new section —
   no separate step. This is what makes the release immutable from this
   point on (see
   [`SUPPLY_CHAIN_SECURITY.md`](SUPPLY_CHAIN_SECURITY.md#immutable-releases)).
4. **That tag push triggers
   [`.github/workflows/release.yml`](../.github/workflows/release.yml)**,
   entirely independently of `release-please`. `release.yml` itself is
   just a thin caller: it hands off to
   [`.github/workflows/release-build.yml`](../.github/workflows/release-build.yml)
   as a reusable workflow — a separate file with its own identity is what
   gets this to SLSA Build Level 3 rather than Level 2 (see
   [`SUPPLY_CHAIN_SECURITY.md`](SUPPLY_CHAIN_SECURITY.md#build-provenance-and-attestations)).
   The called workflow runs:
   - `verify-version`: re-checks the tag matches `Cargo.toml`’s version
     (a server-side safety net — this should never fail, since the tag
     only ever gets created from a commit that just set that version).
   - `build` (once per architecture, amd64 and arm64): builds the
     container via [`Containerfile`](../Containerfile), which also
     generates the SBOM (see
     [`scripts/sbom/README.md`](../scripts/sbom/README.md)) and pushes
     each architecture’s image to `ghcr.io` by digest.
   - `manifest`: combines both architectures into a multi-arch manifest,
     tagged both `vX.Y.Z` and `latest`.
   - `attest`: publishes signed SBOM and build-provenance attestations
     for both per-architecture images, plus a build-provenance
     attestation for the manifest list.
5. **Once that workflow finishes, verify it** (see “Verifying a release”
   below) — this step is manual, unlike 1–4.

Steps 1–3 are immediate (a few minutes for required checks plus the
merge queue's minimum wait); step 4 (the actual container build) takes
roughly another 20–25 minutes.

## Verifying a release

Unlike the steps above, this one is on you to run — nothing currently
triggers it automatically:

```sh
./scripts/verify-release.sh vX.Y.Z
```

It waits for (or, if already finished, immediately checks)
`release.yml`’s run for that tag, then confirms both a build-provenance
and an SBOM attestation exist for each per-architecture image — the same
two checks described in
[`SUPPLY_CHAIN_SECURITY.md`](SUPPLY_CHAIN_SECURITY.md#build-provenance-and-attestations),
done for real rather than assumed. This is exactly what was done by hand
to confirm v0.6.9, the first release cut with the old `cut-release.sh`
script — see the comment trail on
[brawer/osmdiffs#562](https://github.com/brawer/osmdiffs/pull/562)
for that walkthrough, which is what `verify-release.sh` automates.

## Setting up the release-please token

`release-please.yml` needs a token in the `RELEASE_PLEASE_TOKEN` repo
secret — the default `GITHUB_TOKEN` won't work. GitHub deliberately
doesn't let a `GITHUB_TOKEN`-authored push or PR trigger further workflow
runs (to prevent recursive-workflow loops), which means `test.yml`'s
`pull_request`-triggered "Execute unit and integration tests" check would
never run against the release PR, and it could never clear the branch
ruleset's required-status-check to be merged. A Personal Access Token
isn't subject to that restriction.

One-time setup (repeat only if the token expires or is revoked):

1. Create a **fine-grained PAT**
   ([github.com/settings/personal-access-tokens](https://github.com/settings/personal-access-tokens/new)),
   scoped to only the `brawer/osmdiffs` repository, with **Contents:
   Read and write** and **Pull requests: Read and write** repository
   permissions. Give it an expiration and put a reminder somewhere to
   rotate it before then — a repo secret with write access is worth
   treating with the same care as any other credential.
2. Store it as the repo secret `RELEASE_PLEASE_TOKEN`:
   ```sh
   gh secret set RELEASE_PLEASE_TOKEN --repo brawer/osmdiffs
   ```
   (paste the token when prompted).

Until this secret exists, `release-please.yml` will still run (it uses
`${{ secrets.RELEASE_PLEASE_TOKEN }}`, which is just empty/falls back to
the default token when unset) but the PR it opens won't be mergeable —
its required check will never appear. If a release PR is stuck showing
no status at all for `Execute unit and integration tests`, this is why.

## Rules

- **Never push a `v*` tag by hand, and never hand-edit `CHANGELOG.md`
  for an already-released version.** `release.yml` would still build and
  publish a container for a hand-pushed tag, but you'd have skipped
  `release-please`'s version-consistency bookkeeping, and immutability
  protections apply to tags that went through a real GitHub Release —
  not to a bare tag pushed directly.
- **Releases are immutable. If one’s bad, cut a new patch version and
  leave the bad one as-is.** You can’t fix a published release in place,
  and you can’t reuse or move its tag even if you delete it.
- **Anyone with write access can merge the release PR.** There’s no
  separate approval gate for this beyond the normal merge-queue checks —
  we don’t have enough people for dedicated release roles.

## If it goes wrong

- **The release PR fails its required checks**: it just sits open,
  unmergeable, same as any other failing PR. Look at why CI failed
  (usually something landed on `main` after the PR was last updated;
  `release-please` rebases it automatically on the next push), fix it,
  and merge once green.
- **The release PR merges, but no GitHub Release shows up**: check
  `release-please.yml`'s run for the merge commit — a missing or
  expired `RELEASE_PLEASE_TOKEN` (see above) is the most likely cause,
  since the workflow would have run as the plain `GITHUB_TOKEN` and
  quietly lost write access to create the release. Once fixed, re-run
  the failed workflow from the Actions tab; it's safe to retry.
- **The tag/release gets created, but `release.yml` then fails** (e.g. a
  build failure): the release is already immutable at this point, so
  there’s no “redo.” Fix whatever broke the build (or `main`), and cut a
  new patch version. The failed tag’s GitHub Release will just exist
  without a correspondingly published, attested container — that’s a
  known, accepted consequence of immutability, not a bug.
- **`verify-release.sh` is interrupted, or times out** (~20–25 min; a
  crashed machine, a dropped connection, or a truly stuck workflow): the
  release itself is unaffected — it was already created before this
  wait began. Just re-run
  `./scripts/verify-release.sh vX.Y.Z` once you’re ready to check on it
  again; it picks up wherever the run currently stands.

## Where things live

- [`release-please-config.json`](../release-please-config.json) /
  [`.release-please-manifest.json`](../.release-please-manifest.json) —
  `release-please`'s version-bump rules and current-version bookkeeping
- [`.github/workflows/release-please.yml`](../.github/workflows/release-please.yml) —
  runs on every push to `main`; opens/updates the release PR, then tags
  + publishes the release once that PR is merged
- [`CHANGELOG.md`](../CHANGELOG.md) — maintained by `release-please`;
  don't hand-edit entries for already-released versions
- [`scripts/verify-release.sh`](../scripts/verify-release.sh) — the
  manual post-release verification step (see “Verifying a release”
  above)
- [`.github/workflows/release.yml`](../.github/workflows/release.yml) —
  triggers on a pushed tag, calls `release-build.yml`
- [`.github/workflows/release-build.yml`](../.github/workflows/release-build.yml) —
  build, SBOM, attest
- [`Containerfile`](../Containerfile) — how the container gets built
- [`.github/release.yml`](../.github/release.yml) — categorization rules
  for GitHub's own auto-generated release notes; no longer the primary
  changelog (that's `CHANGELOG.md` now), kept only as a fallback for
  manually running `gh release create --generate-notes`
- [`scripts/sbom/README.md`](../scripts/sbom/README.md) — how the SBOM
  itself is generated

## Known gaps, not yet in place

- **Production deployment isn’t wired up yet.** This process ends at “a
  correctly built, SBOM’d, attested container sits in `ghcr.io`” — what
  happens after that, to actually run this in production (scheduling,
  where it runs), doesn’t exist yet. What it should take to get there
  once it does — hardware sizing, required configuration, what to
  monitor — is written down in
  [`PRODUCTION.md`](PRODUCTION.md), from real testing rather than
  guesswork.
- A few low-priority, deliberately-deferred items are tracked separately
  and don’t block anything: automated freshness checks for vendored
  dependencies
  ([#555](https://github.com/brawer/osmdiffs/issues/555)), moving
  `cargo-cyclonedx` off Alpine’s edge repo once it’s available in stable
  ([#556](https://github.com/brawer/osmdiffs/issues/556)), and
  watching for an emerging standard on index-level SBOMs for multi-arch
  images ([#589](https://github.com/brawer/osmdiffs/issues/589)).
