//! HTTP downloads for `fetch_inputs`, retried with exponential backoff
//! when the network or the server fails, and resumed where they left off
//! instead of started over. Added after the first scheduled production
//! run lost its planet download, and with it the whole weekly run, to a
//! single "connection reset by peer" (see
//! <https://github.com/brawer/osmdiffs/issues/960>).
//!
//! - [`download_file`] streams a (possibly huge) file to disk. If the
//!   connection breaks partway, it asks for the rest with an HTTP
//!   `Range` request, guarded by `If-Range`, so that a file which
//!   changed on the server in the meantime is downloaded again from the
//!   start rather than spliced together from two versions. The same
//!   works across pipeline runs: a `.part` file left on disk is resumed
//!   by the next run.
//! - [`get_bytes`] fetches a small document (`history.json`,
//!   `datapackage.json`, ...) into memory, with the same retries.
//! - [`get_range`] fetches part of a file with a `Range` request, such
//!   as one entry of an older AllThePlaces dump (see `atp::remote_zip`),
//!   with the same retries.
//!
//! What gets retried: failing to connect, a connection that breaks or
//! stalls (see [`STALL_TIMEOUT`]), a body that ends before its
//! announced length, and the HTTP statuses that say "try again later"
//! (408, 429 and any 5xx). Any other HTTP error, such as a 404, fails
//! right away, as does a failure writing to the local disk; so a
//! missing input still fails fast. The exception is a file that the
//! server already served successfully, in this run or in an earlier
//! one whose `.part` file is being resumed: a later 404 or 410 for it,
//! or any 4xx answer to a `Range` request, is most likely a CDN glitch
//! and is retried with a longer backoff (see [`GLITCH_BACKOFF`] and
//! <https://github.com/brawer/osmdiffs/issues/1007>). Every attempt and
//! every failure is logged with its URL.

use crate::make_download_bar;
use crate::utils::to_hex;
use anyhow::{Context, Result, anyhow};
use aws_lc_rs::digest::{Context as DigestContext, SHA256};
use futures_util::StreamExt;
use indicatif::{MultiProgress, ProgressBar};
use reqwest::header::{CONTENT_RANGE, ETAG, HeaderMap, IF_RANGE, LAST_MODIFIED, RANGE};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// How many attempts in a row may fail before a download gives up. An
/// attempt that received some data before failing resets the count, so
/// a multi-hour planet download survives any number of resets as long
/// as it keeps making progress.
pub(crate) const MAX_ATTEMPTS: u32 = 5;

/// How long to wait before the second attempt; doubled for each
/// further one (10s, 20s, 40s, 80s).
const INITIAL_BACKOFF: Duration = Duration::from_secs(10);

/// How long to wait after the first, second, ... failure in a row when
/// the server claimed that a file it had already served was gone, as a
/// Bunny CDN edge briefly did in #1007; the last entry repeats. Longer
/// than [`INITIAL_BACKOFF`]'s schedule, so that the glitch has a chance
/// to clear up within [`MAX_ATTEMPTS`].
const GLITCH_BACKOFF: [Duration; 4] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
];

/// How long a request may wait for the response headers, or for the
/// next chunk of the body, before the attempt counts as failed. Not a
/// deadline on the whole transfer, which for the planet takes hours;
/// see [`FileDownload::timeout`] for that.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// One file to download with [`download_file`].
pub(crate) struct FileDownload<'a> {
    /// What to download.
    pub url: &'a str,

    /// Where the bytes go while downloading. The caller renames it to
    /// its final name once [`download_file`] returns. If it already
    /// exists, along with the sidecar that [`download_file`] keeps next
    /// to it, the download resumes from where it stopped.
    pub part_path: &'a Path,

    /// Progress bar label for the download.
    pub label: &'a str,

    /// Progress bar label for re-hashing the part of a file that an
    /// earlier run already downloaded.
    pub hash_label: &'a str,

    /// Upper bound on each attempt, header to last byte; `None` for no
    /// bound beyond [`STALL_TIMEOUT`].
    pub timeout: Option<Duration>,
}

/// What [`download_file`] downloaded.
#[derive(Debug, PartialEq)]
pub(crate) struct Downloaded {
    /// SHA-256 of the whole file, as lowercase hex.
    pub sha256: String,

    /// Size of the whole file in bytes.
    pub bytes: u64,
}

/// What identifies the version of a file being downloaded, so that a
/// resumed download can tell whether the server still has the same
/// file. Persisted next to the `.part` file (see [`sidecar_path`]) so
/// that a later run can resume too.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
    /// Size of the whole file, if the server told.
    total: Option<u64>,
}

