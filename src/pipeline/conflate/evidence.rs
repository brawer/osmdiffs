//! Evidence for a match between an AllThePlaces and an OpenStreetMap
//! feature, for the `match.evidence` column of `conflated.parquet`:
//! how far apart they are, and which of their tags agree. This only
//! describes a match; it doesn't decide one.

use crate::matchers::contact::{
    AtpContacts, Nondescript, is_email_key, is_phone_key, is_website_key,
};
use crate::matchers::{normalize_name, split_values};
use geo::{Distance, Euclidean, Geometry, MapCoords};
use std::sync::LazyLock;

const EARTH_RADIUS_METERS: f64 = 6_371_000.0;

/// The shortest distance, in meters, between `atp` and `osm`: 0 when
/// one touches or contains the other, such as an ATP point inside an
/// OSM building. Not the Hausdorff distance, which from a point to a
/// building is the distance to its farthest corner, so a shop in a big
/// mall would look far off. Both are projected to a plane around the
/// first coordinate of `atp`, which is precise enough at the distances
/// that matching allows.
pub(super) fn distance_meters(atp: &Geometry, osm: &Geometry) -> Option<f64> {
    use geo::CoordsIter;
    let origin = atp.coords_iter().next()?;
    let scale = origin.y.to_radians().cos();
    let to_meters = |c: geo::Coord| geo::Coord {
        x: (c.x - origin.x).to_radians() * scale * EARTH_RADIUS_METERS,
        y: (c.y - origin.y).to_radians() * EARTH_RADIUS_METERS,
    };
    let d = Euclidean.distance(&atp.map_coords(to_meters), &osm.map_coords(to_meters));
    d.is_finite().then_some(d)
}

/// The tags whose values agree between ATP and OSM, in the order of
/// [AGREEING]. A tag value with several `;`-separated values agrees if
/// any of them does. Phone numbers, emails and websites are compared as
/// the matchers compare them ([AtpContacts]), so `+41 44 123 45 67`
/// and `044 123 45 67` agree in Switzerland; `atp_position` gives the
/// country for numbers in national format.
pub(super) fn agreeing<'a>(
    atp: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    osm: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    atp_position: Option<(f64, f64)>,
) -> Vec<String> {
    AGREEING
        .iter()
        .filter(|(_, check)| match check {
            Check::Text(keys, normalize) => {
                let values = |tags: &mut dyn Iterator<Item = (&'a str, &'a str)>| -> Vec<String> {
                    tags.filter(|(k, _)| keys.contains(k))
                        .flat_map(|(_, v)| split_values(v))
                        .filter_map(normalize)
                        .collect()
                };
                let a = values(&mut atp.clone());
                let o = values(&mut osm.clone());
                a.iter().any(|v| o.contains(v))
            }
            Check::Contact(is_key) => {
                let contacts = AtpContacts::from_tags(
                    atp.clone()
                        .filter(|&(k, _)| is_key(k) || k == "addr:country"),
                    atp_position,
                    &NONDESCRIPT,
                );
                !contacts.is_empty() && contacts.agrees(osm.clone().filter(|&(k, _)| is_key(k)))
            }
        })
        .map(|(name, _)| name.to_string())
        .collect()
}

static NONDESCRIPT: LazyLock<Nondescript> = LazyLock::new(Nondescript::none);

type Normalize = fn(&str) -> Option<String>;

/// How [agreeing] compares a tag.
enum Check {
    /// The values of these keys, normalized.
    Text(&'static [&'static str], Normalize),
    /// Contacts of the keys for which the function is true, as
    /// [AtpContacts] compares them.
    Contact(fn(&str) -> bool),
}

/// Tags compared by [agreeing], by the name they get in its result.
const AGREEING: &[(&str, Check)] = &[
    ("name", Check::Text(&["name"], text)),
    ("brand", Check::Text(&["brand"], text)),
    ("phone", Check::Contact(is_phone_key)),
    ("email", Check::Contact(is_email_key)),
    ("website", Check::Contact(is_website_key)),
    (
        "opening_hours",
        Check::Text(&["opening_hours"], opening_hours),
    ),
    ("addr:street", Check::Text(&["addr:street"], text)),
    ("addr:housenumber", Check::Text(&["addr:housenumber"], text)),
    ("addr:postcode", Check::Text(&["addr:postcode"], text)),
    ("addr:city", Check::Text(&["addr:city"], text)),
    ("ref", Check::Text(&["ref"], text)),
    ("ref:IFOPT", Check::Text(&["ref:IFOPT"], text)),
    ("uic_ref", Check::Text(&["uic_ref"], text)),
];

fn text(value: &str) -> Option<String> {
    let v = normalize_name(value);
    (!v.is_empty()).then_some(v)
}

fn opening_hours(value: &str) -> Option<String> {
    let v: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    (!v.is_empty()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f64, y: f64) -> Geometry {
        Geometry::from(geo::Point::new(x, y))
    }

    #[test]
    fn test_distance_meters() {
        // 0.001° of latitude is about 111 m.
        let d = distance_meters(&point(8.54, 47.378), &point(8.54, 47.379)).unwrap();
        assert!((d - 111.2).abs() < 0.5, "{d}");
        let ring: Vec<(f64, f64)> = vec![
            (8.539, 47.377),
            (8.541, 47.377),
            (8.541, 47.379),
            (8.539, 47.379),
        ];
        let building = Geometry::from(geo::Polygon::new(ring.into(), Vec::new()));
        assert_eq!(distance_meters(&point(8.54, 47.378), &building), Some(0.0));
        // 0.001° east of the building, at 47.378° N: about 75 m.
        let d = distance_meters(&point(8.542, 47.378), &building).unwrap();
        assert!((d - 75.3).abs() < 0.5, "{d}");
    }

    #[test]
    fn test_agreeing() {
        let atp = [
            ("name", "Coop  Pronto"),
            ("phone", "+41 44 123 45 67"),
            ("website", "https://www.coop.ch/de/pronto/?utm_source=x"),
            ("opening_hours", "Mo-Sa 06:00-22:00"),
            ("addr:street", "Bahnhofstrasse"),
            ("addr:country", "CH"),
        ];
        let osm = [
            ("name", "coop pronto"),
            ("contact:phone", "044 123 45 67;044 999 99 99"),
            ("website", "http://coop.ch/de/pronto"),
            ("opening_hours", "Mo-Sa 06:00-21:00"),
            ("addr:street", "Löwenstrasse"),
        ];
        assert_eq!(
            agreeing(
                atp.iter().copied(),
                osm.iter().copied(),
                Some((8.54, 47.378))
            ),
            vec!["name", "phone", "website"]
        );
        assert!(agreeing(atp.iter().copied(), std::iter::empty(), None).is_empty());
        // Another number: no agreement on phone.
        let other = [("phone", "+41 44 765 43 21")];
        assert!(agreeing(atp.iter().copied(), other.iter().copied(), None).is_empty());
    }
}
