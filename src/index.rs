use crate::native::{self, Span};
use crate::paged::Paged;
use anyhow::{Result, ensure};
use rayon::prelude::*;
use serde::Deserialize;
use serde::Serialize;
use std::{
    borrow::Cow,
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

// A missing adjacent-byte bit proves a substring cannot match; collisions only add work.
fn signature(text: &str) -> u64 {
    text.as_bytes().windows(2).fold(0, |bits, pair| {
        let key = u32::from(pair[0]) << 8 | u32::from(pair[1]);
        bits | (1u64 << (key.wrapping_mul(0x9e3779b1) >> 26))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signature_filter_preserves_substrings_and_snapshot_isolation() {
        let mut index = Index::new("/synthetic".into(), 5000);
        for id in 0..5000 {
            let name = normalize(&format!("资料cafe\u{301}_report_{id}.pdf"));
            index
                .put(&Record {
                    id,
                    parent: 0,
                    name: name.as_str().into(),
                    folded: name.as_str().into(),
                    full: "ziliaoreport".into(),
                    initials: "zlr".into(),
                    kind: if id == 0 { 1 } else { 0 },
                    dev: 0,
                    ino: id as u64,
                    size: 0,
                    mtime: 0,
                })
                .unwrap();
        }
        let old = index.clone();
        let mut oracle = index.clone();
        for id in 0..oracle.slots() {
            oracle.signatures[id] = u64::MAX;
        }
        for mode in [MatchMode::Auto, MatchMode::Filename, MatchMode::Pinyin] {
            let options = QueryOptions {
                mode,
                limit: 50,
                ..Default::default()
            };
            for q in [
                "资",
                "资料",
                "料c",
                "CAFÉ",
                "cafe\u{301}",
                "é_r",
                "port",
                "_4096",
                "ilia",
                "zlr",
                "r",
                "pdf",
                "不存在",
                "-absent-",
            ] {
                let result = |index: &Index| {
                    index
                        .query(q, &options)
                        .into_iter()
                        .map(|h| (h.id, h.score))
                        .collect::<Vec<_>>()
                };
                assert_eq!(result(&index), result(&oracle), "{q}");
            }
        }
        index
            .put(&Record {
                id: 4096,
                parent: 0,
                name: "全新名字".into(),
                folded: "全新名字".into(),
                full: "quanxinmingzi".into(),
                initials: "qxmz".into(),
                kind: 0,
                dev: 0,
                ino: 4096,
                size: 9,
                mtime: 42,
            })
            .unwrap();
        let published = index.clone();
        index.kinds[4096] |= 128;
        for q in ["全新名字", "quanxinmingzi", "qxmz"] {
            assert!(
                old.query(
                    q,
                    &QueryOptions {
                        limit: 20,
                        ..Default::default()
                    }
                )
                .is_empty()
            );
            assert_eq!(
                published.query(
                    q,
                    &QueryOptions {
                        limit: 20,
                        ..Default::default()
                    }
                )[0]
                .id,
                4096
            );
            assert!(
                index
                    .query(
                        q,
                        &QueryOptions {
                            limit: 20,
                            ..Default::default()
                        }
                    )
                    .is_empty()
            );
        }
    }

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
                    name: name.as_str().into(),
                    folded: name.as_str().into(),
                    full: "".into(),
                    initials: "".into(),
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
        let mut cache = QueryCache::default();
        for query in [
            "a",
            "ar",
            "arch",
            "archive_target",
            "archive_target_69998",
            "target",
            "t",
            "ta",
            "not",
            "nothing",
        ] {
            for kind in [None, Some(0), Some(1)] {
                let options = QueryOptions {
                    limit: 20,
                    kind,
                    ..Default::default()
                };
                let actual = cache.query(&index, query, &options);
                let expected = index.query(query, &options);
                assert_eq!(
                    actual.iter().map(|h| (h.id, h.score)).collect::<Vec<_>>(),
                    expected.iter().map(|h| (h.id, h.score)).collect::<Vec<_>>(),
                    "{query}"
                );
            }
        }
        cache.clear();
        cache.query(
            &index,
            "archive_target",
            &QueryOptions {
                limit: 1,
                ..Default::default()
            },
        );
        let last = cache.query(
            &index,
            "archive_target_69998",
            &QueryOptions {
                limit: 1,
                ..Default::default()
            },
        );
        assert_eq!(
            last[0].id, 69_998,
            "retain candidates beyond the previous Top-K"
        );
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, clap::ValueEnum)]
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

#[derive(Default)]
pub(crate) struct QueryCache {
    query: String,
    mode: MatchMode,
    ids: Vec<u32>,
    pub examined: usize,
}

impl QueryCache {
    pub fn clear(&mut self) {
        self.query.clear();
        self.ids.clear();
    }

    pub fn query(&mut self, index: &Index, query: &str, options: &QueryOptions) -> Vec<Hit> {
        let q = normalize(query.trim());
        let reuse = !self.query.is_empty()
            && !q.contains('/')
            && options.mode == self.mode
            && q.starts_with(&self.query);
        let candidates = reuse.then_some(self.ids.as_slice());
        self.examined = candidates.map_or(index.slots(), <[u32]>::len);
        let mut matches = Vec::new();
        let result = index.query_inner(&q, options, candidates, Some(&mut matches));
        self.query = if q.contains('/') || options.limit == 0 {
            String::new()
        } else {
            q
        };
        self.mode = options.mode;
        self.ids = matches;
        result
    }
}

pub(crate) struct Record<'a> {
    pub id: u32,
    pub parent: u32,
    pub name: Cow<'a, str>,
    pub folded: Cow<'a, str>,
    pub full: Cow<'a, str>,
    pub initials: Cow<'a, str>,
    pub kind: u8,
    pub dev: i32,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Clone)]