impl Validators {
    fn from_response(response: &Response, total: Option<u64>) -> Self {
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .map(str::to_owned)
        };
        Validators {
            etag: header(ETAG),
            last_modified: header(LAST_MODIFIED),
            total,
        }
    }

    /// The value for an `If-Range` header: a strong ETag if there is
    /// one, otherwise the `Last-Modified` date. A weak ETag
    /// (`W/"..."`) isn't allowed in `If-Range`.
    fn if_range(&self) -> Option<&str> {
        match &self.etag {
            Some(etag) if !etag.starts_with("W/") => Some(etag),
            _ => self.last_modified.as_deref(),
        }
    }

    /// Whether `other`, from the response to a resumed request,
    /// describes the same file. Only compares what both know.
    fn same_file(&self, other: &Validators) -> bool {
        fn agree<T: PartialEq>(a: &Option<T>, b: &Option<T>) -> bool {
            match (a, b) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
        }
        agree(&self.etag, &other.etag)
            && agree(&self.last_modified, &other.last_modified)
            && agree(&self.total, &other.total)
    }
}

/// Where [`download_file`] keeps the [`Validators`] of `part_path`.
fn sidecar_path(part_path: &Path) -> PathBuf {
    let mut path = part_path.to_path_buf();
    path.add_extension("validators.json");
    path
}

/// Why an attempt failed: whether trying again could help.
enum Failure {
    Retry(anyhow::Error),
    /// Worth retrying, but after a longer wait: the server said that a
    /// file it had already served was gone (see [`glitch_status`]).
    Glitch(anyhow::Error),
    Fatal(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(err: anyhow::Error) -> Self {
        Failure::Fatal(err)
    }
}

/// Whether an HTTP error status is worth retrying.
fn retryable_status(status: StatusCode) -> bool {
    status.is_server_error()
        || status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
}

/// Whether an HTTP error status, for a file that the server already
/// served successfully, is more likely a passing glitch than a real
/// answer: a 404 or 410, or any 4xx to a `Range` request (`ranged`).
fn glitch_status(status: StatusCode, ranged: bool) -> bool {
    status == StatusCode::NOT_FOUND
        || status == StatusCode::GONE
        || (ranged && status.is_client_error())
}

/// Classifies a request that failed before any response arrived: a
/// request that couldn't even be built (e.g. a malformed URL) will
/// never work, anything else (DNS, connect, TLS, timeout) might.
fn send_failure(err: reqwest::Error, url: &str) -> Failure {
    let builder = err.is_builder();
    let err = anyhow::Error::new(err).context(format!("Failed to GET {url}"));
    if builder {
        Failure::Fatal(err)
    } else {
        Failure::Retry(err)
    }
}

/// How long to wait after the `failures`-th failure in a row.
fn backoff(failures: u32) -> Duration {
    if cfg!(test) {
        return Duration::from_millis(1);
    }
    INITIAL_BACKOFF * 2u32.pow(failures.saturating_sub(1))
}

/// Like [`backoff`], but for a [`Failure::Glitch`].
fn glitch_backoff(failures: u32) -> Duration {
    if cfg!(test) {
        return Duration::from_millis(1);
    }
    let i = (failures.saturating_sub(1) as usize).min(GLITCH_BACKOFF.len() - 1);
    GLITCH_BACKOFF[i]
}

/// Downloads `download.url` to `download.part_path`, retrying and
/// resuming as described in the module documentation. Returns the
/// SHA-256 and size of the whole file, hashed while streaming; on a
/// download resumed from an earlier run, the part already on disk is
/// hashed first. On success the sidecar is removed again, and the
/// caller is expected to rename the part file to its final name.
///
/// Dropping the returned future mid-download (e.g. because another
/// input's fetch failed, see `pipeline::inputs`) leaves the part file
/// and its sidecar behind, for the next run to resume.
pub(crate) async fn download_file(
    client: &Client,
    progress: &MultiProgress,
    download: &FileDownload<'_>,
) -> Result<Downloaded> {
    let mut state = DownloadState::open(download).await?;
    let mut failures = 0;
    loop {
        let before = state.downloaded;
        let (err, glitch) = match state.attempt(client, progress, download).await {
            Ok(()) => break,
            Err(Failure::Fatal(err)) => return Err(err),
            Err(Failure::Retry(err)) => (err, false),
            Err(Failure::Glitch(err)) => (err, true),
        };
        if state.downloaded > before {
            failures = 0;
        }
        failures += 1;
        if failures >= MAX_ATTEMPTS {
            return Err(err.context(format!(
                "Giving up on {} after {failures} failed attempts in a row, \
                 with {} bytes downloaded",
                state.url, state.downloaded
            )));
        }
        let wait = if glitch {
            glitch_backoff(failures)
        } else {
            backoff(failures)
        };
        log::warn!(
            url = state.url.as_str(),
            bytes_downloaded = state.downloaded,
            attempt = failures;
            "Download of {} failed after {} bytes, retrying in {wait:?}: {err:#}",
            state.url,
            state.downloaded
        );
        tokio::time::sleep(wait).await;
    }
    state
        .file
        .flush()
        .await
        .with_context(|| format!("Failed to flush {}", download.part_path.display()))?;
    if let Some(bar) = &state.bar {
        bar.finish();
    }
    // Hashed in full by now: either fresh, or resumed after hashing the
    // part on disk (see `attempt`).
    let hasher = state
        .hasher
        .take()
        .ok_or_else(|| anyhow!("{} was never hashed", download.part_path.display()))?;
    remove_if_exists(&sidecar_path(download.part_path)).await?;
    Ok(Downloaded {
        sha256: to_hex(hasher.finish().as_ref()),
        bytes: state.downloaded,
    })
}

/// Where a [`download_file`] stands, kept across its attempts.
struct DownloadState {
    /// The part file, opened for appending.
    file: tokio::fs::File,

