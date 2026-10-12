use crate::native::{self, Span};
use anyhow::{Result, ensure};
use rayon::prelude::*;
use serde::Deserialize;
use serde::Serialize;
use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use unicode_normalization::UnicodeNormalization;

pub fn normalize(s: &str) -> String {
    if s.is_ascii() {
        s.to_ascii_lowercase()
    } else {
        s.nfc().flat_map(char::to_lowercase).nfc().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parallel_partitions_keep_global_ids_and_uncapped_top_k() {
        let mut index = Index::new("/synthetic".into(), 70_000);
        for id in 0..70_000 {
            let name = if id == 0 {
                String::new()
            } else if id == 69_999 {
                "target.txt".into()
            } else {
                format!("archive_target_{id:05}.txt")
            };
            index
                .put(&Record {
                    id,
                    parent: 0,
                    name: &name,
                    folded: &name,
                    full: "",
                    initials: "",
                    kind: if id == 0 { 1 } else { 0 },
                    dev: 0,
                    ino: id as u64,
                    size: 0,
                    mtime: 0,
                })
                .unwrap();
        }
        let best = index.query(
            "target",
            &QueryOptions {
                limit: 1,
                ..Default::default()
            },
        );
        assert_eq!(best[0].id, 69_999);
        assert_eq!(best[0].path, Path::new("/synthetic/target.txt"));
        let first = index.query(
            "archive_target",
            &QueryOptions {
                limit: 20,
                ..Default::default()
            },
        );
        assert_eq!(
            first.iter().map(|h| h.id).collect::<Vec<_>>(),
            (1..=20).collect::<Vec<_>>()
        );
    }
}

#[derive(Default, Clone, Copy, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum MatchMode {
    #[default]
    Auto,
    Filename,
    Pinyin,
}

#[derive(Default)]
pub struct QueryOptions {
    pub limit: usize,
    pub kind: Option<u8>,
    pub extension: Option<String>,
    pub under: Option<PathBuf>,
    pub mode: MatchMode,
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub id: u32,
    pub path: PathBuf,
    pub is_directory: bool,
    pub size: u64,
    pub mtime: i64,
    pub score: i32,
    pub match_kind: &'static str,
}

pub(crate) struct Record<'a> {
    pub id: u32,
    pub parent: u32,
    pub name: &'a str,
    pub folded: &'a str,
    pub full: &'a str,
    pub initials: &'a str,
    pub kind: u8,
    pub dev: i32,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
}

pub struct Index {
    pub root: PathBuf,
    blob: Vec<u8>,
    names: Vec<Span>,
    folded: Vec<Span>,
    full: Vec<Span>,
    initials: Vec<Span>,
    pub(crate) parents: Vec<u32>,
    pub(crate) kinds: Vec<u8>,
    pub(crate) devices: Vec<i32>,
    pub(crate) inodes: Vec<u64>,
    pub(crate) sizes: Vec<u64>,
    pub(crate) mtimes: Vec<i64>,
}

