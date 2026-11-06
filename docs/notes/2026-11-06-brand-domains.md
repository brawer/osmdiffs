# Brands from website domains

Many OpenStreetMap (OSM) features link to their brand's website but
don't carry `brand:wikidata`, so `PoiMatcher`
([`src/matchers/poi_matcher.rs`](../../src/matchers/poi_matcher.rs))
could only match them by phone, email or brand name. AllThePlaces (ATP)
knows which domains belong to which brand: every Denner store that it
lists has a page on `denner.ch` and carries `brand:wikidata=Q379911`.
This note describes how we use that as a fallback for a missing
`brand:wikidata`, and how well it works in Switzerland and Japan.

## URL normalization

[`urls.rs`](../../src/matchers/contact/urls.rs) normalizes each
`;`-separated part of a `website`, `contact:website` or `url` value:

- `http`, `https` and no scheme are the same; other schemes such as
  `mailto:` give nothing.
- The host is lowercased, without `www.`, a trailing dot, a user or a
  port. Punycode labels are decoded to Unicode with the `idna` crate.
  The OSM wiki asks mappers to write domains in Unicode, but the data
  is mixed.
- Path and query are percent-decoded if the result is UTF-8. A trailing
  `/`, `index.html`, `index.htm` or `index.php` goes, and so do tracking
  parameters (`utm_*`, `fbclid`, `gclid`). The other parameters are
  sorted.
- The fragment goes, except for single-page app routes (`#/…`, `#!…`).
- Everything is lowercased. Paths are case-sensitive in theory, but
  servers that serve different pages for `/Zuerich` and `/zuerich` are
  rare.

Values without `%` or `xn--` skip the decoding steps. Values that are
already lowercase skip lowercasing.

The registrable domain comes from Mozilla's Public Suffix List, through
the `psl` crate. Its private section covers hosts that give each owner
a subdomain, such as `github.io` or `blogspot.com`. Hosts where the
owner sits in the path, such as `facebook.com/migros` or
`sites.google.com/view/hotel-adler`, are in a short hand-kept list.
Other Google hosts (Maps, `goo.gl`) give no domain at all.

This PR only uses the registrable domain. The full normalized URL is
for the next step, URLs as contact evidence.

## Which domains belong to a brand

While `import_atp` writes `alltheplaces.parquet`, it also collects the
domains of every ATP feature's website. A feature's brand ID is its
`brand:wikidata`, or if that is missing, its `operator:wikidata`, or
else its `network:wikidata`. A feature whose first such tag lists
several IDs, such as a car dealer for two brands, is skipped. A domain
belongs to a brand if all of these hold:

1. At least two ATP features link to it.
2. All of them have the same brand ID. One feature without an ID is
   enough to disqualify the domain: hotels on a tourism portal, or shops
   on a mall's website, have no brand in common.
3. It holds at least 10% of that brand's ATP features with a website.

Rule 3 is there for spiders that list a brand's stockists. In the
2026-10-10 run, `soeder_ch` gives the soap maker's `brand:wikidata` to
Micasa, Pfister and many pharmacies, each with its own website. Before
rule 3, I checked the Swiss domain-to-brand map against Swiss OSM
features that have both a website and a brand or operator ID:
2,037 agreed and 109 disagreed, almost all because of `soeder_ch`. With
rule 3 it was 2,025 to 8. The 8 are parent and child brands, such as
`jumbo.ch` (Jumbo) on OSM features tagged as Coop, or `post.ch` on
PostFinance.

The result goes to `alltheplaces.brand-domains.tsv`, as domain, `Q…`
and feature count, so it is easy to read. Built from the whole
2026-10-10 run, it has 2,676 domains, such as `denner.ch` (886
features), `post.ch` (15,594) and `posten.no` (10,681). Two have
non-ASCII names: `körperformen.com` and `letkøb.dk`. No social network
or Google host made it in.

## Matching

`PoiMatcher` gets a third kind of brand-level evidence, next to phones
and emails (0.98) and brand names (0.96). If the ATP feature's brand ID
has domains in the map, an OSM feature is of that brand when either:

