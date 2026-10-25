//! Fetches one resource of a [Frictionless Data
//! Package](https://datapackage.org/), verified against the SHA-256
//! that the package's `datapackage.json` lists for it. QRank and
//! OSMViews both publish their builds this way.

use crate::make_download_bar;
use crate::pipeline::inputs::Input;
use crate::utils::to_hex;
use anyhow::{Context, Ok, Result, anyhow, bail};
use aws_lc_rs::digest::{Context as DigestContext, SHA256};
use futures_util::StreamExt;
use indicatif::MultiProgress;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use tokio::{fs::File, io::AsyncWriteExt};

/// Which resource of which data package to fetch, and where to put it.
pub(crate) struct PackagedResource {
    /// The resource's `name` in `datapackage.json`, e.g. `"qrank"`.
    pub name: &'static str,

    /// Filename, within `workdir`, of the downloaded resource.
    pub filename: &'static str,

    /// Filename, within `workdir`, of the [`PackagedMetadata`] sidecar.
    pub meta_filename: &'static str,

    /// Progress bar label.
    pub label: &'static str,

    /// Upper bound on the whole download.
    pub timeout: Duration,
}

/// Provenance of a fetched resource, so it can be cited once it feeds
/// into published output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackagedMetadata {
    /// The build date, as given by the data package's `version`, e.g.
    /// "2026-10-11".
    pub version: String,

    /// The dated URL we downloaded, e.g. `qrank-<YYYYMMDD>.csv.gz`.
    /// Dated files never change.
    pub url: String,

    /// The data package descriptor we discovered `url` from.
    pub datapackage_url: String,

    /// The publisher's own provenance BOM for this build, if the data
    /// package lists one.
    pub bom_url: Option<String>,

    /// SHA-256 of the downloaded file, as lowercase hex. Checked against
    /// the data package before this metadata is written.
    pub sha256: String,

    /// Size of the downloaded file in bytes.
    pub bytes: u64,
}

/// Makes the latest build of `resource` available as
/// `workdir/<resource.filename>`. Reused if it already exists (a hard
/// error if its sidecar is then missing, since there would be no way to
/// tell which build it is); otherwise discovered via `datapackage_url`,
/// downloaded, verified and its metadata persisted alongside.
pub(crate) async fn fetch_packaged(
    resource: &PackagedResource,
    datapackage_url: &str,
    client: &Client,
    progress: &MultiProgress,
    workdir: &Path,
) -> Result<Input<PackagedMetadata>> {
    let out_path = workdir.join(resource.filename);
    let meta_json_path = workdir.join(resource.meta_filename);
    if tokio::fs::try_exists(&out_path).await? {
        let metadata = read_meta_json(&meta_json_path).await.with_context(|| {
            format!(
                "{} exists, but its metadata could not be read from {}",
                out_path.display(),
                meta_json_path.display()
            )
        })?;
        return Ok(Input {
            path: out_path,
            metadata,
        });
    }

    let mut metadata = fetch_datapackage(client, datapackage_url, resource.name).await?;
    let mut tmp_path = out_path.clone();
    tmp_path.add_extension("tmp");
    let (sha256, bytes) = download(client, progress, resource, &metadata.url, &tmp_path).await?;
    if sha256 != metadata.sha256 {
        bail!(
            "SHA-256 of {} is {sha256}, but {datapackage_url} says {}",
            metadata.url,
            metadata.sha256
        );
    }
    metadata.bytes = bytes;
    write_meta_json(&metadata, &meta_json_path).await?;
    tokio::fs::rename(&tmp_path, &out_path).await?; // atomic file system operation
    Ok(Input {
        path: out_path,
        metadata,
    })
}

