# Postal partners

In the AllThePlaces run of 2026-11-14, 150,587 features in 43 spiders are `amenity=yes` with
`post_office=post_partner`: a postal or parcel counter inside another business, such as a DHL counter in a kiosk or a
FedEx counter in a Walgreens. Their match mask was empty, so `import_atp` dropped them
([brawer/osmdiffs#1016](https://github.com/brawer/osmdiffs/issues/1016)). The largest spiders are `fedex` (57,269),
`easy_point_tr` (9,899), `deutsche_post_de` (9,884), `royal_mail_gb` (8,296), `postnl_nl_be` (8,204), `packeta`
(6,703), `la_poste_fr` (5,986) and `dpd_de` (4,879).

OSM doesn't map a partner as a place of its own. It puts the tags on the host, such as `shop=kiosk` with
`post_office=post_partner` and `post_office:brand=DHL`, and sometimes maps an `amenity=post_office` node inside the
store. So a partner either matches the OSM feature that already says it is a partner, or else is `located_in` its host,
with edits that add the `post_office` tags. A partner with neither gets no row, since there is no stand-alone place to
add.

## What ATP gives

The spiders say which network a partner belongs to in different ways: `post_office:brand` (`royal_mail_gb`,
`postnl_nl_be`, `packeta`, `swiss_post_ch`, `dpd_de`), `post_office:service_provider` (`kuroneko_jp`,
`easy_point_tr`), `brand` (`fedex`, `thailand_post_th`), `operator` (`correios_br`), or not at all
(`deutsche_post_de`, `czech_post_cz`).

Most give the host's name as the partner's `name`, such as `Kiosk Waibel` or `REWE Adrian Powierski OHG` for
`deutsche_post_de` and `Volg` or `Coop` for `swiss_post_ch`. `fedex` names its counters `FedEx OnSite` and gives the
host in `located_in` (13,039 of 17,747 in the US: 6,742 Walgreens, 6,222 Dollar General, 74 Duane Reade); its 4,708
`FedEx Authorized ShipCenter` counters don't say where they are. `correios_br` gives the agency's name (`AGF
GAMELEIRA`), `thailand_post_th` and `la_poste_fr` only a `branch`.

## How far OSM is

For each ATP partner, I looked for an OSM feature with `post_office=post_partner`, or an `amenity=post_office` without
another `post_office` value, of the same network ([The rules](#the-rules)), and for an OSM store whose name is the
host's name, and for comparison the same after moving the ATP partner 2 km in a random direction ("chance"). ATP of
2026-11-14 filtered by `addr:country`, against the OSM extracts of the [eval regions](2026-11-06-eval-regions.md);
partners count when there is an OSM store or post office within 1 km.

| spider, region | ATP partners | OSM partner of the network ≤ 30 m | host by name ≤ 100 m | either | chance: partner ≤ 30 m | chance: host ≤ 100 m |
|---|---:|---:|---:|---:|---:|---:|
| deutsche_post_de, Berlin | 770 | 5% | 46% | 48% | 0% | 0% |
| dpd_de, Berlin | 23 | 0% | 22% | 22% | 0% | 0% |
| fedex, Massachusetts | 234 | 16% | 59% | 59% | 0% | 0% |
| swiss_post_ch, Switzerland | 1,321 | 41% | 67% | 72% | 0% | 0% |
| correios_br, Brazil Sudeste | 1,719 | 6% | 0% | 7% | 0% | 0% |
| fedex, Brazil Sudeste | 56 | 0% | 0% | 0% | 0% | 0% |
| thailand_post_th, Thailand | 205 | 4% | 0% | 4% | 0% | 0% |
| fedex, UAE | 57 | 0% | 0% | 0% | 0% | 0% |

Chance stays under 0.5% everywhere. Beyond 100 m it doesn't: in Berlin, a store of the host's name 100–200 m away was
about as common after the move (9 partners) as at the real position (11).

OSM maps few partners. Berlin has 196 features with `post_office=post_partner` (52 `shop=convenience`, 35
`shop=kiosk`, 24 `shop=newsagent`, 24 `amenity=post_office`), and 179 more post offices without a `post_office` tag,
35 of them DHL ones in kiosks and stores. Switzerland has 551 partners, mostly supermarkets (310) and convenience stores
(120); Massachusetts has 40, all `FedEx OnSite` on a Walgreens. Their `post_office:brand` varies: `DHL`, `DHL
Paketshop`, `DHL-Paketshop` and `Deutsche Post` in Berlin, `Die Post`, `Die Post AG`, `La Poste` and `Post CH AG` in
Switzerland.

The address alone doesn't find the host. In Berlin, 429 of the 770 DHL partners have an OSM store at their street and
house number within 30 m, but in a sample of 40 of these, half were another business in the same building: a
hairdresser, a pub, a dentist.

## The rules

A new matcher in `matchers::post_partner` takes ATP features that have `post_office=post_partner` and nothing else
that says what they are. A partner that ATP lists as a store, such as the 2,953 Lawson stores of `lawson_jp` with
`shop=convenience`, is the store, and the POI matcher takes it as before. The tag gets a mask bit, so that
`import_atp` keeps the partners.

- **The network must agree.** Two partners may be of one network if they share a Wikidata ID or a word of their
  `post_office:brand`, `post_office:service_provider`, `brand` or `operator` (folded, at least three letters, without
  generic words such as `post`, `poste`, `paketshop`, `partner`, `onsite`), or if one side has nothing to compare.
  `Die Post` and `Die Post AG` have no such words, so they compare as unknown. On the OSM side, an
  `amenity=post_office` without a postal brand counts by its `brand` and `operator`, or else by its `name`, so a post
  office named `Hermes PaketShop` isn't a DHL partner. `deutsche_post_de` gives no brand, so its partners count as `DHL`
  (Q489815), which also agrees with `Deutsche Post` (Q157645).
- **`same`, rule `post_partner`:** an OSM partner of the network within 30 m, by distance alone.
- **`same`, rule `post_partner_name`:** an OSM partner of the network within 100 m that is also the host by name, as
  below. It scores higher than the first.
- **A partner of an unknown network only matches an OSM `post_office=post_partner`**, not a bare post office. In the
  first run over Japan, three Yamato partners of `kuroneko_jp`, before the matcher read `post_office:service_provider`,
  took Japan Post offices.
- **`located_in`, rule `post_partner`:** otherwise, the nearest OSM store within 100 m whose `name` or `brand` is the
  host's name: the same words after folding and without legal forms (`GmbH`, `OHG`, `e.K.`, `AG`, `Inc`), or one
  name followed by more words, as in `REWE Adrian Powierski OHG` for `REWE`. The host's name is the partner's
  `located_in`, or else its `name`, unless the name is the network's own, as in `FedEx OnSite` or `Postpartner
  Nendeln`. At the partner's address, it is enough that the distinctive words of one name are all in the other, as in
  `Hust Getränkemarkt` for `Getränkemarkt Hust` or `Harmonie` for `Bäckerei Harmonie`. Generic words such as `kiosk`,
  `späti`, `markt` or `bäckerei`, numbers and words of the address don't count, so `Backshop Waterkant` isn't `Physio
  an der Waterkant`. Car parks, parcel lockers and the other `NOT_HOST_CATEGORIES` of `located_in` are never hosts.
- **No `missing_in_osm`.** A partner without either gets no row.
- **No coverage.** Partners don't count for deletion candidates: that DHL lists a counter says nothing about which OSM
  stores are gone. A host is marked as matched, like other `located_in` hosts, so a store that DHL puts a counter in
  is no deletion candidate any more.
- **Exclusive.** Two partners of one spider don't take the same OSM partner.
- **Not in the contact tables.** A partner's phone and website don't go into the nondescript phones and emails, nor
  into the brand domains. In Thailand, a partner had the phone of its post office, which made that phone nondescript and
  dropped the office's own `missing_in_osm` row; a Royal Mail partner linking `royalmail.com` would make the domain
  look shared. With this, no other ATP feature's row changed in any region.

### Edits

For both relations, the edits only add `post_office` tags: `post_office=post_partner` if OSM has no `post_office` tag,
and the network as `post_office:brand` with its `post_office:brand:wikidata` if OSM has none. If OSM's brand is
another network's, ATP's is appended, as in `Hermes PaketShop;DHL`, and so is its Wikidata ID if OSM's IDs line up with
its brands. If OSM has the brand but not its ID, the ID is added. Opening hours, phones and websites aren't suggested,
since they may be the counter's rather than the store's. An operator such as `Empresa Brasileira de Correios e
Telégrafos` isn't suggested as a brand.

The edits of several ATP features for one OSM feature are now merged: the other tags still come from one of them, but
the `post_office` tags add up, so a kiosk that is a DHL and a DPD partner gets `DHL;DPD`. The edits for one feature now
come in a fixed order. On main, which edit of several wins depends on thread timing: two runs of main on the same
Berlin `conflated.parquet` gave different tags for 6 features. With a fixed order, 1–583 edits per region (583 in
Japan, 117 in Switzerland, 29 in Berlin) get the tags of another of their ATP features than on main. That is the same
for main's matches and needs no partners.

## Results in the eval regions

ATP of 2026-11-14 filtered by `addr:country`, with osmdiffs at 64f6037 against this change, each a full run up to
`conflated.parquet`, and `suggest_edits` on it.

| region | ATP partners in the country | `same` | `located_in` | deletion candidates | edits with `post_office` tags | other rows changed |
|---|---:|---:|---:|---:|---:|---:|
| Germany: Berlin | 17,695 (DHL, DPD, GLS) | +40 | +325 | −13 hosts | 354 | 0 |
| US: Massachusetts | 17,747 (FedEx) | +39 | +99 | −4 hosts | 138 | 0 |
| Switzerland | 1,325 | +933 | +128 | −6 hosts | 954 | 0 |
| Brazil: Sudeste | 5,961 | +139 | +10 | 0 | 149 | 0 |
| Thailand | 232 | +9 | 0 | −8 post offices | 9 | 0 |
| Japan | (`kuroneko_jp`) | 0 | +40 | 0 | 40 | 0 |
| UAE, Kenya, Karnataka, Tamil Nadu | 62 in UAE | 0 | 0 | 0 | 0 | 0 |

As with the lockers, ATP covers the whole country while Berlin, Massachusetts and Brazil Sudeste are extracts, so most
of a country's partners lie outside them. In Berlin, 770 DHL partners are inside, and 358 of them now have a row.

- **Berlin.** 39 DHL partners are `same` as an OSM partner or DHL post office (13 also by name), and 319 are
  `located_in` their host: REWE and EDEKA supermarkets, kiosks, Spätis, bakeries, florists and tailors. 12 REWE and a
  Nahkauf had been deletion candidates, and now host a DHL counter. Two hosts already were Hermes partners and get
  `Hermes PaketShop;DHL`.
- **Massachusetts.** 38 FedEx OnSite counters are `same` as an OSM Walgreens that already has `post_office:brand=FedEx
  OnSite`, and 99 are `located_in` the Walgreens or Dollar General named in their `located_in`. Four Walgreens had
  been deletion candidates. One ShipCenter is `same` as the OSM `FedEx Ship Center` post office 7 m away.
- **Switzerland.** 476 partners are `same` as an OSM partner that is also their host by name, mostly Volg, Coop, Spar
  and Denner stores, and 457 by distance alone, mostly OSM post offices mapped inside the store, such as `Postagentur
  Dachsen (im Volg)`. 128 are `located_in` a store without the tags yet. 603 edits add `post_office:brand=Die Post`,
  and 347 more add only its Wikidata ID to an existing `Die Post`.
- **Brazil.** 139 Correios agencies (`AGF`, `AGC`) are `same` as an OSM Correios post office; the edits add
  `post_office=post_partner`.
- **Thailand.** 9 Thailand Post partners are `same` as OSM post offices, such as `Thailand Post Shop`. 8 of these had
  been deletion candidates: ATP's list of post offices doesn't have them, since they are partners.
- **Japan.** `kuroneko_jp` failed in the 2026-11-14 run, so the pipeline took it from an older run, which lists Yamato
  partners with `post_office:service_provider=ヤマト運輸`. 40 are `located_in` their host, such as pharmacies,
  dry cleaners and supermarkets.

## Precision

I checked samples by hand. Of 40 random new rows in Berlin, 37 look right; three are doubtful: `CIGO im EDEKA` is
`located_in` the EDEKA rather than the CIGO inside it, which OSM lacks, and `Bistro Dogan` and `Lion Bäckerei` are
`located_in` an OSM `Bistro` and `Lion` at their address, which are probably the same stores. Of 30 in Switzerland, 29
look right; `Drogerie Prima Ott AG` is `located_in` the `Prima` supermarket at its address. All 138 rows in
Massachusetts are in the Walgreens or Dollar General that ATP names, or the matching FedEx post office. Of 25 in
Brazil, all are Correios agencies, one of them `located_in` itself: an OSM post office `PCL Ponto de Coleta A Diniz`
without brand, whose name doesn't agree with ATP's operator, so it is the host rather than `same`. The 40 in Japan all
have the host's name.

Before the fixes above, the first runs showed two more kinds of wrong match, which the rules now handle: a DHL partner
that took an OSM post office named `Hermes PaketShop`, and a `Backshop Waterkant` in `Physio an der Waterkant`.

## What we left out

- **Partners whose host ATP doesn't name.** FedEx's 4,708 ShipCenters in the US, and `thailand_post_th`,
  `la_poste_fr` (only a `branch` such as `PARIS MALAKOFF CARREFOUR CITY RP`) and `correios_br`, only match an OSM
  partner.
- **Names with the city or network in front.** `czech_post_cz` names its partners `Olomouc Hana Chvojková`, and
  `easy_point_tr` `Easy Point Esnaf - Baran Petshop Akşemsettin`. Neither is in an eval region, and both need the prefix
  removed first.
- **`czech_post_cz` and `easy_point_tr` have no network to suggest**; their partners only get `post_office=post_partner`.
- **Removing `post_office` tags.** An OSM store whose network no longer lists it as a partner could lose its tags, but
  the partner lists are incomplete in OSM's view as long as most partners aren't matched.
