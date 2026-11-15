use crate::pipeline::download;
use crate::pipeline::inputs::Input;
use anyhow::{Context, Ok, Result};
use indicatif::MultiProgress;
use std::path::{Path, PathBuf};

use super::OsmMetadata;

/// Stable "latest" alias: `planet.openstreetmap.org` 302-redirects this
/// to the actual dated filename, currently hosted on a real AWS S3
/// bucket (`osm-planet-eu-central-1.s3.dualstack.eu-central-1.amazonaws.com`,
/// confirmed by hand with `curl -sIL`) -- `reqwest` follows the
/// redirect transparently.
pub(crate) const OSM_PLANET_URL: &str =
    "https://planet.openstreetmap.org/pbf/planet-latest.osm.pbf";

/// Makes the OpenStreetMap planet available as a local input in
/// `workdir`, downloading it from `url` only if nothing in `workdir` can
/// stand in for it. In order:
///
/// 1. The `.pbf` exists, with its `.meta.json` sidecar: reused as is.
/// 2. The `.pbf` exists without a sidecar -- e.g. a regional extract
///    dropped in for cloud testing (`scripts/test-on-hetzner`), rather
///    than a file this function downloaded itself: its metadata is
///    computed (header + full SHA-256) and persisted, no download.
/// 3. The `.pbf` is gone, but its sidecar and the index built from it
///    (`osm-features.index`, see `import_osm`) are both still there --
///    e.g. the PBF was deleted by hand to free its ~90GB once the index
///    was built: only the metadata is returned, and the returned path
///    does not exist. `import_osm` then reuses the index.
/// 4. Otherwise: downloaded (resuming a `.part` file left over from an
///    interrupted attempt, if any, and retrying when the connection
///    breaks, see [`crate::pipeline::download`]), hashed while streaming.
///
/// Async, with all blocking file work in `spawn_blocking`, so it can run
/// concurrently with the other input fetches (see `pipeline::inputs`).
pub async fn fetch_planet(
    url: &str,
    http_client: &reqwest::Client,
    progress: &MultiProgress,
    workdir: &Path,
) -> Result<Input<OsmMetadata>> {
    let pbf_path: PathBuf = workdir.join(super::PLANET_PBF_FILENAME);

    let cached = {
        let (pbf_path, workdir, progress) =
            (pbf_path.clone(), workdir.to_path_buf(), progress.clone());
        tokio::task::spawn_blocking(move || cached_planet(&pbf_path, &workdir, &progress)).await??
    };
    if let Some(metadata) = cached {
        return Ok(Input {
            path: pbf_path,
            metadata,
        });
    }

    let sha256 = download_osm_planet(url, http_client, progress, &pbf_path).await?;
    let metadata = {
        let (pbf_path, workdir) = (pbf_path.clone(), workdir.to_path_buf());
        tokio::task::spawn_blocking(move || super::persist_metadata(&pbf_path, &workdir, sha256))
            .await??
    };
    Ok(Input {
        path: pbf_path,
        metadata,
    })
}

/// Cases 1-3 of [`fetch_planet`]'s doc comment: the planet's metadata, if
/// `workdir` already has what it takes to skip the download; `None` if
/// it has to be downloaded. Blocking.
fn cached_planet(
    pbf_path: &Path,
    workdir: &Path,
    progress: &MultiProgress,
) -> Result<Option<OsmMetadata>> {
    let cached_metadata = super::read_cached_metadata(workdir);
    if pbf_path.exists() {
        if let Result::Ok(metadata) = cached_metadata {
            return Ok(Some(metadata));
        }
        let metadata = super::compute_and_persist_metadata(pbf_path, workdir, progress)?;
        return Ok(Some(metadata));
    }
    if let Result::Ok(metadata) = cached_metadata
        && super::index_exists(workdir)
    {
        return Ok(Some(metadata));
    }
    Ok(None)
}

