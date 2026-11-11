//! AllThePlaces' own statistics for a run, from its `_results.json`:
//! how many features and errors each spider had. [super::fetch] stores
//! the file in the workdir, and [super::spider_counts] compares it with
//! what we read from the dump.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// What AllThePlaces reports for one spider.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub(in crate::pipeline) struct SpiderRunStats {
    pub spider: String,

    /// Features the spider wrote.
    #[serde(default)]
    pub features: u64,

    /// Errors the spider logged while running, such as pages that
    /// failed to load or parse.
    #[serde(default)]
    pub errors: u64,
}

#[derive(Deserialize)]
struct Results {
    results: Vec<SpiderRunStats>,
}

/// Parses the contents of `_results.json`.
pub(super) fn parse(json: &[u8]) -> Result<Vec<SpiderRunStats>> {
    Ok(serde_json::from_slice::<Results>(json)?.results)
}

/// Reads the statistics that `fetch_atp` stored in `workdir`, by
/// spider, or `None` if there are none.
pub(in crate::pipeline) fn read(workdir: &Path) -> Result<Option<HashMap<String, SpiderRunStats>>> {
    let path = workdir.join(super::fetch::STATS_JSON_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read(&path).with_context(|| format!("could not read {path:?}"))?;
    let stats = parse(&json).with_context(|| format!("could not parse {path:?}"))?;
    Ok(Some(
        stats.into_iter().map(|s| (s.spider.clone(), s)).collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse() -> Result<()> {
        let json = br#"{"count": 2, "results": [
            {"spider": "365_bank_sk", "filename": "locations/spiders/365_bank_sk.py",
             "errors": 0, "features": 283, "elapsed_time": 11.8},
            {"spider": "broken", "errors": 3}
        ]}"#;
        let stats = parse(json)?;
        assert_eq!(
            stats,
            vec![
                SpiderRunStats {
                    spider: "365_bank_sk".into(),
                    features: 283,
                    errors: 0
                },
                SpiderRunStats {
                    spider: "broken".into(),
                    features: 0,
                    errors: 3
                },
            ]
        );
        assert!(parse(b"[]").is_err());
        Ok(())
    }
}
