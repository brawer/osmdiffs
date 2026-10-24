//! Finds phone and fax numbers that AllThePlaces lists for places far
//! apart from each other -- hotlines, call centers, central
//! switchboards -- and writes them to a [U64Set] on disk, so that
//! matching can ignore them as a signal.
//!
//! Many chains give every branch the same number (all 25,933 HDFC Bank
//! branches in India share one; FedEx uses one for 14,611 locations).
//! Such a number says nothing about which place a feature is. A number
//! shared by features at the *same* site is different: a car dealer
//! listed once per brand, or a Western Union desk inside a pharmacy, is
//! still a correct local identifier, so those numbers stay usable. In
//! the 2026-10-10 worldwide ATP run, about 23% of features with a number
//! shared it with another feature of the same spider, but three
//! quarters of those were co-located; the far-apart ones affected about
//! 11% of features with a number.
//!
//! A number is denied if the bounding box around every place carrying
//! it, across all spiders, has a diagonal of at least
//! [MIN_SWITCHBOARD_SPREAD_METERS]. The bounding box doesn't depend on
//! the order in which places arrive, and its diagonal is never shorter
//! than the largest distance between two of the places (and at most
//! about 1.4 times it), which is precise enough for this threshold.
//!
//! Computed on the side while `import_atp` writes `alltheplaces.parquet`:
//! one [Sighting] per (place, number) goes into an external sort by
//! number, and a single streaming pass over the sorted sightings finds
//! the switchboards. Not consumed by anything yet; phone numbers are a
//! planned matching signal.

use anyhow::Result;
use ext_sort::{ExternalSorter, ExternalSorterBuilder, buffer::LimitedBufferBuilder};
use geo::{Distance, Haversine, Point};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::places::{Place, PlaceReader};
use crate::tables::U64Set;

/// Filename, within `workdir`, of the denylist written by
/// [PhoneDenylistBuilder::finish].
pub const PHONE_DENYLIST_FILENAME: &str = "alltheplaces.phone-denylist";

/// Places carrying the same number whose bounding box diagonal is at
/// least this long make that number a switchboard.
pub const MIN_SWITCHBOARD_SPREAD_METERS: f64 = 1000.0;

/// Tag keys whose values may hold phone or fax numbers. Phone and fax
/// share one key space: in ATP, thousands of numbers show up as a phone
/// on one place and as a fax on another.
const PHONE_KEYS: &[&str] = &[
    "phone",
    "fax",
    "mobile",
    "contact:phone",
    "contact:fax",
    "contact:mobile",
];

/// Shortest and longest number of digits we accept, including the
/// country code. E.164 numbers have at most 15 digits, so every
/// accepted number fits into a `u64`.
const MIN_DIGITS: usize = 7;
const MAX_DIGITS: usize = 15;

/// Parses a phone tag value into numbers, one per `;`-separated part.
///
/// AllThePlaces normalizes phone numbers to international format with
/// libphonenumber, as in `+41 44 417 77 77` or `+1 800-463-3339`. We
/// encode each such number as the `u64` of its digits, country code
/// first; country codes never start with 0, so the encoding is unique.
/// Parts that aren't in international format -- a few spiders emit
/// local numbers, or junk like `9350-0` or `1800 PARKING` -- are
/// skipped, since we can't tell which country they belong to.
pub fn parse_phone_numbers(value: &str) -> impl Iterator<Item = u64> + '_ {
    value.split(';').filter_map(parse_phone_number)
}

fn parse_phone_number(part: &str) -> Option<u64> {
    let rest = part.trim().strip_prefix('+')?;
    let mut number: u64 = 0;
    let mut digits = 0;
    for c in rest.chars() {
        match c {
            '0'..='9' => {
                if digits == 0 && c == '0' {
                    return None;
                }
                digits += 1;
                if digits > MAX_DIGITS {
                    return None;
                }
                number = number * 10 + u64::from(c as u8 - b'0');
            }
            ' ' | '-' | '.' | '(' | ')' | '\u{a0}' => {}
            _ => return None,
        }
    }
    (digits >= MIN_DIGITS).then_some(number)
}

/// Whether `key` is a tag key that may hold phone or fax numbers.
pub fn is_phone_key(key: &str) -> bool {
    PHONE_KEYS.contains(&key)
}

