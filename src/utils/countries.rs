//! Which country a point lies in, for deletion candidates and for
//! parsing phone numbers in national format.
//!
//! Uses the [country-boundaries](https://crates.io/crates/country-boundaries)
//! crate, whose boundaries are derived from OpenStreetMap
//! (© OpenStreetMap contributors, ODbL) via JOSM's `boundaries.osm`.

use country_boundaries::{BOUNDARIES_ODBL_360X180, BoundingBox, CountryBoundaries, LatLon};
use std::sync::LazyLock;

/// The version of the `country-boundaries` crate, which ships the
/// boundary data; listed in the provenance BOM. A test checks it
/// against `Cargo.lock`.
pub(crate) const VERSION: &str = "1.2.0";

/// Where the boundary data of `country-boundaries` comes from.
pub(crate) const SOURCE_URL: &str =
    "https://josm.openstreetmap.de/export/HEAD/josm/trunk/resources/data/boundaries.osm";

/// The boundary data, as shipped by `country-boundaries`.
pub(crate) const DATA: &[u8] = BOUNDARIES_ODBL_360X180;

static BOUNDARIES: LazyLock<CountryBoundaries> = LazyLock::new(|| {
    CountryBoundaries::from_reader(DATA).expect("country-boundaries ships valid data")
});

/// A country, by its two-letter ISO 3166-1 code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Country([u8; 2]);

impl Country {
    pub const SWITZERLAND: Country = Country(*b"CH");
    pub const LIECHTENSTEIN: Country = Country(*b"LI");
    pub const GERMANY: Country = Country(*b"DE");
    pub const ITALY: Country = Country(*b"IT");
    /// Mainland China; Hong Kong, Macau and Taiwan have their own codes.
    pub const CHINA: Country = Country(*b"CN");

    /// The two-letter code, such as `CH`.
    pub fn code(&self) -> &str {
        std::str::from_utf8(&self.0).expect("ASCII")
    }

    /// The two-letter code as bytes, such as `*b"CH"`.
    pub fn code_bytes(&self) -> [u8; 2] {
        self.0
    }

    /// The country with the two-letter code `code`, if it is one that
    /// [country] can return.
    pub fn from_code(code: [u8; 2]) -> Option<Country> {
        all().binary_search(&Country(code)).ok().map(|i| all()[i])
    }
}

static ALL: LazyLock<Vec<Country>> = LazyLock::new(|| {
    let world = BoundingBox::new(-90.0, -180.0, 90.0, 179.999_999).expect("valid bounds");
    let mut all: Vec<Country> = BOUNDARIES
        .intersecting_ids(world)
        .into_iter()
        .filter_map(|id| <[u8; 2]>::try_from(id.as_bytes()).ok())
        .map(Country)
        .collect();
    all.sort();
    all
});

/// Every country that [country] can return, sorted by code.
pub fn all() -> &'static [Country] {
    &ALL
}

/// The country that (`lng`, `lat`), in degrees, lies in, or `None` if
/// it lies in none, such as in the open sea.
pub fn country(lng: f64, lat: f64) -> Option<Country> {
    let position = LatLon::new(lat, lng).ok()?;
    // Ids are sorted from the smallest region to the largest, such as
    // `["US-TX", "US"]`; subdivisions have a hyphen.
    BOUNDARIES
        .ids(position)
        .into_iter()
        .find_map(|id| <[u8; 2]>::try_from(id.as_bytes()).ok())
        .map(Country)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(lng: f64, lat: f64) -> Option<String> {
        country(lng, lat).map(|c| c.code().to_string())
    }

    #[test]
    fn test_version_matches_cargo_lock() {
        let lock = include_str!("../../Cargo.lock");
        let entry = format!("name = \"country-boundaries\"\nversion = \"{VERSION}\"");
        assert!(lock.contains(&entry), "update VERSION to match Cargo.lock");
    }

    #[test]
    fn test_country() {
        let ch = Some("CH".to_string());
        assert_eq!(code(8.5402, 47.3782), ch); // Zürich HB
        assert_eq!(code(7.5896, 47.5476), ch); // Basel SBB
        assert_eq!(code(9.1700, 47.6510), ch); // Kreuzlingen
        assert_eq!(code(8.9511, 46.0037), ch); // Lugano

        let de = Some("DE".to_string());
        assert_eq!(code(9.1772, 47.6586), de); // Konstanz
        assert_eq!(code(8.6900, 47.6960), de); // Büsingen, an enclave
        assert_eq!(code(7.6103, 47.5935), de); // Weil am Rhein
        assert_eq!(code(8.9710, 45.9690), Some("IT".to_string())); // Campione
        assert_eq!(code(9.5209, 47.1410), Some("LI".to_string())); // Vaduz
        assert_eq!(code(-97.0, 33.0), Some("US".to_string())); // Texas

        assert_eq!(code(121.4737, 31.2304), Some("CN".to_string())); // Shanghai
        assert_eq!(code(114.1694, 22.3193), Some("HK".to_string())); // Hong Kong
        assert_eq!(code(113.5439, 22.1987), Some("MO".to_string())); // Macau
        assert_eq!(code(121.5654, 25.0330), Some("TW".to_string())); // Taipei

        assert_eq!(code(-30.0, 40.0), None); // Atlantic
        assert_eq!(code(0.0, 91.0), None); // invalid
    }

    #[test]
    fn test_all() {
        let all = all();
        // Fits into a byte, with 0 for none; see OsmFeatureIndex. As of
        // country-boundaries 1.2.0, there are exactly 255. If a newer
        // version has more, OsmFeatureIndex needs two bytes per feature.
        assert!(all.len() <= 255, "{}", all.len());
        assert!(all.len() > 190, "{}", all.len());
        assert!(all.windows(2).all(|w| w[0] < w[1]));
        assert!(all.contains(&Country::SWITZERLAND));
        assert!(all.contains(&Country(*b"HK")));
        assert_eq!(Country::from_code(*b"CH"), Some(Country::SWITZERLAND));
        assert_eq!(Country::from_code(*b"ZZ"), None);
        // Every country found on a grid over the world is in `all`.
        for lat in (-89..90).step_by(1) {
            for lng in (-179..180).step_by(1) {
                if let Some(c) = country(lng as f64 + 0.5, lat as f64 + 0.5) {
                    assert!(all.binary_search(&c).is_ok(), "{c:?}");
                }
            }
        }
    }
}
