# Matching by phone number and email address

Investigation note behind the contact evidence in `PoiMatcher`
([`src/matchers/contact/`](../../src/matchers/contact/)). Before this
change, an AllThePlaces (ATP) feature could only match an OpenStreetMap
(OSM) feature with the same `brand:wikidata`. Most OSM shops have no
`brand:wikidata`, and many ATP spiders (schools, post offices, hotels
from tourism boards) have none either. The question was whether phone
numbers and email addresses can fill that gap, and at what precision.

## Short answer

Yes, with a category check. Phones and emails are less noisy than
names, but they identify a contact endpoint, which often serves
several features at one site: a flower shop inside a supermarket lists
the supermarket's number, a Western Union counter the number of the
shop it sits in, a hospital department the hospital's. The
[nondescript sets](2026-10-24-shared-phone-numbers.md) remove
hotlines and central mailboxes (values spread over 1 km or more), but
they keep same-site sharing on purpose. So a shared value counts only
when the two main categories are equal or a known pair, such as
`tourism=hotel` and `amenity=restaurant`. A `brand:wikidata` conflict
still vetoes the match, whatever the phone says.

## How well the signals agree

On Swiss `brand:wikidata` matches where both sides carry the value,
ATP and OSM agree on the phone 84% of the time, on email 91%, and on
the exact name only 70%. Most phone mismatches are branch vs. head
office numbers or stale OSM data, so a phone that differs is no reason
against a match. When phone and `brand:wikidata` both pick a feature,
they pick the same one in 92% of cases (email: 98%). The rest, in a
sample, were the mall or building, another business at the same site,
or a duplicate OSM node where only the duplicate had the phone.

In Switzerland, about a fifth of OSM shops have a phone, whether or not
they have `brand:wikidata`. On the ATP side, 72% of rows with a
descriptive phone have no OSM feature anywhere with that number, so
recall is limited by OSM, not by the matcher.

## The rule

For each OSM candidate within 400 m:

1. Both sides have `brand:wikidata` and share an ID: score 1.0 ×
   distance score, as before.
2. Both sides have `brand:wikidata` and share none: no match.
3. Otherwise, a descriptive phone or email agrees: 0.98 × category
   factor × distance score. The category factor is 1.0 for equal main
   categories, 0.95 for a pair in
   [`category_pairs.tsv`](../../src/matchers/contact/category_pairs.tsv),
   and 0 otherwise. Beyond 150 m, the OSM street and house number must
   also appear in the ATP address.

The main category is the first of `shop`, `amenity`, `tourism`,
`leisure`, `healthcare`, `craft` and `office` that a feature has.
Phone and fax keys are compared together, since ATP lists many
numbers as a phone on one feature and as a fax on another.

ATP features of the shop, restaurant, lodging, school and fuel kinds
get a matcher if they have `brand:wikidata` or a descriptive phone or
email. Before, only shops with `brand:wikidata` did. The same change
adds cafes, fast food, bars, schools, colleges, doctors, clinics, car
rental, cinemas, money transfer, vets, fitness and sports centres,
museums and `healthcare=*` to the match masks.

## Category pairs

`scripts/infer_contact_category_pairs.py` reads a run without the
category check and counts the pairs of main categories among contact
matches. Names are the independent check: among contact matches with
equal categories, ATP and OSM share a name or brand word 83% of the
time. A pair is kept if its own rate is at least half of that, with at
least 20 name-agreeing matches. `florist` and `supermarket` share many
numbers but almost never a name, so that pair is dropped.

The script kept 61 pairs from the planet. A hand check of samples
showed that 18 of them still mostly join two features at one site, so
they were removed: clinic or doctors and hospital, hospital and its
departments, money transfer and supermarkets or pharmacies (Western
Union counters), post office and supermarkets or copy shops, library
and museum, childcare or kindergarten and school (Zürich names each
after-school care after its school), clothes and department store,
cafe and fast food (McCafé), and fuel and convenience. The file lists
them as comments, with the reason. To update the list, rerun the
script on a fresh planet run and review the changes by hand.

## Planet results

ATP run `2026-10-10-13-32-41` against the planet of the same week,
2,097,628 ATP rows that the matcher looked at before this change.

| Variant | Matched ATP rows |
|---|---:|
| `brand:wikidata`, shops only (before) | 825,901 |
| `brand:wikidata`, wider categories | 1,233,452 |
| `brand:wikidata` or phone and email, wider categories (this change) | PLANET_PR |

No match from before was lost. To measure precision without hand
labels, a test run hid the brand tags on both sides and checked
whether contact evidence alone finds the OSM feature that
`brand:wikidata` found. Of about 183,000 such rows, it found the same
feature in 96.4% of cases with no category check, 97.6% with equal
categories only, and 97.1% with equal categories or a reviewed pair.

The wider categories add 407,551 matches, mostly fast food (183k),
fuel stations (125k), cafes, restaurants and hotels. Their
`brand:wikidata` matches lie close: for charging stations, the median
distance is 8 m.

Hand checks of new contact matches found about 95.6% right with the
inferred pairs, and about 97% after the review. The weakest group
before the review was ATP rows without `brand:wikidata` that matched
through a category pair: 60% right within 150 m and 33% beyond. After
the review, 37 of 40 sampled rows in that group were right.

The largest gains are public and semi-public data: UK schools
(`gov_dfe_gias_gb`), USPS, NHS England, Poste Italiane, Slovenská
pošta, Chunghwa Post, Italian libraries (`iccu_it`), WorldCat and
`discover_swiss`. Among brand spiders, car dealers lead, since OSM
often maps a dealer by its own name without brand tags. Phone does
almost all the work; email alone adds little.

## Phone parsing

ATP writes phone numbers in international format, and so does most of
OSM. The matcher reads `+…` values with the same fast parser as the
nondescript sets, and only the rest with the
[`phonenumber`](https://crates.io/crates/phonenumber) crate, in the
ATP feature's `addr:country`. On the planet, that made 246,447 crate
calls at about 63 µs each, 15 CPU-seconds in all; the match step went
from 34 s to 35 s. Calling the crate for every value instead would make
13.3 million calls and slow the match step from 34 s to 190 s.

National-format numbers are common in some countries. Among OSM phone
values near ATP features, they make up 38% in India, 25% in Thailand,
20% in Romania, 19.5% in Japan, 15.4% in South Korea and 8.9% in
Mexico, but under 2% in most of Europe and North America. In France
(1.3%), the crate can parse only 22% of them.

## Caveats and follow-ups

- Same-site errors remain where the categories are equal or paired:
  a Shell station shop matched to the IGA supermarket next door, a
  hotel matched to the hotel building's other hotel. One-to-one
  resolution, where each OSM feature keeps only its best ATP row,
  would catch some of these.
- A shared phone with disagreeing `brand:wikidata` is vetoed. Often
  this is a rebrand where the number stayed (Audika took over KIND in
  2026; McDonald Nissan became Larry H. Miller Nissan). These would
  make good candidates for human-reviewed brand updates. Regional
  names are not rebrands, though: Fressnapf is Maxi Zoo in
  French-speaking Switzerland.
- The precision figures come from small hand-labelled samples (about
  400 rows in all) and from the hide-the-wikidata test, which only
  covers features that brand spiders know.