pub struct Index {
    pub root: PathBuf,
    blob: Paged<u8, 65536>,
    signatures: Paged<u64>,
    names: Paged<Span>,
    folded: Paged<Span>,
    full: Paged<Span>,
    initials: Paged<Span>,
    pub(crate) parents: Paged<u32>,
    pub(crate) kinds: Paged<u8>,
    pub(crate) devices: Paged<i32>,
    pub(crate) inodes: Paged<u64>,
    pub(crate) sizes: Paged<u64>,
    pub(crate) mtimes: Paged<i64>,
}

impl Index {
    pub(crate) fn new(root: PathBuf, capacity: usize) -> Self {
        Self {
            root,
            blob: Paged::with_capacity(capacity.saturating_mul(32)),
            signatures: Paged::with_capacity(capacity),
            names: Paged::with_capacity(capacity),
            folded: Paged::with_capacity(capacity),
            full: Paged::with_capacity(capacity),
            initials: Paged::with_capacity(capacity),
            parents: Paged::with_capacity(capacity),
            kinds: Paged::with_capacity(capacity),
            devices: Paged::with_capacity(capacity),
            inodes: Paged::with_capacity(capacity),
            sizes: Paged::with_capacity(capacity),
            mtimes: Paged::with_capacity(capacity),
        }
    }
    fn text(&self, span: Span) -> &str {
        // Only validated UTF-8 names/keys are admitted to this owned byte pool.
        unsafe {
            std::str::from_utf8_unchecked(self.blob.slice(span.start as usize, span.len as usize))
        }
    }
    fn intern(&mut self, value: &str) -> Result<Span> {
        ensure!(
            self.blob
                .len()
                .checked_add(value.len() + 65536)
                .is_some_and(|n| n <= u32::MAX as usize),
            "name pool exceeds 4 GiB; split the index"
        );
        ensure!(value.len() <= 65536, "filename key exceeds 64 KiB");
        let span = Span {
            start: self.blob.append(value.as_bytes()) as u32,
            len: value.len() as u32,
        };
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
            let name = self.intern(&r.name)?;
            let folded = if r.name == r.folded {
                name
            } else {
                self.intern(&r.folded)?
            };
            let full = self.intern(&r.full)?;
            let initials = self.intern(&r.initials)?;
            (name, folded, full, initials)
        };
        let bits = if unchanged_keys {
            self.signatures[i]
        } else {
            signature(&r.folded) | signature(&r.full) | signature(&r.initials)
        };
        if i == self.kinds.len() {
            self.signatures.push(bits);
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
            if !unchanged_keys {
                self.signatures[i] = bits;
                self.names[i] = name;
                self.folded[i] = folded;
                self.full[i] = full;
                self.initials[i] = initials;
            }
            if self.parents[i] != r.parent {
                self.parents[i] = r.parent;
            }
            if self.kinds[i] != r.kind {
                self.kinds[i] = r.kind;
            }
            if self.devices[i] != r.dev {
                self.devices[i] = r.dev;
            }
            if self.inodes[i] != r.ino {
                self.inodes[i] = r.ino;
            }
            if self.sizes[i] != r.size {
                self.sizes[i] = r.size;
            }
            if self.mtimes[i] != r.mtime {
                self.mtimes[i] = r.mtime;
            }
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
            + self.signatures.capacity() * 8
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
        self.query_inner(query, options, None, None)
    }

