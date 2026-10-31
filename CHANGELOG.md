# Changelog

## [0.8.8](https://github.com/brawer/osmdiffs/compare/v0.8.7...v0.8.8) (2026-10-31)


### 🆕 Enhancements

* add importance column to conflated.parquet ([#853](https://github.com/brawer/osmdiffs/issues/853)) ([2d017a2](https://github.com/brawer/osmdiffs/commit/2d017a232c89e4da7df7b664c3a4bab5d33da554))
* collect brand Wikidata IDs from OSM as well as ATP ([#847](https://github.com/brawer/osmdiffs/issues/847)) ([d87e4b8](https://github.com/brawer/osmdiffs/commit/d87e4b85634e007948f45a3cdd80932dc01daa03))
* compute denylist of ATP switchboard phone numbers ([#844](https://github.com/brawer/osmdiffs/issues/844)) ([b1ef57f](https://github.com/brawer/osmdiffs/commit/b1ef57feec311433b5f58cd4141133aed6fc8bbd))
* compute nondescript email addresses from AllThePlaces ([#846](https://github.com/brawer/osmdiffs/issues/846)) ([b38b49e](https://github.com/brawer/osmdiffs/commit/b38b49e8773f03da72f2643e84919e8e410f0c16))
* compute OSMViews ranks for AllThePlaces locations ([#851](https://github.com/brawer/osmdiffs/issues/851)) ([6e383c4](https://github.com/brawer/osmdiffs/commit/6e383c44ec6e0d7423b35661f125fae342de3cc8))
* compute QRank quantiles for brand Wikidata IDs ([#848](https://github.com/brawer/osmdiffs/issues/848)) ([e1b6a99](https://github.com/brawer/osmdiffs/commit/e1b6a99365742371f1830c44c0fe3e091ab2daac))
* **conflate:** match transit features one to one within a spider ([#864](https://github.com/brawer/osmdiffs/issues/864)) ([0343aae](https://github.com/brawer/osmdiffs/commit/0343aaeca73fd9c3255de0fecb3a5ff4e64008ee))
* **matchers:** match by phone number and email address ([#850](https://github.com/brawer/osmdiffs/issues/850)) ([ca68536](https://github.com/brawer/osmdiffs/commit/ca68536ffc39e43b837a450f8e6cb138ef800863))
* **matchers:** match public transport stops, platforms and tracks ([#863](https://github.com/brawer/osmdiffs/issues/863)) ([4093e8b](https://github.com/brawer/osmdiffs/commit/4093e8ba7ae3d41a4f1df3c9db1ba88b1f2a2b39))
* **matchers:** split MatchMask into category and modifier bits, add importance ([#852](https://github.com/brawer/osmdiffs/issues/852)) ([e3bcdd9](https://github.com/brawer/osmdiffs/commit/e3bcdd94a7cb537adb1dfd19e61fefa67bbba4d6))
* **tiles:** show the most important places at coarse zooms ([#854](https://github.com/brawer/osmdiffs/issues/854)) ([2f85f6d](https://github.com/brawer/osmdiffs/commit/2f85f6d6a6f7b9eb6e3e3ae35bf7e104ae543338))


### 🐞 Bug Fixes

* **deps:** bump tokio to 1.54.1 for RUSTSEC-2026-0386 ([#849](https://github.com/brawer/osmdiffs/issues/849)) ([8eab159](https://github.com/brawer/osmdiffs/commit/8eab159dc851a8ef012148e420f7d0b967914a56))
* **deps:** bump zstd crates for RUSTSEC-2026-0410 ([#869](https://github.com/brawer/osmdiffs/issues/869)) ([a44b9be](https://github.com/brawer/osmdiffs/commit/a44b9bea6db8f00e1e70a3e688a049a35c7a1cd7))
* **deps:** bump zstd crates for RUSTSEC-2026-0410 and related advisories ([a44b9be](https://github.com/brawer/osmdiffs/commit/a44b9bea6db8f00e1e70a3e688a049a35c7a1cd7))
* **deps:** upgrade to Rust 1.99.1 and bump dependencies ([#870](https://github.com/brawer/osmdiffs/issues/870)) ([aedffd7](https://github.com/brawer/osmdiffs/commit/aedffd7403f831b903da891a0c80cd10f3d4329b))
* **matchers:** match multi-valued brand:wikidata tags ([#842](https://github.com/brawer/osmdiffs/issues/842)) ([e883f63](https://github.com/brawer/osmdiffs/commit/e883f63d6738a717c40140b161e8411029b06581))
* **matchers:** treat bike-share stations as street furniture ([#843](https://github.com/brawer/osmdiffs/issues/843)) ([c298614](https://github.com/brawer/osmdiffs/commit/c298614fcf556592b63bc7803476ef0550508d43))


### 📚 Documentation

* add design note on merging ranks into importance ([#855](https://github.com/brawer/osmdiffs/issues/855)) ([0706845](https://github.com/brawer/osmdiffs/commit/07068458951438299215068ed013c3f6140adc2c))
* list protoc as a build prerequisite in CONTRIBUTING.md ([#840](https://github.com/brawer/osmdiffs/issues/840)) ([5eb4d45](https://github.com/brawer/osmdiffs/commit/5eb4d454478b9edf3a6597c3593c1cf28127ef23))
* **outputs:** add Windows download instructions ([#865](https://github.com/brawer/osmdiffs/issues/865)) ([d436fd4](https://github.com/brawer/osmdiffs/commit/d436fd40d505cecfc0c2a5c33d5b64fd34098df4))


### 🚧 Maintenance

* **ci:** Bump actions/download-artifact from 8.0.1 to 8.0.2 ([#857](https://github.com/brawer/osmdiffs/issues/857)) ([4ccc765](https://github.com/brawer/osmdiffs/commit/4ccc76596cf94a68bd5b5922677400f79e9159ac))
* **ci:** Bump docker/login-action from 4.6.0 to 4.7.0 ([#859](https://github.com/brawer/osmdiffs/issues/859)) ([9968e32](https://github.com/brawer/osmdiffs/commit/9968e32ea33fc98c71ac0a344cd1ad7d528763cc))
* **ci:** Bump Swatinem/rust-cache from 2.9.2 to 2.9.3 ([#860](https://github.com/brawer/osmdiffs/issues/860)) ([fb35a43](https://github.com/brawer/osmdiffs/commit/fb35a439148ba3dbd1f522c10b1b7faf548fd2f4))
* **ci:** Bump taiki-e/install-action from 2.87.27 to 2.88.0 ([#858](https://github.com/brawer/osmdiffs/issues/858)) ([d1bd3c7](https://github.com/brawer/osmdiffs/commit/d1bd3c79165c01e6287c5cd0b73bb2266e814a5e))
* **ci:** Bump the codeql group with 3 updates ([#856](https://github.com/brawer/osmdiffs/issues/856)) ([684150a](https://github.com/brawer/osmdiffs/commit/684150aad1a4b2df5dafc871ad88268e4983e96c))
* embed dependency list with cargo-auditable ([#861](https://github.com/brawer/osmdiffs/issues/861)) ([373eeca](https://github.com/brawer/osmdiffs/commit/373eecaddda9693a344b08d13331a93630e91ed4))
* rename phone denylist to nondescript phones ([#845](https://github.com/brawer/osmdiffs/issues/845)) ([f30f130](https://github.com/brawer/osmdiffs/commit/f30f13044d442cb2a2713d7a83b1891bbd9eb533))

## [0.8.7](https://github.com/brawer/osmdiffs/compare/v0.8.6...v0.8.7) (2026-10-23)


### 🐞 Bug Fixes

* **deps:** Bump rustls and webpki crates for security advisories ([#839](https://github.com/brawer/osmdiffs/issues/839)) ([d19072c](https://github.com/brawer/osmdiffs/commit/d19072c41f40d191be55f3b21f0e7b8a13ae62bc))
* drop resource type from datapackage.json ([6b40600](https://github.com/brawer/osmdiffs/commit/6b40600fc835717289a7ca4acde371ef032f4c83))
* **pipeline:** Drop resource type from datapackage.json ([#838](https://github.com/brawer/osmdiffs/issues/838)) ([6b40600](https://github.com/brawer/osmdiffs/commit/6b40600fc835717289a7ca4acde371ef032f4c83))


### 🚧 Maintenance

* **ci:** Bump actions/upload-artifact from 7.0.1 to 7.0.2 ([#832](https://github.com/brawer/osmdiffs/issues/832)) ([0fb543e](https://github.com/brawer/osmdiffs/commit/0fb543eb03635b96fac99998cb59aa6aa0ea9209))
* **ci:** Bump astral-sh/setup-uv from 10.2.0 to 10.3.0 ([#835](https://github.com/brawer/osmdiffs/issues/835)) ([12e7e77](https://github.com/brawer/osmdiffs/commit/12e7e77a61efb769f6d24725898f644a1fa9e639))
* **ci:** Bump ossf/scorecard-action from 2.4.4 to 2.5.0 ([#833](https://github.com/brawer/osmdiffs/issues/833)) ([a4e45af](https://github.com/brawer/osmdiffs/commit/a4e45af9db55e7a93578efba72ae78f4fdb5380f))
* **ci:** Bump taiki-e/install-action from 2.87.21 to 2.87.24 ([#827](https://github.com/brawer/osmdiffs/issues/827)) ([5a34868](https://github.com/brawer/osmdiffs/commit/5a348681103360e948abb15664554a165caee912))
* **ci:** Bump taiki-e/install-action from 2.87.24 to 2.87.27 ([#836](https://github.com/brawer/osmdiffs/issues/836)) ([6169d3b](https://github.com/brawer/osmdiffs/commit/6169d3bffaecbe2add714fbdb82051a1016bc7b5))
* **ci:** Bump the codeql group across 1 directory with 3 updates ([#834](https://github.com/brawer/osmdiffs/issues/834)) ([8238469](https://github.com/brawer/osmdiffs/commit/823846993b798e7c327f970f0dee040024359cec))
* **deps:** Bump rust from 1.98.1-alpine3.23 to 1.99.0-alpine3.23 ([#828](https://github.com/brawer/osmdiffs/issues/828)) ([41af560](https://github.com/brawer/osmdiffs/commit/41af560852f6f27f0c481439b180aa33394c8de5))
* **deps:** Bump rust from 1.99.0-alpine3.23 to 1.99.1-alpine3.23 ([#837](https://github.com/brawer/osmdiffs/issues/837)) ([9cecbaa](https://github.com/brawer/osmdiffs/commit/9cecbaa6318ebb34d7d262d6790c1ba435933205))
* **pipeline:** Fetch all inputs up front, in parallel ([#831](https://github.com/brawer/osmdiffs/issues/831)) ([6d5402c](https://github.com/brawer/osmdiffs/commit/6d5402c539eb9f0d49ec46c4568d3384818805a4)), closes [#830](https://github.com/brawer/osmdiffs/issues/830)

## [0.8.6](https://github.com/brawer/osmdiffs/compare/v0.8.5...v0.8.6) (2026-10-05)


### 📚 Documentation

* add learnings from a real Infomaniak Kubernetes run ([#817](https://github.com/brawer/osmdiffs/issues/817)) ([cd4cb88](https://github.com/brawer/osmdiffs/commit/cd4cb8883aee0fb576fa3b977f8af8d0243cce5b))
* mark osmdiffs.brawer.ch as the final hostname ([#818](https://github.com/brawer/osmdiffs/issues/818)) ([8af7c1f](https://github.com/brawer/osmdiffs/commit/8af7c1feb4460f2440dcd5499ba25c80c7036579))
* note the image runs as UID 1000 ([#812](https://github.com/brawer/osmdiffs/issues/812)) ([bf80100](https://github.com/brawer/osmdiffs/commit/bf8010053b55877523215c52e2b25bc8184dccfb))


### 🚧 Maintenance

* **ci:** Bump astral-sh/setup-uv from 10.0.1 to 10.1.0 ([#816](https://github.com/brawer/osmdiffs/issues/816)) ([426560a](https://github.com/brawer/osmdiffs/commit/426560aad6ca18d00937bf0c555189c991832cfc))
* **ci:** Bump astral-sh/setup-uv from 10.1.0 to 10.2.0 ([#823](https://github.com/brawer/osmdiffs/issues/823)) ([951e1b5](https://github.com/brawer/osmdiffs/commit/951e1b50dc0949f5fb732a6b502b9166de2efcfd))
* **ci:** Bump taiki-e/install-action from 2.87.8 to 2.87.21 ([#821](https://github.com/brawer/osmdiffs/issues/821)) ([2826a36](https://github.com/brawer/osmdiffs/commit/2826a3628ba15fbdc00e768e32735d09c039fabe))
* **ci:** Bump the codeql group with 3 updates ([#814](https://github.com/brawer/osmdiffs/issues/814)) ([c6c4ca5](https://github.com/brawer/osmdiffs/commit/c6c4ca5b5d6b3e62b4d17f3b9b7890b89ef24e8e))
* **ci:** Bump the codeql group with 3 updates ([#820](https://github.com/brawer/osmdiffs/issues/820)) ([d7bad60](https://github.com/brawer/osmdiffs/commit/d7bad60e3fb0887c21e171c1ae9cdd5011b657d3))
* **ci:** Bump the codeql group with 3 updates ([#822](https://github.com/brawer/osmdiffs/issues/822)) ([ad2a1ae](https://github.com/brawer/osmdiffs/commit/ad2a1aea2b7a7c000697f26685879502ff505a1f))
* **deps:** Bump boto3 from 1.43.89 to 1.43.103 in /scripts in the uv group ([#824](https://github.com/brawer/osmdiffs/issues/824)) ([75b4c16](https://github.com/brawer/osmdiffs/commit/75b4c162c4eeafb41475cc6d18361e64e26308ab))
* **deps:** Bump boto3 in /scripts in the uv group ([75b4c16](https://github.com/brawer/osmdiffs/commit/75b4c162c4eeafb41475cc6d18361e64e26308ab))
* **deps:** Bump rust from 1.98.0-alpine3.23 to 1.98.1-alpine3.23 ([#819](https://github.com/brawer/osmdiffs/issues/819)) ([d571233](https://github.com/brawer/osmdiffs/commit/d571233524f45c0449b06d9cdc9e69ead7ceb308))
* **deps:** Bump the cargo group with 55 updates ([#825](https://github.com/brawer/osmdiffs/issues/825)) ([1151d82](https://github.com/brawer/osmdiffs/commit/1151d82de7c99fa8f507f36f001f8f4e8db5dcd8))
* **deps:** Bump urllib3 from 2.7.0 to 2.8.0 in /scripts ([#826](https://github.com/brawer/osmdiffs/issues/826)) ([8a7d50d](https://github.com/brawer/osmdiffs/commit/8a7d50d726c41d4f912f10a3afa8f65366fae697))

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
