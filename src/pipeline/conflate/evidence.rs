//! Evidence for a match between an AllThePlaces and an OpenStreetMap
//! feature, for the `match.evidence` column of `conflated.parquet`:
//! how far apart they are, and which of their tags agree. This only
//! describes a match; it doesn't decide one.

use crate::matchers::contact::{
    AtpContacts, Nondescript, is_email_key, is_phone_key, is_website_key,
};
use crate::matchers::{normalize_name, split_values};
use crate::utils::opening_hours::same_schedule;
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
/// country for numbers in national format. Postcodes are compared as
/// [postcode] normalizes them, and a house number at the start or end
/// of `addr:street` counts as `addr:housenumber` ([split_address]).
pub(super) fn agreeing<'a>(
    atp: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    osm: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    atp_position: Option<(f64, f64)>,
) -> Vec<String> {
    let atp = split_address(atp);
    let osm = split_address(osm);
    let atp = atp.iter().copied();
    let osm = osm.iter().copied();
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
            Check::Whole(key, same) => {
                let a = atp.clone().find(|(k, _)| k == key).map(|(_, v)| v);
                let o = osm.clone().find(|(k, _)| k == key).map(|(_, v)| v);
                match (a, o) {
                    (Some(a), Some(o)) => same(a, o),
                    _ => false,
                }
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
    /// The whole value of this key, compared by the function: for
    /// values whose `;`-separated parts mean nothing on their own, such
    /// as the rules of `opening_hours`.
    Whole(&'static str, fn(&str, &str) -> bool),
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
        Check::Whole("opening_hours", same_schedule),
    ),
    ("addr:street", Check::Text(&["addr:street"], text)),
    ("addr:housenumber", Check::Text(&["addr:housenumber"], text)),
    ("addr:postcode", Check::Text(&["addr:postcode"], postcode)),
    ("addr:city", Check::Text(&["addr:city"], text)),
    ("ref", Check::Text(&["ref"], text)),
    ("ref:IFOPT", Check::Text(&["ref:IFOPT"], text)),
    ("uic_ref", Check::Text(&["uic_ref"], text)),
];

/// The names of the tags that [agreeing] compares, in its order.
pub(in crate::pipeline) fn compared_tags() -> impl Iterator<Item = &'static str> {
    AGREEING.iter().map(|(name, _)| *name)
}

/// Whether `tags` have a value for the tag that [agreeing] calls
/// `name`, such as any phone key for `phone`. Like [agreeing], this
/// counts a house number in `addr:street` as `addr:housenumber`.
pub(in crate::pipeline) fn has_compared_tag<'a>(
    name: &str,
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> bool {
    let Some((_, check)) = AGREEING.iter().find(|(n, _)| *n == name) else {
        return false;
    };
    split_address(tags).into_iter().any(|(k, v)| {
        !v.trim().is_empty()
            && match check {
                Check::Text(keys, _) => keys.contains(&k),
                Check::Whole(key, _) => k == *key,
                Check::Contact(is_key) => is_key(k),
            }
    })
}

fn text(value: &str) -> Option<String> {
    let v = normalize_name(value);
    (!v.is_empty()).then_some(v)
}

/// A postcode without its formatting: only its letters and digits,
/// lowercased, with full-width forms (`０６０`) made ASCII. So
/// `7591 BG` and `7591BG` agree, and so do the Japanese `060-0001`,
/// `0600001` and `〒060-0001`. A US ZIP+4 such as `46901-4131` gives
/// its first five digits, because OSM mostly has only those; nine
/// digits are no other country's postcode format.
fn postcode(value: &str) -> Option<String> {
    let mut v: String = value
        .chars()
        .map(|c| match c {
            '\u{FF10}'..='\u{FF19}' | '\u{FF21}'..='\u{FF3A}' | '\u{FF41}'..='\u{FF5A}' => {
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            }
            _ => c,
        })
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if v.len() == 9 && v.bytes().all(|b| b.is_ascii_digit()) {
        v.truncate(5);
    }
    (!v.is_empty()).then_some(v)
}

/// `tags`, with a house number moved out of `addr:street` into
/// `addr:housenumber` when there is no `addr:housenumber`: `Darmstädter
/// Str. 236` becomes `Darmstädter Str.` and `236`, `123 Main St`
/// becomes `Main St` and `123`. Many ATP spiders put the whole first
/// address line into `addr:street`.
fn split_address<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<(&'a str, &'a str)> {
    let mut tags: Vec<_> = tags.collect();
    if tags.iter().any(|&(k, _)| k == "addr:housenumber") {
        return tags;
    }
    let Some(i) = tags.iter().position(|&(k, _)| k == "addr:street") else {
        return tags;
    };
    if let Some((street, number)) = split_house_number(tags[i].1) {
        tags[i].1 = street;
        tags.push(("addr:housenumber", number));
    }
    tags
}

/// `street` without the house number at its end (`Nussdorferplatz 7`,
/// `Hauptstrasse 12a`, `Via Roma 3-5`) or start (`123 Main St`), and
/// that number; `None` if neither end looks like a house number, or
/// nothing would be left of the street.
fn split_house_number(street: &str) -> Option<(&str, &str)> {
    let street = street.trim();
    if let Some((rest, last)) = street.rsplit_once(char::is_whitespace)
        && is_house_number(last, true)
        && !rest.trim().is_empty()
    {
        return Some((rest.trim_end(), last));
    }
    if let Some((first, rest)) = street.split_once(char::is_whitespace)
        && is_house_number(first, false)
        && !rest.trim().is_empty()
    {
        return Some((rest.trim_start(), first));
    }
    None
}