    /// How many bytes of the file are in `file`.
    downloaded: u64,

    /// SHA-256 state over the `downloaded` bytes. `None` while resuming
    /// a part file from an earlier run whose bytes haven't been hashed
    /// yet: that only happens once the server agreed to resume.
    hasher: Option<DigestContext>,

    /// The version of the file being downloaded; `None` until a server
    /// response said.
    validators: Option<Validators>,

    /// Whether the server has already served this file successfully,
    /// in this run or in the earlier one that left the part file
    /// behind. If so, a later 404 or the like is retried as a
    /// [`Failure::Glitch`] instead of failing the download.
    served: bool,

    /// What to request next: `FileDownload::url` at first, then the URL
    /// the server redirected it to, so that a retry continues the same
    /// file even if the redirect would by now point somewhere else
    /// (e.g. `planet-latest` moving on to a newer planet).
    url: String,

    bar: Option<ProgressBar>,
}

impl DownloadState {
    /// Picks up the part file and its sidecar left by an earlier run, if
    /// both are there; otherwise starts from an empty part file.
    async fn open(download: &FileDownload<'_>) -> Result<Self> {
        let sidecar = sidecar_path(download.part_path);
        let validators = match tokio::fs::read(&sidecar).await {
            Ok(data) => serde_json::from_slice::<Validators>(&data).ok(),
            Err(_) => None,
        };
        let existing = tokio::fs::metadata(download.part_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let resume = validators.is_some() && existing > 0;
        // The sidecar is written only once a `200 OK` arrived.
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(download.part_path)
            .await
            .with_context(|| format!("Failed to open {}", download.part_path.display()))?;
        let mut state = DownloadState {
            file,
            downloaded: existing,
            hasher: None,
            validators,
            served: resume,
            url: download.url.to_owned(),
            bar: None,
        };
        if !resume {
            // Without a sidecar there's no telling which version of
            // the file the part holds, so it can't safely be resumed.
            state.restart(download.part_path).await?;
        }
        Ok(state)
    }

    /// Throws away whatever was downloaded so far.
    async fn restart(&mut self, part_path: &Path) -> Result<()> {
        self.file
            .set_len(0)
            .await
            .with_context(|| format!("Failed to truncate {}", part_path.display()))?;
        self.downloaded = 0;
        self.hasher = Some(DigestContext::new(&SHA256));
        self.validators = None;
        remove_if_exists(&sidecar_path(part_path)).await?;
        if let Some(bar) = &self.bar {
            bar.set_position(0);
        }
        Ok(())
    }

    /// One request, streaming its body to the part file until it ends
    /// or fails.
    async fn attempt(
        &mut self,
        client: &Client,
        progress: &MultiProgress,
        download: &FileDownload<'_>,
    ) -> Result<(), Failure> {
        let response = self.request(client, download).await?;
        let total = self.validators.as_ref().and_then(|v| v.total);
        let bar = self.bar.get_or_insert_with(|| {
            let bar = make_download_bar(progress, download.label, total);
            bar.set_position(self.downloaded);
            bar
        });
        if let Some(total) = total {
            bar.set_length(total);
        }
        if self.hasher.is_none() {
            // Resuming an earlier run's part file: hash what's already
            // there, so the result covers the whole file.
            let (part_path, len, progress, label) = (
                download.part_path.to_path_buf(),
                self.downloaded,
                progress.clone(),
                download.hash_label.to_owned(),
            );
            let hasher = tokio::task::spawn_blocking(move || {
                hash_prefix(&part_path, len, &progress, &label)
            })
            .await
            .context("hashing task failed")??;
            self.hasher = Some(hasher);
        }
        let hasher = self.hasher.as_mut().expect("hasher set above");

        let mut stream = response.bytes_stream();
        loop {
            let next = tokio::time::timeout(STALL_TIMEOUT, stream.next())
                .await
                .map_err(|_| {
                    Failure::Retry(anyhow!(
                        "No data received from {} for {STALL_TIMEOUT:?}",
                        self.url
                    ))
                })?;
            let Some(chunk) = next else {
                break;
            };
            let chunk = chunk
                .with_context(|| {
                    format!("Error reading chunk from response stream of {}", self.url)
                })
                .map_err(Failure::Retry)?;
            hasher.update(&chunk);
            self.file
                .write_all(&chunk)
                .await
                .with_context(|| format!("Failed to write to {}", download.part_path.display()))?;
            self.downloaded += chunk.len() as u64;
            bar.inc(chunk.len() as u64);
        }
        if let Some(total) = total
            && self.downloaded != total
        {
            return Err(Failure::Retry(anyhow!(
                "Response from {} ended after {} of {total} bytes",
                self.url,
                self.downloaded
            )));
        }
        Ok(())
    }

    /// Sends the request for the next attempt: for the rest of the file
    /// if something was downloaded already, otherwise for all of it.
    /// Returns a response whose body continues exactly where the part
    /// file ends, starting over if the server can't or won't resume.
    async fn request(
        &mut self,
        client: &Client,
        download: &FileDownload<'_>,
    ) -> Result<Response, Failure> {
        loop {
            let resume_from = self.downloaded;
            let mut request = client.get(&self.url);
            if let Some(timeout) = download.timeout {
                request = request.timeout(timeout);
            }
            if resume_from > 0 {
                request = request.header(RANGE, format!("bytes={resume_from}-"));
                if let Some(if_range) = self.validators.as_ref().and_then(|v| v.if_range()) {
                    request = request.header(IF_RANGE, if_range);
                }
            }
            log::info!(
                url = self.url.as_str(),
                resume_from = resume_from;
                "Downloading {} from byte {resume_from}",
                self.url
            );
            let response = tokio::time::timeout(STALL_TIMEOUT, request.send())
                .await
                .map_err(|_| {
                    Failure::Retry(anyhow!(
                        "No response from {} within {STALL_TIMEOUT:?}",
                        self.url
                    ))
                })?
                .map_err(|err| send_failure(err, &self.url))?;
            let status = response.status();

            if resume_from > 0 {
                let restart_because = match status {
                    StatusCode::PARTIAL_CONTENT => {
                        let range = parse_content_range(response.headers());
                        let found = Validators::from_response(&response, range.and_then(|r| r.1));
                        match range {
                            Some((start, _)) if start != resume_from => {
                                Some(format!("it resumed at byte {start}"))
                            }
                            None => Some("its Content-Range was missing or invalid".to_owned()),
                            Some(_) => match &self.validators {
                                Some(known) if !known.same_file(&found) => {
                                    Some("the file changed in the meantime".to_owned())
                                }
                                _ => None,
                            },
                        }
                    }
                    StatusCode::OK => Some("it sent the whole file".to_owned()),
                    StatusCode::RANGE_NOT_SATISFIABLE => {
                        Some("it said the range was not satisfiable".to_owned())
                    }
                    _ => None,
                };
                if let Some(reason) = restart_because {
                    log::warn!(
                        url = self.url.as_str(),
                        status = status.as_u16();
                        "Could not resume download of {} at byte {resume_from} \
                         because {reason}; starting over",
                        self.url
                    );
                    self.restart(download.part_path).await?;
                    if status == StatusCode::OK {
                        self.begin(&response, download.part_path).await?;
                        return Ok(response);
                    }
                    continue;
                }
                if status == StatusCode::PARTIAL_CONTENT {
                    // Learn the size, if the response that started the
                    // download didn't tell.
                    let total = parse_content_range(response.headers()).and_then(|r| r.1);
                    if let Some(validators) = &mut self.validators
                        && validators.total.is_none()
                    {
                        validators.total = total;
                    }
                    self.url = response.url().to_string();
                    self.served = true;
                    return Ok(response);
                }
            }

            if status == StatusCode::OK && resume_from == 0 {
                self.begin(&response, download.part_path).await?;
                return Ok(response);
            }
            let err = anyhow!("{} returned HTTP status {status}", self.url);
            return Err(if retryable_status(status) {
                Failure::Retry(err)
            } else if self.served && glitch_status(status, resume_from > 0) {
                Failure::Glitch(err.context(format!(
                    "{} was served before, so this is likely a temporary glitch",
                    self.url
                )))
            } else {
                Failure::Fatal(err)
            });
        }
    }

    /// Records the version of the file that a full (`200 OK`) response
    /// is about to send, persisting it for a later run to resume.
    async fn begin(&mut self, response: &Response, part_path: &Path) -> Result<()> {
        let validators = Validators::from_response(response, response.content_length());
        let sidecar = sidecar_path(part_path);
        tokio::fs::write(&sidecar, serde_json::to_vec(&validators)?)
            .await
            .with_context(|| format!("Failed to write {}", sidecar.display()))?;
        self.validators = Some(validators);
        self.url = response.url().to_string();
        self.served = true;
        Ok(())
    }
}

/// The first byte and the full length (if known) from a `206` response's
/// `Content-Range: bytes <first>-<last>/<length or *>` header.
fn parse_content_range(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let value = headers.get(CONTENT_RANGE)?.to_str().ok()?;
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (first, _last) = range.split_once('-')?;
    let first = first.trim().parse().ok()?;
    let total = match total.trim() {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    Some((first, total))
}

async fn remove_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

/// SHA-256 state after hashing the first `len` bytes of `path`, the
/// part of a file that an earlier run already downloaded. Blocking.
fn hash_prefix(
    path: &Path,
    len: u64,
    progress: &MultiProgress,
    label: &str,
) -> Result<DigestContext> {
    let file = std::fs::File::open(path).with_context(|| format!("could not open `{path:?}`"))?;
    let mut reader = file.take(len);
    let bar = make_download_bar(progress, label, Some(len));
    let mut hasher = DigestContext::new(&SHA256);
    let mut buf = vec![0u8; 8 * 1024 * 1024]; // 8 MiB
    let mut total: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("could not read `{path:?}`"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
        bar.inc(n as u64);
    }
    bar.finish();
    if total != len {
        return Err(anyhow!(
            "`{path:?}` shrank while resuming its download: expected {len} bytes, read {total}"
        ));
    }
    Ok(hasher)
}

/// Fetches a small document at `url` into memory, retrying as described
/// in the module documentation. `timeout` bounds each attempt.
pub(crate) async fn get_bytes(client: &Client, url: &str, timeout: Duration) -> Result<Vec<u8>> {
    Ok(get(client, url, None, timeout).await?.body)
}

/// Part of a file, as [`get_range`] fetched it.
#[derive(Debug, PartialEq)]
pub(crate) struct Ranged {
    /// Where [`Self::body`] starts in the file.
    pub first: u64,

    /// The length of the whole file.
    pub total: u64,

    pub body: Vec<u8>,
}

/// Fetches part of the file at `url` with an HTTP `Range` request,
/// retrying like [`get_bytes`]. `range` is the header's value, such as
/// `bytes=100-199`, or `bytes=-100` for the last 100 bytes. A server
/// that answers with anything but `206 Partial Content` and a
/// `Content-Range` with the file's length doesn't get its body read,
/// since that might be the whole file, gigabytes of it: a `200 OK` is
/// retried, as the AllThePlaces CDN sends one now and then, and a
/// missing or bad `Content-Range` fails right away.
pub(crate) async fn get_range(
    client: &Client,
    url: &str,
    range: &str,
    timeout: Duration,
) -> Result<Ranged> {
    let got = get(client, url, Some(range), timeout).await?;
    let (first, total) = got
        .range
        .ok_or_else(|| anyhow!("{url} gave no Content-Range for {range}"))?;
    let total = total.ok_or_else(|| anyhow!("{url} gave no file length for {range}"))?;
    Ok(Ranged {
        first,
        total,
        body: got.body,
    })
}

/// What [`get`] received.
struct Got {
    body: Vec<u8>,
    /// From the `Content-Range` header, for a ranged request.
    range: Option<(u64, Option<u64>)>,
}

/// Fetches `url` into memory, or with `range`, the part that a `Range`
/// header of that value asks for; see [`get_bytes`] and [`get_range`].
async fn get(client: &Client, url: &str, range: Option<&str>, timeout: Duration) -> Result<Got> {
    let mut failures = 0;
    loop {
        let attempt = async {
            let mut request = client.get(url).timeout(timeout);
            if let Some(range) = range {
                request = request.header(RANGE, range);
            }
            let response = request.send().await.map_err(|err| send_failure(err, url))?;
            let status = response.status();
            if !status.is_success() {
                let err = anyhow!("{url} returned HTTP status {status}");
                return Err(if retryable_status(status) {
                    Failure::Retry(err)
                } else {
                    Failure::Fatal(err)
                });
            }
            let content_range = match range {
                None => None,
                Some(range) => {
                    // Retried: the AllThePlaces CDN now and then answers
                    // a range request with the whole file. Dropping the
                    // response unread closes the connection.
                    if status != StatusCode::PARTIAL_CONTENT {
                        return Err(Failure::Retry(anyhow!(
                            "{url} answered {range} with HTTP status {status}, not 206"
                        )));
                    }
                    Some(parse_content_range(response.headers()).ok_or_else(|| {
                        Failure::Fatal(anyhow!("{url} gave no valid Content-Range for {range}"))
                    })?)
                }
            };
            let body = response
                .bytes()
                .await
                .with_context(|| format!("Failed to read response from {url}"))
                .map_err(Failure::Retry)?;
            Ok(Got {
                body: body.to_vec(),
                range: content_range,
            })
        };
        match attempt.await {
            Ok(got) => return Ok(got),
            Err(Failure::Fatal(err)) => return Err(err),
            Err(Failure::Retry(err) | Failure::Glitch(err)) => {
                failures += 1;
                if failures >= MAX_ATTEMPTS {
                    return Err(err.context(format!(
                        "Giving up on {url} after {failures} failed attempts"
                    )));
                }
                let wait = backoff(failures);
                log::warn!(
                    url = url,
                    attempt = failures;
                    "Fetching {url} failed, retrying in {wait:?}: {err:#}"
                );
                tokio::time::sleep(wait).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indicatif::ProgressDrawTarget;
    use mockito::{Matcher, Server};
    use tokio::io::AsyncReadExt;

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

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn sha256(data: &[u8]) -> String {
        crate::utils::sha256_hex(data)
    }

    fn file_download<'a>(url: &'a str, part_path: &'a Path) -> FileDownload<'a> {
        FileDownload {
            url,
            part_path,
            label: "test",
            hash_label: "test.hash",
            timeout: None,
        }
    }

    /// A server that answers the first request with response headers
    /// for `full.len()` bytes, but drops the connection after sending
    /// only the first `cut` of them -- like the reset in #960. The
    /// second request is expected to ask for the rest with `Range` and
    /// `If-Range`, and gets it as a `206` with `etag_on_resume`. If that
    /// isn't the original ETag, a third request gets the whole file
    /// again. Returns the server's URL and the requests it saw.
    async fn flaky_server(
        full: Vec<u8>,
        cut: usize,
        etag_on_resume: &'static str,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let connections = if etag_on_resume == "\"v1\"" { 2 } else { 3 };
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for i in 0..connections {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                requests.push(String::from_utf8_lossy(&buf[..n]).to_lowercase());
                let (head, body) = match i {
                    0 => (
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\netag: \"v1\"\r\n\r\n",
                            full.len()
                        ),
                        &full[..cut], // connection reset mid-body
                    ),
                    1 => (
                        format!(
                            "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\n\
                             content-range: bytes {cut}-{}/{}\r\netag: {etag_on_resume}\r\n\r\n",
                            full.len() - cut,
                            full.len() - 1,
                            full.len()
                        ),
                        &full[cut..],
                    ),
                    _ => (
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\netag: {etag_on_resume}\r\n\r\n",
                            full.len()
                        ),
                        &full[..],
                    ),
                };
                // The client may hang up early, e.g. on a 206 it rejects.
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body).await;
                let _ = socket.flush().await;
            }
            requests
        });
        (url, handle)
    }