/// Reads `datapackage_url` and returns the metadata of its resource
/// called `name`. `sha256` is the hash the data package claims, still to be
/// checked against the actual download; `bytes` is the claimed size.
async fn fetch_datapackage(
    client: &Client,
    datapackage_url: &str,
    name: &str,
) -> Result<PackagedMetadata> {
    let package: serde_json::Value = client
        .get(datapackage_url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .with_context(|| format!("Failed to GET {datapackage_url}"))?
        .error_for_status()
        .with_context(|| format!("Server returned error for {datapackage_url}"))?
        .json()
        .await
        .with_context(|| format!("Failed to parse {datapackage_url}"))?;
    parse_datapackage(&package, datapackage_url, name)
}

fn parse_datapackage(
    package: &serde_json::Value,
    datapackage_url: &str,
    name: &str,
) -> Result<PackagedMetadata> {
    let version = package["version"]
        .as_str()
        .ok_or_else(|| anyhow!("{datapackage_url} has no version"))?;
    let resources = package["resources"]
        .as_array()
        .ok_or_else(|| anyhow!("{datapackage_url} has no resources"))?;
    let resource = |name: &str| resources.iter().find(|r| r["name"] == name);

    let found =
        resource(name).ok_or_else(|| anyhow!("{datapackage_url} has no {name} resource"))?;
    let path = found["path"]
        .as_str()
        .ok_or_else(|| anyhow!("{name} resource in {datapackage_url} has no path"))?;
    let sha256 = found["hash"]
        .as_str()
        .and_then(|h| h.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow!("{name} resource in {datapackage_url} has no sha256 hash"))?;
    let bom_url = resource("bom")
        .and_then(|r| r["path"].as_str())
        .map(|p| resolve(datapackage_url, p));

    Ok(PackagedMetadata {
        version: version.to_string(),
        url: resolve(datapackage_url, path),
        datapackage_url: datapackage_url.to_string(),
        bom_url,
        sha256: sha256.to_ascii_lowercase(),
        bytes: found["bytes"].as_u64().unwrap_or(0),
    })
}

/// Resolves a data package resource `path` against the descriptor's
/// own URL, as the Frictionless spec says: a bare filename lives next
/// to `datapackage.json`.
fn resolve(datapackage_url: &str, path: &str) -> String {
    if path.starts_with("https://") || path.starts_with("http://") {
        return path.to_string();
    }
    match datapackage_url.rsplit_once('/') {
        Some((base, _)) => format!("{base}/{path}"),
        None => path.to_string(),
    }
}

/// Streams `url` to `dest`, returning its SHA-256 (lowercase hex) and
/// size.
async fn download(
    client: &Client,
    progress: &MultiProgress,
    resource: &PackagedResource,
    url: &str,
    dest: &Path,
) -> Result<(String, u64)> {
    let mut file = File::create(dest)
        .await
        .with_context(|| format!("Failed to create file {}", dest.display()))?;
    let response = client
        .get(url)
        .timeout(resource.timeout)
        .send()
        .await
        .with_context(|| format!("Failed to GET {url}"))?
        .error_for_status()
        .with_context(|| format!("Server returned error for {url}"))?;
    let bar = make_download_bar(progress, resource.label, response.content_length());
    let mut stream = response.bytes_stream();
    let mut hasher = DigestContext::new(&SHA256);
    let mut bytes = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Error reading chunk from response stream")?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .context("Failed to write chunk to disk")?;
        bytes += chunk.len() as u64;
        bar.inc(chunk.len() as u64);
    }
    file.flush()
        .await
        .with_context(|| format!("Failed to flush {}", dest.display()))?;
    bar.finish();
    Ok((to_hex(hasher.finish().as_ref()), bytes))
}

async fn write_meta_json(metadata: &PackagedMetadata, dest: &Path) -> Result<()> {
    // Via a temp file and rename, so a crash mid-write can't leave a
    // truncated sidecar that a restart would fail to parse.
    let mut tmp = dest.to_path_buf();
    tmp.add_extension("tmp");
    tokio::fs::write(&tmp, serde_json::to_string(metadata)?)
        .await
        .with_context(|| format!("Failed to write {}", tmp.display()))?;
    tokio::fs::rename(&tmp, dest)
        .await
        .with_context(|| format!("Failed to rename {} to {}", tmp.display(), dest.display()))?;
    Ok(())
}

