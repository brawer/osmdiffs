# Regions for evaluating matching changes

Most matching changes so far were measured on Switzerland alone. That's
quick, and we know the data well, but it risks rules that only work
there: Swiss phone formats, Latin-script brand names, a dense and
well-tagged OSM, and AllThePlaces (ATP) spiders that mostly cover
European chains. From now on, a matching change should be measured on
this fixed set of regions, each in a separate run with the OSM extract
of the region and ATP filtered to its country.

| region | OSM extract | why |
|---|---|---|
| Switzerland | whole country | our reference, comparable with earlier notes |
| Japan | whole country, or Kantō | CJK names, national-format phones, Japanese and Punycode domains, OSM with few brand tags |
| United States | one mid-size state, such as Massachusetts | most ATP spiders are American; US addresses and phone numbers; dense chains |
| Germany | one state, such as Berlin or Baden-Württemberg | dense OSM mapping with many `brand:wikidata` tags, a good test for false positives |
| Brazil | Sudeste (São Paulo, Rio de Janeiro, Minas Gerais) | Portuguese, Brazilian addresses and phones |
| Thailand | whole country | Thai script, phone numbers in national format |
| Kenya | whole country | an African country with some ATP coverage |
| United Arab Emirates | whole country | Arabic script, right-to-left names |
| India | southern zone (Bengaluru, Chennai) | Kannada and Tamil script, many national-format phones |

ATP coverage differs a lot between countries. In the 2026-10-10 run,
these are the ATP features with a website, per country, as a rough
guide to how much a region can tell us:

| country | features with a website |
|---|---:|
| United States | 1,179,628 |
| Japan | 616,387 |
| Germany | 126,125 |
| India | 97,215 |
| Switzerland | 37,475 |
| South Africa | 32,493 |
| Brazil | 14,310 |
| Thailand | 10,730 |
| United Arab Emirates | 2,617 |
| Saudi Arabia | 1,901 |
| Morocco | 1,458 |
| Kenya | 529 |
| Egypt | 458 |
| Tunisia | 371 |
| Ghana | 354 |
| Nigeria | 199 |
| Rwanda | 21 |

So Rwanda or Tunisia would say little, while Kenya and the UAE are about
the smallest that still give a few hundred matches to look at.

## How to run

- Regional extracts come from Geofabrik or openstreetmap.fr (some cloud
  sandboxes can't reach Geofabrik). Each run takes a few minutes, except
  the whole of Japan or the United States.
- Filter ATP to the country by `addr:country`, as for the Swiss
  baseline, and put the zip and the pipeline's other inputs in a
  workdir. Then run the pipeline once with `main` and once with the
  change, and compare `conflated.parquet`: new, changed and lost
  matches, OSM-only rows (deletion candidates), and a random sample of
  new matches checked by hand.
- Tables that the pipeline builds from ATP as a whole, such as the
  nondescript phones and emails or the brand domains, come out
  differently from a filtered ATP. Where that matters, also try the
  table built from the whole ATP run, as production does. For brand
  domains in Switzerland, the difference was 375 against 377 new
  matches (see `2026-11-06-brand-domains.md`).
- Report results per region. A rule that helps one region and hurts
  another needs a second look before it is merged.
