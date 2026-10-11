//! The pipeline's first step, `fetch_inputs`: makes every external input
//! available locally in `workdir`, downloading whatever is missing, all
//! in parallel. Every later step works only on local files, so a run
//! that can't get its inputs fails within minutes instead of hours into
//! processing (see <https://github.com/brawer/osmdiffs/issues/830>).
//!
//! # When a cached input is reused
//!
//! Per input, nothing is downloaded if `workdir` already has the input's
//! metadata sidecar (`*.meta.json`), *and* either
//!
//! - the raw input file itself, or
//! - the artifact derived from it (`alltheplaces.parquet` for
//!   AllThePlaces, `osm-features.index` for OpenStreetMap), so the raw
//!   file was presumably deleted by hand to save disk space once it had
//!   served its purpose.
//!
//! In the second case, the returned [`Input::path`] doesn't exist; the
//! downstream step reuses its derived artifact instead, and provenance
//! only needs the metadata. One exception: a planet `.pbf` without a
//! sidecar (e.g. a regional extract dropped in for testing) is accepted
//! too, with its metadata computed on the spot. See
//! [`super::atp::fetch_atp`] and [`super::osm::fetch_planet`] for each
//! input's exact rule.

use anyhow::Result;
use indicatif::MultiProgress;
use std::path::{Path, PathBuf};

use super::{AtpMetadata, OsmMetadata, atp, osm};

/// One pipeline input, available locally once `fetch_inputs` is done.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Input<M> {
    /// Where the raw input lives in `workdir`. Might not exist if the
    /// artifact derived from it is reused instead; see the module
    /// documentation.
    pub path: PathBuf,

    /// Provenance of the input, as persisted in its metadata sidecar.
    pub metadata: M,
}

/// Every external input of the pipeline.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Inputs {
    pub atp: Input<AtpMetadata>,
    pub osm: Input<OsmMetadata>,
}

/// Where to fetch the pipeline's inputs from. Only ever not the
/// [`Default`] in tests.
pub(crate) struct Sources {
    /// AllThePlaces' `history.json`, which points to the actual dump.
    pub atp_history_url: String,

    /// The OpenStreetMap planet PBF.
    pub osm_planet_url: String,
}

impl Default for Sources {
    fn default() -> Self {
        Sources {
            atp_history_url: atp::ATP_RUN_HISTORY_URL.to_string(),
            osm_planet_url: osm::OSM_PLANET_URL.to_string(),
        }
    }
}

/// Fetches every input in parallel, each logged as its own sub-step
/// (`fetch_inputs.atp`, `fetch_inputs.osm`). If any fetch fails, the
/// others are cancelled right away (a cancelled planet download leaves a
/// `.part` file that the next attempt resumes), and the error is
/// returned. See the module documentation for when a cached input is
/// reused instead of downloaded.
pub(crate) async fn fetch_inputs(
    http_client: &reqwest::Client,
    progress: &MultiProgress,
    workdir: &Path,
    sources: &Sources,
) -> Result<Inputs> {
    let (atp, osm) = futures_util::future::try_join(
        super::run_step_async(
            "fetch_inputs.atp",
            atp::fetch_atp(&sources.atp_history_url, http_client, progress, workdir),
        ),
        super::run_step_async(
            "fetch_inputs.osm",
            osm::fetch_planet(&sources.osm_planet_url, http_client, progress, workdir),
        ),
    )
    .await?;
    Ok(Inputs { atp, osm })
}