async fn read_meta_json(path: &Path) -> Result<PackagedMetadata> {
    let data = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_str(&data).with_context(|| format!("Failed to parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indicatif::ProgressDrawTarget;
    use mockito::Server;
    use serde_json::json;
    use tempfile::TempDir;

    /// SHA-256 of the literal bytes b"data".
    const DATA_SHA256: &str = "3a6eb0790f39ac87c94f3856b2dd2c5d110e6811602261a9a923d3bb23adc8b7";

    fn test_client() -> Client {
        Client::builder()
            // Mockito runs on 127.0.0.1; no proxy needed.
            .no_proxy()
            .build()
            .expect("failed to build test client")
    }

    fn hidden_progress() -> MultiProgress {
        MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
    }

    const QRANK: PackagedResource = PackagedResource {
        name: "qrank",
        filename: "qrank.csv.gz",
        meta_filename: "qrank.meta.json",
        label: "qrank.fetch   ",
        timeout: Duration::from_secs(10),
    };

    fn datapackage(qrank_hash: &str) -> serde_json::Value {
        json!({
            "name": "qrank",
            "version": "2026-10-11",
            "resources": [
                {"name": "qrank", "path": "qrank-20261011.csv.gz",
                 "bytes": 4, "hash": format!("sha256:{qrank_hash}")},
                {"name": "item_signals", "path": "item_signals-20261011.parquet",
                 "bytes": 1, "hash": "sha256:00"},
                {"name": "bom", "path": "qrank-20261011.cdx.json",
                 "bytes": 1, "hash": "sha256:00", "describes": "qrank"},
            ]
        })
    }

    async fn mock_server(qrank_hash: &str) -> Result<(mockito::ServerGuard, Vec<mockito::Mock>)> {
        let mut server = Server::new_async().await;
        let package = server
            .mock("GET", "/download/datapackage.json")
            .with_status(200)
            .with_header("Content-Type", "application/json")
            .with_body(datapackage(qrank_hash).to_string())
            .create_async()
            .await;
        let data = server
            .mock("GET", "/download/qrank-20261011.csv.gz")
            .with_status(200)
            .with_body(b"data")
            .create_async()
            .await;
        Ok((server, vec![package, data]))
    }

    #[tokio::test]
    async fn test_fetch_packaged() -> Result<()> {
        let (server, mocks) = mock_server(DATA_SHA256).await?;
        let url = format!("{}/download/datapackage.json", server.url());
        let workdir = TempDir::new()?;
        let Input { path, metadata } = fetch_packaged(
            &QRANK,
            &url,
            &test_client(),
            &hidden_progress(),
            workdir.path(),
        )
        .await?;
        for m in &mocks {
            m.assert_async().await;
        }
        assert_eq!(path, workdir.path().join("qrank.csv.gz"));
        assert_eq!(tokio::fs::read(&path).await?, b"data");
        assert_eq!(metadata.version, "2026-10-11");
        assert_eq!(
            metadata.url,
            format!("{}/download/qrank-20261011.csv.gz", server.url())
        );
        assert_eq!(
            metadata.bom_url,
            Some(format!("{}/download/qrank-20261011.cdx.json", server.url()))
        );
        assert_eq!(metadata.sha256, DATA_SHA256);
        assert_eq!(metadata.bytes, 4);

        // The sidecar was persisted, and a second call reuses the file
        // without going back to the (now unreachable) server.
        let persisted = read_meta_json(&workdir.path().join(QRANK.meta_filename)).await?;
        assert_eq!(persisted, metadata);
        let again = fetch_packaged(
            &QRANK,
            "http://127.0.0.1:9/datapackage.json",
            &test_client(),
            &hidden_progress(),
            workdir.path(),
        )
        .await?;
        assert_eq!(again.metadata, metadata);
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_packaged_rejects_hash_mismatch() -> Result<()> {
        let (server, _mocks) = mock_server(&"0".repeat(64)).await?;
        let url = format!("{}/download/datapackage.json", server.url());
        let workdir = TempDir::new()?;
        let err = fetch_packaged(
            &QRANK,
            &url,
            &test_client(),
            &hidden_progress(),
            workdir.path(),
        )
        .await
        .expect_err("a corrupt download must not be accepted");
        assert!(format!("{err:#}").contains("SHA-256"), "{err:#}");
        assert!(!workdir.path().join("qrank.csv.gz").exists());
        assert!(!workdir.path().join(QRANK.meta_filename).exists());
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_packaged_requires_sidecar_for_existing_file() -> Result<()> {
        let workdir = TempDir::new()?;
        std::fs::write(workdir.path().join("qrank.csv.gz"), b"data")?;
        let err = fetch_packaged(
            &QRANK,
            "http://127.0.0.1:9/datapackage.json",
            &test_client(),
            &hidden_progress(),
            workdir.path(),
        )
        .await
        .expect_err("a QRank file of unknown origin must not be used");
        assert!(format!("{err:#}").contains("qrank.meta.json"), "{err:#}");
        Ok(())
    }

    #[test]
    fn test_parse_datapackage_errors() {
        let url = "https://example.org/download/datapackage.json";
        let no_qrank = json!({"version": "2026-10-11", "resources": []});
        assert!(parse_datapackage(&no_qrank, url, "qrank").is_err());
        let bad_hash = json!({"version": "2026-10-11", "resources": [
            {"name": "qrank", "path": "q.csv.gz", "hash": "md5:abc"}]});
        assert!(parse_datapackage(&bad_hash, url, "qrank").is_err());
        assert!(parse_datapackage(&json!({}), url, "qrank").is_err());
    }

    #[test]
    fn test_resolve() {
        let url = "https://qrank.toolforge.org/download/datapackage.json";
        assert_eq!(
            resolve(url, "qrank-20261011.csv.gz"),
            "https://qrank.toolforge.org/download/qrank-20261011.csv.gz"
        );
        assert_eq!(
            resolve(url, "https://mirror.example/q.csv.gz"),
            "https://mirror.example/q.csv.gz"
        );
    }
}
