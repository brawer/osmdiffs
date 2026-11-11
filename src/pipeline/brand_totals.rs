//! For each brand, operator and network that features refer to by
//! Wikidata ID: how many OpenStreetMap features it has per country and
//! main category, and a name for it, from OSM or AllThePlaces. The
//! spider stats compare these totals with AllThePlaces, and show the
//! names so that their readers needn't look up Wikidata IDs.
//!
//! [super::wikidata_ids::collect_wikidata_ids] fills them in, since it
//! visits the same features anyway.

use crate::matchers::{contact::main_category, parse_wikidata_ids};
use crate::utils::countries::Country;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use super::wikidata_ids::WIKIDATA_KEYS;

/// Filename, within `workdir`, of the OSM feature totals.
const FEATURES_FILENAME: &str = "osm-brands.tsv";

/// Filename, within `workdir`, of the names.
const NAMES_FILENAME: &str = "brand-names.tsv";

pub(super) fn features_path(workdir: &Path) -> PathBuf {
    workdir.join(FEATURES_FILENAME)
}

pub(super) fn names_path(workdir: &Path) -> PathBuf {
    workdir.join(NAMES_FILENAME)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    Osm,
    Atp,
}

/// Wikidata ID, and the key of the name, such as `brand`.
type NameKey = (u64, &'static str);

#[derive(Debug, Default)]
pub(super) struct BrandTotals {
    /// OSM features per Wikidata ID, country and main category (see
    /// [main_category]). A feature that refers to the same ID twice,
    /// such as by `brand:wikidata` and `operator:wikidata`, counts once.
    features: HashMap<(u64, Option<Country>, Option<String>), u64>,

    /// How often OSM and ATP features use each name, in that order.
    names: HashMap<NameKey, HashMap<String, [u64; 2]>>,
}

impl BrandTotals {
    /// Adds a feature with `tags` that lies in `country`, which only
    /// matters for OSM features. Names count only where the Wikidata
    /// tag names a single ID: with several, there's no telling which
    /// name goes with which.
    pub fn add<'a>(
        &mut self,
        source: Source,
        tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
        country: Option<Country>,
    ) {
        let mut ids: Vec<u64> = Vec::new();
        for key in WIKIDATA_KEYS {
            let Some((_, value)) = tags.clone().find(|(k, _)| *k == key) else {
                continue;
            };
            let found: Vec<u64> = parse_wikidata_ids(value).collect();
            let name_key = key.strip_suffix(":wikidata").expect("Wikidata key");
            if let [id] = found[..]
                && let Some((_, name)) = tags.clone().find(|(k, _)| *k == name_key)
            {
                let name = clean_name(name);
                if !name.is_empty() {
                    let counts = self
                        .names
                        .entry((id, name_key))
                        .or_default()
                        .entry(name)
                        .or_default();
                    counts[source as usize] += 1;
                }
            }
            ids.extend(found);
        }
        if source != Source::Osm || ids.is_empty() {
            return;
        }
        ids.sort_unstable();
        ids.dedup();
        let category = main_category(tags);
        for id in ids {
            *self
                .features
                .entry((id, country, category.clone()))
                .or_default() += 1;
        }
    }

    pub fn merge(mut self, other: BrandTotals) -> BrandTotals {
        if self.features.len() + self.names.len() < other.features.len() + other.names.len() {
            return other.merge(self);
        }
        for (key, n) in other.features {
            *self.features.entry(key).or_default() += n;
        }
        for (key, names) in other.names {
            let mine = self.names.entry(key).or_default();
            for (name, [osm, atp]) in names {
                let counts = mine.entry(name).or_default();
                counts[0] += osm;
                counts[1] += atp;
            }
        }
        self
    }

    /// Writes the feature totals to [features_path] and a name per
    /// Wikidata ID and key to [names_path], both sorted, and returns
    /// how many lines each has. The name is the most common one in
    /// Latin script, or if there is none, the most common one: enough
    /// to tell brands apart for readers of the spider stats.
    pub fn write(&self, workdir: &Path) -> Result<(usize, usize)> {
        let mut features: Vec<_> = self.features.iter().collect();
        features.sort_by_cached_key(|((id, country, category), _)| {
            (*id, code(*country), category.clone())
        });
        let num_features = features.len();
        write_tsv(
            &features_path(workdir),
            "wikidata_id\tcountry\tcategory\tfeatures",
            features.into_iter().map(|((id, country, category), n)| {
                format!(
                    "Q{id}\t{}\t{}\t{n}",
                    code(*country),
                    category.as_deref().unwrap_or("")
                )
            }),
        )?;

        let mut keys: Vec<&NameKey> = self.names.keys().collect();
        keys.sort();
        let lines: Vec<String> = keys
            .into_iter()
            .filter_map(|k| {
                let (name, [osm, atp]) = display_name(&self.names[k])?;
                let (id, key) = k;
                Some(format!("Q{id}\t{key}\t{name}\t{osm}\t{atp}"))
            })
            .collect();
        let num_names = lines.len();
        write_tsv(
            &names_path(workdir),
            "wikidata_id\tkey\tname\tosm\tatp",
            lines.into_iter(),
        )?;
        Ok((num_features, num_names))
    }
}

