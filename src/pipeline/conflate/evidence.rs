//! Evidence for a match between an AllThePlaces and an OpenStreetMap
//! feature, for the `match.evidence` column of `conflated.parquet`:
//! how far apart they are, and which of their tags agree. This only
//! describes a match; it doesn't decide one.

use crate::matchers::{normalize_name, split_values};
use geo::{Distance, Euclidean, Geometry, MapCoords};

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

/// The tags whose values agree between ATP and OSM, by the first key
/// of each group, in the order of [AGREEING]. A tag value with several
/// `;`-separated values agrees if any of them does.
pub(super) fn agreeing<'a>(
    atp: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    osm: impl Iterator<Item = (&'a str, &'a str)> + Clone,
) -> Vec<String> {
    AGREEING
        .iter()
        .filter(|(keys, normalize)| {
            let values = |tags: &mut dyn Iterator<Item = (&'a str, &'a str)>| -> Vec<String> {
                tags.filter(|(k, _)| keys.contains(k))
                    .flat_map(|(_, v)| split_values(v))
                    .filter_map(normalize)
                    .collect()
            };
            let a = values(&mut atp.clone());
            let o = values(&mut osm.clone());
            a.iter().any(|v| o.contains(v))
        })
        .map(|(keys, _)| keys[0].to_string())
        .collect()
}

type Normalize = fn(&str) -> Option<String>;

/// Tags compared by [agreeing]: keys that mean the same, and how to
/// normalize their values.
const AGREEING: &[(&[&str], Normalize)] = &[
    (&["name"], text),
    (&["brand"], text),
    (&["phone", "contact:phone", "contact:mobile"], phone),
    (&["email", "contact:email"], text),
    (&["website", "contact:website", "url"], website),
    (&["opening_hours"], opening_hours),
    (&["addr:street"], text),
    (&["addr:housenumber"], text),
    (&["addr:postcode"], text),
    (&["addr:city"], text),
    (&["ref"], text),
    (&["ref:IFOPT"], text),
    (&["uic_ref"], text),
];

fn text(value: &str) -> Option<String> {
    let v = normalize_name(value);
    (!v.is_empty()).then_some(v)
}

/// The last nine digits, which leaves out country and trunk prefixes:
/// `+41 44 123 45 67` and `044 123 45 67` agree.
fn phone(value: &str) -> Option<String> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    (digits.len() >= 7).then(|| digits[digits.len().saturating_sub(9)..].to_string())
}

/// Lowercased host and path, without scheme, `www.`, query, fragment
/// and trailing slash.
fn website(value: &str) -> Option<String> {
    let v = value.trim().to_lowercase();
    let v = v
        .strip_prefix("https://")
        .or_else(|| v.strip_prefix("http://"))
        .unwrap_or(&v);
    let v = v.strip_prefix("www.").unwrap_or(v);
    let v = v
        .split(['?', '#'])
        .next()
        .unwrap_or(v)
        .trim_end_matches('/');
    (!v.is_empty()).then(|| v.to_string())
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
            ("website", "https://www.coop.ch/de/pronto/?utm=x"),
            ("opening_hours", "Mo-Sa 06:00-22:00"),
            ("addr:street", "Bahnhofstrasse"),
        ];
        let osm = [
            ("name", "coop pronto"),
            ("contact:phone", "044 123 45 67;044 999 99 99"),
            ("website", "http://coop.ch/de/pronto"),
            ("opening_hours", "Mo-Sa 06:00-21:00"),
            ("addr:street", "Löwenstrasse"),
        ];
        assert_eq!(
            agreeing(atp.iter().copied(), osm.iter().copied()),
            vec!["name", "phone", "website"]
        );
        assert!(agreeing(atp.iter().copied(), std::iter::empty()).is_empty());
    }
}
