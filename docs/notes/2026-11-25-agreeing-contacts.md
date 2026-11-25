# Preferring the candidate whose contact agrees

When an AllThePlaces (ATP) feature had several OpenStreetMap (OSM) candidates with its `brand:wikidata`,
`PoiMatcher` scored them by distance alone, so the closest one won, even when a slightly farther one had the same
phone number or website ([brawer/osmdiffs#1049](https://github.com/brawer/osmdiffs/issues/1049)). This came up in
the [office eval](2026-11-24-offices.md): OSM often maps two offices of one estate agency, its sales and its
property management, where ATP lists one. The ATP office then matched the closer OSM office, and the one with its
own phone became a deletion candidate.

## The change

`PoiMatcher::score_osm_candidate` multiplies the score of every candidate that doesn't share a descriptive phone
number, email address or website with the ATP feature by a new `CONTACT_DISAGREES_FACTOR` of 0.95. Close by, a
shared contact is now worth about 20 m: at 400 m, the distance score loses 1/400 per meter, and 5% of the score is
20 m. Farther out it is worth a bit less, since the score is smaller there.

Three details:

- **Every rule, not just `brand_wikidata`.** The factor applies to all rules except `official_id`, whose score
  ignores evidence factors, and `contact`, whose contact agrees by definition. So among `brand_name`, `address`
  and other candidates, the order of the rules stays as it was. The one rule whose rank changes is `contact`
  (0.98): an OSM feature without a brand of its own but with our phone now wins over one with our `brand:wikidata`
  and another phone, at about the same distance. Before, the branded one always won.
- **ATP features without contacts get the factor too.** The first version applied it only to ATP features
  that had a contact. That made their scores lower than those of features of the same spider without one, and
  under exclusive matching ([2026-11-08](2026-11-08-exclusive-poi-matching.md)) the features of a spider compete by score.
  In Île-de-France, `e_leclerc` "Location" in Coulommiers (with a phone) lost its shopping center to "Drive" (without a
  phone) and moved to an OSM feature 337 m away. With the factor on all candidates that don't agree, a contact that agrees
  raises a score, and a feature with no contacts or with contacts that differ is treated the same, as before.
- **A preference, not a veto.** About one in six right `brand:wikidata` matches has different phone numbers on the two
  sides, so differing contacts still never rule a candidate out.

`agrees` uses the same nondescript sets as the `contact` rule, so a brand hotline or home page doesn't count.

## Results in the eval regions

Each run used ATP of 2026-11-14, filtered by `addr:country`, and ran the pipeline with osmdiffs at 87c1014 against
this change. The extracts were the [eval regions](2026-11-06-eval-regions.md) and Île-de-France (openstreetmap.fr,
2026-11-25), as for the offices. Japan was the whole country (openstreetmap.fr, 2026-11-25). Fallbacks for broken
spiders were turned off. Only ATP features and deletion candidates inside the region's polygon count.

"Moved" means matched to another OSM feature on the branch. "Contact agrees" counts moved rows whose phone, email
or website is on both sides (`agreeing_tags`), before and after the move. "Contact-sharing deletion candidates" are
deletion candidates with a phone, or a website that is more than a home page, that is also on an ATP feature of
the same `brand:wikidata` within 50 m, which matched another OSM feature. These are the cases the issue is about.

| region | ATP rows | matched main → branch | moved | moved: contact agrees, old → new | lost | gained | deletion candidates | contact-sharing deletion candidates |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Switzerland | 151,744 | 77,811 → 77,811 | 7 | 0 → 5 | 0 | 0 | 2,471 → 2,471 | 6 → 5 |
| Japan | 315,122 | 111,273 → 111,272 | 119 | 0 → 111 | 3 | 2 | 7,778 → 7,795 | 93 → 28 |
| Massachusetts | 26,910 | 8,528 → 8,527 | 11 | 0 → 11 | 1 | 0 | 874 → 871 | 11 → 5 |
| Berlin | 13,057 | 7,289 → 7,289 | 8 | 0 → 8 | 0 | 0 | 379 → 377 | 2 → 0 |
| Île-de-France | 69,943 | 28,837 → 28,833 | 46 | 2 → 45 | 5 | 1 | 2,868 → 2,859 | 74 → 55 |
| Brazil Sudeste | 23,928 | 7,692 → 7,692 | 1 | 0 → 1 | 0 | 0 | 1,425 → 1,426 | 1 → 1 |
| Thailand | 33,316 | 9,891 → 9,891 | 0 | | 0 | 0 | 1,586 → 1,586 | 0 → 0 |
| Kenya | 2,602 | 524 → 524 | 0 | | 0 | 0 | 90 → 90 | 0 → 0 |
| UAE | 5,128 | 672 → 672 | 0 | | 0 | 0 | 92 → 92 | 0 → 0 |
| Karnataka | 7,618 | 1,440 → 1,440 | 0 | | 0 | 0 | 261 → 261 | 0 → 0 |
| Tamil Nadu | 7,867 | 906 → 906 | 0 | | 0 | 0 | 109 → 109 | 0 → 0 |

Over all regions, 192 matches moved. 2 of them had a contact in common with their old OSM feature, and 181 have one
with the new one. The contact-sharing deletion candidates went from 187 to 94. Thailand, Kenya, the UAE and India
didn't change at all: their spiders rarely give a branch's own phone, or OSM rarely has one.

The phone check from [CONFLATED_PARQUET.md](../outputs/CONFLATED_PARQUET.md#estimating-the-precision-of-a-rule)
for `brand_wikidata` (rows where both sides have a phone, share where they agree) went up a little everywhere:
Switzerland 81.5% → 81.7% (of 977), Japan 89.2% → 89.4% (12,314), Massachusetts 93.3% → 93.4% (2,618), Berlin
85.3% → 85.4% (1,037), Île-de-France 91.6% → 91.8% (10,927).

### What moved

By hand, almost all moves are what we wanted:

- **Japan:** convenience stores (7-Eleven 31, Lawson 19, FamilyMart 8, Ministop 6) and Doutor (10), where OSM has
  the store twice, often a bare `ローソン` node next to `ローソン笠岡生江浜店` with the phone. The ATP feature now takes
  the one with its phone. Hard Off and Off House (11), two brands of one company that often share a building, now
  each take their own OSM shop.
- **Massachusetts:** Star Market and Walmart Supercenter move from the OSM pharmacy inside the store (same brand,
  a few meters closer) to the store itself, whose phone they share.
- **Île-de-France:** Orpi, Société Générale, Monoprix, Au Vieux Campeur and others, the same pattern as the
  offices. Car dealers move from a bare `Škoda` or `Seat` node to the dealer's own node with its phone, by the
  `contact` rule. Krys Audition moves from the Krys optician next door to its own shop.
- **Switzerland and Berlin:** single cases of the same kind, such as Roche Bobois in Zurich, a Denns BioMarkt in
  Berlin and a Telekom shop at Alexanderplatz.

The few moves where neither side shares a contact (8 in Japan, 2 in Switzerland) are knock-on effects of exclusive
matching: one feature of a spider takes the OSM feature whose phone agrees, and another feature of the spider that
had it falls back to its next candidate. In Japan these are Doutor and FamilyMart branches in central Tokyo, 100–300
m away, whose phones differ before and after. In Switzerland, "Porsche Zentrum Zürich" now takes its own OSM node, and
"Porsche Studio Zürich", 20 m off, no longer shares it.

### Lost and gained

9 matches were lost and 3 gained, all by exclusive matching. In Île-de-France, an "LCL Evry Agence Pro" lost
the OSM bank to "LCL Evry Centre", whose website the OSM node has; an AXA mobile advisor and an Optical Center
"OC Mobile" van lost the agency or shop whose phone belongs to another ATP feature; and two WorldCat libraries
lost an address match 129 m away. All of these are right. In Japan, three Doutor branches lost and two gained an
OSM shop 350–390 m away; with or without the change, these are the far ends of what the 400 m radius allows.

One move in Île-de-France is not better: the Carrefour Montesson store now takes the hypermarket's OSM polygon,
which has its phone, rather than a bare `Carrefour` node 16 m away. Its "Parapharmacie" then moves from the
polygon to that node, 111 m away. Before, the store had the node and the parapharmacie the polygon. Both are half
right; the parapharmacie should be `located_in` the hypermarket.

### Deletion candidates

The deletion candidates barely changed in number (17,933 → 17,937), but some changed places. In Japan, 71 went
away and 88 were added. The ones that went away mostly have a phone (66 of 71), and the ones added mostly have
none (83 of 88): when OSM has a store twice, the copy with the phone now matches, and the copy without one is the
deletion candidate. That is what a mapper should look at, since one of the two is a duplicate.

The contact-sharing deletion candidates that remain are almost all cases where both OSM features carry the ATP
feature's contact. In Île-de-France, 49 of the 55 do, such as "Orpi Philippe Immobilier" and "Orpi Philippe
Gestion" in Longjumeau, the example from the issue: since 2026-11-21, OSM gives both the same phone and website, so
contacts can't tell them apart and distance decides. Of the 13 office deletion candidates of this kind in
Île-de-France, 3 went away; 9 of the other 10 are like Longjumeau. In all, office deletion candidates there went
from 196 to 194.

### Cost

`contacts.agrees` now also runs for `brand_wikidata` candidates, after the cheaper checks. Run times didn't change
measurably (Japan 233 s on main and 219 s on the branch).

## Follow-ups

None. The duplicates in OSM that this brings out are a matter for mappers.
