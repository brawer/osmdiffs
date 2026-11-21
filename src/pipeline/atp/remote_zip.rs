//! Reads single entries of a zip file on a web server with HTTP range
//! requests, without downloading the whole file. For the files of
//! spiders in an older AllThePlaces dump (see [super::fallback]): the
//! dump of 2026-10-31 is 2.9 GB with 5,781 entries, but its central
//! directory takes 0.6 MB and a spider's file rarely more than a few.
//!
//! Handles what Python's `zipfile`, which writes the dumps, produces:
//! entries that are stored or deflated, and the Zip64 extensions for
//! offsets and sizes past 4 GiB. No encryption, no multi-disk archives.

use crate::pipeline::download::{self, Ranged};
use anyhow::{Context, Result, anyhow, bail};
use flate2::Crc;
use reqwest::Client;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// How much of the end of the file to fetch first: the end of central
/// directory record (22 bytes) with the longest possible comment, and
/// the Zip64 locator before it. In a dump without a comment, the
/// central directory often fits as well.
const TAIL_BYTES: u64 = 22 + 0xFFFF + 20;

/// How much of an entry's compressed data to fetch with one request.
const CHUNK_BYTES: u64 = if cfg!(test) {
    64 * 1024
} else {
    16 * 1024 * 1024
};

/// How long one range request may take.
const TIMEOUT: Duration = Duration::from_secs(300);

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const LOCAL_HEADER_LEN: u64 = 30;

/// Where an entry's data is, from the central directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    method: u16,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    local_header_offset: u64,
}

/// The central directory of a zip file at a URL.
#[derive(Debug)]
pub(super) struct RemoteZip {
    url: String,
    entries: HashMap<String, Entry>,
}

impl RemoteZip {
    /// Reads the central directory of the zip file at `url`, in two or
    /// three range requests.
    pub async fn open(client: &Client, url: &str) -> Result<Self> {
        let open = async {
            let tail = get(client, url, &format!("bytes=-{TAIL_BYTES}")).await?;
            let (offset, size) = match find_directory(&tail.body, tail.first)? {
                Directory::At { offset, size } => (offset, size),
                Directory::Zip64 { record } => {
                    let buf = fetch(client, url, &tail, record, 56).await?;
                    parse_zip64_eocd(&buf)?
                }
            };
            let buf = fetch(client, url, &tail, offset, size).await?;
            parse_directory(&buf)
        };
        let entries = open
            .await
            .with_context(|| format!("could not read the zip directory of {url}"))?;
        Ok(RemoteZip {
            url: url.to_string(),
            entries,
        })
    }

    /// The names of the entries, such as `output/sdek.geojson`.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Writes the uncompressed data of entry `name` to `out`, checking
    /// its size and CRC-32, and returns its size.
    pub async fn extract(&self, client: &Client, name: &str, out: &mut impl Write) -> Result<u64> {
        let extract = async {
            let entry = self
                .entries
                .get(name)
                .ok_or_else(|| anyhow!("no entry {name}"))?;
            // The local header repeats the name, and may have another
            // extra field than the central directory, so its length is
            // only known once it is read.
            let start = entry.local_header_offset;
            let header = get_part(client, &self.url, start, LOCAL_HEADER_LEN).await?;
            let data_start = start + local_header_len(&header)?;
            // In chunks, so that a large file, such as the 2.5 million
            // addresses of `au_nsw_addresses`, needn't fit in memory.
            let mut inflater = Inflater::new(entry, out)?;
            let end = data_start + entry.compressed_size;
            let mut pos = data_start;
            while pos < end {
                let len = CHUNK_BYTES.min(end - pos);
                inflater.write(&get_part(client, &self.url, pos, len).await?)?;
                pos += len;
            }
            inflater.finish()
        };
        extract
            .await
            .with_context(|| format!("could not extract {name} from {}", self.url))
    }
}

