//! Finds what this project published last time, so that a new output
//! can name its predecessor. `spider-stats.parquet` names last week's
//! file in its key-value metadata, so that a static page can fetch both
//! and show how each spider changed (see
//! <https://github.com/brawer/osmdiffs/issues/999>).
//!
//! Runs in `fetch_inputs`, but unlike the real inputs it never fails the
//! pipeline: on the very first run there is nothing to find, and a
//! missing link to last week is no reason to throw away a week's run.
//! What it found is kept in [`FILENAME`], so that a resumed run names
//! the same predecessor even after this run has replaced the manifest.

use crate::pipeline::download;
use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// Our own manifest, as published by `upload_datapackage`.
pub(crate) const DATAPACKAGE_URL: &str = "https://osmdiffs.brawer.ch/data/datapackage.json";

/// Sidecar in `workdir` that keeps what [`fetch_previous_release`] found.
pub(crate) const FILENAME: &str = "previous-release.json";

/// What the manifest listed when this run started.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PreviousRelease {
    /// The manifest's `version`, the build date, e.g. "2026-11-09".
    pub version: String,

    /// Basename of its `spider-stats` resource, such as
    /// `spider-stats-20261109-a1b2c3d4.parquet`, if it listed one.
    /// Relative, like everything in the manifest: it lives next to the
    /// file that names it.
    pub spider_stats: Option<String>,
}

impl PreviousRelease {
    /// The predecessor of this run's `spider-stats.parquet`, built on
    /// `version`: `None` if the manifest listed none, or if it was an
    /// earlier run on the same inputs (a rerun from a fresh workdir),
    /// whose file would then claim to be its own predecessor.
    pub fn spider_stats_before(&self, version: &str) -> Option<&str> {
        if self.version.as_str() >= version {
            log::warn!(
                previous = self.version.as_str(),
                current = version;
                "previous_release: the published release is not older than this one, \
                 so spider-stats.parquet names no previous file"
            );
            return None;
        }
        self.spider_stats.as_deref()
    }
}

/// Reads the manifest at `datapackage_url`, unless `workdir` already
/// keeps what an earlier attempt of this run found. Returns `None`, and
/// logs why, if the manifest can't be fetched or makes no sense.
pub(crate) async fn fetch_previous_release(
    datapackage_url: &str,
    client: &Client,
    workdir: &Path,
) -> Result<Option<PreviousRelease>> {
    if let Some(kept) = read_cached(workdir)? {
        return Ok(Some(kept));
    }

    let found = async {
        let body = download::get_bytes(client, datapackage_url, Duration::from_secs(60)).await?;
        let package: serde_json::Value = serde_json::from_slice(&body)
            .with_context(|| format!("Failed to parse {datapackage_url}"))?;
        parse(&package).with_context(|| format!("Failed to read {datapackage_url}"))
    }
    .await;
    let previous = match found {
        Ok(previous) => previous,
        Err(err) => {
            log::warn!(
                url = datapackage_url,
                error = format!("{err:#}");
                "previous_release: could not read the published manifest, \
                 so this run's outputs name no previous release"
            );
            return Ok(None);
        }
    };

    // Via a temp file and rename, so a crash mid-write can't leave a
    // truncated sidecar that a restart would fail to parse.
    let path = workdir.join(FILENAME);
    let mut tmp = path.clone();
    tmp.add_extension("tmp");
    tokio::fs::write(&tmp, serde_json::to_string(&previous)?).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(Some(previous))
}

/// The [`PreviousRelease`] that `package`, our `datapackage.json`, lists.
fn parse(package: &serde_json::Value) -> Result<PreviousRelease> {
    let version = package["version"]
        .as_str()
        .ok_or_else(|| anyhow!("no version"))?;
    let resources = package["resources"]
        .as_array()
        .ok_or_else(|| anyhow!("no resources"))?;
    let spider_stats = resources
        .iter()
        .find(|r| r["name"] == "spider-stats")
        .and_then(|r| r["path"].as_str())
        // Only a bare basename resolves next to the file naming it.
        .filter(|p| !p.is_empty() && !p.contains('/'))
        .map(str::to_string);
    Ok(PreviousRelease {
        version: version.to_string(),
        spider_stats,
    })
}

/// What [`fetch_previous_release`] kept in `workdir`, if anything.
fn read_cached(workdir: &Path) -> Result<Option<PreviousRelease>> {
    let path = workdir.join(FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let previous = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(previous))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;
    use serde_json::json;

    fn test_client() -> Client {
        Client::builder()
            // Mockito runs on 127.0.0.1; no proxy needed.
            .no_proxy()
            .build()
            .expect("failed to build test client")
    }

    fn manifest() -> serde_json::Value {
        json!({
            "version": "2026-11-09",
            "resources": [
                {"name": "conflated", "path": "conflated-20261109-11111111.parquet"},
                {"name": "spider-stats", "path": "spider-stats-20261109-22222222.parquet"},
            ],
        })
    }

    #[test]
    fn test_parse() -> Result<()> {
        assert_eq!(
            parse(&manifest())?,
            PreviousRelease {
                version: "2026-11-09".into(),
                spider_stats: Some("spider-stats-20261109-22222222.parquet".into()),
            }
        );

        // Releases before spider-stats.parquet existed.
        let old = json!({"version": "2026-10-26", "resources": []});
        assert_eq!(parse(&old)?.spider_stats, None);

        // A path elsewhere would not resolve next to the new file.
        let elsewhere = json!({
            "version": "2026-11-09",
            "resources": [{"name": "spider-stats", "path": "https://example.org/x.parquet"}],
        });
        assert_eq!(parse(&elsewhere)?.spider_stats, None);

        assert!(parse(&json!({"resources": []})).is_err());
        Ok(())
    }

    #[test]
    fn test_spider_stats_before() {
        let previous = parse(&manifest()).unwrap();
        assert_eq!(
            previous.spider_stats_before("2026-11-16"),
            Some("spider-stats-20261109-22222222.parquet")
        );
        // A rerun on the same inputs, or a clock gone backwards.
        assert_eq!(previous.spider_stats_before("2026-11-09"), None);
        assert_eq!(previous.spider_stats_before("2026-11-02"), None);
    }

    #[tokio::test]
    async fn test_fetch_and_keep() -> Result<()> {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/data/datapackage.json")
            .with_body(manifest().to_string())
            .expect(1)
            .create_async()
            .await;
        let url = format!("{}/data/datapackage.json", server.url());
        let workdir = tempfile::tempdir()?;

        let first = fetch_previous_release(&url, &test_client(), workdir.path()).await?;
        assert_eq!(
            first.as_ref().and_then(|p| p.spider_stats.as_deref()),
            Some("spider-stats-20261109-22222222.parquet")
        );
        assert_eq!(read_cached(workdir.path())?, first);

        // A resumed run keeps what the first attempt found, even though
        // the manifest has since moved on.
        let again = fetch_previous_release(&url, &test_client(), workdir.path()).await?;
        assert_eq!(again, first);
        mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_missing_manifest_is_no_error() -> Result<()> {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/data/datapackage.json")
            .with_status(404)
            .create_async()
            .await;
        let url = format!("{}/data/datapackage.json", server.url());
        let workdir = tempfile::tempdir()?;

        assert_eq!(
            fetch_previous_release(&url, &test_client(), workdir.path()).await?,
            None
        );
        // Nothing kept, so the next attempt asks again.
        assert_eq!(read_cached(workdir.path())?, None);
        Ok(())
    }
}
