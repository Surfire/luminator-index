use crate::{
    index::{Index, Record, normalize},
    native::{self, Dirent, Pinyin},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub struct Catalog {
    conn: Connection,
}
pub struct WriterLock(File);
impl WriterLock {
    pub fn acquire(db: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(db.with_extension("lock"))?;
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another scan/serve process owns this index"
        );
        Ok(Self(file))
    }
}
impl Drop for WriterLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[derive(Default, Debug, Serialize)]
pub struct UpdateStats {
    pub directories: u64,
    pub added: u64,
    pub changed: u64,
    pub removed: u64,
    pub skipped: u64,
    pub cloud_directories: u64,
    pub elapsed_ms: f64,
}

impl Catalog {
    pub fn open(db: &Path, write: bool) -> Result<Self> {
        let conn = if write {
            Connection::open(db)?
        } else {
            Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?
        };
        conn.busy_timeout(Duration::from_secs(5))?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            version == 1 || (write && version == 0),
            "unsupported index version"
        );
        if write {
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
                CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS files(id INTEGER PRIMARY KEY,parent INTEGER NOT NULL,name TEXT NOT NULL,
                    folded TEXT NOT NULL,full TEXT NOT NULL,initials TEXT NOT NULL,kind INTEGER NOT NULL,
                    dev INTEGER NOT NULL,ino INTEGER NOT NULL,size INTEGER NOT NULL,mtime INTEGER NOT NULL,live INTEGER NOT NULL);
                CREATE INDEX IF NOT EXISTS children ON files(parent);
                CREATE INDEX IF NOT EXISTS identity ON files(dev,ino);
                PRAGMA user_version=1;")?;
        }
        ensure!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))? == 1,
            "unsupported index version"
        );
        Ok(Self { conn })
    }
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }
    fn set_meta(&self, key: &str, value: impl ToString) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value.to_string()],
        )?;
        Ok(())
    }
    pub fn cursor(&self) -> Result<u64> {
        Ok(self
            .meta("cursor")?
            .unwrap_or_default()
            .parse()
            .unwrap_or(0))
    }
    pub fn checkpoint_cursor(&self, cursor: u64) -> Result<()> {
        self.set_meta("cursor", cursor)
    }
    pub fn load(&self) -> Result<Index> {
        let root = self
            .meta("root")?
            .context("index is empty; run scan first")?;
        let count = self
            .conn
            .query_row("SELECT count(*) FROM files", [], |r| r.get::<_, u32>(0))?
            as usize;
        let mut index = Index::new(root.into(), count);
        let mut stmt = self.conn.prepare("SELECT id,parent,name,folded,full,initials,kind,dev,ino,size,mtime,live FROM files ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let (name, folded, full, initials): (String, String, String, String) =
                (r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?);
            let kind: u8 = r.get(6)?;
            let live: bool = r.get(11)?;
            let id: u32 = r.get(0)?;
            let parent: u32 = r.get(1)?;
            ensure!(
                (parent as usize) < count && (id == 0 || parent != id),
                "invalid parent in index"
            );
            index.put(&Record {
                id,
                parent,
                name: &name,
                folded: &folded,
                full: &full,
                initials: &initials,
                kind: kind | if live { 0 } else { 128 },
                dev: r.get(7)?,
                ino: r.get::<_, i64>(8)? as u64,
                size: r.get::<_, i64>(9)? as u64,
                mtime: r.get(10)?,
            })?;
        }
        Ok(index)
    }
    fn write(&self, index: &mut Index, r: &Record<'_>) -> Result<bool> {
        let i = r.id as usize;
        if i < index.slots()
            && index.kinds[i] == r.kind
            && index.parents[i] == r.parent
            && index.name(r.id) == r.name
            && index.devices[i] == r.dev
            && index.inodes[i] == r.ino
            && index.sizes[i] == r.size
            && index.mtimes[i] == r.mtime
        {
            return Ok(false);
        }
        self.conn.prepare_cached("INSERT INTO files VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1)
            ON CONFLICT(id) DO UPDATE SET parent=excluded.parent,name=excluded.name,folded=excluded.folded,
            full=excluded.full,initials=excluded.initials,kind=excluded.kind,dev=excluded.dev,ino=excluded.ino,
            size=excluded.size,mtime=excluded.mtime,live=1")?
            .execute(params![r.id,r.parent,r.name,r.folded,r.full,r.initials,r.kind,r.dev,r.ino as i64,r.size as i64,r.mtime])?;
        index.put(r)?;
        Ok(true)
    }
    fn remove_tree(&self, index: &mut Index, id: u32) -> Result<u64> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE tree(id) AS (SELECT ?1 UNION ALL
            SELECT f.id FROM files f JOIN tree t ON f.parent=t.id WHERE f.id<>t.id)
            UPDATE files SET live=0 WHERE live=1 AND id IN tree RETURNING id",
        )?;
        let mut rows = stmt.query([id])?;
        let mut removed = 0;
        while let Some(r) = rows.next()? {
            let i = r.get::<_, u32>(0)? as usize;
            index.kinds[i] |= 128;
            removed += 1;
        }
        Ok(removed)
    }
    fn reusable_id(&self, index: &Index, entry: Dirent) -> Result<Option<u32>> {
        let mut stmt=self.conn.prepare_cached("SELECT id FROM files WHERE dev=?1 AND ino=?2 AND (kind & 3)=?3 ORDER BY live DESC,id DESC")?;
        let mut rows = stmt.query(params![entry.dev, entry.ino as i64, entry.kind])?;
        while let Some(row) = rows.next()? {
            let id: u32 = row.get(0)?;
            if id != 0
                && (index.kinds[id as usize] & 128 != 0
                    || fs::symlink_metadata(index.path(id))
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound))
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
    fn sync_tree(
        &self,
        index: &mut Index,
        scope: (PathBuf, u32),
        initial: bool,
        recursive: bool,
        pinyin: &Pinyin,
        stats: &mut UpdateStats,
    ) -> Result<()> {
        let mut pending = vec![(scope.0, scope.1, recursive)];
        while let Some((path, parent, recursive)) = pending.pop() {
            let mut old: HashMap<String, u32> = HashMap::new();
            if !initial {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT name,id FROM files WHERE parent=?1 AND id<>?1 AND live=1",
                )?;
                for row in stmt.query_map([parent], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))
                })? {
                    let (n, id) = row?;
                    old.insert(n, id);
                }
            }
            let result = native::read_dir(&path, |name, e, dataless| {
                let existing = old.remove(name);
                // An entry replaced at the same name with a different inode is a new file.
                let same = existing.filter(|&id| {
                    index.devices[id as usize] == e.dev
                        && index.inodes[id as usize] == e.ino
                        && index.kinds[id as usize] & 3 == e.kind as u8
                });
                if let Some(id) = existing.filter(|_| same.is_none()) {
                    stats.removed += self.remove_tree(index, id)?;
                }
                let reused = if same.is_none() && !initial {
                    self.reusable_id(index, e)?
                } else {
                    None
                };
                let id = match same.or(reused) {
                    Some(id) => id,
                    None => u32::try_from(index.slots()).context("too many file slots")?,
                };
                let is_new = id as usize == index.slots();
                let revived = !is_new && index.kinds[id as usize] & 128 != 0;
                if reused.is_some() {
                    old.retain(|_, old_id| *old_id != id);
                }
                let folded = normalize(name);
                let (full, initials) = pinyin.keys(&folded);
                if self.write(
                    index,
                    &Record {
                        id,
                        parent,
                        name,
                        folded: &folded,
                        full: &full,
                        initials: &initials,
                        kind: e.kind as u8 | if dataless { 4 } else { 0 },
                        dev: e.dev,
                        ino: e.ino,
                        size: e.size,
                        mtime: e.mtime,
                    },
                )? {
                    if is_new {
                        stats.added += 1;
                    } else {
                        stats.changed += 1;
                    }
                }
                if e.kind == 1 {
                    if dataless {
                        stats.cloud_directories += 1;
                    } else if recursive || is_new || revived {
                        pending.push((path.join(name), id, true));
                    }
                }
                Ok(())
            });
            match result {
                Ok(skipped) => {
                    stats.directories += 1;
                    stats.skipped += skipped;
                    // Enumeration errors must not turn unseen, previously indexed files into deletions.
                    if skipped == 0 {
                        for id in old.values() {
                            stats.removed += self.remove_tree(index, *id)?;
                        }
                    }
                }
                Err(e) => {
                    if !path.exists() {
                        stats.removed += self.remove_tree(index, parent)?;
                    } else {
                        stats.skipped += 1;
                        eprintln!("enumeration skipped: {e:#}");
                    }
                }
            }
        }
        Ok(())
    }
    pub fn scan(&self, root: &Path) -> Result<(Index, UpdateStats)> {
        let started = Instant::now();
        let root = root.canonicalize()?;
        ensure!(root.is_dir(), "root must be a directory");
        let metadata = fs::metadata(&root)?;
        let uuid = native::volume_uuid(metadata.dev() as i32)?;
        let saved_root = self.meta("root")?;
        ensure!(
            saved_root.as_ref().is_none_or(|r| Path::new(r) == root),
            "this index belongs to another root; use a separate database"
        );
        let initial = saved_root.is_none();
        let mut index = if initial {
            Index::new(root.clone(), 1024)
        } else {
            self.load()?
        };
        let pinyin = Pinyin::new()?;
        let cursor = native::event_id();
        let tx = self.conn.unchecked_transaction()?;
        self.set_meta("root", root.to_str().context("root is not UTF-8")?)?;
        self.set_meta("uuid", uuid)?;
        self.write(
            &mut index,
            &Record {
                id: 0,
                parent: 0,
                name: "",
                folded: "",
                full: "",
                initials: "",
                kind: 1,
                dev: metadata.dev() as i32,
                ino: metadata.ino(),
                size: 0,
                mtime: metadata.mtime(),
            },
        )?;
        let mut stats = UpdateStats::default();
        self.sync_tree(&mut index, (root, 0), initial, true, &pinyin, &mut stats)?;
        self.set_meta("cursor", cursor)?;
        self.set_meta("skipped", stats.skipped)?;
        self.set_meta("cloud_directories", stats.cloud_directories)?;
        tx.commit()?;
        stats.elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
        Ok((index, stats))
    }
    pub(crate) fn apply_events(
        &self,
        index: &mut Index,
        events: &[native::Event],
        pinyin: &Pinyin,
    ) -> Result<UpdateStats> {
        let started = Instant::now();
        let mut dirs = HashSet::new();
        let mut cursor = self.cursor()?;
        let mut full = false;
        for e in events {
            cursor = cursor.max(e.id);
            if e.flags & (1 | 2 | 4 | 8 | 32 | 64 | 128) != 0 {
                full = true;
            }
            if e.flags == 16 {
                continue;
            } // HistoryDone only.
            if !e.path.starts_with(&index.root) {
                continue;
            }
            if e.path.is_dir() {
                dirs.insert(e.path.clone());
            }
            if let Some(parent) = e.path.parent().filter(|p| p.starts_with(&index.root)) {
                dirs.insert(parent.to_path_buf());
            }
        }
        let mut stats = UpdateStats::default();
        let tx = self.conn.unchecked_transaction()?;
        if full {
            self.sync_tree(
                index,
                (index.root.clone(), 0),
                false,
                true,
                pinyin,
                &mut stats,
            )?;
        } else {
            let mut dirs: Vec<_> = dirs.into_iter().collect();
            dirs.sort_by_key(|p| p.components().count());
            for path in dirs {
                if let Ok(meta) = fs::symlink_metadata(&path) {
                    if !meta.is_dir() {
                        continue;
                    }
                    let id:Option<u32>=self.conn.query_row("SELECT id FROM files WHERE dev=?1 AND ino=?2 AND (kind & 3)=1 AND live=1 LIMIT 1",
                        params![meta.dev() as i32,meta.ino() as i64],|r|r.get(0)).optional()?;
                    if let Some(id) = id {
                        self.sync_tree(index, (path, id), false, false, pinyin, &mut stats)?;
                    }
                }
            }
        }
        self.set_meta("cursor", cursor)?;
        self.set_meta("skipped", stats.skipped)?;
        self.set_meta("cloud_directories", stats.cloud_directories)?;
        tx.commit()?;
        stats.elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
        Ok(stats)
    }
    pub(crate) fn history_valid(&self, index: &Index) -> Result<bool> {
        let meta = fs::metadata(&index.root)?;
        Ok(
            self.meta("uuid")?.as_deref() == Some(native::volume_uuid(meta.dev() as i32)?.as_str())
                && self.cursor()? <= native::event_id(),
        )
    }
}