/// Fetches `len` bytes at `offset`, from `tail` if they are in it.
async fn fetch(
    client: &Client,
    url: &str,
    tail: &Ranged,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>> {
    if offset >= tail.first && offset + len <= tail.first + tail.body.len() as u64 {
        let start = (offset - tail.first) as usize;
        return Ok(tail.body[start..start + len as usize].to_vec());
    }
    get_part(client, url, offset, len).await
}

async fn get_part(client: &Client, url: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let range = format!("bytes={offset}-{}", offset + len - 1);
    let got = get(client, url, &range).await?;
    if got.first != offset || got.body.len() as u64 != len {
        bail!(
            "asked for {range}, got {} bytes at {}",
            got.body.len(),
            got.first
        );
    }
    Ok(got.body)
}

async fn get(client: &Client, url: &str, range: &str) -> Result<Ranged> {
    download::get_range(client, url, range, TIMEOUT).await
}

/// Where the central directory is, as far as the end of central
/// directory record tells.
#[derive(Debug, PartialEq, Eq)]
enum Directory {
    At {
        offset: u64,
        size: u64,
    },
    /// Only the Zip64 end of central directory record, at this offset,
    /// tells.
    Zip64 {
        record: u64,
    },
}

/// Finds the central directory from the last bytes of a zip file,
/// `tail`, which start at offset `tail_start` in the file.
fn find_directory(tail: &[u8], tail_start: u64) -> Result<Directory> {
    // Searched backwards, since the comment could contain the
    // signature; the record is at least 22 bytes long.
    let pos = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_at(tail, i) == Some(EOCD_SIGNATURE))
        .ok_or_else(|| anyhow!("no end of central directory record"))?;
    let eocd = &tail[pos..];
    let size = u32_at(eocd, 12).expect("checked length");
    let offset = u32_at(eocd, 16).expect("checked length");
    let entries = u16_at(eocd, 10).expect("checked length");
    if size != u32::MAX && offset != u32::MAX && entries != u16::MAX {
        return Ok(Directory::At {
            offset: offset.into(),
            size: size.into(),
        });
    }
    let locator = pos
        .checked_sub(20)
        .map(|start| &tail[start..pos])
        .filter(|l| u32_at(l, 0) == Some(ZIP64_LOCATOR_SIGNATURE))
        .ok_or_else(|| {
            anyhow!(
                "no Zip64 locator before the end of central directory record at {}",
                tail_start + pos as u64
            )
        })?;
    Ok(Directory::Zip64 {
        record: u64_at(locator, 8).expect("checked length"),
    })
}

/// The offset and size of the central directory, from the Zip64 end of
/// central directory record.
fn parse_zip64_eocd(buf: &[u8]) -> Result<(u64, u64)> {
    if u32_at(buf, 0) != Some(ZIP64_EOCD_SIGNATURE) {
        bail!("no Zip64 end of central directory record");
    }
    let size = u64_at(buf, 40).ok_or_else(|| anyhow!("short Zip64 record"))?;
    let offset = u64_at(buf, 48).ok_or_else(|| anyhow!("short Zip64 record"))?;
    Ok((offset, size))
}