impl Index {
    pub(crate) fn new(root: PathBuf, capacity: usize) -> Self {
        Self {
            root,
            blob: Vec::with_capacity(capacity.saturating_mul(32)),
            names: Vec::with_capacity(capacity),
            folded: Vec::with_capacity(capacity),
            full: Vec::with_capacity(capacity),
            initials: Vec::with_capacity(capacity),
            parents: Vec::with_capacity(capacity),
            kinds: Vec::with_capacity(capacity),
            devices: Vec::with_capacity(capacity),
            inodes: Vec::with_capacity(capacity),
            sizes: Vec::with_capacity(capacity),
            mtimes: Vec::with_capacity(capacity),
        }
    }
    fn text(&self, span: Span) -> &str {
        // Only validated UTF-8 names/keys are admitted to this owned byte pool.
        unsafe {
            std::str::from_utf8_unchecked(
                &self.blob[span.start as usize..span.start as usize + span.len as usize],
            )
        }
    }
    fn intern(&mut self, value: &str) -> Result<Span> {
        ensure!(
            self.blob
                .len()
                .checked_add(value.len())
                .is_some_and(|n| n <= u32::MAX as usize),
            "name pool exceeds 4 GiB; split the index"
        );
        let span = Span {
            start: self.blob.len() as u32,
            len: value.len() as u32,
        };
        self.blob.extend_from_slice(value.as_bytes());
        Ok(span)
    }
    pub(crate) fn put(&mut self, r: &Record<'_>) -> Result<()> {
        ensure!(
            r.id as usize <= self.kinds.len(),
            "non-contiguous file IDs in catalog"
        );
        let i = r.id as usize;
        let unchanged_keys = i < self.kinds.len()
            && self.text(self.names[i]) == r.name
            && self.text(self.folded[i]) == r.folded
            && self.text(self.full[i]) == r.full
            && self.text(self.initials[i]) == r.initials;
        let (name, folded, full, initials) = if unchanged_keys {
            (
                self.names[i],
                self.folded[i],
                self.full[i],
                self.initials[i],
            )
        } else {
            let name = self.intern(r.name)?;
            let folded = if r.name == r.folded {
                name
            } else {
                self.intern(r.folded)?
            };
            let full = self.intern(r.full)?;
            let initials = self.intern(r.initials)?;
            (name, folded, full, initials)
        };
        if i == self.kinds.len() {
            self.names.push(name);
            self.folded.push(folded);
            self.full.push(full);
            self.initials.push(initials);
            self.parents.push(r.parent);
            self.kinds.push(r.kind);
            self.devices.push(r.dev);
            self.inodes.push(r.ino);
            self.sizes.push(r.size);
            self.mtimes.push(r.mtime);
        } else {
            self.names[i] = name;
            self.folded[i] = folded;
            self.full[i] = full;
            self.initials[i] = initials;
            self.parents[i] = r.parent;
            self.kinds[i] = r.kind;
            self.devices[i] = r.dev;
            self.inodes[i] = r.ino;
            self.sizes[i] = r.size;
            self.mtimes[i] = r.mtime;
        }
        Ok(())
    }
    pub fn entries(&self) -> usize {
        self.kinds
            .iter()
            .filter(|&&k| k & 128 == 0)
            .count()
            .saturating_sub(usize::from(
                self.kinds.first().is_some_and(|k| k & 128 == 0),
            ))
    }
    pub fn slots(&self) -> usize {
        self.kinds.len()
    }
    pub(crate) fn name(&self, id: u32) -> &str {
        self.text(self.names[id as usize])
    }
    pub fn path(&self, id: u32) -> PathBuf {
        let mut parts = Vec::new();
        let mut p = id as usize;
        for _ in 0..self.kinds.len() {
            if p == 0 {
                break;
            }
            parts.push(self.text(self.names[p]));
            p = self.parents[p] as usize;
        }
        let mut path = self.root.clone();
        for part in parts.into_iter().rev() {
            path.push(part);
        }
        path
    }
    pub fn allocated_bytes(&self) -> usize {
        self.blob.capacity()
            + (self.names.capacity()
                + self.folded.capacity()
                + self.full.capacity()
                + self.initials.capacity())
                * 8
            + self.parents.capacity() * 4
            + self.kinds.capacity()
            + self.devices.capacity() * 4
            + (self.inodes.capacity() + self.sizes.capacity() + self.mtimes.capacity()) * 8
    }
    pub fn name_bytes(&self) -> usize {
        self.blob.len()
    }
    pub fn query(&self, query: &str, options: &QueryOptions) -> Vec<Hit> {
        let q = normalize(query.trim());
        if q.is_empty() || options.limit == 0 {
            return Vec::new();
        }
        let path_query = q.contains('/');
        let mut ids = vec![0u32; self.kinds.len()];
        let mut scores = vec![0i32; self.kinds.len()];
        let ranges = if path_query {
            // Normalize each distinct parent once, rather than allocating a path per file.
            let (prefix, final_component) = q.rsplit_once('/').unwrap();
            let suffix = format!("{prefix}/");
            let mut parents = vec![0u8; self.slots()];
            let mut count = 0;
            for (i, &kind) in self.kinds.iter().enumerate().skip(1) {
                if kind & 128 != 0 {
                    continue;
                }
                let p = self.parents[i] as usize;
                if parents[p] == 0 {
                    let parent = format!(
                        "{}/",
                        normalize(&self.path(p as u32).to_string_lossy()).trim_end_matches('/')
                    );
                    parents[p] = 1
                        | if parent.contains(&q) { 2 } else { 0 }
                        | if parent.ends_with(&suffix) { 4 } else { 0 };
                }
                if parents[p] & 2 != 0
                    || (parents[p] & 4 != 0
                        && self.text(self.folded[i]).starts_with(final_component))
                {
                    ids[count] = i as u32;
                    scores[count] = 1000;
                    count += 1;
                }
            }
            std::iter::once(0..count).collect::<Vec<_>>()
        } else {
            let scan = |start: usize, out: &mut [u32], ranks: &mut [i32]| unsafe {
                // Read-only index spans and disjoint writable slices stay alive until C returns.
                native::lumi_search(
                    self.blob.as_ptr(),
                    self.folded.as_ptr(),
                    self.full.as_ptr(),
                    self.initials.as_ptr(),
                    self.kinds.as_ptr(),
                    start as u32,
                    (start + out.len()) as u32,
                    q.as_ptr(),
                    q.len(),
                    i32::from(q.is_ascii() && q.bytes().any(|b| b.is_ascii_alphabetic())),
                    match options.mode {
                        MatchMode::Auto => 0,
                        MatchMode::Filename => 1,
                        MatchMode::Pinyin => 2,
                    },
                    out.as_mut_ptr(),
                    ranks.as_mut_ptr(),
                )
            };
            static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
            if self.slots() >= 65_536
                && let Some(pool) =
                    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(4).build().ok())
            {
                let chunk = self.slots().div_ceil(4);
                pool.install(|| {
                    ids.par_chunks_mut(chunk)
                        .zip(scores.par_chunks_mut(chunk))
                        .enumerate()
                        .map(|(n, (out, ranks))| {
                            let start = n * chunk;
                            start..start + scan(start, out, ranks)
                        })
                        .collect::<Vec<_>>()
                })
            } else {
                std::iter::once(0..scan(0, &mut ids, &mut scores)).collect::<Vec<_>>()
            }
        };
        let limit = options.limit.min(10_000);
        let mut best = BinaryHeap::with_capacity(limit + 1);
        let mut scope_parents = options.under.as_ref().map(|_| vec![0u8; self.slots()]);
        for j in ranges.into_iter().flatten() {
            let id = ids[j];
            let i = id as usize;
            if options.kind.is_some_and(|k| self.kinds[i] & 3 != k) {
                continue;
            }
            if let Some(ext) = &options.extension
                && !Path::new(self.name(id)).extension().is_some_and(|s| {
                    s.to_string_lossy()
                        .eq_ignore_ascii_case(ext.trim_start_matches('.'))
                })
            {
                continue;
            }
            if let (Some(under), Some(parents)) = (&options.under, &mut scope_parents) {
                let parent = self.parents[i] as usize;
                if parents[parent] == 0 {
                    parents[parent] = if self.path(parent as u32).starts_with(under) {
                        2
                    } else {
                        1
                    };
                }
                if parents[parent] != 2
                    && !(under.file_name() == Some(std::ffi::OsStr::new(self.name(id)))
                        && self.path(id) == *under)
                {
                    continue;
                }
            }
            let rank = (scores[j], Reverse(id));
            if best.len() < limit {
                best.push(Reverse(rank));
            } else if best.peek().is_some_and(|worst| rank > worst.0) {
                best.pop();
                best.push(Reverse(rank));
            }
        }
        let mut ranks = best.into_iter().map(|r| r.0).collect::<Vec<_>>();
        ranks.sort_unstable_by(|a, b| b.cmp(a));
        ranks
            .into_iter()
            .map(|(score, Reverse(id))| Hit {
                id,
                path: self.path(id),
                is_directory: self.kinds[id as usize] & 3 == 1,
                size: self.sizes[id as usize],
                mtime: self.mtimes[id as usize],
                score: score / 4,
                match_kind: if path_query {
                    "path"
                } else if score & 3 != 0 {
                    "pinyin"
                } else {
                    "name"
                },
            })
            .collect()
    }
}