/// The name to show, out of `names` with their counts: the most
/// common in Latin script, or else the most common, with ties broken
/// by the name itself.
fn display_name(names: &HashMap<String, [u64; 2]>) -> Option<(&String, [u64; 2])> {
    let rank = |(name, counts): &(&String, &[u64; 2])| (is_latin(name), counts[0] + counts[1]);
    names
        .iter()
        .max_by(|a, b| rank(a).cmp(&rank(b)).then(b.0.cmp(a.0)))
        .map(|(name, counts)| (name, *counts))
}

/// Whether every letter of `name` is in Latin script, such as
/// `Migros` or `Żabka`, unlike `ミニストップ`.
fn is_latin(name: &str) -> bool {
    name.chars().filter(|c| c.is_alphabetic()).all(
        |c| matches!(c, 'A'..='Z' | 'a'..='z' | '\u{00C0}'..='\u{024F}' | '\u{1E00}'..='\u{1EFF}'),
    )
}

fn code(country: Option<Country>) -> String {
    country.map(|c| c.code().to_string()).unwrap_or_default()
}

/// `name` without surrounding spaces, and with tabs and line breaks
/// replaced, so it fits into one TSV column.
fn clean_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// OSM features per Wikidata ID, country and main category.
pub(super) type OsmFeatureCounts = HashMap<(u64, Option<Country>, String), u64>;

/// Reads [features_path].
pub(super) fn read_features(workdir: &Path) -> Result<OsmFeatureCounts> {
    let mut features = HashMap::new();
    for cols in read_tsv(&features_path(workdir), 4)? {
        let n = cols[3]
            .parse()
            .with_context(|| format!("bad count {:?}", cols[3]))?;
        features.insert(
            (
                parse_id(&cols[0])?,
                parse_country(&cols[1]),
                cols[2].clone(),
            ),
            n,
        );
    }
    Ok(features)
}

/// Reads [names_path] into one name per Wikidata ID: its `brand` name,
/// or else its `operator` name, or else its `network` name.
pub(super) fn read_names(workdir: &Path) -> Result<HashMap<u64, String>> {
    let rank = |key: &str| {
        ["brand", "operator", "network"]
            .iter()
            .position(|k| *k == key)
    };
    let mut best: HashMap<u64, (usize, String)> = HashMap::new();
    for cols in read_tsv(&names_path(workdir), 5)? {
        let Some(r) = rank(&cols[1]) else { continue };
        let id = parse_id(&cols[0])?;
        if best.get(&id).is_none_or(|(old, _)| r < *old) {
            best.insert(id, (r, cols[2].clone()));
        }
    }
    Ok(best.into_iter().map(|(id, (_, name))| (id, name)).collect())
}

fn parse_id(id: &str) -> Result<u64> {
    id.strip_prefix('Q')
        .and_then(|n| n.parse().ok())
        .with_context(|| format!("bad Wikidata ID {id:?}"))
}

fn parse_country(code: &str) -> Option<Country> {
    <[u8; 2]>::try_from(code.as_bytes())
        .ok()
        .and_then(Country::from_code)
}

/// The lines of a TSV file after its header, split into `columns`.
fn read_tsv(path: &Path, columns: usize) -> Result<Vec<Vec<String>>> {
    let data = std::fs::read_to_string(path).with_context(|| format!("could not read {path:?}"))?;
    data.lines()
        .skip(1)
        .map(|line| {
            let cols: Vec<String> = line.split('\t').map(String::from).collect();
            anyhow::ensure!(cols.len() == columns, "bad line in {path:?}: {line:?}");
            Ok(cols)
        })
        .collect()
}