/// The entries of a central directory, by name.
fn parse_directory(buf: &[u8]) -> Result<HashMap<String, Entry>> {
    let mut entries = HashMap::new();
    let mut pos = 0;
    while pos < buf.len() {
        let header = &buf[pos..];
        let field = |off: usize| u16_at(header, off).ok_or_else(|| anyhow!("short entry"));
        if u32_at(header, 0) != Some(CENTRAL_SIGNATURE) {
            bail!("bad central directory entry at {pos}");
        }
        let method = field(10)?;
        let crc32 = u32_at(header, 16).ok_or_else(|| anyhow!("short entry"))?;
        let mut compressed_size: u64 = u32_at(header, 20).unwrap_or(0).into();
        let mut uncompressed_size: u64 = u32_at(header, 24).unwrap_or(0).into();
        let name_len = field(28)? as usize;
        let extra_len = field(30)? as usize;
        let comment_len = field(32)? as usize;
        let mut local_header_offset: u64 = u32_at(header, 42)
            .ok_or_else(|| anyhow!("short entry"))?
            .into();
        let end = 46 + name_len + extra_len + comment_len;
        if header.len() < end {
            bail!("short entry at {pos}");
        }
        let name = std::str::from_utf8(&header[46..46 + name_len])
            .with_context(|| format!("entry name at {pos} is not UTF-8"))?
            .to_string();

        // The Zip64 extra field has the values whose 32-bit field is
        // all ones, in this order.
        let mut extra = &header[46 + name_len..46 + name_len + extra_len];
        while extra.len() >= 4 {
            let id = u16_at(extra, 0).expect("checked length");
            let len = u16_at(extra, 2).expect("checked length") as usize;
            let data = extra
                .get(4..4 + len)
                .ok_or_else(|| anyhow!("bad extra field of {name}"))?;
            if id == 0x0001 {
                let mut values = data
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| u64::from_le_bytes(*c));
                let mut zip64 = |v: &mut u64| -> Result<()> {
                    if *v == u64::from(u32::MAX) {
                        *v = values
                            .next()
                            .ok_or_else(|| anyhow!("short Zip64 field of {name}"))?;
                    }
                    Ok(())
                };
                zip64(&mut uncompressed_size)?;
                zip64(&mut compressed_size)?;
                zip64(&mut local_header_offset)?;
            }
            extra = &extra[4 + len..];
        }
        entries.insert(
            name,
            Entry {
                method,
                crc32,
                compressed_size,
                uncompressed_size,
                local_header_offset,
            },
        );
        pos += end;
    }
    Ok(entries)
}

/// The length of a local file header, from its first 30 bytes.
fn local_header_len(header: &[u8]) -> Result<u64> {
    if u32_at(header, 0) != Some(LOCAL_SIGNATURE) {
        bail!("bad local file header");
    }
    let name_len = u16_at(header, 26).ok_or_else(|| anyhow!("short local header"))?;
    let extra_len = u16_at(header, 28).ok_or_else(|| anyhow!("short local header"))?;
    Ok(LOCAL_HEADER_LEN + u64::from(name_len) + u64::from(extra_len))
}

/// Writes the uncompressed `data` of `entry` to `out`.
#[cfg(test)]
fn decompress(entry: &Entry, data: &[u8], out: &mut impl Write) -> Result<u64> {
    let mut inflater = Inflater::new(entry, out)?;
    inflater.write(data)?;
    inflater.finish()
}

/// Uncompresses the data of an entry, fed to it in chunks, to `out`,
/// and checks its size and CRC-32.
struct Inflater<'a, W: Write> {
    entry: &'a Entry,
    sink: Sink<Checked<'a, W>>,
}

enum Sink<W: Write> {
    Stored(W),
    Deflated(flate2::write::DeflateDecoder<W>),
}

impl<'a, W: Write> Inflater<'a, W> {
    fn new(entry: &'a Entry, out: &'a mut W) -> Result<Self> {
        let checked = Checked {
            inner: out,
            crc: Crc::new(),
            written: 0,
        };
        let sink = match entry.method {
            0 => Sink::Stored(checked),
            8 => Sink::Deflated(flate2::write::DeflateDecoder::new(checked)),
            method => bail!("unsupported compression method {method}"),
        };
        Ok(Inflater { entry, sink })
    }

    fn write(&mut self, chunk: &[u8]) -> Result<()> {
        match &mut self.sink {
            Sink::Stored(w) => w.write_all(chunk)?,
            Sink::Deflated(w) => w.write_all(chunk)?,
        }
        Ok(())
    }

    fn finish(self) -> Result<u64> {
        let checked = match self.sink {
            Sink::Stored(w) => w,
            Sink::Deflated(w) => w.finish()?,
        };
        if checked.written != self.entry.uncompressed_size {
            bail!(
                "got {} bytes, not {}",
                checked.written,
                self.entry.uncompressed_size
            );
        }
        if checked.crc.sum() != self.entry.crc32 {
            bail!("bad CRC-32");
        }
        checked.inner.flush()?;
        Ok(checked.written)
    }
}

