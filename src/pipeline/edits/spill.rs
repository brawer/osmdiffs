//! A simple external sort for the passes of `make_edits`: items go in
//! in any order and come out sorted by key, while the heap holds at
//! most one buffer of encoded items, plus one decoded item per sorted
//! run during the merge. Runs are spool files in a temporary directory
//! under the workdir, removed when the spill is dropped.

use crate::tables::{RecordReader, RecordWriter};
use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::Path;
use tempfile::TempDir;

/// What the allocator adds to each buffered record, roughly, so that
/// the budget of a spill bounds its real heap.
const ALLOCATION_OVERHEAD: usize = 32;

/// Items of type `T`, sorted by keys of type `K`. Equal keys come out
/// in no particular order, so a caller that needs a stable order puts
/// a sequence number into the key.
pub(super) struct Spill<K, T> {
    dir: TempDir,
    budget: usize,
    buffered: Vec<(K, Vec<u8>)>,
    buffered_bytes: usize,
    runs: Vec<RecordReader>,
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
        let dir = tempfile::Builder::new()
            .prefix(&format!("edits-{name}-"))
            .tempdir_in(workdir)
            .with_context(|| format!("could not create a spill directory in {workdir:?}"))?;
        Ok(Spill {
            dir,
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
        let path = self.dir.path().join(format!("run-{}", self.runs.len()));
        let mut writer = RecordWriter::create(&path)?;
        for (_, record) in self.buffered.drain(..) {
            writer.write(&record)?;
        }
        writer.close()?;
        self.buffered = Vec::new();
        self.buffered_bytes = 0;
        self.runs.push(RecordReader::open(&path)?);
        Ok(())
    }

    /// The items, sorted by key. When everything fit into one buffer,
    /// nothing was written to disk.
    pub fn sorted(mut self) -> Result<Sorted<K, T>> {
        if self.runs.is_empty() {
            self.buffered.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            let buffered = std::mem::take(&mut self.buffered);
            return Ok(Sorted {
                _dir: self.dir,
                source: Source::Memory(buffered.into_iter()),
            });
        }
        self.write_run()?;
        let mut runs = Vec::with_capacity(self.runs.len());
        let mut heap = BinaryHeap::with_capacity(self.runs.len());
        for (i, reader) in self.runs.iter().enumerate() {
            let mut records: Box<dyn Iterator<Item = Result<Vec<u8>>>> = Box::new(reader.iter()?);
            if let Some(record) = records.next() {
                let (key, item): (K, T) = rmp_serde::from_slice(&record?)?;
                heap.push(Reverse(Head { key, item, run: i }));
            }
            runs.push(records);
        }
        Ok(Sorted {
            _dir: self.dir,
            source: Source::Merge { runs, heap },
        })
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
    _dir: TempDir,
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