    fn query_inner(
        &self,
        query: &str,
        options: &QueryOptions,
        candidates: Option<&[u32]>,
        matches: Option<&mut Vec<u32>>,
    ) -> Vec<Hit> {
        let q = normalize(query.trim());
        if q.is_empty() || options.limit == 0 {
            return Vec::new();
        }
        let path_query = q.contains('/');
        let count = candidates.map_or(self.slots(), <[u32]>::len);
        let mut ids = vec![0u32; count];
        let mut scores = vec![0i32; count];
        let ranges = if path_query {
            // Normalize each distinct parent once, rather than allocating a path per file.
            let (prefix, final_component) = q.rsplit_once('/').unwrap();
            let suffix = format!("{prefix}/");
            let mut parents = vec![0u8; self.slots()];
            let mut count = 0;
            for (page, ((kinds, ancestors), names)) in self
                .kinds
                .chunks()
                .zip(self.parents.chunks())
                .zip(self.folded.chunks())
                .enumerate()
            {
                for (slot, &kind) in kinds.iter().enumerate() {
                    let i = page * 4096 + slot;
                    if kind & 128 != 0 {
                        continue;
                    }
                    if i == 0 {
                        if normalize(&self.root.to_string_lossy()).contains(&q) {
                            ids[count] = 0;
                            scores[count] = 1000;
                            count += 1;
                        }
                        continue;
                    }
                    let p = ancestors[slot] as usize;
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
                            && self.text(names[slot]).starts_with(final_component))
                    {
                        ids[count] = i as u32;
                        scores[count] = 1000;
                        count += 1;
                    }
                }
            }
            std::iter::once(0..count).collect::<Vec<_>>()
        } else {
            let blob_pages = self.blob.pointers();
            let name_pages = self.folded.pointers();
            let full_pages = self.full.pointers();
            let initial_pages = self.initials.pointers();
            let kind_pages = self.kinds.pointers();
            let signature_pages = self.signatures.pointers();
            let required = signature(&q);
            let scan = |start: usize, out: &mut [u32], ranks: &mut [i32]| unsafe {
                // Read-only index spans and disjoint writable slices stay alive until C returns.
                native::lumi_search(
                    blob_pages.as_ptr(),
                    name_pages.as_ptr(),
                    full_pages.as_ptr(),
                    initial_pages.as_ptr(),
                    kind_pages.as_ptr(),
                    signature_pages.as_ptr(),
                    required,
                    candidates.map_or(std::ptr::null(), <[u32]>::as_ptr),
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
            if count >= 65_536
                && let Some(pool) =
                    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(4).build().ok())
            {
                let chunk = count.div_ceil(4);
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
        if let Some(matches) = matches {
            matches.extend(
                ranges
                    .iter()
                    .flat_map(|range| range.clone().map(|j| ids[j])),
            );
        }
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
