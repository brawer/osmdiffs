//! Fetches the [name-suggestion-index](https://nsi.guide) (NSI): its
//! `dist/json/nsi.min.json`, which lists the names of brands, operators
//! and networks with their Wikidata IDs and where they apply. Matching
//! uses it to find a brand in the `name` of an OpenStreetMap feature;
//! see [crate::matchers::brand_names].
//!
//! NSI publishes to npm about weekly. We take the latest version that
//! [jsDelivr](https://www.jsdelivr.com/) resolves, download the file from
//! jsDelivr's CDN under that version, and check it against the SHA-256
//! that jsDelivr's package listing gives for it.

use crate::pipeline::download;
use crate::pipeline::inputs::Input;
use anyhow::{Context, Result, anyhow, bail};
use indicatif::MultiProgress;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Asks jsDelivr for NSI's latest version on npm.
pub(crate) const NSI_RESOLVE_URL: &str =
    "https://data.jsdelivr.com/v1/packages/npm/name-suggestion-index/resolved?specifier=latest";

/// jsDelivr's CDN for npm packages.
pub(crate) const NSI_CDN_URL: &str = "https://cdn.jsdelivr.net/npm";

/// The npm package.
const PACKAGE: &str = "name-suggestion-index";

/// The file we fetch, within the package.
const FILE_IN_PACKAGE: &str = "/dist/json/nsi.min.json";

/// Filename, within `workdir`, of the downloaded file.
const FILENAME: &str = "nsi.json";

/// Filename, within `workdir`, of its [NsiMetadata].
const META_FILENAME: &str = "nsi.meta.json";

/// NSI's license, as an SPDX ID; for the provenance BOM.
pub(crate) const LICENSE: &str = "BSD-3-Clause";

/// NSI's license text.
pub(crate) const LICENSE_URL: &str =
    "https://github.com/osmlab/name-suggestion-index/blob/main/LICENSE.md";

/// Where `fetch_nsi` puts NSI in `workdir`.
pub(crate) fn path(workdir: &Path) -> PathBuf {
    workdir.join(FILENAME)
}

/// Provenance of the fetched file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NsiMetadata {
    /// The npm version, such as `8.0.20261101`.
    pub version: String,

    /// The URL we downloaded, with the version in it, so it never
    /// changes.
    pub url: String,

    /// SHA-256 of the downloaded file, as lowercase hex. Checked
    /// against jsDelivr's listing before this metadata is written.
    pub sha256: String,

    /// Size of the downloaded file in bytes.
    pub bytes: u64,
}

/// The base name of the BOM component, for [crate::pipeline::provenance].
pub(crate) const BOM_REF: &str = FILENAME;

/// Makes the latest NSI available as `workdir/nsi.json`. Reused if it
/// already exists (a hard error if its sidecar is then missing, since
/// there would be no way to tell which version it is).
pub(crate) async fn fetch_nsi(
    resolve_url: &str,
    cdn_url: &str,
    client: &Client,
    progress: &MultiProgress,
    workdir: &Path,
) -> Result<Input<NsiMetadata>> {
    let out_path = path(workdir);
    let meta_path = workdir.join(META_FILENAME);
    if tokio::fs::try_exists(&out_path).await? {
        let metadata = read_cached_metadata(workdir).with_context(|| {
            format!(
                "{} exists, but its metadata could not be read from {}",
                out_path.display(),
                meta_path.display()
            )
        })?;
        return Ok(Input {
            path: out_path,
            metadata,
        });
    }

    let timeout = Duration::from_secs(60);
    let resolved: serde_json::Value =
        serde_json::from_slice(&download::get_bytes(client, resolve_url, timeout).await?)
            .with_context(|| format!("Failed to parse {resolve_url}"))?;
    let version = resolved["version"]
        .as_str()
        .ok_or_else(|| anyhow!("{resolve_url} has no version"))?;
    let listing_url = resolved["links"]["self"]
        .as_str()
        .ok_or_else(|| anyhow!("{resolve_url} has no links.self"))?;
    let listing_url = format!("{listing_url}?structure=flat");
    let listing: serde_json::Value =
        serde_json::from_slice(&download::get_bytes(client, &listing_url, timeout).await?)
            .with_context(|| format!("Failed to parse {listing_url}"))?;
    let expected = listed_sha256(&listing, FILE_IN_PACKAGE)
        .ok_or_else(|| anyhow!("{listing_url} has no SHA-256 for {FILE_IN_PACKAGE}"))?;

    let url = format!("{cdn_url}/{PACKAGE}@{version}{FILE_IN_PACKAGE}");
    let mut tmp_path = out_path.clone();
    tmp_path.add_extension("tmp");
    let download::Downloaded { sha256, bytes } = download::download_file(
        client,
        progress,
        &download::FileDownload {
            url: &url,
            part_path: &tmp_path,
            label: "nsi.fetch     ",
            hash_label: "nsi.fetch     ",
            timeout: Some(Duration::from_secs(300)),
        },
    )
    .await?;
    if sha256 != expected {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        bail!("SHA-256 of {url} is {sha256}, but {listing_url} says {expected}");
    }
    let metadata = NsiMetadata {
        version: version.to_string(),
        url,
        sha256,
        bytes,
    };
    // Via a temp file and rename, so a crash mid-write can't leave a
    // truncated sidecar.
    let mut meta_tmp = meta_path.clone();
    meta_tmp.add_extension("tmp");
    tokio::fs::write(&meta_tmp, serde_json::to_string(&metadata)?).await?;
    tokio::fs::rename(&meta_tmp, &meta_path).await?;
    tokio::fs::rename(&tmp_path, &out_path).await?;
    Ok(Input {
        path: out_path,
        metadata,
    })
}

