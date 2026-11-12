//! What `import_atp` did with each spider's features: how many it read,
//! and how many it left out and why. Written to
//! `alltheplaces.spider-counts.tsv` in the workdir, since the features
//! left out aren't in `alltheplaces.parquet` to count again later.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use super::run_stats::SpiderRunStats;

/// Filename, within `workdir`, of the counts that `import_atp` writes.
const FILENAME: &str = "alltheplaces.spider-counts.tsv";

const HEADER: &str = "spider\tfeatures\tnot_for_osm\tinvalid\tin_china\tdatum\tdataset_attributes";

/// The header before the file had `dataset_attributes`; [read] still
/// reads such files, with no attributes, and [is_current] tells
/// `import_atp` to count again where it can.
const OLD_HEADER: &str = "spider\tfeatures\tnot_for_osm\tinvalid\tin_china\tdatum";

/// Path of the counts that `import_atp` writes in `workdir`.
pub(in crate::pipeline) fn path(workdir: &Path) -> PathBuf {
    workdir.join(FILENAME)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::pipeline) struct SpiderCounts {
    pub spider: String,

    /// Features in the spider's file of the dump. A spider that crashed
    /// has an empty file, and none.
    pub features: u64,

    /// Features left out because their license doesn't allow using them
    /// in OpenStreetMap.
    pub not_for_osm: u64,

    /// Features left out because they have no geometry we understand,
    /// or don't name their spider.
    pub invalid: u64,

    /// Features in mainland China.
    pub in_china: u64,

    /// The datum we found for the features in mainland China (`wgs84`,
    /// `gcj02` or `bd09`), or empty if there are none or we didn't
    /// look, as in tests without OSM data. The other two are converted
    /// to WGS-84.
    pub datum: String,

    /// The `dataset_attributes` of the spider's FeatureCollection, such
    /// as `license` or `attribution:name`, sorted by key. Values that
    /// aren't JSON strings are kept as JSON text. Empty for a spider
    /// that crashed, since its file is empty.
    pub dataset_attributes: Vec<(String, String)>,
}

impl SpiderCounts {
    /// Features that went into `alltheplaces.parquet`.
    pub fn written(&self) -> u64 {
        self.features - self.not_for_osm - self.invalid
    }
}

/// Writes `counts`, sorted by spider, to `out` via a temporary file.
pub(super) fn write(counts: &mut [SpiderCounts], out: &Path) -> Result<()> {
    counts.sort_by(|a, b| a.spider.cmp(&b.spider));
    let mut tmp = out.to_path_buf();
    tmp.add_extension("tmp");
    let mut w = BufWriter::new(
        std::fs::File::create(&tmp).with_context(|| format!("could not create {tmp:?}"))?,
    );
    writeln!(w, "{HEADER}")?;
    for c in counts.iter() {
        // As a JSON object, which escapes any tabs and newlines.
        let attrs: serde_json::Map<String, serde_json::Value> = c
            .dataset_attributes
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            c.spider,
            c.features,
            c.not_for_osm,
            c.invalid,
            c.in_china,
            c.datum,
            serde_json::Value::Object(attrs)
        )?;
    }
    w.into_inner()?.sync_all()?;
    std::fs::rename(&tmp, out)?;
    Ok(())
}

/// Reads counts that [write] wrote.
pub(in crate::pipeline) fn read(path: &Path) -> Result<Vec<SpiderCounts>> {
    let file = std::fs::File::open(path).with_context(|| format!("could not open {path:?}"))?;
    let mut lines = BufReader::new(file).lines();
    let num_cols = match lines.next().transpose()?.as_deref() {
        Some(HEADER) => 7,
        Some(OLD_HEADER) => 6,
        _ => anyhow::bail!("{path:?} doesn't start with the expected header"),
    };
    let mut counts = Vec::new();
    for line in lines {
        let line = line?;
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() != num_cols {
            anyhow::bail!("bad line in {path:?}: {line:?}");
        }
        let [spider, features, not_for_osm, invalid, in_china, datum] = cols[..6] else {
            unreachable!("checked the number of columns");
        };
        let dataset_attributes = match cols.get(6) {
            Some(json) => parse_attributes(json)
                .with_context(|| format!("bad dataset_attributes in {path:?}: {line:?}"))?,
            None => Vec::new(),
        };
        let num = |s: &str| -> Result<u64> {
            s.parse()
                .with_context(|| format!("bad number in {path:?}: {line:?}"))
        };
        counts.push(SpiderCounts {
            spider: spider.to_string(),
            features: num(features)?,
            not_for_osm: num(not_for_osm)?,
            invalid: num(invalid)?,
            in_china: num(in_china)?,
            datum: datum.to_string(),
            dataset_attributes,
        });
    }
    Ok(counts)
}

