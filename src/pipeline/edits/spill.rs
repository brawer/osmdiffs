//! A simple external sort for the passes of `make_edits`: items go in
//! in any order and come out sorted by key, while the heap holds at
//! most one buffer of encoded items, plus one decoded item per sorted
//! run during the merge. All runs go into a single temporary spool file
//! under the workdir, one lz4 frame after the other, which is removed
//! when the spill is dropped. During the merge, every run reads its own
//! frame with positional reads, so a spill holds one file descriptor no
//! matter how many runs it writes.

use crate::tables::{read_records, write_records};
use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;
use tempfile::{NamedTempFile, TempPath};

/// What the allocator adds to each buffered record, roughly, so that
/// the budget of a spill bounds its real heap.
const ALLOCATION_OVERHEAD: usize = 32;

/// Items of type `T`, sorted by keys of type `K`. Equal keys come out
/// in no particular order, so a caller that needs a stable order puts
/// a sequence number into the key.
pub(super) struct Spill<K, T> {
    file: NamedTempFile,
    budget: usize,
    buffered: Vec<(K, Vec<u8>)>,
    buffered_bytes: usize,
    /// Where each run is in `file`, as a range of bytes.
    runs: Vec<std::ops::Range<u64>>,
    len: u64,
    _item: std::marker::PhantomData<T>,
}

impl<K, T> Spill<K, T>
where
    K: Ord + Serialize + DeserializeOwned,
    T: Serialize + DeserializeOwned,
{
    /// A spill that writes its runs below `workdir`, buffering about
    /// `budget` bytes of encoded items at a time.
    pub fn new(workdir: &Path, name: &str, budget: usize) -> Result<Spill<K, T>> {
        let file = tempfile::Builder::new()
            .prefix(&format!("edits-{name}-"))
            .tempfile_in(workdir)
            .with_context(|| format!("could not create a spill file in {workdir:?}"))?;
        Ok(Spill {
            file,
            budget,
            buffered: Vec::new(),
            buffered_bytes: 0,
            runs: Vec::new(),
            len: 0,
            _item: std::marker::PhantomData,
        })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn push(&mut self, key: K, item: &T) -> Result<()> {
        let record = rmp_serde::to_vec(&(&key, item))?;
        self.buffered_bytes += record.capacity() + size_of::<(K, Vec<u8>)>() + ALLOCATION_OVERHEAD;
        self.buffered.push((key, record));
        self.len += 1;
        if self.buffered_bytes >= self.budget {
            self.write_run()?;
        }
        Ok(())
    }

    fn write_run(&mut self) -> Result<()> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        self.buffered.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let start = self.runs.last().map_or(0, |run| run.end);
        let file = self.file.as_file_mut();
        write_records(
            &mut *file,
            self.buffered.drain(..).map(|(_, record)| record),
        )
        .with_context(|| format!("failed to write a run to {:?}", self.file.path()))?;
        let end = self.file.as_file_mut().stream_position()?;
        self.buffered = Vec::new();
        self.buffered_bytes = 0;
        self.runs.push(start..end);
        Ok(())
    }

    /// The items, sorted by key. When everything fit into one buffer,
    /// nothing was written to disk.
    pub fn sorted(mut self) -> Result<Sorted<K, T>> {
        if self.runs.is_empty() {
            self.buffered.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            let buffered = std::mem::take(&mut self.buffered);
            return Ok(Sorted {
                _path: self.file.into_temp_path(),
                source: Source::Memory(buffered.into_iter()),
            });
        }
        self.write_run()?;
        let (file, path) = self.file.into_parts();
        let file = Arc::new(file);
        let mut runs = Vec::with_capacity(self.runs.len());
        let mut heap = BinaryHeap::with_capacity(self.runs.len());
        for (i, range) in self.runs.iter().enumerate() {
            let chunk = Chunk {
                file: Arc::clone(&file),
                pos: range.start,
                end: range.end,
            };
            let mut records: Box<dyn Iterator<Item = Result<Vec<u8>>>> =
                Box::new(read_records(chunk));
            if let Some(record) = records.next() {
                let (key, item): (K, T) = rmp_serde::from_slice(&record?)?;
                heap.push(Reverse(Head { key, item, run: i }));
            }
            runs.push(records);
        }
        Ok(Sorted {
            _path: path,
            source: Source::Merge { runs, heap },
        })
    }
}

/// One run of a spill file: a range of bytes, read with positional
/// reads so that all runs can share the file, and its descriptor.
struct Chunk {
    file: Arc<File>,
    pos: u64,
    end: u64,
}

impl Read for Chunk {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = buf.len().min((self.end - self.pos) as usize);
        if len == 0 {
            return Ok(0);
        }
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(&*self.file, &mut buf[..len], self.pos)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(&*self.file, &mut buf[..len], self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// The next item of one run, ordered by key and then by run.
struct Head<K, T> {
    key: K,
    item: T,
    run: usize,
}

impl<K: Ord, T> PartialEq for Head<K, T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.run == other.run
    }
}