/// The metadata that [fetch_nsi] stored in `workdir`.
pub(crate) fn read_cached_metadata(workdir: &Path) -> Result<NsiMetadata> {
    let path = workdir.join(META_FILENAME);
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_str(&data).with_context(|| format!("Failed to parse {}", path.display()))
}

/// The SHA-256, as lowercase hex, that a jsDelivr package listing
/// (`?structure=flat`) gives for the file `name`. jsDelivr gives it in
/// base64.
fn listed_sha256(listing: &serde_json::Value, name: &str) -> Option<String> {
    let file = listing["files"]
        .as_array()?
        .iter()
        .find(|f| f["name"] == name)?;
    let bytes = decode_base64(file["hash"].as_str()?)?;
    if bytes.len() != 32 {
        return None;
    }
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Decodes standard base64, with or without padding.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes().take_while(|&c| c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indicatif::ProgressDrawTarget;
    use mockito::Server;
    use serde_json::json;

    fn test_client() -> Client {
        Client::builder()
            .no_proxy()
            .build()
            .expect("failed to build test client")
    }

    fn hidden_progress() -> MultiProgress {
        MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
    }

    #[test]
    fn test_decode_base64() {
        assert_eq!(decode_base64("ZGF0YQ==").unwrap(), b"data");
        assert_eq!(decode_base64("ZGF0YQ").unwrap(), b"data");
        assert_eq!(decode_base64("").unwrap(), b"");
        assert!(decode_base64("a*b").is_none());
    }

    /// SHA-256 of b"data", in base64 and hex.
    const DATA_SHA256_B64: &str = "Om6weQ85rIfJTzhWst0sXREOaBFgImGpqSPTuyOtyLc=";
    const DATA_SHA256: &str = "3a6eb0790f39ac87c94f3856b2dd2c5d110e6811602261a9a923d3bb23adc8b7";

    async fn serve(server: &mut Server, hash: &str) {
        let base = server.url();
        server
            .mock("GET", "/resolved")
            .with_body(
                json!({
                    "version": "8.0.20261101",
                    "links": {"self": format!("{base}/packages/name-suggestion-index@8.0.20261101")}
                })
                .to_string(),
            )
            .create_async()
            .await;
        server
            .mock("GET", "/packages/name-suggestion-index@8.0.20261101")
            .match_query(mockito::Matcher::UrlEncoded(
                "structure".into(),
                "flat".into(),
            ))
            .with_body(
                json!({"files": [
                    {"name": "/dist/json/nsi.json", "hash": "AAAA", "size": 1},
                    {"name": "/dist/json/nsi.min.json", "hash": hash, "size": 4}
                ]})
                .to_string(),
            )
            .create_async()
            .await;
        server
            .mock(
                "GET",
                "/npm/name-suggestion-index@8.0.20261101/dist/json/nsi.min.json",
            )
            .with_body("data")
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn test_fetch_nsi() -> Result<()> {
        let mut server = Server::new_async().await;
        serve(&mut server, DATA_SHA256_B64).await;
        let workdir = tempfile::tempdir()?;
        let resolve = format!("{}/resolved", server.url());
        let cdn = format!("{}/npm", server.url());
        let input = fetch_nsi(
            &resolve,
            &cdn,
            &test_client(),
            &hidden_progress(),
            workdir.path(),
        )
        .await?;
        assert_eq!(input.path, workdir.path().join("nsi.json"));
        assert_eq!(std::fs::read(&input.path)?, b"data");
        let expected = NsiMetadata {
            version: "8.0.20261101".to_string(),
            url: format!("{cdn}/name-suggestion-index@8.0.20261101/dist/json/nsi.min.json"),
            sha256: DATA_SHA256.to_string(),
            bytes: 4,
        };
        assert_eq!(input.metadata, expected);
        assert_eq!(read_cached_metadata(workdir.path())?, expected);

        // A second fetch reuses the file, even without a server.
        let again = fetch_nsi(
            "http://127.0.0.1:9/resolved",
            "http://127.0.0.1:9/npm",
            &test_client(),
            &hidden_progress(),
            workdir.path(),
        )
        .await?;
        assert_eq!(again.metadata, expected);
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_nsi_wrong_hash() -> Result<()> {
        let mut server = Server::new_async().await;
        serve(&mut server, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").await;
        let workdir = tempfile::tempdir()?;
        let err = fetch_nsi(
            &format!("{}/resolved", server.url()),
            &format!("{}/npm", server.url()),
            &test_client(),
            &hidden_progress(),
            workdir.path(),
        )
        .await
        .expect_err("hash mismatch");
        assert!(format!("{err:#}").contains("SHA-256"), "{err:#}");
        assert!(!workdir.path().join("nsi.json").exists());
        assert!(!workdir.path().join("nsi.meta.json").exists());
        Ok(())
    }
}
