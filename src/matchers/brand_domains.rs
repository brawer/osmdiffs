//! Website domains that belong to one brand, as a fallback for
//! OpenStreetMap features without `brand:wikidata`.
//!
//! Many OSM features link to their brand's website, but don't name the
//! brand's Wikidata item. AllThePlaces knows which domains belong to
//! which brand: every Denner store it lists has a page on `denner.ch`,
//! and carries `brand:wikidata=Q379911`. So `import_atp` collects, for
//! every [registrable domain](super::contact::parse_website_domains)
//! in AllThePlaces, the Wikidata IDs of the features that link to it,
//! and keeps the domains that clearly belong to one brand. An OSM
//! feature whose website is on such a domain is then taken to be of
//! that brand. For the background, see
//! `docs/notes/2026-11-06-brand-domains.md`.
//!
//! A domain belongs to a brand if all of these hold:
//!
//! - At least [MIN_FEATURES] AllThePlaces features link to it.
//! - Each of them has the same [brand ID](brand_id). A single feature
//!   without one disqualifies the domain: hotels on a tourism portal,
//!   or shops on a mall's website, have no brand in common.
//! - It holds at least [MIN_SHARE] of the features of that brand.
//!   This drops the shops that a spider for a brand's stockists links
//!   to: in the 2026-10-10 run, `soeder_ch` gives the soap maker's
//!   `brand:wikidata` to Micasa, Pfister and many pharmacies, each
//!   with their own website.

use super::contact::{is_website_key, parse_website_domains};
use super::parse_wikidata_ids;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

/// A domain needs this many AllThePlaces features to belong to a brand.
pub const MIN_FEATURES: u32 = 2;

/// A domain needs this share of its brand's AllThePlaces features (the
/// ones with a website) to belong to that brand.
pub const MIN_SHARE: f64 = 0.1;

/// The Wikidata ID that a feature's website speaks for: its
/// `brand:wikidata`, or if it has none, its `operator:wikidata`, or
/// else its `network:wikidata`. `None` if the first of these tags that
/// the feature has lists several IDs, such as a car dealer of two
/// brands, or if it has none of them.
pub fn brand_id<'a>(tags: impl Iterator<Item = (&'a str, &'a str)> + Clone) -> Option<u64> {
    for key in ["brand:wikidata", "operator:wikidata", "network:wikidata"] {
        if let Some((_, v)) = tags.clone().find(|(k, _)| *k == key) {
            let mut ids = parse_wikidata_ids(v);
            let (Some(id), None) = (ids.next(), ids.next()) else {
                return None;
            };
            return Some(id);
        }
    }
    None
}

/// Whether the first Wikidata key of a feature lists several IDs,
/// which makes it neither evidence for nor against a domain.
fn has_several_ids<'a>(tags: impl Iterator<Item = (&'a str, &'a str)> + Clone) -> bool {
    for key in ["brand:wikidata", "operator:wikidata", "network:wikidata"] {
        if let Some((_, v)) = tags.clone().find(|(k, _)| *k == key) {
            return parse_wikidata_ids(v).nth(1).is_some();
        }
    }
    false
}

/// The registrable domains in a feature's website tags, without
/// duplicates.
pub fn website_domains<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<String> {
    let mut domains: Vec<String> = tags
        .filter(|(k, _)| is_website_key(k))
        .flat_map(|(_, v)| parse_website_domains(v).collect::<Vec<_>>())
        .collect();
    domains.sort_unstable();
    domains.dedup();
    domains
}

/// Collects AllThePlaces features, one at a time, and finds the
/// domains that belong to one brand.
#[derive(Debug, Default)]
pub struct BrandDomainsBuilder {
    domains: HashMap<String, DomainStats>,
    /// Features with a website, per brand ID.
    brand_features: HashMap<u64, u32>,
}

#[derive(Debug, Default)]
struct DomainStats {
    /// The brand ID of the features seen so far; 0 for none yet.
    brand: u64,
    features: u32,
    /// A feature without brand ID, or with another one, links here.
    mixed: bool,
}

