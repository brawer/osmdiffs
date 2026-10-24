//! Finds *nondescript* phone numbers and email addresses: values that
//! AllThePlaces lists for places far apart from each other, such as
//! hotlines, call centers, central switchboards or a chain's customer
//! service mailbox. Writes them to one [U64Set] per [Signal] on disk, so
//! that matching can give them less weight. A nondescript value is no
//! reason against a match; it just doesn't make one more likely.
//!
//! Many chains give every branch the same number or address (all 25,933
//! HDFC Bank branches in India share one number; 1,449 KinderCare
//! centers share `care@kindercare.com`). Such a value says nothing about
//! which place a feature is. A value shared by features at the *same*
//! site is different: a car dealer listed once per brand, or a Western
//! Union desk inside a pharmacy, is still a correct local identifier,
//! so those values stay usable. In the 2026-10-10 worldwide ATP run, the
//! far-apart ones affected about 10% of imported features with a phone
//! number and about 16% of those with an email address; see
//! `docs/notes/2026-10-24-shared-phone-numbers.md` and
//! `docs/notes/2026-10-24-shared-email-addresses.md`.
//!
//! A value is nondescript if the S2 latitude/longitude rectangle around
//! every place carrying it, across all spiders, has a diagonal of at
//! least [MIN_NONDESCRIPT_SPREAD_METERS]. The diagonal is never shorter
//! than the largest distance between two of the places (and at most
//! about 1.4 times it), which is precise enough for this threshold.
//!
//! Computed on the side while `import_atp` writes `alltheplaces.parquet`:
//! one [Sighting] per (place, signal, value) goes into a single external
//! sort, and a single streaming pass over the sorted sightings finds the
//! nondescript values of every signal. Not consumed by anything yet;
//! phones and emails are planned matching signals.

use anyhow::Result;
use ext_sort::{ExternalSorter, ExternalSorterBuilder, buffer::LimitedBufferBuilder};
use s2::{cellid::CellID, latlng::LatLng, point::Point, rect::Rect, s1::ChordAngle};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::places::{Place, PlaceReader};
use crate::tables::U64Set;

mod emails;
mod phones;

use emails::{is_email_key, parse_emails};
use phones::{is_phone_key, parse_phone_numbers};

/// Places carrying the same value whose bounding box diagonal is at
/// least this long make that value nondescript.
pub const MIN_NONDESCRIPT_SPREAD_METERS: f64 = 1000.0;

static MIN_NONDESCRIPT_SPREAD: LazyLock<ChordAngle> =
    LazyLock::new(|| crate::matchers::meters_to_chord_angle(MIN_NONDESCRIPT_SPREAD_METERS));

/// A kind of value that can be nondescript.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Signal {
    /// Phone and fax numbers, encoded as the `u64` of their digits
    /// (see [parse_phone_numbers]).
    Phone,
    /// Email addresses, encoded as a 64-bit hash of the normalized
    /// address (see [parse_emails] and [emails::email_hash]).
    Email,
}

impl Signal {
    pub const ALL: [Signal; 2] = [Signal::Phone, Signal::Email];

    /// Filename, within `workdir`, of this signal's set of nondescript
    /// values, as written by [write].
    pub fn filename(self) -> &'static str {
        match self {
            Signal::Phone => "alltheplaces.nondescript-phones",
            Signal::Email => "alltheplaces.nondescript-emails",
        }
    }

    pub fn path(self, workdir: &Path) -> PathBuf {
        workdir.join(self.filename())
    }

    fn name(self) -> &'static str {
        match self {
            Signal::Phone => "phones",
            Signal::Email => "emails",
        }
    }
}

/// Paths, within `workdir`, of every signal's set of nondescript values.
pub fn paths(workdir: &Path) -> Vec<PathBuf> {
    Signal::ALL.iter().map(|s| s.path(workdir)).collect()
}