/// Whether the counts at `path` were written by this version, with the
/// dataset attributes, rather than by an earlier one.
pub(super) fn is_current(path: &Path) -> Result<bool> {
    let file = std::fs::File::open(path).with_context(|| format!("could not open {path:?}"))?;
    let first = BufReader::new(file).lines().next().transpose()?;
    Ok(first.as_deref() == Some(HEADER))
}

/// The dataset attributes of a spider's FeatureCollection, as string
/// key/value pairs sorted by key; see
/// [SpiderCounts::dataset_attributes].
pub(super) fn attribute_pairs(attrs: &serde_json::Value) -> Vec<(String, String)> {
    let Some(attrs) = attrs.as_object() else {
        return Vec::new();
    };
    let mut pairs: Vec<(String, String)> = attrs
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| {
            let v = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), v)
        })
        .collect();
    pairs.sort();
    pairs
}

fn parse_attributes(json: &str) -> Result<Vec<(String, String)>> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    if !value.is_object() {
        anyhow::bail!("not a JSON object");
    }
    Ok(attribute_pairs(&value))
}

/// Totals of `counts`, compared with what AllThePlaces says it wrote,
/// for the log.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct Summary {
    pub spiders: u64,
    pub features: u64,
    pub not_for_osm: u64,
    pub invalid: u64,
    pub written: u64,
    pub converted: u64,
    /// Spiders with a file, but no features in it.
    pub empty: u64,
    /// Spiders for which AllThePlaces reports errors.
    pub with_errors: u64,
    /// Spiders for which AllThePlaces reports a different number of
    /// features than its dump has, and up to ten of them.
    pub miscounted: u64,
    pub miscounted_examples: Vec<String>,
    /// Spiders in AllThePlaces' statistics that aren't in the dump.
    pub missing: u64,
    /// Spiders in the dump that aren't in AllThePlaces' statistics.
    pub unlisted: u64,
}

pub(super) fn summarize(
    counts: &[SpiderCounts],
    run_stats: Option<&HashMap<String, SpiderRunStats>>,
) -> Summary {
    let mut s = Summary::default();
    for c in counts {
        s.spiders += 1;
        s.features += c.features;
        s.not_for_osm += c.not_for_osm;
        s.invalid += c.invalid;
        s.written += c.written();
        if c.datum == "gcj02" || c.datum == "bd09" {
            s.converted += c.in_china;
        }
        if c.features == 0 {
            s.empty += 1;
        }
    }
    let Some(run_stats) = run_stats else {
        return s;
    };
    let by_spider: HashMap<&str, &SpiderCounts> =
        counts.iter().map(|c| (c.spider.as_str(), c)).collect();
    let mut miscounted = Vec::new();
    for stats in run_stats.values() {
        if stats.errors > 0 {
            s.with_errors += 1;
        }
        match by_spider.get(stats.spider.as_str()) {
            Some(c) if c.features != stats.features => miscounted.push(stats.spider.clone()),
            Some(_) => {}
            None => s.missing += 1,
        }
    }
    s.unlisted = counts
        .iter()
        .filter(|c| !run_stats.contains_key(&c.spider))
        .count() as u64;
    miscounted.sort();
    s.miscounted = miscounted.len() as u64;
    miscounted.truncate(10);
    s.miscounted_examples = miscounted;
    s
}

