# Matching by website

Phone numbers and email addresses already count as contact evidence in
`PoiMatcher` ([`src/matchers/contact/`](../../src/matchers/contact/),
see [`2026-10-25-phone-and-email-matching.md`](2026-10-25-phone-and-email-matching.md)).
This note adds websites, the third common contact in OpenStreetMap (OSM)
and AllThePlaces (ATP). Many hotels, schools, libraries and car dealers
have a page of their own but no `brand:wikidata`.

## The rule

The `website`, `contact:website` and `url` values on both sides are
normalized as described in
[`2026-11-06-brand-domains.md`](2026-11-06-brand-domains.md): scheme,
`www.`, case, Punycode, percent escapes, a trailing `/` or `index.html`
and tracking parameters make no difference. Each normalized URL is
hashed to 64 bits, with the same hash as emails. If an ATP feature and
an OSM candidate share a URL, it is a contact match like a shared phone
or email: 0.98 times the category factor times the distance score, and
beyond 150 m the street address must agree too.

On Swiss ATP features that already match an OSM feature, and that
carry a website on both sides, the URLs are the same 36% of the time
and the hosts 70% of the time; in Japan, 19% and 28%. Most of the rest
are a brand's home page on one side and a store page on the other, or a
store page on a store locator such as `navitime.co.jp`. So, as for
phones and emails, a website that differs is no reason against a match.

## Nondescript websites

A URL that ATP lists for places far apart says nothing about which
place a feature is, like a hotline. The [nondescript
sets](2026-10-24-shared-phone-numbers.md) get a third one,
`alltheplaces.nondescript-websites`, and contact matching skips the
URLs in it.

For phones and emails, a value is nondescript when the places that
carry it span 1 km or more. Places at one site, such as a pharmacy and
the Western Union desk inside it, often share a number. For websites,
1 km was too loose. In the first Swiss run, 76 of the new matches were
Zürich kindergartens. The city's school units each have a page such as
`stadt-zuerich.ch/schulen/de/riedhof/ueberuns/standorte/kindergaerten.html`
for all their kindergartens, a few hundred meters apart, and ATP's
`Spulenweg 2` matched OSM's `Kindergarten Pergamin 1+2` 128 m away. So
websites are nondescript from 100 m. Places that share a page within
100 m, such as several kindergarten classes in one building or a car
dealer listed once per brand, still match. With that, the Swiss
kindergarten matches went from 76 to 10, and the hotel matches from 464
to 463. Switzerland has 268 nondescript websites, against 110 at 1 km.

## Deletion candidates

An ATP feature with a website and an `operator:wikidata`, but no
`brand:wikidata`, brand name, phone or email, now gets a matcher. In
the first Swiss run, that made 157 OSM features deletion candidates,
145 of them post offices: ATP's `swiss_post_ch` gives each post office
a page of its own, so the spider counted for the coverage of Swiss
Post, but it doesn't list the post agencies in village shops. As with
brand domains, an ATP feature whose only evidence is a website no longer counts for
coverage (`Matcher::counts_for_coverage`). With that, no region has
new OSM-only rows.

## Results

Same setup as in
[`2026-11-06-brand-domains.md`](2026-11-06-brand-domains.md), on the
[eval regions](2026-11-06-eval-regions.md): each region on its
own, with ATP filtered to it, comparing the pipeline with brand domains
against the one that adds websites. These numbers are the baseline for
later changes to website matching.

| region | nondescript websites | matched pairs before | new | moved | lost | OSM-only rows before → after |
|---|---:|---:|---:|---:|---:|---:|
| Switzerland | 268 | 64,439 | 504 | 6 | 0 | 2,917 → 2,917 |
| Japan | – | 107,531 | 42 | 6 | 0 | 15,459 → 15,459 |
| Massachusetts | 25 | 8,239 | 53 | 0 | 0 | 1,745 → 1,745 |
| Berlin | 23 | 6,424 | 5 | 0 | 0 | 517 → 517 |
| Brazil Sudeste | 78 | 7,069 | 0 | 0 | 0 | 4,705 → 4,705 |
| Thailand | 54 | 9,187 | 1 | 0 | 0 | 3,250 → 3,250 |
| Kenya | 2 | 461 | 1 | 0 | 0 | 127 → 127 |
| United Arab Emirates | 28 | 755 | 2 | 0 | 0 | 285 → 285 |
| Karnataka | 38 | 1,335 | 1 | 0 | 0 | 414 → 414 |
| Tamil Nadu | 34 | 956 | 0 | 0 | 0 | 189 → 189 |

The Japanese set of nondescript websites wasn't kept. No region lost a
match or got new OSM-only rows. Distances of the new and moved matches:

| region | under 25 m | 25–50 m | 50–150 m | 150 m or more |
|---|---:|---:|---:|---:|
| Switzerland | 442 | 34 | 32 | 2 |
| Japan | 42 | 1 | 4 | 1 |
| Massachusetts | 23 | 10 | 20 | 0 |
| Berlin | 0 | 4 | 0 | 1 |
| Thailand | 0 | 0 | 1 | 0 |
| Kenya | 1 | 0 | 0 | 0 |
| United Arab Emirates | 2 | 0 | 0 | 0 |
| Karnataka | 1 | 0 | 0 | 0 |

What matched differs a lot between regions:

- **Switzerland**: 463 hotels and 9 hostels, nearly all from tourism
  board spiders without a brand, plus 29 Zürich schools and
  kindergartens and a few car dealers. 461 of the 510 shared URLs are
  a home page, such as `https://www.hotel-tivoli.ch/`.
- **Japan**: chain stores whose OSM feature has the store's own page on
  a store locator, such as `https://sugi-net.jp/stores/001996`
  (Sugi Pharmacy) or `pkg.navitime.co.jp/…/spot/detail?code=…` (banks,
  Matsunoya). Japanese chains already matched well by brand, so few
  are new. Query parameters matter here: the store's code is in them.
- **Massachusetts**: 34 public and college libraries, and 16 car
  dealers and their service and parts shops. Libraries are often mapped as the building, so 20 matches
  are 50 to 150 m apart.
- **Berlin, Thailand, Kenya, the UAE, Karnataka**: one to five each,
  mostly hotels (Hyatt in Bangkok, Nairobi and Dubai, a Lindner hotel
  in Berlin on `hyatt.com`), libraries (three in Berlin, the Mohammed
  Bin Rashid Library in Dubai) and car dealers.
- **Brazil Sudeste and Tamil Nadu**: none. Their ATP has few websites
  that OSM shares.

I looked at 30 random new matches in Switzerland and Japan, at 12 in
Massachusetts, and at all of them elsewhere. All were the same place.
The names often differ, which is where websites help most: ATP's
"Worcester Polytechnic Institute" is OSM's "George C. Gordon Library",
and "Hyatt Regency Bangkok Sukhumvit" is
"โรงแรม ไฮแอท รีเจนซี่ กรุงเทพฯ สุขุมวิท". The farthest were two Swiss hotels, Corona
(342 m) and Arenenberg (368 m), whose OSM street is in the ATP address.