/// One place carrying one value of one signal, as sorted by [sorter].
/// Fixed-size, so the sort buffer can budget by item count. Sorts by
/// signal, then value, so each (signal, value) is one run of sightings.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Sighting {
    signal: Signal,
    value: u64,
    s2_cell_id: u64,
}

/// Returns one [Sighting] for each phone number and email address in
/// `place`'s tags, at the place's `s2_cell_id`. A value listed twice on
/// the same place yields two identical sightings, which doesn't change
/// any rectangle.
pub fn sightings(place: &Place) -> Vec<Sighting> {
    let mut result = Vec::new();
    for (key, value) in &place.tags {
        let (signal, values): (Signal, Box<dyn Iterator<Item = u64>>) = if is_phone_key(key) {
            (Signal::Phone, Box::new(parse_phone_numbers(value)))
        } else if is_email_key(key) {
            (Signal::Email, Box::new(parse_emails(value)))
        } else {
            continue;
        };
        result.extend(values.map(|value| Sighting {
            signal,
            value,
            s2_cell_id: place.s2_cell_id,
        }));
    }
    result
}

/// External sorter for [Sighting]s. Feed it with `sort_by(…,
/// Sighting::cmp)`, then stream the result through [find_nondescript].
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

/// Nondescript values, per [Signal], each in ascending order.
#[derive(Debug, Default, PartialEq)]
pub struct Nondescript {
    pub phones: Vec<u64>,
    pub emails: Vec<u64>,
}

impl Nondescript {
    pub fn get(&self, signal: Signal) -> &[u64] {
        match signal {
            Signal::Phone => &self.phones,
            Signal::Email => &self.emails,
        }
    }

    fn push(&mut self, signal: Signal, value: u64) {
        match signal {
            Signal::Phone => self.phones.push(value),
            Signal::Email => self.emails.push(value),
        }
    }
}

/// Streams over sightings sorted by (signal, value) and returns every
/// value whose places are spread out over at least
/// [MIN_NONDESCRIPT_SPREAD_METERS].
pub fn find_nondescript<E>(
    sorted: impl Iterator<Item = std::result::Result<Sighting, E>>,
) -> std::result::Result<Nondescript, E> {
    struct Group {
        signal: Signal,
        value: u64,
        // Only the cell id while the group has a single cell, which is
        // the common case: most values belong to a single place.
        first_cell: u64,
        rect: Option<Rect>,
    }

    impl Group {
        fn new(s: &Sighting) -> Group {
            Group {
                signal: s.signal,
                value: s.value,
                first_cell: s.s2_cell_id,
                rect: None,
            }
        }

        fn add(&mut self, s: &Sighting) {
            if s.s2_cell_id == self.first_cell && self.rect.is_none() {
                return;
            }
            let rect = self
                .rect
                .get_or_insert_with(|| Rect::from(LatLng::from(CellID(self.first_cell))));
            *rect = &*rect + &LatLng::from(CellID(s.s2_cell_id));
        }

        fn is_nondescript(&self) -> bool {
            let Some(rect) = &self.rect else {
                return false;
            };
            let diagonal = Point::from(rect.lo()).chordangle(&Point::from(rect.hi()));
            diagonal >= *MIN_NONDESCRIPT_SPREAD
        }
    }

    let mut result = Nondescript::default();
    let mut group: Option<Group> = None;
    for s in sorted {
        let s = s?;
        match group.as_mut() {
            Some(g) if g.signal == s.signal && g.value == s.value => g.add(&s),
            _ => {
                if let Some(g) = group.replace(Group::new(&s))
                    && g.is_nondescript()
                {
                    result.push(g.signal, g.value);
                }
            }
        }
    }
    if let Some(g) = group
        && g.is_nondescript()
    {
        result.push(g.signal, g.value);
    }
    Ok(result)
}