/// Downloads the planet file over plain HTTPS instead of BitTorrent.
/// BitTorrent made real sense when OSM's planet was mirrored across a
/// loosely-provisioned swarm of volunteer peers; today's distribution
/// is a redirect straight to a well-provisioned cloud object store (see
/// `OSM_PLANET_URL`'s doc comment) that a single HTTP connection can
/// already saturate -- the swarm's resilience bought less than it used
/// to when it ultimately bottoms out at a handful of HTTPS mirrors
/// anyway. Uses `http_client` (the same properly-configured client
/// `import_atp` already uses -- see `main.rs::build_client()`) rather
/// than standing up a separate BitTorrent client with its own,
/// differently-configured HTTP stack for tracker/webseed access.
///
/// Resumable: downloads into `pbf_path` with a `.part` suffix, which
/// [`download::download_file`] resumes with HTTP `Range` requests after
/// a broken connection, or in a later run (S3 advertises
/// `Accept-Ranges: bytes` for this file). Equally, dropping this future
/// mid-download (e.g. because another input's fetch failed, see
/// `pipeline::inputs`) just leaves a `.part` file for the next attempt
/// to resume.
///
/// Returns the SHA-256 of the whole file, as lowercase hex, computed
/// while streaming rather than in a second read pass over ~90GB; on a
/// download resumed from an earlier run, the already-downloaded prefix
/// is hashed first.
async fn download_osm_planet(
    url: &str,
    http_client: &reqwest::Client,
    progress: &MultiProgress,
    pbf_path: &Path,
) -> Result<String> {
    let part_path = pbf_path.with_extension("pbf.part");
    let downloaded = download::download_file(
        http_client,
        progress,
        &download::FileDownload {
            url,
            part_path: &part_path,
            label: "osm.fetch     ",
            hash_label: "osm.hash      ",
            // The planet takes hours; only stalls are timed out.
            timeout: None,
        },
    )
    .await?;
    tokio::fs::rename(&part_path, pbf_path)
        .await
        .with_context(|| {
            format!(
                "Failed to rename {} to {}",
                part_path.display(),
                pbf_path.display()
            )
        })?;
    Ok(downloaded.sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::osm::{PLANET_PBF_FILENAME, read_cached_metadata};
    use indicatif::ProgressDrawTarget;
    use mockito::Server;

    fn test_data_path(filename: &str) -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests");
        path.push("test_data");
        path.push(filename);
        path
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            // Mockito runs on 127.0.0.1; no proxy needed.
            .no_proxy()
            .build()
            .expect("failed to build test client")
    }

    fn hidden_progress() -> MultiProgress {
        MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
    }

    /// A URL nothing listens on: any attempt to download from it fails,
    /// so a test passing with it shows nothing was downloaded.
    const UNREACHABLE_URL: &str = "http://127.0.0.1:9/planet.osm.pbf";

    #[tokio::test]
    async fn test_fetch_planet_computes_metadata_for_preexisting_pbf_without_sidecar() -> Result<()>
    {
        // A .pbf dropped into the workdir without its .meta.json sidecar
        // -- e.g. a regional extract fetched for cloud testing, rather
        // than a file this function downloaded itself -- should have its
        // metadata computed on the spot, not trigger a fresh download or
        // a hard error.
        let workdir = tempfile::tempdir()?;
        let pbf_path = workdir.path().join(PLANET_PBF_FILENAME);
        std::fs::copy(test_data_path("zugerland.osm.pbf"), &pbf_path)?;
        assert!(read_cached_metadata(workdir.path()).is_err());

        let input = fetch_planet(
            UNREACHABLE_URL,
            &test_client(),
            &hidden_progress(),
            workdir.path(),
        )
        .await?;

        assert_eq!(input.path, pbf_path);
        let metadata = input.metadata;
        // Must agree with what mod.rs's own test_read_header reports for
        // the same fixture.
        assert_eq!(
            metadata.replication_timestamp,
            time::UtcDateTime::from_unix_timestamp(1769501462)? // 2026-01-27T08:11:02Z
        );
        assert_eq!(metadata.source, None);
        assert_eq!(metadata.writing_program, Some("osmx".to_owned()));
        assert!(metadata.sha256.is_some());

        // The sidecar should now exist on disk, matching what was returned.
        assert_eq!(read_cached_metadata(workdir.path())?, metadata);

        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_planet_reuses_index_when_pbf_was_deleted() -> Result<()> {
        // The PBF was deleted by hand after the index was built from it:
        // its sidecar and the index stand in for it, no download.
        let workdir = tempfile::tempdir()?;
        std::fs::copy(
            test_data_path("planet-latest.osm.pbf.meta.json"),
            workdir
                .path()
                .join(format!("{PLANET_PBF_FILENAME}.meta.json")),
        )?;
        std::fs::write(workdir.path().join("osm-assemble.strings"), b"")?;
        crate::tables::OsmFeatureIndex::write_stub_for_test(
            &workdir.path().join("osm-features.index"),
        )?;

        let input = fetch_planet(
            UNREACHABLE_URL,
            &test_client(),
            &hidden_progress(),
            workdir.path(),
        )
        .await?;
        assert_eq!(input.metadata, read_cached_metadata(workdir.path())?);
        assert!(!input.path.exists());
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_planet_downloads_without_index() -> Result<()> {
        // A sidecar alone, with neither the PBF nor the index, isn't
        // enough to skip the download: import_osm would have nothing to
        // work from.
        let fixture = std::fs::read(test_data_path("zugerland.osm.pbf"))?;
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/planet.osm.pbf")
            .with_status(200)
            .with_body(&fixture)
            .create_async()
            .await;
        let workdir = tempfile::tempdir()?;
        std::fs::copy(
            test_data_path("planet-latest.osm.pbf.meta.json"),
            workdir
                .path()
                .join(format!("{PLANET_PBF_FILENAME}.meta.json")),
        )?;

        let url = format!("{}/planet.osm.pbf", server.url());
        let input = fetch_planet(&url, &test_client(), &hidden_progress(), workdir.path()).await?;
        mock.assert_async().await;

        assert_eq!(std::fs::read(&input.path)?, fixture);
        let expected_sha256 = crate::utils::hash_file(&input.path, &hidden_progress(), "", false)?;
        assert_eq!(input.metadata.sha256, Some(expected_sha256.sha256));
        assert_eq!(input.metadata.writing_program, Some("osmx".to_owned()));
        assert_eq!(read_cached_metadata(workdir.path())?, input.metadata);
        assert!(!workdir.path().join("planet-latest.osm.pbf.part").exists());
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_planet_resumes_and_hashes_whole_file() -> Result<()> {
        // A `.part` file left over from an interrupted download, with
        // the sidecar that identifies its version, is resumed with a
        // Range request; the hash must still cover the whole file, not
        // just the bytes fetched this time.
        let fixture = std::fs::read(test_data_path("zugerland.osm.pbf"))?;
        let split = fixture.len() / 3;
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/planet.osm.pbf")
            .match_header("range", format!("bytes={split}-").as_str())
            .match_header("if-range", "\"planet-v1\"")
            .with_status(206)
            .with_header(
                "content-range",
                &format!("bytes {split}-{}/{}", fixture.len() - 1, fixture.len()),
            )
            .with_header("etag", "\"planet-v1\"")
            .with_body(&fixture[split..])
            .create_async()
            .await;
        let workdir = tempfile::tempdir()?;
        std::fs::write(
            workdir.path().join("planet-latest.osm.pbf.part"),
            &fixture[..split],
        )?;
        std::fs::write(
            workdir
                .path()
                .join("planet-latest.osm.pbf.part.validators.json"),
            r#"{"etag": "\"planet-v1\""}"#,
        )?;

        let url = format!("{}/planet.osm.pbf", server.url());
        let input = fetch_planet(&url, &test_client(), &hidden_progress(), workdir.path()).await?;
        mock.assert_async().await;

        assert_eq!(std::fs::read(&input.path)?, fixture);
        let expected_sha256 = crate::utils::hash_file(&input.path, &hidden_progress(), "", false)?;
        assert_eq!(input.metadata.sha256, Some(expected_sha256.sha256));
        assert!(
            !workdir
                .path()
                .join("planet-latest.osm.pbf.part.validators.json")
                .exists()
        );
        Ok(())
    }
}
