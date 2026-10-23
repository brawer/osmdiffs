//! Cheap street address agreement check between AllThePlaces and
//! OpenStreetMap, used to confirm far-away match candidates. No
//! address parsing: OSM's split `addr:street`/`addr:housenumber` are
//! looked up in whatever address text the ATP feature carries.

fn find<'a>(tags: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    tags.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Case- and accent-folded alphanumeric tokens, split at
/// non-alphanumeric characters and at letter/digit boundaries, but
/// otherwise unsegmented: `Zürcherstr. 4` → `zurcherstr`, `4`;
/// `銀座4-6-16` → `銀座`, `4`, `6`, `16`. This needs no word
/// segmentation, so it works the same for scripts without spaces.
fn fold_tokens(s: &str) -> Vec<String> {
    use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut cur_numeric = false;
    for c in s
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
    {
        let c = if c == 'ß' { 's' } else { c };
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let numeric = c.is_numeric();
        if !cur.is_empty() && numeric != cur_numeric {
            tokens.push(std::mem::take(&mut cur));
        }
        cur_numeric = numeric;
        cur.push(c);
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Format-agnostic check that needs no address parsing on the ATP
/// side: OSM's `addr:housenumber` tokens must appear in order in ATP's
/// address text (`349-B` in `Mythenquai 349b`), and OSM's
/// `addr:street` must appear in it with spaces and punctuation
/// removed, or start with one ATP word of 5+ letters (abbreviations
/// like `Zürcherstr.`). The ATP text is all of `addr:street_address`,
/// `addr:street`, `addr:housenumber` and `addr:full` that are present.
pub fn osm_address_in_atp_text(atp_tags: &[(&str, &str)], osm_tags: &[(&str, &str)]) -> bool {
    let (Some(street), Some(hn)) = (
        find(osm_tags, "addr:street"),
        find(osm_tags, "addr:housenumber"),
    ) else {
        return false;
    };
    let text: Vec<&str> = [
        "addr:street_address",
        "addr:street",
        "addr:housenumber",
        "addr:full",
    ]
    .iter()
    .filter_map(|k| find(atp_tags, k))
    .collect();
    if text.is_empty() {
        return false;
    }
    let atp = fold_tokens(&text.join(" "));
    let hn = fold_tokens(hn);
    let street: String = fold_tokens(street).concat();
    if hn.is_empty() || street.is_empty() {
        return false;
    }
    // OSM's house number tokens, in order, somewhere in ATP's text.
    let hn_ok = atp.windows(hn.len()).any(|w| w == hn.as_slice());
    let compact = atp.concat();
    let street_ok = compact.contains(&street)
        || atp.iter().any(|t| {
            t.chars().count() >= 5
                && !t.chars().any(|c| c.is_ascii_digit())
                && street.starts_with(t.as_str())
        });
    hn_ok && street_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_check() {
        let osm = [("addr:street", "Zürcherstrasse"), ("addr:housenumber", "4")];
        let yes = |t: &[(&str, &str)]| osm_address_in_atp_text(t, &osm);
        assert!(yes(&[("addr:full", "Zürcherstrasse 4, 8640 Rapperswil")]));
        assert!(yes(&[("addr:street_address", "ZURCHERSTRASSE 4")]));
        assert!(yes(&[("addr:street_address", "Zürcherstr. 4")]));
        assert!(yes(&[
            ("addr:street", "Zürcherstrasse"),
            ("addr:housenumber", "4")
        ]));
        assert!(!yes(&[("addr:full", "Zürcherstrasse 6, 8640 Rapperswil")]));
        assert!(!yes(&[("addr:full", "Bahnhofstrasse 4, 8640 Rapperswil")]));
        let osm_b = [("addr:street", "Mythenquai"), ("addr:housenumber", "349-B")];
        assert!(osm_address_in_atp_text(
            &[("addr:street_address", "Mythenquai 349b")],
            &osm_b
        ));
    }
}
