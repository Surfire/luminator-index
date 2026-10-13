use crate::{Catalog, WriterLock, index::normalize, native};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::os::darwin::fs::MetadataExt as DarwinMetadataExt;
use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
unsafe extern "C" {
    fn lumi_disable_materialization() -> i32;
}
fn guard_cloud() -> Result<()> {
    ensure!(
        unsafe { lumi_disable_materialization() } == 0,
        "cannot prohibit cloud downloads"
    );
    unsafe {
        libc::umask(0o077);
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct Part {
    locator: String,
    text: String,
}
#[derive(Deserialize)]
struct Extracted {
    state: String,
    parts: Vec<Part>,
}

fn connect(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    let tables: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    let app_id: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    ensure!(
        tables == 0 || app_id == 0x4c554d43,
        "not a Luminator content database"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    db.busy_timeout(Duration::from_secs(2))?;
    db.execute_batch("PRAGMA application_id=1280658755; PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
        CREATE TABLE IF NOT EXISTS settings(root TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS documents(path TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, state TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS chunks(id INTEGER PRIMARY KEY, path TEXT NOT NULL REFERENCES documents(path) ON DELETE CASCADE, locator TEXT NOT NULL, text TEXT NOT NULL, normalized TEXT NOT NULL, tokens TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);
        CREATE VIRTUAL TABLE IF NOT EXISTS content_fts USING fts5(tokens,content='chunks',content_rowid='id',detail=none,columnsize=0);
        CREATE TRIGGER IF NOT EXISTS chunk_insert AFTER INSERT ON chunks BEGIN INSERT INTO content_fts(rowid,tokens) VALUES(new.id,new.tokens); END;
        CREATE TRIGGER IF NOT EXISTS chunk_delete AFTER DELETE ON chunks BEGIN INSERT INTO content_fts(content_fts,rowid,tokens) VALUES('delete',old.id,old.tokens); END;")?;
    Ok(db)
}

pub fn root_from_names(names: &Path) -> Result<PathBuf> {
    Ok(Catalog::open(names, false)?
        .meta("root")?
        .context("name index has no root")?
        .into())
}

fn bind(db: &Connection, root: &Path, destination: &Path) -> Result<()> {
    ensure!(
        !destination.starts_with(root),
        "content database must be outside the source root"
    );
    let saved: Option<String> = db
        .query_row("SELECT root FROM settings", [], |r| r.get(0))
        .optional()?;
    if let Some(saved) = saved {
        ensure!(
            Path::new(&saved) == root,
            "content database belongs to another root"
        );
    } else {
        db.execute(
            "INSERT INTO settings VALUES(?1)",
            [root.to_string_lossy().as_ref()],
        )?;
    }
    Ok(())
}

fn compact(s: &str) -> String {
    normalize(s)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}
fn terms(s: &str) -> String {
    let chars: Vec<_> = s.chars().collect();
    let mut words = BTreeSet::new();
    for c in &chars {
        words.insert(format!("u{:x}", *c as u32));
    }
    for pair in chars.windows(2) {
        words.insert(format!("b{:x}x{:x}", pair[0] as u32, pair[1] as u32));
    }
    words.into_iter().collect::<Vec<_>>().join(" ")
}
fn fingerprint(path: &Path) -> Result<(String, bool, u64)> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_file() && !m.file_type().is_symlink(),
        "not a regular file"
    );
    Ok((
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            m.dev(),
            m.ino(),
            m.size(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
            m.st_flags()
        ),
        m.st_flags() & 0x40000000 != 0,
        m.size(),
    ))
}

fn extract(path: &Path, helper: &Path, output: &Path) -> Result<Extracted> {
    if matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "txt" | "md"
    ) {
        let mut bytes = Vec::new();
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 8 * 1024 * 1024, "text exceeds limit");
        let text = String::from_utf8(bytes).context("not UTF-8")?;
        return Ok(Extracted {
            state: if text.trim().is_empty() {
                "no_text"
            } else {
                "ready"
            }
            .into(),
            parts: vec![Part {
                locator: "text".into(),
                text,
            }],
        });
    }
    let stdout = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(output)?;
    let mut child = Command::new(helper)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    let outcome = loop {
        if let Some(status) = child.try_wait()? {
            break status.success();
        }
        if STOP.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(30) {
            child.kill()?;
            child.wait()?;
            break false;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let bytes = fs::read(output);
    let _ = fs::remove_file(output);
    ensure!(outcome, "extractor stopped or timed out");
    Ok(serde_json::from_slice(&bytes?)?)
}

fn store(db: &Connection, path: &str, fp: &str, extracted: &Extracted) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    tx.execute("INSERT INTO documents VALUES(?1,?2,?3) ON CONFLICT(path) DO UPDATE SET fingerprint=excluded.fingerprint,state=excluded.state",params![path,fp,extracted.state])?;
    tx.execute("DELETE FROM chunks WHERE path=?1", [path])?;
    for part in &extracted.parts {
        let normalized_part = normalize(&part.text);
        let chars: Vec<_> = normalized_part
            .char_indices()
            .filter(|(_, c)| !c.is_whitespace())
            .collect();
        let mut start = 0;
        while start < chars.len() {
            let end = (start + 1024).min(chars.len());
            let byte_end = if end == chars.len() {
                normalized_part.len()
            } else {
                chars[end].0
            };
            let text = &normalized_part[chars[start].0..byte_end];
            let normalized = compact(text);
            if !normalized.is_empty() {
                tx.execute("INSERT INTO chunks(path,locator,text,normalized,tokens) VALUES(?1,?2,?3,?4,?5)",params![path,format!("{}:char:{}",part.locator,start),text,normalized,terms(&normalized)])?;
            }
            if end == chars.len() {
                break;
            }
            start = end - 128;
        }
    }
    tx.commit()?;
    Ok(())
}

fn scan_into(db: &Connection, root: &Path, helper: &Path, output: &Path) -> Result<Value> {
    let started = Instant::now();
    let mut pending = vec![root.to_path_buf()];
    let mut seen = HashSet::new();
    let (mut cloud_dirs, mut skipped_dirs, mut unsupported, mut updated) = (0, 0, 0, 0);
    while let Some(dir) = pending.pop() {
        if STOP.load(Ordering::Relaxed) {
            break;
        }
        let Ok(metadata) = fs::symlink_metadata(&dir) else {
            skipped_dirs += 1;
            continue;
        };
        if metadata.st_flags() & 0x40000000 != 0 {
            cloud_dirs += 1;
            continue;
        }
        let mut files = Vec::new();
        match native::read_dir(&dir, |name, entry, dataless| {
            let path = dir.join(name);
            if name.starts_with('.') {
                return Ok(());
            }
            if entry.kind == 1 {
                if dataless {
                    cloud_dirs += 1;
                } else if !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("app" | "bundle" | "framework")
                ) {
                    pending.push(path);
                }
            } else if entry.kind == 0 {
                if matches!(
                    path.extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .as_str(),
                    "txt" | "md" | "pdf" | "docx"
                ) {
                    files.push(path);
                } else {
                    unsupported += 1;
                }
            }
            Ok(())
        }) {
            Ok(skipped) => skipped_dirs += usize::from(skipped > 0),
            Err(_) => {
                skipped_dirs += 1;
                continue;
            }
        }
        for path in files {
            if STOP.load(Ordering::Relaxed) {
                break;
            }
            let key = path.to_string_lossy().to_string();
            seen.insert(key.clone());
            let Ok((fp, cloud, size)) = fingerprint(&path) else {
                continue;
            };
            let prior: Option<(String, String)> = db
                .query_row(
                    "SELECT fingerprint,state FROM documents WHERE path=?1",
                    [&key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if prior
                .as_ref()
                .is_some_and(|(p, state)| p == &fp && state != "pending")
            {
                continue;
            }
            let mut result = Extracted {
                state: if cloud {
                    "cloud_only"
                } else if size
                    > if matches!(
                        path.extension()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .as_str(),
                        "txt" | "md"
                    ) {
                        8 * 1024 * 1024
                    } else {
                        64 * 1024 * 1024
                    }
                {
                    "too_large"
                } else {
                    "pending"
                }
                .into(),
                parts: vec![],
            };
            store(db, &key, &fp, &result)?;
            if result.state == "pending" {
                if !path.canonicalize().is_ok_and(|p| p.starts_with(root)) {
                    continue;
                }
                result = extract(&path, helper, output).unwrap_or(Extracted {
                    state: "unreadable".into(),
                    parts: vec![],
                });
                if STOP.load(Ordering::Relaxed) {
                    break;
                }
                if !fingerprint(&path).is_ok_and(|(now, cloud, _)| now == fp && !cloud) {
                    continue;
                }
                store(db, &key, &fp, &result)?;
                updated += 1;
            }
        }
    }
    if !STOP.load(Ordering::Relaxed) {
        let paths: Vec<String> = db
            .prepare("SELECT path FROM documents")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let tx = db.unchecked_transaction()?;
        for path in paths {
            if !seen.contains(&path)
                && fs::symlink_metadata(&path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                tx.execute("DELETE FROM documents WHERE path=?1", [path])?;
            }
        }
        tx.commit()?;
    }
    Ok(
        json!({"updated":updated,"cloud_directories":cloud_dirs,"unreadable_directories":skipped_dirs,"unsupported":unsupported,"elapsed_ms":started.elapsed().as_secs_f64()*1000.}),
    )
}

pub fn scan(path: &Path, root: &Path, helper: &Path, enabled: bool) -> Result<Value> {
    ensure!(
        enabled,
        "content is off; pass --enable to authorize local content indexing"
    );
    guard_cloud()?;
    let canonical_root = root.canonicalize()?;
    let root = canonical_root.as_path();
    let destination = path
        .parent()
        .context("database has no parent")?
        .canonicalize()?
        .join(path.file_name().context("database has no name")?);
    ensure!(
        !destination.starts_with(root),
        "content database must be outside the source root"
    );
    let _lock = WriterLock::acquire(path)?;
    let db = connect(path)?;
    bind(&db, root, path)?;
    let scan = scan_into(&db, root, helper, &path.with_extension("extract.json"))?;
    Ok(json!({"scan":scan,"status":status(&db)?}))
}
fn status(db: &Connection) -> Result<Value> {
    let rows: Vec<(String, i64)> = db
        .prepare("SELECT state,count(*) FROM documents GROUP BY state")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let chunks: i64 = db.query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?;
    Ok(
        json!({"states":rows.into_iter().collect::<std::collections::BTreeMap<_,_>>(),"chunks":chunks}),
    )
}
fn excerpt(text: &str, query: &str) -> String {
    let normalized = compact(text);
    let offset = normalized.find(query).unwrap_or(0);
    let at = normalized[..offset].chars().count();
    let chars: Vec<_> = text
        .char_indices()
        .filter(|(_, c)| !c.is_whitespace())
        .collect();
    let start = at.saturating_sub(60);
    let end = (at + query.chars().count() + 80).min(chars.len());
    let byte_end = if end == chars.len() {
        text.len()
    } else {
        chars[end].0
    };
    format!(
        "{}{}",
        if start > 0 { "…" } else { "" },
        &text[chars[start].0..byte_end]
    )
}

pub fn query(path: &Path, query: Option<&str>, limit: usize) -> Result<Value> {
    guard_cloud()?;
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(2))?;
    let Some(query) = query else {
        return status(&db);
    };
    let started = Instant::now();
    let q = compact(query);
    ensure!(
        q.chars().count() <= 128,
        "content query exceeds 128 characters"
    );
    if q.is_empty() || limit == 0 {
        return Ok(json!({"results":[],"elapsed_ms":0}));
    }
    let expression = terms(&q)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" AND ");
    let mut stmt = db.prepare("SELECT d.path,d.fingerprint,c.locator,c.text FROM content_fts JOIN chunks c ON c.id=content_fts.rowid JOIN documents d ON d.path=c.path WHERE content_fts MATCH ?1 AND instr(c.normalized,?2)>0 AND d.state IN ('ready','partial_limit') ORDER BY d.path,c.id")?;
    let mut rows = stmt.query(params![expression, q])?;
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    while let Some(r) = rows.next()? {
        let path: String = r.get(0)?;
        let fp: String = r.get(1)?;
        if !seen.insert(path.clone())
            || !fingerprint(Path::new(&path)).is_ok_and(|(now, cloud, _)| now == fp && !cloud)
        {
            continue;
        }
        results.push(json!({"path":path,"is_directory":false,"match_kind":"content","locator":r.get::<_,String>(2)?,"snippet":excerpt(&r.get::<_,String>(3)?, &q),"score":1000}));
        if results.len() >= limit.min(100) {
            break;
        }
    }
    Ok(json!({"results":results,"elapsed_ms":started.elapsed().as_secs_f64()*1000.}))
}

pub fn serve(path: &Path, root: &Path, helper: &Path, socket: &Path, enabled: bool) -> Result<()> {
    ensure!(enabled, "content is off; pass --enable");
    guard_cloud()?;
    let canonical_root = root.canonicalize()?;
    let root = canonical_root.as_path();
    let destination = path
        .parent()
        .context("database has no parent")?
        .canonicalize()?
        .join(path.file_name().context("database has no name")?);
    ensure!(
        !destination.starts_with(root),
        "content database must be outside the source root"
    );
    let _lock = WriterLock::acquire(path)?;
    let db = connect(path)?;
    bind(&db, root, path)?;
    drop(db);
    ensure!(
        !socket.starts_with(root),
        "socket must be outside source root"
    );
    if socket.exists() {
        ensure!(
            UnixStream::connect(socket).is_err(),
            "content socket in use"
        );
        fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    STOP.store(false, Ordering::Relaxed);
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let result = thread::scope(|scope| -> Result<()> {
        let worker = scope.spawn(|| -> Result<()> {
            let db = connect(path)?;
            while !STOP.load(Ordering::Relaxed) {
                let stats = scan_into(&db, root, helper, &path.with_extension("extract.json"))?;
                eprintln!("{}", stats);
                for _ in 0..50 {
                    if STOP.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
            Ok(())
        });
        let queries = (|| -> Result<()> {
            while !STOP.load(Ordering::Relaxed) && !worker.is_finished() {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false)?;
                        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
                        let answer = (|| -> Result<Value> {
                            let mut line = String::new();
                            BufReader::new(&mut stream)
                                .take(65537)
                                .read_line(&mut line)?;
                            ensure!(line.len() <= 65536, "request too large");
                            let request: Value = serde_json::from_str(&line)?;
                            query(
                                path,
                                request["query"].as_str(),
                                request["limit"].as_u64().unwrap_or(20) as usize,
                            )
                        })();
                        let reply =
                            answer.unwrap_or_else(|_| json!({"error":"content query failed"}));
                        let _ = writeln!(stream, "{}", reply);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        let mut poll = libc::pollfd {
                            fd: std::os::fd::AsRawFd::as_raw_fd(&listener),
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        unsafe {
                            libc::poll(&mut poll, 1, 100);
                        }
                    }
                    Err(e) => {
                        STOP.store(true, Ordering::Relaxed);
                        return Err(e.into());
                    }
                }
            }
            Ok(())
        })();
        STOP.store(true, Ordering::Relaxed);
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("content worker failed"))??;
        queries
    });
    let _ = fs::remove_file(socket);
    result
}