impl BrandDomainsBuilder {
    pub fn add<'a>(&mut self, tags: impl Iterator<Item = (&'a str, &'a str)> + Clone) {
        let domains = website_domains(tags.clone());
        if domains.is_empty() || has_several_ids(tags.clone()) {
            return;
        }
        let brand = brand_id(tags);
        if let Some(brand) = brand {
            *self.brand_features.entry(brand).or_default() += 1;
        }
        for domain in domains {
            let stats = self.domains.entry(domain).or_default();
            stats.features += 1;
            match brand {
                Some(b) if stats.brand == 0 || stats.brand == b => stats.brand = b,
                _ => stats.mixed = true,
            }
        }
    }

    /// The domains that belong to one brand, sorted by domain.
    pub fn build(self) -> Vec<(String, u64, u32)> {
        let mut result: Vec<(String, u64, u32)> = self
            .domains
            .into_iter()
            .filter(|(_, s)| {
                let total = self.brand_features.get(&s.brand).copied().unwrap_or(0);
                !s.mixed
                    && s.brand != 0
                    && s.features >= MIN_FEATURES
                    && f64::from(s.features) >= MIN_SHARE * f64::from(total)
            })
            .map(|(d, s)| (d, s.brand, s.features))
            .collect();
        result.sort_unstable();
        result
    }
}