    #[tokio::test]
    async fn test_download_resumes_after_connection_reset() -> Result<()> {
        let full = body(100_000);
        let cut = 30_000;
        let (url, server) = flaky_server(full.clone(), cut, "\"v1\"").await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");

        let got = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await?;

        assert_eq!(got.bytes, full.len() as u64);
        assert_eq!(got.sha256, sha256(&full));
        assert_eq!(std::fs::read(&part)?, full);
        assert!(!sidecar_path(&part).exists());
        let requests = server.await?;
        assert!(!requests[0].contains("range:"), "{}", requests[0]);
        assert!(
            requests[1].contains(&format!("range: bytes={cut}-")),
            "{}",
            requests[1]
        );
        assert!(requests[1].contains("if-range: \"v1\""), "{}", requests[1]);
        Ok(())
    }

    #[tokio::test]
    async fn test_download_resumes_part_file_from_earlier_run() -> Result<()> {
        let full = body(50_000);
        let split = 20_000;
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/file")
            .match_header("range", format!("bytes={split}-").as_str())
            .match_header("if-range", "\"v1\"")
            .with_status(206)
            .with_header(
                "content-range",
                &format!("bytes {split}-{}/{}", full.len() - 1, full.len()),
            )
            .with_header("etag", "\"v1\"")
            .with_body(&full[split..])
            .create_async()
            .await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");
        std::fs::write(&part, &full[..split])?;
        std::fs::write(
            sidecar_path(&part),
            serde_json::to_vec(&Validators {
                etag: Some("\"v1\"".to_owned()),
                last_modified: None,
                total: Some(full.len() as u64),
            })?,
        )?;

        let url = format!("{}/file", server.url());
        let got = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await?;
        mock.assert_async().await;

        assert_eq!(got.sha256, sha256(&full));
        assert_eq!(std::fs::read(&part)?, full);
        assert!(!sidecar_path(&part).exists());
        Ok(())
    }