- its own first brand ID (`brand:wikidata`, else `operator:wikidata`,
  else `network:wikidata`) is the same, or
- it has no `brand:wikidata`, and its website is on one of the brand's
  domains.

Like a brand name, this gives factor 0.96 times the category factor
times the distance score. Beyond 150 m, the street address must agree
too. An OSM `brand:wikidata` that differs from the ATP one still vetoes
the match.

The first case came up because of Swiss Post. Its ATP features only have
`operator:wikidata`, and so do most Swiss post offices in OSM, many of
them without a website. Matching on the domain alone would have made
the ATP feature a matcher but left its OSM post office unmatched.

ATP features that get a matcher only through this rule, with nothing but
an `operator:wikidata` or `network:wikidata`, don't count for the
coverage that deletion candidates go by (`Matcher::counts_for_coverage`).
Without that, Switzerland got 361 new deletion candidates, nearly all
post offices. ATP's `swiss_post_ch` lists post offices, but not the
post agencies in village shops that OSM tags with the same
`operator:wikidata`, such as "Post-Agentur Ulrichen".

## Results

Same setup as the earlier notes: a regional OSM extract, and the
2026-10-10 ATP run filtered to the country by `addr:country`. The
brand-domain map was built from that filtered ATP, as the pipeline does
on such a workdir. For Switzerland I also tried the map built from the
whole ATP run. It gave 375 new matches instead of 377, so the regional
runs are a fair stand-in for the planet.

| | Switzerland | Japan |
|---|---:|---:|
| OSM extract | Geofabrik, 2026-10-20 | openstreetmap.fr, 2026-11-05 |
| brand domains | 133 | 146 |
| matched pairs before | 64,062 | 107,123 |
| matched pairs after | 64,439 | 107,531 |
| new matches | 377 | 408 |
| moved to a closer OSM feature | 9 | 10 |
| matches lost | 0 | 0 |
| OSM-only rows before → after | 2,916 → 2,917 | 15,461 → 15,459 |

How the new and moved matches were found:

| | Switzerland | Japan |
|---|---:|---:|
| website domain | 125 | 416 |
| same OSM `operator:wikidata` | 261 | 2 |

All 261 Swiss matches on `operator:wikidata` are Swiss Post.

| distance | Switzerland | Japan |
|---|---:|---:|
| under 25 m | 325 | 348 |
| 25 to 50 m | 47 | 43 |
| 50 to 150 m | 12 | 27 |
| 150 m or more | 2 | 0 |

The brands with the most domain matches were, in Switzerland, Die Post
(55), Denner (19), update Fitness (12), Neuroth (6) and Medbase (5), and
in Japan, 日本郵便 (Japan Post, 240), ホテルルートイン (Route Inn, 25),
スターバックス (Starbucks, 16), ほっともっと (Hotto Motto, 11) and
サイゼリヤ (Saizeriya, 11). Most OSM features had the brand's home page,
such as `https://www.post.japanpost.jp/`, or a store page on the
brand's domain.

The moved matches were ATP features whose earlier match was a feature
of the same brand 124 to 400 m away. They now match one 2 to 61 m away,
such as eight Swiss Denner stores that OSM tags as "Denner Express".

I looked at 30 random new matches in each country. In Japan all 30 had
the same name on both sides, give or take a branch suffix. In
Switzerland, 29 were clearly the same place. The one I'm unsure about
is ATP's "Post Filiale 8411 Winterthur 11 Deutweg", which matched OSM's
"Post Geiselweid" 17 m away.

## International URLs

In the 2026-10-10 ATP run, 1,066 of the 616,387 Japanese features with a
website have a `%` in it, nearly all from `workman_jp`. None have
Punycode, and 32 have non-ASCII characters. On the Swiss OSM side, 388
of 95,925 website values have a `%` and 64 have Punycode. So
normalization rarely matters for domains, but it is cheap.

## Websites that are social media pages

In Switzerland, 0.4% of OSM website values are really social media or
directory pages: 189 Facebook, 71 Instagram, plus Google, Booking and
local.ch links. In ATP it's 0.02%. Such values never become brand
domains: `facebook.com/…` pages are told apart by owner, and none of
them has two ATP features with the same brand.