/// Writes the result of [BrandDomainsBuilder::build] to `path`, as
/// tab-separated `domain`, `Q…` and the number of AllThePlaces
/// features, so that people can read it too.
pub fn write(domains: &[(String, u64, u32)], path: &Path) -> Result<()> {
    let mut tmp = path.to_path_buf();
    tmp.add_extension("tmp");
    let mut out = BufWriter::new(File::create(&tmp)?);
    for (domain, brand, features) in domains {
        writeln!(out, "{domain}\tQ{brand}\t{features}")?;
    }
    out.into_inner()?.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The domains that belong to one brand, as written by [write].
#[derive(Debug, Default)]
pub struct BrandDomains {
    brands: HashMap<String, u64>,
    /// The values of `brands`.
    known: HashSet<u64>,
}

impl BrandDomains {
    pub fn open(path: &Path) -> Result<BrandDomains> {
        let file =
            File::open(path).with_context(|| format!("could not open {}", path.display()))?;
        let mut brands = HashMap::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            let mut cols = line.split('\t');
            let (Some(domain), Some(brand)) = (cols.next(), cols.next()) else {
                continue;
            };
            let Some(brand) = parse_wikidata_ids(brand).next() else {
                continue;
            };
            brands.insert(domain.to_string(), brand);
        }
        Ok(BrandDomains::from_map(brands))
    }

    fn from_map(brands: HashMap<String, u64>) -> BrandDomains {
        let known = brands.values().copied().collect();
        BrandDomains { brands, known }
    }

    /// No domains at all, for tests.
    #[cfg(test)]
    pub fn none() -> BrandDomains {
        BrandDomains::default()
    }

    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, u64)]) -> BrandDomains {
        BrandDomains::from_map(pairs.iter().map(|(d, b)| (d.to_string(), *b)).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.brands.is_empty()
    }

    /// Whether some domain belongs to the brand with Wikidata ID `id`.
    pub fn has_brand(&self, id: u64) -> bool {
        self.known.contains(&id)
    }

    pub fn len(&self) -> usize {
        self.brands.len()
    }

    /// The brand IDs of the domains in a feature's website tags.
    pub fn brands<'a>(&self, tags: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<u64> {
        if self.brands.is_empty() {
            return Vec::new();
        }
        let mut result = Vec::new();
        for (k, v) in tags {
            if !is_website_key(k) {
                continue;
            }
            for domain in parse_website_domains(v) {
                if let Some(brand) = self.brands.get(&domain)
                    && !result.contains(brand)
                {
                    result.push(*brand);
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(b: &mut BrandDomainsBuilder, tags: &[(&str, &str)]) {
        b.add(tags.iter().copied());
    }

    #[test]
    fn test_brand_id() {
        let id = |tags: &[(&str, &str)]| brand_id(tags.iter().copied());
        assert_eq!(id(&[("brand:wikidata", "Q1")]), Some(1));
        assert_eq!(
            id(&[("operator:wikidata", "Q2"), ("brand:wikidata", "Q1")]),
            Some(1)
        );
        assert_eq!(id(&[("operator:wikidata", "Q2")]), Some(2));
        assert_eq!(id(&[("network:wikidata", "Q3")]), Some(3));
        assert_eq!(id(&[("brand:wikidata", "Q1;Q2")]), None);
        assert_eq!(id(&[("brand", "X")]), None);
    }

    #[test]
    fn test_build() {
        let mut b = BrandDomainsBuilder::default();
        // Denner: two stores on denner.ch.
        add(
            &mut b,
            &[
                ("brand:wikidata", "Q1"),
                ("website", "https://www.denner.ch/de/filialen/1"),
            ],
        );
        add(
            &mut b,
            &[
                ("brand:wikidata", "Q1"),
                ("website", "https://denner.ch/de/filialen/2"),
            ],
        );
        // A domain with a single feature.
        add(
            &mut b,
            &[("brand:wikidata", "Q2"), ("website", "https://lonely.ch")],
        );
        // A tourism portal: one hotel has no brand.
        add(
            &mut b,
            &[("brand:wikidata", "Q3"), ("website", "https://portal.ch/a")],
        );
        add(&mut b, &[("website", "https://portal.ch/b")]);
        add(
            &mut b,
            &[("brand:wikidata", "Q3"), ("website", "https://portal.ch/c")],
        );
        // Two brands on one domain.
        add(
            &mut b,
            &[("brand:wikidata", "Q4"), ("website", "https://group.ch/a")],
        );
        add(
            &mut b,
            &[("brand:wikidata", "Q5"), ("website", "https://group.ch/b")],
        );
        // A car dealer of two brands neither helps nor hurts.
        add(
            &mut b,
            &[
                ("brand:wikidata", "Q1;Q6"),
                ("website", "https://denner.ch/x"),
            ],
        );
        // A brand's stockists: 20 on its own domain, 2 each on others.
        for _ in 0..20 {
            add(
                &mut b,
                &[("brand:wikidata", "Q7"), ("website", "https://soap.ch")],
            );
        }
        for _ in 0..2 {
            add(
                &mut b,
                &[("brand:wikidata", "Q7"), ("website", "https://pharmacy.ch")],
            );
        }
        let found = b.build();
        assert_eq!(
            found,
            [
                ("denner.ch".to_string(), 1, 2),
                ("soap.ch".to_string(), 7, 20)
            ]
        );
    }

    #[test]
    fn test_write_and_open() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("brand-domains.tsv");
        write(
            &[
                ("denner.ch".to_string(), 379911, 2),
                ("zürich.ch".to_string(), 72, 5),
            ],
            &path,
        )?;
        assert_eq!(
            std::fs::read_to_string(&path)?,
            "denner.ch\tQ379911\t2\nzürich.ch\tQ72\t5\n"
        );
        let d = BrandDomains::open(&path)?;
        assert_eq!(d.len(), 2);
        let brands = |tags: &[(&str, &str)]| d.brands(tags.iter().copied());
        assert_eq!(brands(&[("website", "https://www.denner.ch/de")]), [379911]);
        assert_eq!(
            brands(&[("contact:website", "http://filialen.denner.ch")]),
            [379911]
        );
        assert_eq!(brands(&[("url", "https://www.xn--zrich-kva.ch/")]), [72]);
        assert_eq!(
            brands(&[(
                "website",
                "https://denner.ch;https://zürich.ch;https://denner.ch/x"
            )]),
            [379911, 72]
        );
        assert!(brands(&[("website", "https://other.ch")]).is_empty());
        assert!(brands(&[("brand:website", "https://denner.ch")]).is_empty());
        Ok(())
    }
}