pub(super) fn log_summary(s: &Summary, has_run_stats: bool) {
    log::info!(
        spiders = s.spiders,
        features = s.features,
        not_for_osm = s.not_for_osm,
        invalid = s.invalid,
        written = s.written,
        converted_datum = s.converted,
        empty_spiders = s.empty,
        has_run_stats = has_run_stats,
        spiders_with_errors = s.with_errors,
        miscounted_spiders = s.miscounted,
        miscounted_examples = s.miscounted_examples.join(",").as_str(),
        missing_spiders = s.missing,
        unlisted_spiders = s.unlisted;
        "import_atp: spider counts"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(spider: &str, features: u64) -> SpiderCounts {
        SpiderCounts {
            spider: spider.into(),
            features,
            ..Default::default()
        }
    }

    #[test]
    fn test_write_read() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let out = path(dir.path());
        let mut written = vec![
            SpiderCounts {
                spider: "zara".into(),
                features: 10,
                not_for_osm: 0,
                invalid: 1,
                in_china: 4,
                datum: "gcj02".into(),
                dataset_attributes: vec![
                    ("attribution:name".into(), "Zara\tSA\nLine 2".into()),
                    ("license".into(), "Creative Commons Zero".into()),
                ],
            },
            counts("crashed", 0),
        ];
        write(&mut written, &out)?;
        assert_eq!(written[0].spider, "crashed");
        assert_eq!(read(&out)?, written);
        assert!(is_current(&out)?);
        assert_eq!(written[1].written(), 9);
        Ok(())
    }

    #[test]
    fn test_read_old_header() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let out = path(dir.path());
        std::fs::write(&out, format!("{OLD_HEADER}\nzara\t10\t0\t1\t4\tgcj02\n"))?;
        let c = read(&out)?;
        assert_eq!((c[0].features, c[0].datum.as_str()), (10, "gcj02"));
        assert!(c[0].dataset_attributes.is_empty());
        assert!(!is_current(&out)?);
        Ok(())
    }

    #[test]
    fn test_attribute_pairs() {
        let attrs = serde_json::json!({
            "license": "CC0",
            "@spider": "x",
            "nsi_id": 42,
            "use:openstreetmap": true,
            "gone": null,
        });
        let pairs = attribute_pairs(&attrs);
        let pairs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("@spider", "x"),
                ("license", "CC0"),
                ("nsi_id", "42"),
                ("use:openstreetmap", "true"),
            ]
        );
        assert!(attribute_pairs(&serde_json::json!("x")).is_empty());
    }

    #[test]
    fn test_summarize() {
        let mut china = counts("china", 5);
        china.in_china = 5;
        china.datum = "bd09".into();
        let mut licensed = counts("licensed", 3);
        licensed.not_for_osm = 3;
        let all = [china, licensed, counts("crashed", 0), counts("ok", 7)];
        let stats = |spider: &str, features, errors| SpiderRunStats {
            spider: spider.into(),
            features,
            errors,
        };
        let run_stats: HashMap<String, SpiderRunStats> = [
            stats("china", 5, 0),
            stats("licensed", 3, 0),
            stats("crashed", 0, 2),
            stats("ok", 8, 0),
            stats("gone", 1, 0),
        ]
        .into_iter()
        .map(|s| (s.spider.clone(), s))
        .collect();

        let s = summarize(&all, Some(&run_stats));
        assert_eq!(s.spiders, 4);
        assert_eq!(s.features, 15);
        assert_eq!(s.not_for_osm, 3);
        assert_eq!(s.written, 12);
        assert_eq!(s.converted, 5);
        assert_eq!(s.empty, 1);
        assert_eq!(s.with_errors, 1);
        assert_eq!(s.miscounted, 1);
        assert_eq!(s.miscounted_examples, vec!["ok".to_string()]);
        assert_eq!(s.missing, 1);
        assert_eq!(s.unlisted, 0);

        let s = summarize(&all, None);
        assert_eq!((s.spiders, s.with_errors, s.missing), (4, 0, 0));
    }
}
