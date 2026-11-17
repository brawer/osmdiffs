//! Whether two street names are the same, allowing for abbreviations
//! and variant spellings: `Darmstädter Str.` and `Darmstädter Straße`,
//! `Nussdorferplatz` and `Nußdorfer Platz`, `N Main St` and `North Main
//! Street`, `Av. Paulista` and `Avenida Paulista`. The abbreviations
//! come from the address dictionaries of
//! [libpostal](https://github.com/openvenues/libpostal), in all of its
//! languages; see `scripts/generate_street_abbreviations.py`.
//!
//! Two words are the same if they are equal, or if the dictionaries
//! list them on one line, such as `st` and `street`. This isn't
//! transitive: `st` is the same as `street` and as `saint`, but
//! `street` and `saint` are different.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Suffixes shorter than this many characters aren't split off a word.
const MIN_SUFFIX_CHARS: usize = 2;

/// A word must keep at least this many characters when a suffix is
/// split off.
const MIN_STEM_CHARS: usize = 3;

struct Dictionary {
    /// For each word, the dictionary lines it occurs on.
    lines: HashMap<String, Vec<u32>>,
    /// Suffixes that may be joined to the word before, longest first.
    suffixes: Vec<String>,
}

static DICTIONARY: LazyLock<Dictionary> =
    LazyLock::new(|| Dictionary::parse(include_str!("street_abbreviations.txt")));

impl Dictionary {
    fn parse(text: &str) -> Dictionary {
        let mut lines: HashMap<String, Vec<u32>> = HashMap::new();
        let mut suffixes = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.starts_with('#') {
                continue;
            }
            let mut fields = line.split('\t');
            let (Some(kind), Some(_lang), Some(forms)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            for form in forms.split('|') {
                let mut words = words(form);
                let (Some(word), None) = (words.next(), words.next()) else {
                    continue; // empty, or several words
                };
                if kind == "suffix" && word.chars().count() >= MIN_SUFFIX_CHARS {
                    suffixes.push(word.clone());
                }
                let ids = lines.entry(word).or_default();
                if !ids.contains(&(i as u32)) {
                    ids.push(i as u32);
                }
            }
        }
        suffixes.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        suffixes.dedup();
        Dictionary { lines, suffixes }
    }

    fn same_word(&self, a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        match (self.lines.get(a), self.lines.get(b)) {
            (Some(a), Some(b)) => a.iter().any(|id| b.contains(id)),
            _ => false,
        }
    }
}

/// `s` lowercased, without accents, and with `ß` as `ss`.
fn fold(s: &str) -> String {
    use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
    let mut out = String::with_capacity(s.len());
    for c in s
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
    {
        match c {
            'ß' => out.push_str("ss"),
            c => out.push(c),
        }
    }
    out
}

/// The folded words of `s`, split at anything not a letter or digit.
pub(crate) fn words(s: &str) -> impl Iterator<Item = String> {
    fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
}

/// The words of a street name, with a joined suffix split off:
/// `Nussdorferplatz` gives `nussdorfer`, `platz`.
pub(crate) fn street_words(s: &str) -> Vec<String> {
    let dict = &*DICTIONARY;
    let mut out = Vec::new();
    for word in words(s) {
        let stem_len = word.chars().count();
        let suffix = dict.suffixes.iter().find(|sx| {
            word.ends_with(sx.as_str()) && stem_len >= sx.chars().count() + MIN_STEM_CHARS
        });
        match suffix {
            Some(sx) if !word.chars().all(|c| c.is_numeric()) => {
                out.push(word[..word.len() - sx.len()].to_string());
                out.push(sx.clone());
            }
            _ => out.push(word),
        }
    }
    out
}

/// Whether two sequences of [street_words] are the same, word by word.
pub(crate) fn same_words(a: &[String], b: &[String]) -> bool {
    let dict = &*DICTIONARY;
    !a.is_empty() && a.len() == b.len() && a.iter().zip(b).all(|(a, b)| dict.same_word(a, b))
}

/// Whether `a` and `b` name the same street.
pub(crate) fn same_street(a: &str, b: &str) -> bool {
    same_words(&street_words(a), &street_words(b))
}

/// Whether the [street_words] of `street` occur, in order and next to
/// each other, in `text`, such as `Hauptstrasse` in `Hauptstr. 4, 8000
/// Zürich`.
pub(crate) fn contains_street(text: &[String], street: &[String]) -> bool {
    !street.is_empty() && text.windows(street.len()).any(|w| same_words(w, street))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_street() {
        for (a, b) in [
            ("Darmstädter Str.", "Darmstädter Straße"),
            ("Hauptstr.", "Hauptstrasse"),
            ("Nussdorferplatz", "Nußdorfer Platz"),
            ("Raabserstr.", "Raabser Straße"),
            ("Main Rd", "Main Road"),
            ("N Main St", "North Main Street"),
            ("S. Archer Avenue", "South Archer Avenue"),
            ("Av. Paulista", "Avenida Paulista"),
            ("R JOAQUIM NABUCO", "Rua Joaquim Nabuco"),
            ("BD DE L'EUROPE", "Boulevard de l'Europe"),
            ("Al. Jana Pawła II", "Aleja Jana Pawła II"),
            ("Dozsa György ut.", "Dózsa György út"),
            ("ул. Ларина", "улица Ларина"),
            ("Gustav-Ree-Anlage", "Gustav-Rée-Anlage"),
        ] {
            assert!(same_street(a, b), "{a} | {b}");
            assert!(same_street(b, a), "{b} | {a}");
        }
        for (a, b) in [
            ("Main St", "Main Ave"),
            ("Bergweg", "Bergstrasse"),
            ("Bahnhofstrasse", "Löwenstrasse"),
            // "st" is short for both, but these differ.
            ("Saint Street", "Street Street"),
            ("Main Street", "Main"),
            ("", ""),
        ] {
            assert!(!same_street(a, b), "{a} | {b}");
        }
    }

    #[test]
    fn test_street_words() {
        assert_eq!(
            street_words("Nussdorferplatz 7"),
            ["nussdorfer", "platz", "7"]
        );
        assert_eq!(street_words("Straße"), ["strasse"]);
        assert_eq!(street_words("Weg"), ["weg"]);
    }

    #[test]
    fn test_contains_street() {
        let text = street_words("Zürcherstr. 4, 8640 Rapperswil");
        assert!(contains_street(&text, &street_words("Zürcherstrasse")));
        assert!(!contains_street(&text, &street_words("Bahnhofstrasse")));
        let text = street_words("4850 W Lloyd Expy");
        assert!(contains_street(
            &text,
            &street_words("West Lloyd Expressway")
        ));
        assert!(!contains_street(&text, &[]));
    }
}
