# First `edits.parquet` on the eval regions

Milestone 3 of the [edits.parquet plan](2026-11-23-edits-parquet.md):
the new `make_edits` step with contact tags, Wikidata IDs and postal
partners, measured on the ten [eval regions](2026-11-06-eval-regions.md)
with the AllThePlaces run of 2026-11-14, split by country.

## Counts

Counts are of changes (tags), except "edits", which counts rows (OSM
elements). "Replacements" are changes with reason `differs`,
`generic_value` or `added_value`; the rest add a missing tag.

| region | `same` rows | edits | phone | fax | email | website | wikidata | post_partner | replacements | with warnings |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Kenya | 531 | 405 | 299 | 1 | 13 | 112 | 32 | 0 | 5 | 2 |
| UAE | 682 | 464 | 331 | 32 | 118 | 279 | 20 | 0 | 34 | 12 |
| Thailand | 9,892 | 5,911 | 3,990 | 27 | 389 | 2,794 | 377 | 27 | 503 | 21 |
| Berlin | 7,282 | 2,579 | 1,260 | 22 | 465 | 1,599 | 50 | 997 | 565 | 14 |
| Switzerland | 78,033 | 15,594 | 4,386 | 151 | 3,501 | 11,030 | 993 | 2,081 | 1,603 | 222 |
| Karnataka | 1,476 | 1,237 | 745 | 11 | 202 | 894 | 73 | 0 | 156 | 6 |
| Tamil Nadu | 934 | 811 | 515 | 5 | 87 | 680 | 33 | 0 | 57 | 3 |
| Brazil Sudeste | 7,848 | 3,393 | 1,779 | 17 | 310 | 1,484 | 335 | 149 | 169 | 6 |
| Massachusetts | 9,075 | 4,597 | 3,682 | 183 | 500 | 2,572 | 31 | 336 | 426 | 55 |
| Japan | 143,590 | 131,117 | 66,830 | 3,028 | 268 | 125,762 | 3,776 | 80 | 25,784 | 331 |

The step itself takes 1 to 5 seconds per region and 25 seconds for
Japan. Japan is large because OSM there has few contact tags: its
143,590 `same` rows give 131,117 edits, mostly a phone number and a
store page. Of its replacements, 13,659 are HELLO CYCLING bike stations whose
OSM website is the network's home page.

The eval runs have no spider statistics of a previous week, so the
gate against broken spider runs did nothing here.

## Hand check

I read a sample stratified by region: 3 changes per region and kind,
166 additions and 96 replacements in all. For each, I compared the
change with the OSM element's tags, the matched spider, the rule and
the distance. I did not open the store pages, so "plausible" means
that nothing in the data speaks against it, not that I checked it
against the live page.

| kind | sample | plausible | doubtful |
|---|---:|---:|---|
| add phone | 30 | 30 | |
| add fax | 28 | 27 | a mobile number as fax (Lacoste, Brazil) |
| add email | 30 | 28 | two staff members' personal addresses (AMAG, Marriott) |
| add website | 30 | 30 | |
| add Wikidata ID | 30 | 29 | a Staybridge Suites tagged `brand=Holiday Inn` that matched a Holiday Inn 95 m away; the ID fits OSM's brand, but the match is likely wrong |
| add postal-partner tags | 18 | 18 | |
| replace phone | 28 | 24 | landline to mobile (Arezzo, Hering, Brazil), a match 260 m away (7-Eleven, Japan), a Bangalore number for a Chennai KFC |
| replace fax | 11 | 11 | |
| replace email | 23 | 18 | two personal addresses, a bank address of another branch, sales to service at Tesla, an EDEKA code that may be a central one |
| replace website | 30 | 29 | a Quaker meeting house's own page replaced by its National Register entry |
| replace postal-partner tags | 4 | 4 | |

Additions look good: 162 of 166. Replacements are weaker, 86 of 96,
and email replacements weakest. Since every change carries its reason,
an import can start with the additions and the `generic_value`
replacements, and leave `differs` for later or for human review.

## Fixed along the way

The first three regions showed edits that only reformatted a value or
churned a good one. These rules now prevent them, each with a unit
test:

- OSM phone numbers with a trunk prefix after the country code, such
  as `+971 02 6120860` or `+971 (0) 4 249 7800`, agree with ATP's
  `+971 2 612 0860`.
- ATP phone and fax values that aren't valid numbers, such as
  `+97197126543333`, or look like placeholders, such as
  `+254 708 000000`, are skipped.
- A website on the same registrable domain as ATP's, or an email
  address at the same domain, is kept: Accor's English page against
  its Arabic one, `locations.dunkindonuts.com` against
  `www.dunkindonuts.com`, Tesla's sales address against its service
  one. Before this rule, Massachusetts had 556 Dunkin' website
  replacements of this kind; now it has 1. One side effect: a Hilton Garden Inn in
  Phuket no longer gets the page of another Hilton hotel, which conflate
  had matched it to.
- A home page counts as generic only on the same site as ATP's page
  (`rossmann.de`), or when more than two OSM features carry it
  (`sej.co.jp` on 3,404 7-Elevens in Japan, `europcar.com/en-db`). The home page
  of another site is most likely the place's own and is kept: before,
  a Swiss restaurant's own site was to be replaced by its page on an
  accessibility guide.
- The old `url` key only counts for agreement and is never edited.

## After review

Sascha decided on the open questions below on 2026-11-23:

- Non-generic email replacements stay, with the warning
  `replaces_email`. It fires 187 times in the ten regions, 142 of them
  in Switzerland.
- Addresses that look like a person's get the warning
  `personal_email`. A local part of two words is flagged, unless one
  of the words is a role (`info`, `reservations`, `filiale` and so on),
  part of the domain or the store's name, or recurs in more than two of
  the chain's addresses. It fires 197 times in the ten regions. Of 40
  sampled, 22 were people's names. The rest were store addresses such
  as `basel.plaza@losteria.ch`, so it stays a warning.
- Registry spiders write their page to `heritage:website`, never to
  `website`: today only `national_register_of_historic_places`, whose
  NPGallery pages are the register's record of the property, with the
  nomination form. In Massachusetts this gives 131 additions and 20
  replacements of a generic `heritage:website`, and no more `website`
  changes. Filed upstream as
  [alltheplaces#20746](https://github.com/alltheplaces/alltheplaces/issues/20746).

## Open questions (as first written)

- **Email replacements.** Should the step replace an OSM email at all
  when it isn't generic? Addresses at another domain are often both
  right, and 5 of 23 in the sample were doubtful.
- **Personal addresses.** Some spiders give the address of a staff
  member, such as `nicholas.destefano@marriott.com`. These could be
  filtered by a `first.last@` pattern, or per spider.
- **Registries as websites.** `national_register_of_historic_places`
  and similar spiders give a registry page, not the place's own. Such
  spiders could be barred from replacing a website.
- **Tenants matched as `same`.** Seven restaurants and bars of the
  Southern Sun Abu Dhabi matched the hotel itself. Their agreeing phone
  number made a correct edit here, but this is a conflate issue.