/// Builds the nondescript values from an existing `alltheplaces.parquet`,
/// for a workdir where that file is reused but a set is missing or
/// stale -- e.g. one from before that set existed, whose zip may
/// already have been deleted. `import_atp` otherwise builds the sets
/// while writing the parquet file, without this extra read.
pub fn build_from_parquet(parquet: &Path, workdir: &Path) -> Result<()> {
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
    write(&find_nondescript(sorted)?, workdir)
}

/// Writes each signal's nondescript values to [Signal::path] as a
/// [U64Set].
pub fn write(nondescript: &Nondescript, workdir: &Path) -> Result<()> {
    for signal in Signal::ALL {
        let values = nondescript.get(signal);
        let out = signal.path(workdir);
        // The lists are small (tens of thousands of values worldwide)
        // and already sorted, so one exactly sized chunk is enough;
        // there's no need to claim the pipeline's whole external-sort
        // budget for them.
        let chunk_bytes = values.len().max(1) * size_of::<u64>();
        U64Set::create(values.iter().copied(), workdir, &out, chunk_bytes)?;
        log::info!(
            signal = signal.name(),
            count = values.len(),
            path = out.display().to_string();
            "import_atp: wrote nondescript values"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::emails::email_hash;
    use super::*;
    use crate::matchers::MatchMask;
    use crate::utils::UtcTimestamp;
    use geo::Coord;

    fn place(lng: f64, lat: f64, tags: &[(&str, &str)]) -> Place {
        let coord = Coord { x: lng, y: lat };
        Place::new(
            &coord,
            "test_spider".to_string(),
            MatchMask::SHOP,
            tags.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            &geo::Geometry::from(geo::Point::from(coord)),
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
                ("email", "Info@Example.ch"),
                ("contact:email", "sales@example.ch"),
                ("description", "Write to help@example.ch"),
            ],
        );
        let s = sightings(&p);
        let values: Vec<(Signal, u64)> = s.iter().map(|s| (s.signal, s.value)).collect();
        assert_eq!(
            values,
            [
                (Signal::Phone, 41444177777),
                (Signal::Phone, 41444177778),
                (Signal::Phone, 41444177779),
                (Signal::Email, email_hash("info@example.ch")),
                (Signal::Email, email_hash("sales@example.ch")),
            ]
        );
        assert!(s.iter().all(|s| s.s2_cell_id == p.s2_cell_id));

        assert!(sightings(&place(8.5, 47.3, &[("name", "No phone")])).is_empty());
    }

    fn sighting(signal: Signal, value: u64, lng: f64, lat: f64) -> Sighting {
        Sighting {
            signal,
            value,
            s2_cell_id: CellID::from(LatLng::from_degrees(lat, lng)).0,
        }
    }

    fn phone(value: u64, lng: f64, lat: f64) -> Sighting {
        sighting(Signal::Phone, value, lng, lat)
    }

    fn email(value: u64, lng: f64, lat: f64) -> Sighting {
        sighting(Signal::Email, value, lng, lat)
    }

    fn nondescript(mut sightings: Vec<Sighting>) -> Nondescript {
        sightings.sort();
        find_nondescript(sightings.into_iter().map(std::result::Result::<_, ()>::Ok)).unwrap()
    }

    #[test]
    fn test_find_nondescript() {
        assert_eq!(nondescript(vec![]), Nondescript::default());

        // A single place is never nondescript.
        assert_eq!(
            nondescript(vec![phone(1, 8.5, 47.3)]),
            Nondescript::default()
        );

        let zurich = (8.5417, 47.3769);
        let bern = (7.4474, 46.9480);
        // About 40 m east of `zurich`.
        let next_door = (8.5422, 47.3769);
        // About 1.5 km north of `zurich`.
        let oerlikon = (8.5417, 47.3904);

        let found = nondescript(vec![
            // Same site, listed three times: not nondescript.
            phone(10, zurich.0, zurich.1),
            phone(10, zurich.0, zurich.1),
            phone(10, next_door.0, next_door.1),
            // Zürich and Bern: nondescript.
            phone(20, zurich.0, zurich.1),
            phone(20, bern.0, bern.1),
            // 1.5 km apart: nondescript.
            phone(30, zurich.0, zurich.1),
            phone(30, oerlikon.0, oerlikon.1),
            // Just once.
            phone(40, bern.0, bern.1),
            // Three places, in no particular order.
            phone(50, next_door.0, next_door.1),
            phone(50, bern.0, bern.1),
            phone(50, zurich.0, zurich.1),
            // About 1.5 km apart, across the antimeridian.
            phone(60, 179.993, -16.5),
            phone(60, -179.993, -16.5),
            // About 400 m apart, across the antimeridian.
            phone(70, 179.998, -16.5),
            phone(70, -179.998, -16.5),
            // The same value as a phone and as an email: separate groups,
            // so neither is spread out on its own.
            phone(80, zurich.0, zurich.1),
            email(80, bern.0, bern.1),
            // Emails follow the same rule; the last group in the stream
            // is nondescript too.
            email(10, zurich.0, zurich.1),
            email(10, next_door.0, next_door.1),
            email(90, zurich.0, zurich.1),
            email(90, bern.0, bern.1),
        ]);
        assert_eq!(found.phones, [20, 30, 50, 60]);
        assert_eq!(found.emails, [90]);
    }

    #[test]
    fn test_find_nondescript_propagates_errors() {
        let input = vec![Ok(phone(1, 8.5, 47.3)), Err("broken")];
        assert_eq!(find_nondescript(input.into_iter()), Err("broken"));
    }

    #[test]
    fn test_sort_and_write() -> Result<()> {
        let workdir = tempfile::tempdir()?;
        let sorter = sorter(workdir.path(), 1024)?;
        let input = vec![
            phone(41444177777, 8.5417, 47.3769),
            phone(18004633339, -87.6, 41.8),
            email(7, 7.4474, 46.9480),
            phone(41444177777, 7.4474, 46.9480),
            phone(18004633339, -87.6, 41.8),
            email(7, 8.5417, 47.3769),
        ];
        let sorted = sorter.sort_by(input.into_iter().map(std::io::Result::Ok), Sighting::cmp)?;
        let found = find_nondescript(sorted)?;
        assert_eq!(found.phones, [41444177777]);
        assert_eq!(found.emails, [7]);

        write(&found, workdir.path())?;
        let phones = U64Set::open(&Signal::Phone.path(workdir.path()))?;
        assert_eq!(phones.iter().collect::<Vec<u64>>(), [41444177777]);
        let emails = U64Set::open(&Signal::Email.path(workdir.path()))?;
        assert_eq!(emails.iter().collect::<Vec<u64>>(), [7]);
        Ok(())
    }

    #[test]
    fn test_build_from_parquet() -> Result<()> {
        let workdir = tempfile::tempdir()?;
        let parquet = workdir.path().join("alltheplaces.parquet");
        let mut writer = crate::places::ParquetWriter::try_new(16, &parquet)?;
        writer.write(place(
            8.5417,
            47.3769,
            &[("phone", "+41 44 417 77 77"), ("email", "info@example.ch")],
        ))?;
        writer.write(place(
            7.4474,
            46.9480,
            &[("phone", "+41 44 417 77 77"), ("email", "INFO@example.ch")],
        ))?;
        writer.write(place(
            7.4474,
            46.9480,
            &[("phone", "+41 31 321 21 11"), ("email", "bern@example.ch")],
        ))?;
        writer.close()?;

        build_from_parquet(&parquet, workdir.path())?;
        let phones = U64Set::open(&Signal::Phone.path(workdir.path()))?;
        assert_eq!(phones.iter().collect::<Vec<u64>>(), [41444177777]);
        let emails = U64Set::open(&Signal::Email.path(workdir.path()))?;
        assert_eq!(
            emails.iter().collect::<Vec<u64>>(),
            [email_hash("info@example.ch")]
        );
        Ok(())
    }
}
