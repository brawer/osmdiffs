# Changelog

## [0.8.5](https://github.com/brawer/osmdiffs/compare/v0.8.4...v0.8.5) (2026-09-15)


### 🐞 Bug Fixes

* **deps:** bump rustls to 0.23.45, fixing RUSTSEC-2026-0285 ([#811](https://github.com/brawer/osmdiffs/issues/811)) ([c20d695](https://github.com/brawer/osmdiffs/commit/c20d6958bc6d15e1d94dabbe00c9f070aa45b66d))
* give every osm.prune/osm.assemble stage a real staleness check ([#809](https://github.com/brawer/osmdiffs/issues/809)) ([75c2460](https://github.com/brawer/osmdiffs/commit/75c2460463e060563f9a54f2543a3a6dea9ee869))


### 📚 Documentation

* point at brawer.ch, drop staging-domain language ([#808](https://github.com/brawer/osmdiffs/issues/808)) ([e83fbe2](https://github.com/brawer/osmdiffs/commit/e83fbe27f7bb289550c42e3e7f7ccc9d143e6852))

## [0.8.4](https://github.com/brawer/osmdiffs/compare/v0.8.3...v0.8.4) (2026-09-13)


### 🆕 Enhancements

* draw the ATP↔OSM connector line in conflated.pmtiles at high zoom ([#775](https://github.com/brawer/osmdiffs/issues/775)) ([#778](https://github.com/brawer/osmdiffs/issues/778)) ([2f082b3](https://github.com/brawer/osmdiffs/commit/2f082b31bca648907b46c5dc4713171518c2cf7d))
* minimal conflated-overview tiles; full-tag detail for unmatched too ([#783](https://github.com/brawer/osmdiffs/issues/783)) ([f91715f](https://github.com/brawer/osmdiffs/commit/f91715f76f8b1e1dc11e465d2e2d79b5acd85838))
* **pipeline:** make conflated.parquet + provenance BOM reproducible ([#792](https://github.com/brawer/osmdiffs/issues/792)) ([344746d](https://github.com/brawer/osmdiffs/commit/344746d9a2b98bafffdf02cd644c828a139daa74)), closes [#791](https://github.com/brawer/osmdiffs/issues/791)
* **pipeline:** normalise --run_id instead of rejecting unsafe values ([#795](https://github.com/brawer/osmdiffs/issues/795)) ([892c4cc](https://github.com/brawer/osmdiffs/commit/892c4cc34f5f3892f3d3eb3084c240f93308305d)), closes [#791](https://github.com/brawer/osmdiffs/issues/791)
* **publish:** dated /data/ uploads + Frictionless datapackage.json ([#794](https://github.com/brawer/osmdiffs/issues/794)) ([8bafa02](https://github.com/brawer/osmdiffs/commit/8bafa02e54e8c3a6f08e10dd17da0c784856554d))
* render conflated.parquet into its own PMTiles archive ([#709](https://github.com/brawer/osmdiffs/issues/709)) ([#777](https://github.com/brawer/osmdiffs/issues/777)) ([3b443b4](https://github.com/brawer/osmdiffs/commit/3b443b43ddcaa932add4218a2dfb0c8d9570d256))


### 📚 Documentation

* document temporary download location for current output ([#772](https://github.com/brawer/osmdiffs/issues/772)) ([d7ccbd5](https://github.com/brawer/osmdiffs/commit/d7ccbd5608d9f5895ebb3801a82545a0bdbe4173))
* record the /data/ edge-caching contract for the Bunny CDN ([#790](https://github.com/brawer/osmdiffs/issues/790)) ([e8f5548](https://github.com/brawer/osmdiffs/commit/e8f55487e1b79f6968ab34f588dff48ac8984ed7))
* split documentation index by audience ([#773](https://github.com/brawer/osmdiffs/issues/773)) ([0dcbf50](https://github.com/brawer/osmdiffs/commit/0dcbf50c3b649c1e5e336773a9bac7323f73d957))
* turn pipeline diagram's processing steps into real boxes ([#774](https://github.com/brawer/osmdiffs/issues/774)) ([98dbf86](https://github.com/brawer/osmdiffs/commit/98dbf86daf0c2f8e65a0b6da5cbc7134ba627bc4))


### 🚧 Maintenance

* **ci:** Bump taiki-e/install-action from 2.86.2 to 2.86.6 ([#771](https://github.com/brawer/osmdiffs/issues/771)) ([3e6b3ab](https://github.com/brawer/osmdiffs/commit/3e6b3ab056de91afb71af1a1ef991c5f5bf939fa))
* **ci:** Bump taiki-e/install-action from 2.86.6 to 2.87.7 ([#789](https://github.com/brawer/osmdiffs/issues/789)) ([0ecc40d](https://github.com/brawer/osmdiffs/commit/0ecc40d9932f4772e218ea413cf8c413dba313db))
* **ci:** Bump taiki-e/install-action from 2.87.7 to 2.87.8 ([#800](https://github.com/brawer/osmdiffs/issues/800)) ([466c296](https://github.com/brawer/osmdiffs/commit/466c2964f5c6250db50bdd82274abfdd47afc7f0))
* **ci:** Bump the codeql group with 3 updates ([#770](https://github.com/brawer/osmdiffs/issues/770)) ([f66f02b](https://github.com/brawer/osmdiffs/commit/f66f02b289440be5db0efd2b60fc4e2475f6a41e))
* **ci:** Bump the codeql group with 3 updates ([#786](https://github.com/brawer/osmdiffs/issues/786)) ([414f64b](https://github.com/brawer/osmdiffs/commit/414f64b4bfc0a1a1f658720baf93c171010d4978))
* **deps:** Bump boto3 from 1.43.75 to 1.43.83 in /scripts in the uv group ([#784](https://github.com/brawer/osmdiffs/issues/784)) ([e69ec8b](https://github.com/brawer/osmdiffs/commit/e69ec8b266713ebed4f6bd1615992bdec3629f18))
* **deps:** Bump boto3 from 1.43.83 to 1.43.89 in /scripts in the uv group ([#799](https://github.com/brawer/osmdiffs/issues/799)) ([b1f750a](https://github.com/brawer/osmdiffs/commit/b1f750a82ff0a5bfa3780783e6fcef408f62f70e))
* **deps:** Bump boto3 in /scripts in the uv group ([b1f750a](https://github.com/brawer/osmdiffs/commit/b1f750a82ff0a5bfa3780783e6fcef408f62f70e))
* **deps:** Bump boto3 in /scripts in the uv group ([e69ec8b](https://github.com/brawer/osmdiffs/commit/e69ec8b266713ebed4f6bd1615992bdec3629f18))
* **deps:** bump chacha20 0.10.1 -&gt; 0.10.2 (yanked) ([#782](https://github.com/brawer/osmdiffs/issues/782)) ([c2f8abe](https://github.com/brawer/osmdiffs/commit/c2f8abe33bee2bea80ef0e86b529bf6faf2a8a8c))
* **deps:** Bump the cargo group across 1 directory with 34 updates ([#805](https://github.com/brawer/osmdiffs/issues/805)) ([81d0152](https://github.com/brawer/osmdiffs/commit/81d01522e03b9c46225070a3764d23971caf8420))
* **deps:** Bump the cargo group with 21 updates ([#785](https://github.com/brawer/osmdiffs/issues/785)) ([3d19965](https://github.com/brawer/osmdiffs/commit/3d19965bb6811a53d930bec413ee5a8572bc8066))
* ignore .DS_Store ([#798](https://github.com/brawer/osmdiffs/issues/798)) ([ce7c5c1](https://github.com/brawer/osmdiffs/commit/ce7c5c19bb9a3d4476226ae49a7c4716d2a2533e))
* merge conflated tile extraction into one scan ([#780](https://github.com/brawer/osmdiffs/issues/780)) ([14027d1](https://github.com/brawer/osmdiffs/commit/14027d1ec438c0660f7e61c1b240d947d79bf909))
* re-vendor osm-testdata grid at 98a9b32, drop redundant unit test ([#781](https://github.com/brawer/osmdiffs/issues/781)) ([53393d4](https://github.com/brawer/osmdiffs/commit/53393d46e8a1937db1d87d9794e95b0548caf771))
* **release:** adopt release-please for version bumps and CHANGELOG.md ([#806](https://github.com/brawer/osmdiffs/issues/806)) ([0a3ab6a](https://github.com/brawer/osmdiffs/commit/0a3ab6a89822b6f7334365b1fa66a05f3adc3330))
* rename crate and binary osm-diffs -&gt; osmdiffs ([1fe3f31](https://github.com/brawer/osmdiffs/commit/1fe3f3135e547c59ee06f9d8119be03fcb140f4f))
* rename crate and binary osm-diffs → osmdiffs ([#803](https://github.com/brawer/osmdiffs/issues/803)) ([1fe3f31](https://github.com/brawer/osmdiffs/commit/1fe3f3135e547c59ee06f9d8119be03fcb140f4f))
* **sbom:** merge tippecanoe.jq/tile-join.jq into one template ([#779](https://github.com/brawer/osmdiffs/issues/779)) ([d265328](https://github.com/brawer/osmdiffs/commit/d26532873f1ada87b12d1f40b83d6a3980af53e0))
* **tables:** dedupe encode_wkb in geometry tables ([#706](https://github.com/brawer/osmdiffs/issues/706)) ([#797](https://github.com/brawer/osmdiffs/issues/797)) ([cdfba0f](https://github.com/brawer/osmdiffs/commit/cdfba0f59d6e56b71311677004b1f8a9d8e7f519))
* **tables:** use crate::geometry::encode_wkb in geometry tables ([#706](https://github.com/brawer/osmdiffs/issues/706)) ([cdfba0f](https://github.com/brawer/osmdiffs/commit/cdfba0f59d6e56b71311677004b1f8a9d8e7f519))
* update in-repo references for the move to brawer/osmdiffs ([#802](https://github.com/brawer/osmdiffs/issues/802)) ([a6ee6e2](https://github.com/brawer/osmdiffs/commit/a6ee6e2d904f61c1c21791fb26831d165e15fcc4))
* **upload:** split S3 into PUBLIC_S3_* and INTERNAL_S3_* buckets ([#793](https://github.com/brawer/osmdiffs/issues/793)) ([30610c7](https://github.com/brawer/osmdiffs/commit/30610c7c91e8652e044aa587bfb7fa61bc6f24cc)), closes [#791](https://github.com/brawer/osmdiffs/issues/791)

## Changelog

Maintained automatically by
[release-please](https://github.com/googleapis/release-please) from
Conventional-Commits PR titles (see
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md#pr-titles-conventional-commits)
and [`docs/RELEASING.md`](docs/RELEASING.md)) — don't hand-edit entries for
released versions.

This file starts from the first release cut after release-please was
adopted. For earlier releases (up to and including
[v0.8.3](https://github.com/brawer/osmdiffs/releases/tag/v0.8.3)), see the
[GitHub Releases page](https://github.com/brawer/osmdiffs/releases).