impl<K: Ord, T> Eq for Head<K, T> {}

impl<K: Ord, T> PartialOrd for Head<K, T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: Ord, T> Ord for Head<K, T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key
            .cmp(&other.key)
            .then_with(|| self.run.cmp(&other.run))
    }
}

enum Source<K, T> {
    Memory(std::vec::IntoIter<(K, Vec<u8>)>),
    Merge {
        runs: Vec<Box<dyn Iterator<Item = Result<Vec<u8>>>>>,
        heap: BinaryHeap<Reverse<Head<K, T>>>,
    },
}

/// The sorted items of a [Spill], as `(key, item)`.
pub(super) struct Sorted<K, T> {
    _path: TempPath,
    source: Source<K, T>,
}

impl<K, T> Iterator for Sorted<K, T>
where
    K: Ord + DeserializeOwned,
    T: DeserializeOwned,
{
    type Item = Result<(K, T)>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.source {
            Source::Memory(items) => {
                let (_, record) = items.next()?;
                Some(rmp_serde::from_slice(&record).map_err(Into::into))
            }
            Source::Merge { runs, heap } => {
                let Reverse(head) = heap.pop()?;
                if let Some(record) = runs[head.run].next() {
                    let next = record.and_then(|r| {
                        rmp_serde::from_slice::<(K, T)>(&r).map_err(anyhow::Error::from)
                    });
                    match next {
                        Ok((key, item)) => heap.push(Reverse(Head {
                            key,
                            item,
                            run: head.run,
                        })),
                        Err(e) => return Some(Err(e)),
                    }
                }
                Some(Ok((head.key, head.item)))
            }
        }
    }
}

/// A `&'static str`, by a name that serde doesn't take for a borrowed
/// string; see [static_str].
pub(super) type StaticStr = &'static str;

/// Deserializes a `&'static str` that was serialized as a string, for
/// the few fields of edits that name one of a fixed set of values,
/// such as a topic. Each distinct value is leaked once.
pub(super) fn static_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
    let s: String = serde::Deserialize::deserialize(d)?;
    Ok(intern(&s))
}

/// Like [static_str], for a list.
pub(super) fn static_strs<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<&'static str>, D::Error> {
    let v: Vec<String> = serde::Deserialize::deserialize(d)?;
    Ok(v.iter().map(|s| intern(s)).collect())
}

fn intern(s: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(s) = seen.get(s) {
        return s;
    }
    let s: &'static str = Box::leak(s.to_string().into_boxed_str());
    seen.insert(s);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(budget: usize, items: &[(u32, String)]) -> Result<Vec<(u32, String)>> {
        let dir = tempfile::tempdir()?;
        let mut spill: Spill<u32, String> = Spill::new(dir.path(), "test", budget)?;
        for (k, v) in items {
            spill.push(*k, v)?;
        }
        assert_eq!(spill.len(), items.len() as u64);
        spill.sorted()?.collect()
    }

    #[test]
    fn sorts_in_memory_and_on_disk() -> Result<()> {
        let items: Vec<(u32, String)> = (0..1000u32)
            .map(|i| ((i * 7919) % 1000, format!("item {i}")))
            .collect();
        let mut expected = items.clone();
        expected.sort();
        assert_eq!(sorted(usize::MAX, &items)?, expected);
        // A tiny budget writes a run for every few items.
        assert_eq!(sorted(200, &items)?, expected);
        assert_eq!(sorted(1, &items)?, expected);
        assert!(sorted(1, &[])?.is_empty());
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn many_runs_share_one_file_descriptor() -> Result<()> {
        let fds = || std::fs::read_dir("/proc/self/fd").map(|d| d.count());
        let dir = tempfile::tempdir()?;
        let mut spill: Spill<u32, u32> = Spill::new(dir.path(), "test", 1)?;
        for i in (0..5000u32).rev() {
            spill.push(i, &i)?;
        }
        assert_eq!(std::fs::read_dir(dir.path())?.count(), 1);
        let before = fds()?;
        let sorted = spill.sorted()?;
        // Other tests open and close files concurrently, so allow some
        // slack; one descriptor per run would add thousands.
        assert!(fds()? < before + 100);
        let out: Vec<u32> = sorted.map(|r| r.map(|(k, _)| k)).collect::<Result<_>>()?;
        assert_eq!(out, (0..5000).collect::<Vec<_>>());
        Ok(())
    }

    #[test]
    fn runs_are_removed() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut spill: Spill<u32, u32> = Spill::new(dir.path(), "test", 1)?;
        spill.push(2, &2)?;
        spill.push(1, &1)?;
        let out: Vec<(u32, u32)> = spill.sorted()?.collect::<Result<_>>()?;
        assert_eq!(out, vec![(1, 1), (2, 2)]);
        assert_eq!(std::fs::read_dir(dir.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn interns_static_strs() {
        let a = intern(&String::from("contact"));
        let b = intern(&String::from("contact"));
        assert!(std::ptr::eq(a, b));
    }
}