#[cfg(test)]
mod tests {
    use super::*;
    use indicatif::ProgressDrawTarget;
    use mockito::Server;
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    fn fixture(name: &str) -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/test_data");
        p.push(name);
        p
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            // Test servers run on 127.0.0.1; no proxy needed.
            .no_proxy()
            .build()
            .expect("failed to build test client")
    }

    fn hidden_progress() -> MultiProgress {
        MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
    }

    /// Sources nothing listens on: any attempt to download from them
    /// fails, so a test passing with these shows nothing was downloaded.
    fn unreachable_sources() -> Sources {
        Sources {
            atp_history_url: "http://127.0.0.1:9/history.json".to_string(),
            osm_planet_url: "http://127.0.0.1:9/planet.osm.pbf".to_string(),
        }
    }

    #[tokio::test]
    async fn test_complete_workdir_downloads_nothing() -> Result<()> {
        let workdir = tempfile::tempdir()?;
        for (src, dst) in [
            ("alltheplaces.zip", "alltheplaces.zip"),
            ("alltheplaces.meta.json", "alltheplaces.meta.json"),
            ("zugerland.osm.pbf", "planet-latest.osm.pbf"),
            (
                "planet-latest.osm.pbf.meta.json",
                "planet-latest.osm.pbf.meta.json",
            ),
        ] {
            symlink(fixture(src), workdir.path().join(dst))?;
        }

        let inputs = fetch_inputs(
            &test_client(),
            &hidden_progress(),
            workdir.path(),
            &unreachable_sources(),
        )
        .await?;

        assert_eq!(inputs.atp.path, workdir.path().join("alltheplaces.zip"));
        assert_eq!(
            inputs.atp.metadata,
            super::super::read_cached_atp_metadata(workdir.path())?
        );
        assert_eq!(
            inputs.osm.path,
            workdir.path().join("planet-latest.osm.pbf")
        );
        assert_eq!(
            inputs.osm.metadata,
            super::super::read_cached_metadata(workdir.path())?
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_derived_artifacts_stand_in_for_deleted_inputs() -> Result<()> {
        // Both raw inputs were deleted after their derived artifacts
        // were built: their sidecars and those artifacts are enough.
        let workdir = tempfile::tempdir()?;
        for name in ["alltheplaces.meta.json", "planet-latest.osm.pbf.meta.json"] {
            symlink(fixture(name), workdir.path().join(name))?;
        }
        for name in [
            "alltheplaces.parquet",
            "osm-features.index",
            "osm-features.index.inverted",
            "osm-assemble.strings",
        ] {
            std::fs::write(workdir.path().join(name), b"")?;
        }

        let inputs = fetch_inputs(
            &test_client(),
            &hidden_progress(),
            workdir.path(),
            &unreachable_sources(),
        )
        .await?;
        assert!(!inputs.atp.path.exists());
        assert!(!inputs.osm.path.exists());
        Ok(())
    }

    #[tokio::test]
    async fn test_failing_fetch_cancels_the_others() -> Result<()> {
        // AllThePlaces fails right away...
        let mut server = Server::new_async().await;
        let atp_history = server
            .mock("GET", "/history.json")
            .with_status(503)
            .create_async()
            .await;

        // ...while the planet server accepts connections but never
        // answers. With no request timeout on the planet download (it
        // takes hours), only cancellation can end that fetch.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let planet_addr = listener.local_addr()?;
        let hang = tokio::spawn(async move {
            let mut connections = Vec::new();
            while let std::result::Result::Ok((socket, _)) = listener.accept().await {
                connections.push(socket);
            }
        });

        let workdir = tempfile::tempdir()?;
        let sources = Sources {
            atp_history_url: format!("{}/history.json", server.url()),
            osm_planet_url: format!("http://{planet_addr}/planet.osm.pbf"),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            fetch_inputs(&test_client(), &hidden_progress(), workdir.path(), &sources),
        )
        .await
        .expect("a failed fetch should cancel the others, not wait for them");
        hang.abort();

        let err = result.expect_err("fetch_inputs should fail if any fetch fails");
        assert!(format!("{err:#}").contains("history.json"), "{err:#}");
        atp_history.assert_async().await;
        assert!(!workdir.path().join("alltheplaces.zip").exists());
        assert!(!workdir.path().join("planet-latest.osm.pbf").exists());
        Ok(())
    }
}
