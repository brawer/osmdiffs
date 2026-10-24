# Shared email addresses in AllThePlaces, and nondescript emails

Investigation note behind `alltheplaces.nondescript-emails`
([`src/pipeline/atp/nondescript/`](../../src/pipeline/atp/nondescript/)),
the email counterpart of
[`2026-10-24-shared-phone-numbers.md`](2026-10-24-shared-phone-numbers.md).
Same question, same method: how often do several ATP features share one
address, and which kind of sharing makes it *nondescript*, so that a
match on it should add little or no confidence. As with phones, this is
no reason against a match.

## Method

Data: the worldwide ATP run `2026-10-10-13-32-41` (5,640 spiders, 41.0M
features). Tags: `email` (558,421 features), `contact:email` (9,480),
department-specific ones like `repair:email`, `sales:email` and
`approvedPreOwned:email`, and `contact:pec` (Italian certified email,
83). Addresses sometimes appear in free-text tags too, but rarely, so
those were skipped.

Values were split at `;` (as in OSM) and `,` (as in some spiders),
trimmed, stripped of `<…>`, `mailto:` and `?…`, lowercased, and checked
against a plain address pattern. Only 0.13% of values fail (`N@D`,
`info@jlrspanos.g`, two addresses joined by `/`), so email is much
cleaner in ATP than phone numbers (1.7% invalid).

## How common is sharing?

559k features (1.4%) have at least one valid address.

| | Features | Share of features with an email |
|---|---:|---:|
| Share an address with another feature of the **same spider** | 182,116 | 32.6% |
| … where the address also appears ≥ 1 km away | 86,966 | 15.6% |
| Share an address with a feature of **another spider** | 22,889 | 4.1% |
| … where the address also appears ≥ 1 km away | 5,146 | 0.9% |
| Address appears ≥ 1 km apart, any spider | **88,198** | **15.8%** |

Sharing is more common than for phones (23.5% within a spider, 11.2%
spread ≥ 1 km): features carry fewer addresses than numbers, and the
ones they carry are more often a central mailbox.

## Three kinds of sharing

Of the addresses shared within a spider, 72% have every feature within
100 m.

**Same site (< 100 m).** As with phones: car dealers with one feature
per brand or service, a bank branch plus its ATM (Punjab National Bank:
16,992 features share an address, only 2 of them spread over 1 km).
The address still points to the right place, so it stays usable.

**Central mailboxes (≥ 100 km).** `care@kindercare.com` (1,449
centers), `directoryupdate-hi@kp.org` (Kaiser Permanente, 1,354),
`csc-leader@kfc1150.com` (KFC Thailand, 1,146), `support@casper.com`
(995). Nondescript.

**Regional mailboxes (10–100 km).** More common than for phones. Every
German Volksbank and Sparkasse publishes one address per bank, not per
branch (`info@volksbanking.de` × 276 within 69 km,
`info@sparkasse-leipzig.de` × 156 within 53 km); likewise dealer groups,
city services (`vantaa-info@vantaa.fi` × 352 within 14 km) and library
networks. Such an address still narrows a match down to a few dozen
places, but with a yes/no flag at 1 km it counts as nondescript.

Also spread out, and nondescript for the same reason:

- **People rather than places**: a franchisee or adviser covering
  several branches (one address at 58 Tropical Smoothie Cafés; Merrill
  Lynch, CIBC and Coldwell Banker advisers). Most spread free-mail
  addresses (`gmail.com`, `mail.ru`, `yandex.ru`) are of this kind.
- **Placeholders**: `etunimi.sukunimi@vantaa.fi` (Finnish for
  "firstname.lastname", 152 features).

The local part doesn't tell the kinds apart: `info@` is the most common
local part among both spread and same-site addresses. The spread does.

Swiss examples: `info@activfitness.ch` × 106 (299 km), Stadt Zürich
`gsz-pga-sekretariat@zuerich.ch` × 96 (8 km), `info@schweizmobil.ch` × 18.

## Across spiders

4,932 addresses appear in two or more spiders. For 96% of them, some
features from different spiders are within 1 km (89% within 100 m):
multi-brand dealers (`audi`, `volkswagen`, `skoda`, `seat`, …) and
overlapping spiders (`nandos_za` repeated in 8 `nandos_*` spiders).
Only 184 have no pair within 1 km, nearly all dealer groups, and grouping
by address alone catches them with the same rule.

## The rule, and what it does in the pipeline

The phone rule carries over unchanged: an address is nondescript when
the bounding rectangle of all places carrying it, across all spiders,
has a diagonal of at least 1 km. `import_atp` computes phones and
emails in the same external sort and streaming pass.

Addresses don't fit into a `u64` the way phone numbers do, so the
pipeline stores the first 8 bytes of the SHA-256 of the normalized
address. With about 430k distinct addresses, the chance of any
collision is about 1 in 200 million, and a collision would only make
one more address nondescript. Whoever reads the file must normalize OSM
values the same way (`normalize_email` and `email_hash`).

On the same ATP run, the pipeline marks 7,995 addresses as nondescript.
That affects 51,975 of the 377,615 imported features with an email
(13.8%). The analysis above counted 10,827 addresses (15.7%) in spiders
usable for OSM; the pipeline imports fewer features (377,615 rather
than 512,075 with an email), most likely because of its further
filtering, so fewer addresses reach the threshold. The top affected
spiders are `volksbank_raiffeisenbank_de` (7,908 features), `bmw_group`,
`sparkasse_de`, `volkswagen` and `mercedes_benz_group`. Adding emails
did not measurably change the run time of `import_atp`.

## Caveats

- Regional mailboxes count as nondescript, though they carry some
  signal. If matching ever needs it, the rectangle's diagonal could be
  stored instead of a yes/no flag, so confidence can scale with spread.
- A placeholder or personal address used at a single site is not caught.
- Addresses in spiders the pipeline drops for licensing reasons don't
  contribute.