/// One place carrying one number, as sorted by [PhoneDenylistBuilder].
/// Fixed-size, so the sort buffer can budget by item count.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Sighting {
    phone: u64,
    lat_e7: i32,
    lng_e7: i32,
}

/// Returns one [Sighting] for each number in `place`'s phone tags, at
/// the place's representative point (the one its `s2_cell_id` encodes).
/// A number listed twice on the same place yields two identical
/// sightings, which doesn't change any bounding box.
pub fn sightings(place: &Place) -> Vec<Sighting> {
    let mut result = Vec::new();
    let mut point: Option<(i32, i32)> = None;
    for (key, value) in &place.tags {
        if !is_phone_key(key) {
            continue;
        }
        for phone in parse_phone_numbers(value) {
            let (lat_e7, lng_e7) = *point.get_or_insert_with(|| {
                let ll = s2::latlng::LatLng::from(s2::cellid::CellID(place.s2_cell_id));
                (to_e7(ll.lat.deg()), to_e7(ll.lng.deg()))
            });
            result.push(Sighting {
                phone,
                lat_e7,
                lng_e7,
            });
        }
    }
    result
}

fn to_e7(deg: f64) -> i32 {
    (deg * 1e7).round() as i32
}

fn from_e7(e7: i32) -> f64 {
    f64::from(e7) / 1e7
}

/// External sorter for [Sighting]s. Feed it with [Self::sort], then
/// stream the result through [find_switchboards].
pub fn sorter(
    workdir: &Path,
    chunk_bytes: usize,
) -> Result<ExternalSorter<Sighting, std::io::Error, LimitedBufferBuilder>> {
    Ok(ExternalSorterBuilder::new()
        .with_tmp_dir(workdir)
        .with_buffer(LimitedBufferBuilder::new(
            chunk_bytes / size_of::<Sighting>(),
            /* preallocate */ false,
        ))
        .build()?)
}

/// Streams over sightings sorted by number and returns, in ascending
/// order, every number whose places are spread out over at least
/// [MIN_SWITCHBOARD_SPREAD_METERS].
pub fn find_switchboards<E>(
    sorted: impl Iterator<Item = std::result::Result<Sighting, E>>,
) -> std::result::Result<Vec<u64>, E> {
    struct Group {
        phone: u64,
        min_lat: i32,
        max_lat: i32,
        min_lng: i32,
        max_lng: i32,
    }

    impl Group {
        fn new(s: &Sighting) -> Group {
            Group {
                phone: s.phone,
                min_lat: s.lat_e7,
                max_lat: s.lat_e7,
                min_lng: s.lng_e7,
                max_lng: s.lng_e7,
            }
        }

        fn add(&mut self, s: &Sighting) {
            self.min_lat = self.min_lat.min(s.lat_e7);
            self.max_lat = self.max_lat.max(s.lat_e7);
            self.min_lng = self.min_lng.min(s.lng_e7);
            self.max_lng = self.max_lng.max(s.lng_e7);
        }

        fn is_switchboard(&self) -> bool {
            if self.min_lat == self.max_lat && self.min_lng == self.max_lng {
                return false;
            }
            let a = Point::new(from_e7(self.min_lng), from_e7(self.min_lat));
            let b = Point::new(from_e7(self.max_lng), from_e7(self.max_lat));
            Haversine.distance(a, b) >= MIN_SWITCHBOARD_SPREAD_METERS
        }
    }

    let mut result = Vec::new();
    let mut group: Option<Group> = None;
    for s in sorted {
        let s = s?;
        match group.as_mut() {
            Some(g) if g.phone == s.phone => g.add(&s),
            _ => {
                if let Some(g) = group.replace(Group::new(&s))
                    && g.is_switchboard()
                {
                    result.push(g.phone);
                }
            }
        }
    }
    if let Some(g) = group
        && g.is_switchboard()
    {
        result.push(g.phone);
    }
    Ok(result)
}