    #[tokio::test]
    async fn test_download_starts_over_if_file_changed() -> Result<()> {
        // The server answers If-Range with the whole, new file.
        let new = body(40_000);
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/file")
            .with_status(200)
            .with_header("etag", "\"v2\"")
            .with_body(&new)
            .create_async()
            .await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");
        std::fs::write(&part, b"stale bytes of the old version")?;
        std::fs::write(
            sidecar_path(&part),
            serde_json::to_vec(&Validators {
                etag: Some("\"v1\"".to_owned()),
                ..Validators::default()
            })?,
        )?;

        let url = format!("{}/file", server.url());
        let got = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await?;
        mock.assert_async().await;

        assert_eq!(got.sha256, sha256(&new));
        assert_eq!(std::fs::read(&part)?, new);
        Ok(())
    }

    #[tokio::test]
    async fn test_download_starts_over_if_resumed_response_has_other_etag() -> Result<()> {
        // A server that ignores If-Range and resumes a different version
        // of the file: its 206 doesn't fit the bytes we have, so the
        // download must start over rather than splice the two together.
        let full = body(60_000);
        let (url, server) = flaky_server(full.clone(), 25_000, "\"v2\"").await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");

        let got = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await?;

        assert_eq!(got.sha256, sha256(&full));
        assert_eq!(std::fs::read(&part)?, full);
        let requests = server.await?;
        assert_eq!(requests.len(), 3);
        assert!(!requests[2].contains("range:"), "{}", requests[2]);
        Ok(())
    }