/// Computes the CRC-32 and size of what is written through it.
struct Checked<'a, W: Write> {
    inner: &'a mut W,
    crc: Crc,
    written: u64,
}

impl<W: Write> Write for Checked<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.crc.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn u16_at(buf: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(buf.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(buf: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(buf.get(off..off + 4)?.try_into().ok()?))
}

fn u64_at(buf: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(buf.get(off..off + 8)?.try_into().ok()?))
}

/// Extracts entry `name` of `zip` to the file `out`, through a
/// temporary file next to it, so that `out` only ever exists complete.
pub(super) async fn extract_to(
    zip: &RemoteZip,
    client: &Client,
    name: &str,
    out: &Path,
) -> Result<u64> {
    let mut tmp = out.to_path_buf();
    tmp.add_extension("tmp");
    let file = std::fs::File::create(&tmp).with_context(|| format!("could not create {tmp:?}"))?;
    let mut writer = std::io::BufWriter::new(file);
    let size = zip.extract(client, name, &mut writer).await?;
    writer
        .into_inner()
        .map_err(|e| e.into_error())
        .and_then(|f| f.sync_all())
        .with_context(|| format!("could not write {tmp:?}"))?;
    std::fs::rename(&tmp, out).with_context(|| format!("could not rename {tmp:?}"))?;
    Ok(size)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Builds a zip file with `entries`, deflated. With `zip64`, every
    /// offset and size goes into the Zip64 fields, as for a dump past
    /// 4 GiB.
    pub fn build_zip(entries: &[(&str, &[u8])], zip64: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        let ones = u32::MAX.to_le_bytes();
        for (name, data) in entries {
            let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
            enc.write_all(data).unwrap();
            let compressed = enc.finish().unwrap();
            let mut crc = Crc::new();
            crc.update(data);
            let offset = out.len() as u64;
            let mut local = Vec::new();
            local.extend(LOCAL_SIGNATURE.to_le_bytes());
            // Version, flags, method, time, and a date that piz accepts.
            local.extend([20, 0, 0, 0, 8, 0, 0, 0, 0x21, 0x5c]);
            local.extend(crc.sum().to_le_bytes());
            local.extend((compressed.len() as u32).to_le_bytes());
            local.extend((data.len() as u32).to_le_bytes());
            local.extend((name.len() as u16).to_le_bytes());
            // An extra field that the central directory doesn't have.
            local.extend(4u16.to_le_bytes());
            local.extend(name.as_bytes());
            local.extend([0xca, 0xfe, 0, 0]);
            out.extend(local);
            out.extend(&compressed);

            central.extend(CENTRAL_SIGNATURE.to_le_bytes());
            central.extend([45, 3, 45, 0, 0, 0, 8, 0, 0, 0, 0x21, 0x5c]);
            central.extend(crc.sum().to_le_bytes());
            let mut extra = Vec::new();
            if zip64 {
                central.extend(ones);
                central.extend(ones);
                extra.extend(1u16.to_le_bytes());
                extra.extend(24u16.to_le_bytes());
                extra.extend((data.len() as u64).to_le_bytes());
                extra.extend((compressed.len() as u64).to_le_bytes());
                extra.extend(offset.to_le_bytes());
            } else {
                central.extend((compressed.len() as u32).to_le_bytes());
                central.extend((data.len() as u32).to_le_bytes());
            }
            central.extend((name.len() as u16).to_le_bytes());
            central.extend((extra.len() as u16).to_le_bytes());
            central.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend(if zip64 {
                ones
            } else {
                (offset as u32).to_le_bytes()
            });
            central.extend(name.as_bytes());
            central.extend(extra);
        }
        let cd_offset = out.len() as u64;
        let cd_size = central.len() as u64;
        out.extend(central);
        let n = entries.len() as u64;
        if zip64 {
            let record = out.len() as u64;
            out.extend(ZIP64_EOCD_SIGNATURE.to_le_bytes());
            out.extend(44u64.to_le_bytes());
            out.extend([45, 3, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend(n.to_le_bytes());
            out.extend(n.to_le_bytes());
            out.extend(cd_size.to_le_bytes());
            out.extend(cd_offset.to_le_bytes());
            out.extend(ZIP64_LOCATOR_SIGNATURE.to_le_bytes());
            out.extend(0u32.to_le_bytes());
            out.extend(record.to_le_bytes());
            out.extend(1u32.to_le_bytes());
        }
        out.extend(EOCD_SIGNATURE.to_le_bytes());
        out.extend([0, 0, 0, 0]);
        if zip64 {
            out.extend([0xff; 4]);
            out.extend(ones);
            out.extend(ones);
        } else {
            out.extend((n as u16).to_le_bytes());
            out.extend((n as u16).to_le_bytes());
            out.extend((cd_size as u32).to_le_bytes());
            out.extend((cd_offset as u32).to_le_bytes());
        }
        out.extend([0, 0]);
        out
    }

    /// Serves `files` (path, contents) over HTTP, answering `Range`
    /// requests the way a CDN does, until the test ends. Returns the
    /// base URL, and the ranges asked for.
    pub async fn serve(
        files: Vec<(String, Vec<u8>)>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let files = std::sync::Arc::new(files);
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let files = files.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let mut n = 0;
                    while !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf[n..]).await {
                            Ok(0) | Err(_) => return,
                            Ok(k) => n += k,
                        }
                    }
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                    let range = request
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("range: ").map(String::from));
                    let Some(data) = files.iter().find(|(p, _)| *p == path).map(|f| &f.1) else {
                        let _ = socket
                            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n")
                            .await;
                        return;
                    };
                    let len = data.len() as u64;
                    let (status, first, last) = match &range {
                        None => ("200 OK", 0, len - 1),
                        Some(r) => {
                            log.lock().unwrap().push(format!("{path} {r}"));
                            let spec = r.trim().strip_prefix("bytes=").unwrap();
                            let (a, b) = spec.split_once('-').unwrap();
                            if a.is_empty() {
                                let n: u64 = b.parse().unwrap();
                                ("206 Partial Content", len.saturating_sub(n), len - 1)
                            } else {
                                let a: u64 = a.parse().unwrap();
                                let b: u64 = b.parse::<u64>().unwrap().min(len - 1);
                                ("206 Partial Content", a, b)
                            }
                        }
                    };
                    let body = &data[first as usize..=last as usize];
                    let mut head = format!(
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n",
                        body.len()
                    );
                    if range.is_some() {
                        head.push_str(&format!("content-range: bytes {first}-{last}/{len}\r\n"));
                    }
                    head.push_str("\r\n");
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(body).await;
                    let _ = socket.flush().await;
                });
            }
        });
        (url, seen)
    }

    pub fn test_client() -> Client {
        Client::builder().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn test_remote_zip() -> Result<()> {
        let big: Vec<u8> = (0..200_000u32)
            .flat_map(|i| (i % 97).to_le_bytes())
            .collect();
        for zip64 in [false, true] {
            let zip = build_zip(
                &[
                    ("output/a.geojson", b"{\"a\": 1}\n"),
                    ("output/empty.geojson", b""),
                    ("output/big.geojson", &big),
                ],
                zip64,
            );
            let (url, seen) = serve(vec![("/output.zip".into(), zip)]).await;
            let client = test_client();
            let url = format!("{url}/output.zip");
            let remote = RemoteZip::open(&client, &url).await?;
            let mut names: Vec<&str> = remote.names().collect();
            names.sort();
            assert_eq!(
                names,
                [
                    "output/a.geojson",
                    "output/big.geojson",
                    "output/empty.geojson"
                ]
            );
            // The small test file fits into the tail.
            assert_eq!(seen.lock().unwrap().len(), 1, "zip64={zip64}");

            let mut out = Vec::new();
            assert_eq!(
                remote
                    .extract(&client, "output/a.geojson", &mut out)
                    .await?,
                9
            );
            assert_eq!(out, b"{\"a\": 1}\n");
            let mut out = Vec::new();
            remote
                .extract(&client, "output/big.geojson", &mut out)
                .await?;
            assert_eq!(out, big);
            let mut out = Vec::new();
            assert_eq!(
                remote
                    .extract(&client, "output/empty.geojson", &mut out)
                    .await?,
                0
            );
            let err = remote
                .extract(&client, "output/missing.geojson", &mut out)
                .await
                .unwrap_err();
            assert!(format!("{err:#}").contains("no entry"), "{err:#}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_remote_zip_fetches_directory_outside_tail() -> Result<()> {
        // Incompressible data, so that the directory is far from the end.
        let mut noise = Vec::with_capacity(300_000);
        let mut x: u32 = 1;
        for _ in 0..300_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            noise.push(x as u8);
        }
        let entries: Vec<(String, Vec<u8>)> = (0..3000)
            .map(|i| (format!("output/spider_{i:04}.geojson"), vec![b'x'; i % 7]))
            .chain([("output/noise.geojson".to_string(), noise.clone())])
            .collect();
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        for zip64 in [false, true] {
            let zip = build_zip(&refs, zip64);
            let (url, seen) = serve(vec![("/o.zip".into(), zip)]).await;
            let client = test_client();
            let remote = RemoteZip::open(&client, &format!("{url}/o.zip")).await?;
            assert_eq!(remote.names().count(), 3001);
            // The tail, which has the Zip64 record too, and the directory.
            let ranges = seen.lock().unwrap().clone();
            assert_eq!(ranges.len(), 2, "{ranges:?}");
            let mut out = Vec::new();
            remote
                .extract(&client, "output/noise.geojson", &mut out)
                .await?;
            assert_eq!(out, noise);
        }

        // With the longest comment, the Zip64 record is outside the tail.
        let mut zip = build_zip(&refs, true);
        let len = zip.len();
        zip[len - 2..].copy_from_slice(&u16::MAX.to_le_bytes());
        zip.extend(std::iter::repeat_n(b'c', u16::MAX as usize));
        let (url, seen) = serve(vec![("/o.zip".into(), zip)]).await;
        let remote = RemoteZip::open(&test_client(), &format!("{url}/o.zip")).await?;
        assert_eq!(remote.names().count(), 3001);
        let ranges = seen.lock().unwrap().clone();
        assert_eq!(ranges.len(), 3, "{ranges:?}");
        Ok(())
    }

    #[test]
    fn test_decompress_checks() {
        let entry = Entry {
            method: 0,
            crc32: 0,
            compressed_size: 3,
            uncompressed_size: 3,
            local_header_offset: 0,
        };
        let err = decompress(&entry, b"abc", &mut Vec::new()).unwrap_err();
        assert!(err.to_string().contains("CRC"), "{err:#}");
        let err = decompress(
            &Entry {
                uncompressed_size: 4,
                ..entry.clone()
            },
            b"abc",
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not 4"), "{err:#}");
        let err = decompress(
            &Entry {
                method: 12,
                ..entry
            },
            b"abc",
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("method 12"), "{err:#}");
    }

    #[test]
    fn test_find_directory_errors() {
        assert!(find_directory(b"not a zip file at all", 0).is_err());
        let mut tail = EOCD_SIGNATURE.to_le_bytes().to_vec();
        tail.extend([0u8; 4]);
        tail.extend([0xff; 12]);
        tail.extend([0, 0]);
        let err = find_directory(&tail, 100).unwrap_err();
        assert!(err.to_string().contains("Zip64 locator"), "{err:#}");
    }
}