/// Builds the denylist from an existing `alltheplaces.parquet`, for a
/// workdir where that file is reused but the denylist is missing or
/// stale -- e.g. one from before this denylist existed, whose zip may
/// already have been deleted. `import_atp` otherwise builds the
/// denylist while writing the parquet file, without this extra read.
pub fn build_from_parquet(parquet: &Path, workdir: &Path, out: &Path) -> Result<()> {
    let reader = PlaceReader::open(parquet)?;
    // Sole external sort in this step, so it gets the full budget.
    let sorter = sorter(workdir, crate::pipeline::EXTERNAL_SORT_CHUNK_BYTES)?;
    let mut read_result: Result<()> = Ok(());
    let input = reader
        .read_all()?
        .map_while(|batch| match batch {
            std::result::Result::Ok(batch) => Some(batch),
            Err(err) => {
                read_result = Err(err);
                None
            }
        })
        .flatten()
        .flat_map(|place| sightings(&place))
        .map(std::io::Result::Ok);
    let sorted = sorter.sort_by(input, Sighting::cmp)?;
    read_result?;
    write_denylist(find_switchboards(sorted)?, workdir, out)
}

/// Writes `switchboards` to `out` as a [U64Set].
pub fn write_denylist(switchboards: Vec<u64>, workdir: &Path, out: &Path) -> Result<()> {
    let count = switchboards.len();
    // The list is small (tens of thousands of numbers worldwide) and
    // already sorted, so one exactly sized chunk is enough; there's no
    // need to claim the pipeline's whole external-sort budget for it.
    let chunk_bytes = count.max(1) * size_of::<u64>();
    U64Set::create(switchboards.into_iter(), workdir, out, chunk_bytes)?;
    log::info!(
        count = count,
        path = out.display().to_string();
        "import_atp: wrote phone denylist"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matchers::MatchMask;
    use crate::utils::UtcTimestamp;
    use geo::Coord;

    #[test]
    fn test_parse_phone_numbers() {
        let parse = |s| parse_phone_numbers(s).collect::<Vec<u64>>();
        let none: [u64; 0] = [];
        assert_eq!(parse("+41 44 417 77 77"), [41444177777]);
        assert_eq!(parse("+1 800-463-3339"), [18004633339]);
        assert_eq!(parse("+81 3-3352-1111"), [81333521111]);
        assert_eq!(parse("+1 (212) 555.0100"), [12125550100]);
        assert_eq!(
            parse("+41 44 417 77 77; +41 44 417 77 78"),
            [41444177777, 41444177778]
        );
        assert_eq!(parse(" +41 44 417 77 77 ;;"), [41444177777]);
        assert_eq!(parse("+41\u{a0}44\u{a0}417\u{a0}77\u{a0}77"), [41444177777]);

        // Not in international format, or junk.
        assert_eq!(parse(""), none);
        assert_eq!(parse("044 417 77 77"), none);
        assert_eq!(parse("9350-0"), none);
        assert_eq!(parse("1800 PARKING"), none);
        assert_eq!(parse("+81 None"), none);
        assert_eq!(parse("+421"), none);
        assert_eq!(parse("+0041 44 417 77 77"), none);
        assert_eq!(parse("+212 5 3771 6200/60"), none);
        assert_eq!(parse("+41 44 413 26 60 KIGA"), none);
        assert_eq!(parse("+1234567890123456"), none);

        // The longest possible E.164 number still fits.
        assert_eq!(parse("+999999999999999"), [999_999_999_999_999]);
    }

    #[test]
    fn test_is_phone_key() {
        assert!(is_phone_key("phone"));
        assert!(is_phone_key("fax"));
        assert!(is_phone_key("contact:phone"));
        assert!(is_phone_key("contact:fax"));
        assert!(!is_phone_key("phone:note"));
        assert!(!is_phone_key("name"));
    }

    fn place(lng: f64, lat: f64, tags: &[(&str, &str)]) -> Place {
        let coord = Coord { x: lng, y: lat };
        Place::new(
            &coord,
            "test_spider".to_string(),
            MatchMask::SHOP,
            tags.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            &geo::Geometry::from(Point::from(coord)),
            UtcTimestamp(time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn test_sightings() {
        let p = place(
            8.5417,
            47.3769,
            &[
                ("name", "Test"),
                ("phone", "+41 44 417 77 77;+41 44 417 77 78"),
                ("fax", "+41 44 417 77 79"),
                ("website", "+41 44 417 77 70"),
            ],
        );
        let s = sightings(&p);
        let phones: Vec<u64> = s.iter().map(|s| s.phone).collect();
        assert_eq!(phones, [41444177777, 41444177778, 41444177779]);
        assert!((from_e7(s[0].lat_e7) - 47.3769).abs() < 1e-6);
        assert!((from_e7(s[0].lng_e7) - 8.5417).abs() < 1e-6);

        assert!(sightings(&place(8.5, 47.3, &[("name", "No phone")])).is_empty());
    }

    fn sighting(phone: u64, lng: f64, lat: f64) -> Sighting {
        Sighting {
            phone,
            lat_e7: to_e7(lat),
            lng_e7: to_e7(lng),
        }
    }

    fn switchboards(mut sightings: Vec<Sighting>) -> Vec<u64> {
        sightings.sort();
        find_switchboards(sightings.into_iter().map(std::result::Result::<_, ()>::Ok)).unwrap()
    }

    #[test]
    fn test_find_switchboards() {
        assert_eq!(switchboards(vec![]), Vec::<u64>::new());

        // A single place is never a switchboard.
        assert_eq!(
            switchboards(vec![sighting(1, 8.5, 47.3)]),
            Vec::<u64>::new()
        );

        let zurich = (8.5417, 47.3769);
        let bern = (7.4474, 46.9480);
        // About 40 m east of `zurich`.
        let next_door = (8.5422, 47.3769);
        // About 1.5 km north of `zurich`.
        let oerlikon = (8.5417, 47.3904);

        assert_eq!(
            switchboards(vec![
                // Same site, listed three times: not a switchboard.
                sighting(10, zurich.0, zurich.1),
                sighting(10, zurich.0, zurich.1),
                sighting(10, next_door.0, next_door.1),
                // Zürich and Bern: a switchboard.
                sighting(20, zurich.0, zurich.1),
                sighting(20, bern.0, bern.1),
                // 1.5 km apart: a switchboard.
                sighting(30, zurich.0, zurich.1),
                sighting(30, oerlikon.0, oerlikon.1),
                // Just once.
                sighting(40, bern.0, bern.1),
                // Last group in the stream is a switchboard too.
                sighting(50, next_door.0, next_door.1),
                sighting(50, bern.0, bern.1),
                sighting(50, zurich.0, zurich.1),
            ]),
            [20, 30, 50]
        );
    }

    #[test]
    fn test_build_from_parquet() -> Result<()> {
        let workdir = tempfile::tempdir()?;
        let parquet = workdir.path().join("alltheplaces.parquet");
        let mut writer = crate::places::ParquetWriter::try_new(16, &parquet)?;
        writer.write(place(8.5417, 47.3769, &[("phone", "+41 44 417 77 77")]))?;
        writer.write(place(7.4474, 46.9480, &[("phone", "+41 44 417 77 77")]))?;
        writer.write(place(7.4474, 46.9480, &[("phone", "+41 31 321 21 11")]))?;
        writer.close()?;

        let out = workdir.path().join(PHONE_DENYLIST_FILENAME);
        build_from_parquet(&parquet, workdir.path(), &out)?;
        let set = U64Set::open(&out)?;
        assert_eq!(set.iter().collect::<Vec<u64>>(), [41444177777]);
        Ok(())
    }

    #[test]
    fn test_find_switchboards_propagates_errors() {
        let input = vec![Ok(sighting(1, 8.5, 47.3)), Err("broken")];
        assert_eq!(find_switchboards(input.into_iter()), Err("broken"));
    }

    #[test]
    fn test_sort_and_write_denylist() -> Result<()> {
        let workdir = tempfile::tempdir()?;
        let sorter = sorter(workdir.path(), 1024)?;
        let input = vec![
            sighting(41444177777, 8.5417, 47.3769),
            sighting(18004633339, -87.6, 41.8),
            sighting(41444177777, 7.4474, 46.9480),
            sighting(18004633339, -87.6, 41.8),
        ];
        let sorted = sorter.sort(input.into_iter().map(std::io::Result::Ok))?;
        let found = find_switchboards(sorted)?;
        assert_eq!(found, [41444177777]);

        let out = workdir.path().join(PHONE_DENYLIST_FILENAME);
        write_denylist(found, workdir.path(), &out)?;
        let set = U64Set::open(&out)?;
        assert!(set.contains(41444177777));
        assert!(!set.contains(18004633339));
        assert_eq!(set.len(), 1);
        Ok(())
    }
}