/// Whether `token` is digits, then optionally one letter (`12a`), then,
/// if `range`, optionally `-` and digits (`3-5`).
fn is_house_number(token: &str, range: bool) -> bool {
    let (number, to) = match token.split_once('-') {
        Some((n, to)) if range => (n, Some(to)),
        Some(_) => return false,
        None => (token, None),
    };
    let digits = number.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let letters = number.len() - digits.len();
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && letters <= 1
        && to.is_none_or(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f64, y: f64) -> Geometry {
        Geometry::from(geo::Point::new(x, y))
    }

    #[test]
    fn test_has_compared_tag() {
        let tags = [
            ("contact:phone", "+41 44 000 00 00"),
            ("name", " "),
            ("brand", "X"),
        ];
        assert!(has_compared_tag("phone", tags.iter().copied()));
        assert!(has_compared_tag("brand", tags.iter().copied()));
        assert!(!has_compared_tag("name", tags.iter().copied()));
        assert!(!has_compared_tag("email", tags.iter().copied()));
        assert!(!has_compared_tag("unknown", tags.iter().copied()));
        assert_eq!(compared_tags().next(), Some("name"));
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

    #[test]
    fn test_postcode() {
        assert_eq!(postcode("7591 BG"), postcode("7591BG"));
        assert_eq!(postcode("7591 BG").as_deref(), Some("7591bg"));
        assert_eq!(postcode("46901-4131").as_deref(), Some("46901"));
        assert_eq!(postcode("46901").as_deref(), Some("46901"));
        assert_eq!(postcode("SW1A 1AA"), postcode("sw1a1aa"));
        // Japan: the forms that ATP spiders use.
        for jp in [
            "060-0001",
            "0600001",
            "〒060-0001",
            "060‐0001",
            "060ｰ0001",
            "０６０－０００１",
        ] {
            assert_eq!(postcode(jp).as_deref(), Some("0600001"), "{jp}");
        }
        assert_eq!(postcode(" - "), None);
    }

    #[test]
    fn test_split_house_number() {
        let split = split_house_number;
        assert_eq!(
            split("Darmstädter Str. 236"),
            Some(("Darmstädter Str.", "236"))
        );
        assert_eq!(split("Nussdorferplatz 7"), Some(("Nussdorferplatz", "7")));
        assert_eq!(split("Hauptstrasse 12a"), Some(("Hauptstrasse", "12a")));
        assert_eq!(split("Via Roma 3-5"), Some(("Via Roma", "3-5")));
        assert_eq!(split("123 Main St"), Some(("Main St", "123")));
        assert_eq!(split("12B  Main St "), Some(("Main St", "12B")));
        assert_eq!(split("Bahnhofstrasse"), None);
        assert_eq!(split("1st Avenue"), None);
        assert_eq!(split("Avenue 12ab"), None);
        assert_eq!(split("3-5 Main St"), None);
        assert_eq!(split("42"), None);
        assert_eq!(split("札幌市中央区北1条西2丁目1-1"), None);
    }

    #[test]
    fn test_agreeing_address() {
        let atp = [
            ("addr:street", "Darmstädter Str. 236"),
            ("addr:postcode", "64625"),
        ];
        let osm = [
            ("addr:street", "Darmstädter Straße"),
            ("addr:housenumber", "236"),
            ("addr:postcode", "64625"),
        ];
        assert_eq!(
            agreeing(atp.iter().copied(), osm.iter().copied(), None),
            vec!["addr:housenumber", "addr:postcode"]
        );
        let atp = [("addr:street", "Nussdorferplatz 7")];
        let osm = [
            ("addr:street", "Nussdorferplatz"),
            ("addr:housenumber", "7"),
        ];
        assert_eq!(
            agreeing(atp.iter().copied(), osm.iter().copied(), None),
            vec!["addr:street", "addr:housenumber"]
        );
        assert!(has_compared_tag("addr:housenumber", atp.iter().copied()));
        // With its own house number, the street is left alone.
        let atp = [("addr:street", "Route 66"), ("addr:housenumber", "1")];
        assert!(agreeing(atp.iter().copied(), osm.iter().copied(), None).is_empty());
        let atp = [("addr:postcode", "46901-4131")];
        let osm = [("addr:postcode", "46901")];
        assert_eq!(
            agreeing(atp.iter().copied(), osm.iter().copied(), None),
            vec!["addr:postcode"]
        );
    }

    #[test]
    fn test_agreeing_opening_hours() {
        let agrees = |a, o| {
            let atp = [("opening_hours", a)];
            let osm = [("opening_hours", o)];
            agreeing(atp.iter().copied(), osm.iter().copied(), None) == ["opening_hours"]
        };
        assert!(agrees("Mo-Su 00:00-24:00", "24/7"));
        assert!(agrees(
            "Mo-Sa 07:30-21:00",
            "Mo-Fr 07:30-21:00; Sa 07:30-21:00"
        ));
        // One rule in common is not agreement.
        assert!(!agrees(
            "Mo-Fr 08:00-18:00; Sa 09:00-12:00",
            "Mo-Fr 08:00-18:00; Sa 10:00-12:00"
        ));
    }
}