fn write_tsv(out: &Path, header: &str, lines: impl Iterator<Item = String>) -> Result<()> {
    let mut tmp = out.to_path_buf();
    tmp.add_extension("tmp");
    let mut w = BufWriter::new(
        std::fs::File::create(&tmp).with_context(|| format!("could not create {tmp:?}"))?,
    );
    writeln!(w, "{header}")?;
    for line in lines {
        writeln!(w, "{line}")?;
    }
    w.into_inner()?.sync_all()?;
    std::fs::rename(&tmp, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CH: Option<Country> = Some(Country::SWITZERLAND);
    const DE: Option<Country> = Some(Country::GERMANY);

    #[test]
    fn test_brand_totals() -> Result<()> {
        let mut a = BrandTotals::default();
        let mut b = BrandTotals::default();
        let migros = [
            ("shop", "supermarket"),
            ("brand", "Migros"),
            ("brand:wikidata", "Q680727"),
            ("operator", "Genossenschaft Migros Zürich"),
            ("operator:wikidata", "Q1"),
        ];
        a.add(Source::Osm, migros.iter().copied(), CH);
        b.add(Source::Osm, migros.iter().copied(), CH);
        let typo = [
            ("shop", "supermarket"),
            ("brand", "Migors"),
            ("brand:wikidata", "Q680727"),
        ];
        b.add(Source::Osm, typo.iter().copied(), CH);
        // ATP names count, but ATP features don't.
        let atp = [
            ("shop", "supermarket"),
            ("brand", "Migros\t"),
            ("brand:wikidata", "Q680727"),
        ];
        b.add(Source::Atp, atp.iter().copied(), CH);
        // Two IDs: no name.
        let two = [
            ("amenity", "fuel"),
            ("brand", "Migrol;Migros"),
            ("brand:wikidata", "Q1;Q680727"),
        ];
        b.add(Source::Osm, two.iter().copied(), DE);
        // The same ID twice counts once.
        let same = [
            ("brand", "Migros"),
            ("brand:wikidata", "Q680727"),
            ("operator:wikidata", "Q680727"),
        ];
        b.add(Source::Osm, same.iter().copied(), None);
        let totals = a.merge(b);

        let dir = tempfile::tempdir()?;
        assert_eq!(totals.write(dir.path())?, (5, 2));
        assert_eq!(
            std::fs::read_to_string(features_path(dir.path()))?,
            "wikidata_id\tcountry\tcategory\tfeatures\n\
             Q1\tCH\tshop=supermarket\t2\n\
             Q1\tDE\tamenity=fuel\t1\n\
             Q680727\t\t\t1\n\
             Q680727\tCH\tshop=supermarket\t3\n\
             Q680727\tDE\tamenity=fuel\t1\n"
        );
        assert_eq!(
            std::fs::read_to_string(names_path(dir.path()))?,
            "wikidata_id\tkey\tname\tosm\tatp\n\
             Q1\toperator\tGenossenschaft Migros Zürich\t2\t0\n\
             Q680727\tbrand\tMigros\t3\t1\n"
        );
        Ok(())
    }

    #[test]
    fn test_read() -> Result<()> {
        let mut totals = BrandTotals::default();
        let tags = [
            ("shop", "supermarket"),
            ("brand", "Migros"),
            ("brand:wikidata", "Q680727"),
            ("operator", "Migros Zürich"),
            ("operator:wikidata", "Q680727"),
        ];
        totals.add(Source::Osm, tags.iter().copied(), CH);
        totals.add(Source::Osm, tags.iter().copied(), None);
        let dir = tempfile::tempdir()?;
        totals.write(dir.path())?;
        let features = read_features(dir.path())?;
        assert_eq!(features[&(680727, CH, "shop=supermarket".to_string())], 1);
        assert_eq!(features[&(680727, None, "shop=supermarket".to_string())], 1);
        assert_eq!(read_names(dir.path())?[&680727], "Migros");
        Ok(())
    }

    #[test]
    fn test_display_name() {
        let shown = |list: &[(&str, u64)]| {
            let names: HashMap<String, [u64; 2]> =
                list.iter().map(|(n, c)| (n.to_string(), [*c, 0])).collect();
            display_name(&names).map(|(n, _)| n.clone())
        };
        assert_eq!(shown(&[("A", 1), ("B", 4), ("C", 4)]), Some("B".into()));
        assert_eq!(
            shown(&[("ミニストップ", 9), ("Ministop", 2)]),
            Some("Ministop".into())
        );
        assert_eq!(
            shown(&[("ミニストップ", 9), ("ローソン", 2)]),
            Some("ミニストップ".into())
        );
        assert_eq!(shown(&[]), None);
    }

    #[test]
    fn test_is_latin() {
        assert!(is_latin("Migros"));
        assert!(is_latin("Coop Pronto 24/7"));
        assert!(is_latin("Żabka"));
        assert!(is_latin("Phở Hòa"));
        assert!(!is_latin("ミニストップ"));
        assert!(!is_latin("Пятёрочка"));
        assert!(!is_latin("7-Eleven セブン"));
    }
}