    #[tokio::test]
    async fn test_download_ignores_part_file_without_sidecar() -> Result<()> {
        let full = body(10_000);
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/file")
            .match_header("range", Matcher::Missing)
            .with_status(200)
            .with_body(&full)
            .create_async()
            .await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");
        std::fs::write(&part, b"who knows what this is")?;

        let url = format!("{}/file", server.url());
        let got = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await?;
        mock.assert_async().await;
        assert_eq!(std::fs::read(&part)?, full);
        assert_eq!(got.bytes, full.len() as u64);
        Ok(())
    }

    #[tokio::test]
    async fn test_download_without_content_length() -> Result<()> {
        // Chunked, so no Content-Length: shown as a spinner, not a bar.
        let full = body(20_000);
        let mut server = Server::new_async().await;
        let chunk = full.clone();
        let mock = server
            .mock("GET", "/file")
            .with_status(200)
            .with_chunked_body(move |w| w.write_all(&chunk))
            .create_async()
            .await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");

        let url = format!("{}/file", server.url());
        let got = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await?;
        mock.assert_async().await;
        assert_eq!(got.sha256, sha256(&full));
        Ok(())
    }

    #[tokio::test]
    async fn test_download_retries_server_errors_then_gives_up() -> Result<()> {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/file")
            .with_status(503)
            .expect(MAX_ATTEMPTS as usize)
            .create_async()
            .await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");

        let url = format!("{}/file", server.url());
        let err = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await
        .expect_err("should give up");
        mock.assert_async().await;
        let msg = format!("{err:#}");
        assert!(msg.contains(&url) && msg.contains("503"), "{msg}");
        Ok(())
    }

    #[tokio::test]
    async fn test_download_does_not_retry_not_found() -> Result<()> {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/file")
            .with_status(404)
            .expect(1)
            .create_async()
            .await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");

        let url = format!("{}/file", server.url());
        let err = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await
        .expect_err("404 should fail");
        mock.assert_async().await;
        assert!(format!("{err:#}").contains("404"), "{err:#}");
        Ok(())
    }

    /// A server that answers its `i`-th connection with `responses[i]`,
    /// a response head (status line and headers) and a body, which may
    /// be shorter than the head announces, then hangs up. Returns the
    /// server's URL and the requests it saw.
    async fn scripted_server(
        responses: Vec<(String, Vec<u8>)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (head, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                requests.push(String::from_utf8_lossy(&buf[..n]).to_lowercase());
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.flush().await;
            }
            requests
        });
        (url, handle)
    }

    fn ok_head(len: usize) -> String {
        format!("HTTP/1.1 200 OK\r\ncontent-length: {len}\r\netag: \"v1\"\r\n\r\n")
    }

    fn partial_head(from: usize, len: usize) -> String {
        format!(
            "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\n\
             content-range: bytes {from}-{}/{len}\r\netag: \"v1\"\r\n\r\n",
            len - from,
            len - 1
        )
    }

    fn not_found_head() -> String {
        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".to_owned()
    }

    #[tokio::test]
    async fn test_download_retries_not_found_after_partial_download() -> Result<()> {
        // #1007: the connection broke off, and the CDN briefly answered
        // the resumed request with a 404 for a file it had just served.
        let full = body(80_000);
        let cut = 30_000;
        let (url, server) = scripted_server(vec![
            (ok_head(full.len()), full[..cut].to_vec()),
            (not_found_head(), Vec::new()),
            (not_found_head(), Vec::new()),
            (partial_head(cut, full.len()), full[cut..].to_vec()),
        ])
        .await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");

        let got = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await?;

        assert_eq!(got.sha256, sha256(&full));
        assert_eq!(std::fs::read(&part)?, full);
        let requests = server.await?;
        assert_eq!(requests.len(), 4);
        assert!(
            requests[3].contains(&format!("range: bytes={cut}-")),
            "{}",
            requests[3]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_download_retries_not_found_when_resuming_earlier_run() -> Result<()> {
        // The 200 that started the download came in an earlier run,
        // which left the part file and its sidecar behind.
        let full = body(50_000);
        let split = 20_000;
        let (url, server) = scripted_server(vec![
            (not_found_head(), Vec::new()),
            (partial_head(split, full.len()), full[split..].to_vec()),
        ])
        .await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");
        std::fs::write(&part, &full[..split])?;
        std::fs::write(
            sidecar_path(&part),
            serde_json::to_vec(&Validators {
                etag: Some("\"v1\"".to_owned()),
                last_modified: None,
                total: Some(full.len() as u64),
            })?,
        )?;

        let got = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await?;

        assert_eq!(got.sha256, sha256(&full));
        assert_eq!(server.await?.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_download_gives_up_on_lasting_not_found_after_partial_download() -> Result<()> {
        // The first failure (the broken connection) made progress, so
        // only the 404s after it count towards MAX_ATTEMPTS.
        let full = body(10_000);
        let mut responses = vec![(ok_head(full.len()), full[..4_000].to_vec())];
        for _ in 1..MAX_ATTEMPTS {
            responses.push((not_found_head(), Vec::new()));
        }
        let (url, server) = scripted_server(responses).await;
        let dir = tempfile::tempdir()?;
        let part = dir.path().join("file.part");

        let err = download_file(
            &test_client(),
            &hidden_progress(),
            &file_download(&url, &part),
        )
        .await
        .expect_err("should give up");
        let msg = format!("{err:#}");
        assert!(msg.contains("Giving up") && msg.contains("404"), "{msg}");
        assert_eq!(server.await?.len(), MAX_ATTEMPTS as usize);
        Ok(())
    }

    #[tokio::test]
    async fn test_get_bytes_retries() -> Result<()> {
        let mut server = Server::new_async().await;
        let failing = server
            .mock("GET", "/doc.json")
            .with_status(502)
            .expect(2)
            .create_async()
            .await;
        let url = format!("{}/doc.json", server.url());
        let client = test_client();
        let fetch = tokio::spawn({
            let url = url.clone();
            async move { get_bytes(&client, &url, Duration::from_secs(10)).await }
        });
        // Let the first two attempts fail, then serve the document.
        while !failing.matched_async().await {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let _ok = server
            .mock("GET", "/doc.json")
            .with_status(200)
            .with_body("{}")
            .create_async()
            .await;
        assert_eq!(fetch.await??, b"{}");
        Ok(())
    }

    #[tokio::test]
    async fn test_get_range() -> Result<()> {
        let mut server = Server::new_async().await;
        let _tail = server
            .mock("GET", "/output.zip")
            .match_header("range", "bytes=-3")
            .with_status(206)
            .with_header("content-range", "bytes 7-9/10")
            .with_body("789")
            .create_async()
            .await;
        let _ignored = server
            .mock("GET", "/output.zip")
            .match_header("range", "bytes=0-1")
            .with_status(200)
            .with_body("0123456789")
            .create_async()
            .await;
        let _no_length = server
            .mock("GET", "/output.zip")
            .match_header("range", "bytes=2-3")
            .with_status(206)
            .with_header("content-range", "bytes 2-3/*")
            .with_body("23")
            .create_async()
            .await;
        let url = format!("{}/output.zip", server.url());
        let client = test_client();
        let timeout = Duration::from_secs(10);
        assert_eq!(
            get_range(&client, &url, "bytes=-3", timeout).await?,
            Ranged {
                first: 7,
                total: 10,
                body: b"789".to_vec()
            }
        );
        // A server that ignores the range would send the whole file.
        let err = get_range(&client, &url, "bytes=0-1", timeout)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("not 206"), "{err:#}");
        assert!(err.to_string().contains("Giving up"), "{err:#}");
        let err = get_range(&client, &url, "bytes=2-3", timeout)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no file length"), "{err:#}");
        Ok(())
    }

    #[test]
    fn test_sha256_matches_nist_test_vector() {
        // Exercises the exact same aws_lc_rs::digest call path
        // download_file() uses (DigestContext::new(&SHA256), .update(),
        // .finish(), to_hex()), against SHA-256's standard "abc" test
        // vector.
        let mut hasher = DigestContext::new(&SHA256);
        hasher.update(b"abc");
        // Expected SHA-256("abc"), as per
        // https://csrc.nist.gov/CSRC/media/Projects/Cryptographic-Standards-and-Guidelines/documents/examples/SHA256.pdf
        assert_eq!(
            to_hex(hasher.finish().as_ref()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_parse_content_range() {
        let mut headers = HeaderMap::new();
        assert_eq!(parse_content_range(&headers), None);
        headers.insert(CONTENT_RANGE, "bytes 100-199/200".parse().unwrap());
        assert_eq!(parse_content_range(&headers), Some((100, Some(200))));
        headers.insert(CONTENT_RANGE, "bytes 100-199/*".parse().unwrap());
        assert_eq!(parse_content_range(&headers), Some((100, None)));
        headers.insert(CONTENT_RANGE, "items 1-2/3".parse().unwrap());
        assert_eq!(parse_content_range(&headers), None);
    }

    #[test]
    fn test_validators() {
        let strong = Validators {
            etag: Some("\"a\"".to_owned()),
            last_modified: Some("Sun, 15 Nov 2026 07:00:00 GMT".to_owned()),
            total: Some(10),
        };
        assert_eq!(strong.if_range(), Some("\"a\""));
        let weak = Validators {
            etag: Some("W/\"a\"".to_owned()),
            ..strong.clone()
        };
        assert_eq!(weak.if_range(), Some("Sun, 15 Nov 2026 07:00:00 GMT"));
        assert!(strong.same_file(&Validators {
            etag: Some("\"a\"".to_owned()),
            ..Validators::default()
        }));
        assert!(!strong.same_file(&Validators {
            total: Some(11),
            ..Validators::default()
        }));
        assert_eq!(backoff(1), Duration::from_millis(1)); // cfg!(test)
        assert_eq!(glitch_backoff(1), Duration::from_millis(1)); // cfg!(test)
    }

    #[test]
    fn test_glitch_status() {
        assert!(glitch_status(StatusCode::NOT_FOUND, false));
        assert!(glitch_status(StatusCode::GONE, false));
        assert!(!glitch_status(StatusCode::FORBIDDEN, false));
        assert!(glitch_status(StatusCode::FORBIDDEN, true));
        assert!(!glitch_status(StatusCode::OK, true));
    }
}
