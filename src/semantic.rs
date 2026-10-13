//! Optional text embeddings over a read-only content index. No source-file body reads.
use crate::{WriterLock, content};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const DIM: usize = 768;
const MODEL: &str = "2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135";
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

struct Embedder {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl Drop for Embedder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Embedder {
    fn load(helper: &Path, model: &Path) -> Result<Self> {
        let mut child = Command::new(helper)
            .arg(model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut this = Self {
            child,
            input,
            output,
        };
        let ready = this.reply()?;
        ensure!(
            ready["dimensions"] == DIM && ready["ready"] == true,
            "embedding model failed to load"
        );
        Ok(this)
    }
    fn reply(&mut self) -> Result<Value> {
        let mut poll = libc::pollfd {
            fd: self.output.get_ref().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let started = Instant::now();
        while unsafe { libc::poll(&mut poll, 1, 100) } == 0 {
            ensure!(!STOP.load(Ordering::Relaxed), "stopping");
            ensure!(
                started.elapsed() < Duration::from_secs(20),
                "embedding worker timed out"
            );
        }
        let mut line = String::new();
        self.output.by_ref().take(128 * 1024).read_line(&mut line)?;
        ensure!(!line.is_empty(), "embedding worker exited");
        Ok(serde_json::from_str(&line)?)
    }
    fn encode(&mut self, text: &str) -> Result<Vec<f32>> {
        writeln!(self.input, "{}", json!({"texts":[text]}))?;
        self.input.flush()?;
        let response = self.reply()?;
        ensure!(response.get("error").is_none(), "{}", response["error"]);
        let vector: Vec<f32> = serde_json::from_value(response["vectors"][0].clone())?;
        ensure!(
            vector.len() == DIM && vector.iter().all(|x| x.is_finite()),
            "invalid embedding"
        );
        Ok(vector)
    }
}

#[derive(Default)]
struct Vectors {
    ids: Vec<i64>,
    values: Vec<f32>,
    positions: HashMap<i64, usize>,
}
impl Vectors {
    fn put(&mut self, id: i64, vector: &[f32]) {
        let slot = *self.positions.entry(id).or_insert_with(|| {
            self.ids.push(id);
            self.ids.len() - 1
        });
        if self.values.len() == slot * DIM {
            self.values.extend_from_slice(vector);
        } else {
            self.values[slot * DIM..(slot + 1) * DIM].copy_from_slice(vector);
        }
    }
    fn remove(&mut self, id: i64) {
        if let Some(slot) = self.positions.remove(&id) {
            let last = self.ids.len() - 1;
            self.ids.swap_remove(slot);
            if slot != last {
                self.values
                    .copy_within(last * DIM..(last + 1) * DIM, slot * DIM);
                self.positions.insert(self.ids[slot], slot);
            }
            self.values.truncate(last * DIM);
        }
    }
}
fn open(path: &Path, source: &Path) -> Result<Connection> {
    ensure!(path != source, "semantic and content databases must differ");
    let db = Connection::open(path)?;
    let app: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let tables: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table'",
        [],
        |r| r.get(0),
    )?;
    ensure!(app == 0x4c554d53 || tables == 0, "not a semantic database");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    db.busy_timeout(Duration::from_secs(1))?;
    db.execute_batch("PRAGMA application_id=1280658771; PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS vectors(id INTEGER PRIMARY KEY, fingerprint TEXT NOT NULL, vector BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS config(identity TEXT NOT NULL);")?;
    let uri = format!(
        "file:{}?mode=ro",
        source
            .canonicalize()?
            .to_string_lossy()
            .replace('%', "%25")
            .replace('?', "%3F")
            .replace('#', "%23")
    );
    db.execute("ATTACH DATABASE ?1 AS source", [uri])?;
    let root: String = db.query_row("SELECT root FROM source.settings", [], |r| r.get(0))?;
    ensure!(
        !path.starts_with(&root),
        "semantic database must be outside source root"
    );
    let identity = format!(
        "{}|{}|768|search-v1",
        source.canonicalize()?.display(),
        MODEL
    );
    let saved: Option<String> = db
        .query_row("SELECT identity FROM config", [], |r| r.get(0))
        .optional()?;
    if let Some(saved) = saved {
        ensure!(
            saved == identity,
            "index model or content source changed; choose a new semantic database"
        );
    } else {
        db.execute("INSERT INTO config VALUES(?1)", [identity])?;
    }
    Ok(db)
}
fn fresh(path: &str, fingerprint: &str) -> bool {
    content::fingerprint(Path::new(path)).is_ok_and(|(now, cloud, _)| !cloud && now == fingerprint)
}
fn prune(db: &Connection, vectors: &mut Vectors) -> Result<()> {
    let ids = db.prepare("SELECT v.id FROM vectors v LEFT JOIN source.chunks c ON c.id=v.id LEFT JOIN source.documents d ON d.path=c.path WHERE c.id IS NULL OR d.fingerprint!=v.fingerprint OR d.state NOT IN ('ready','partial_limit')")?.query_map([], |r| r.get::<_,i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        db.execute("DELETE FROM vectors WHERE id=?1", [id])?;
        vectors.remove(id);
    }
    Ok(())
}
fn search(
    db: &Connection,
    vectors: &Vectors,
    vector: &[f32],
    query: &str,
    hybrid: bool,
    limit: usize,
) -> Result<Value> {
    let mut scores: Vec<_> = vectors
        .values
        .chunks_exact(DIM)
        .zip(&vectors.ids)
        .map(|(v, id)| (*id, v.iter().zip(vector).map(|(a, b)| a * b).sum::<f32>()))
        .filter(|(_, score)| *score >= 0.55)
        .collect();
    scores.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    // Reject weak queries before fusion; cosine values are not probabilities.
    if scores.first().is_none_or(|(_, score)| *score < 0.74) {
        return Ok(json!({"results":[]}));
    }
    let expression = content::terms(&query.to_lowercase())
        .split_whitespace()
        .filter(|word| word.starts_with('b'))
        .take(64)
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut lexical = HashMap::new();
    if hybrid && !expression.is_empty() {
        let mut statement = db.prepare("SELECT f.rowid FROM source.content_fts f JOIN vectors v ON v.id=f.rowid WHERE content_fts MATCH ?1 AND length(v.vector)>0 ORDER BY bm25(content_fts) LIMIT 200")?;
        for (rank, row) in statement
            .query_map([expression], |r| r.get::<_, i64>(0))?
            .enumerate()
        {
            lexical.insert(row?, rank);
        }
    }
    let mut ranked: Vec<_> = scores
        .into_iter()
        .enumerate()
        .map(|(rank, (id, cosine))| {
            let score = 1.0 / (60.0 + rank as f32 + 1.0)
                + lexical
                    .get(&id)
                    .map_or(0.0, |lex| 1.0 / (60.0 + *lex as f32 + 1.0));
            (id, cosine, score)
        })
        .collect();
    ranked.sort_unstable_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stmt = db.prepare("SELECT c.path,c.locator,c.text,d.fingerprint FROM source.chunks c JOIN source.documents d ON d.path=c.path JOIN vectors v ON v.id=c.id WHERE c.id=?1 AND v.fingerprint=d.fingerprint AND d.state IN ('ready','partial_limit')")?;
    for (id, cosine, score) in ranked {
        let row = stmt
            .query_row([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .optional()?;
        if let Some((path, locator, text, fp)) = row {
            if seen.contains(&path) || !fresh(&path, &fp) {
                continue;
            }
            seen.insert(path.clone());
            results.push(json!({"path":path,"is_directory":false,"match_kind":"semantic","snippet":text.chars().take(200).collect::<String>(),"locator":locator,"score":(score*1_000_000.) as i64,"similarity":cosine}));
            if results.len() >= limit.clamp(1, 20) {
                break;
            }
        }
    }
    Ok(json!({"results":results}))
}

pub fn serve(
    path: &Path,
    source: &Path,
    helper: &Path,
    model: &Path,
    socket: &Path,
    mut paused: bool,
) -> Result<()> {
    content::guard_cloud()?;
    let hash = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(model)
        .output()?;
    ensure!(
        hash.status.success() && String::from_utf8_lossy(&hash.stdout).starts_with(MODEL),
        "select the verified EmbeddingGemma 2 Q8_0 model"
    );
    let _lock = WriterLock::acquire(path)?;
    let db = open(path, source)?;
    let mut vectors = Vectors::default();
    prune(&db, &mut vectors)?;
    for row in db
        .prepare("SELECT id,vector FROM vectors WHERE length(vector)>0")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?
    {
        let (id, bytes) = row?;
        ensure!(bytes.len() == DIM * 4, "invalid saved vector");
        let v: Vec<_> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        vectors.put(id, &v);
    }
    if socket.exists() {
        ensure!(
            UnixStream::connect(socket).is_err(),
            "semantic socket in use"
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
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    let mut worker: Option<Embedder> = None;
    let mut last_used = Instant::now();
    let mut next_work = Instant::now();
    let mut last_build = Instant::now();
    let mut pending = std::collections::VecDeque::new();
    let mut pending_refreshed = Instant::now();
    let mut last_error: Option<String> = None;
    let mut last_query: Option<(String, Vec<f32>)> = None;
    let result = (|| -> Result<()> {
        while !STOP.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
                    let response = (|| -> Result<Value> {
                        let mut line = String::new();
                        BufReader::new(&mut stream)
                            .take(65537)
                            .read_line(&mut line)?;
                        ensure!(line.len() <= 65536, "request too large");
                        let request: Value = serde_json::from_str(&line)?;
                        if let Some(value) =
                            request["paused"].as_bool().filter(|value| *value != paused)
                        {
                            paused = value;
                            next_work = Instant::now();
                        }
                        if let Some(query) = request["query"].as_str() {
                            ensure!(query.chars().count() <= 512, "query too long");
                            if query.trim().is_empty() || vectors.ids.is_empty() {
                                return Ok(json!({"results":[]}));
                            }
                            let begin = Instant::now();
                            if last_query.as_ref().is_none_or(|(q, _)| q != query) {
                                if worker.is_none() {
                                    worker = Some(Embedder::load(helper, model)?);
                                }
                                let vector = worker
                                    .as_mut()
                                    .unwrap()
                                    .encode(&format!("task: search result | query: {query}"))?;
                                last_query = Some((query.into(), vector));
                            }
                            last_used = Instant::now();
                            if last_build.elapsed() < Duration::from_secs(2) {
                                next_work = Instant::now() + Duration::from_millis(300);
                            }
                            let mut answer = search(
                                &db,
                                &vectors,
                                &last_query.as_ref().unwrap().1,
                                query,
                                request["mode"].as_str() != Some("semantic"),
                                request["limit"].as_u64().unwrap_or(10) as usize,
                            )?;
                            answer["elapsed_ms"] = json!(begin.elapsed().as_secs_f64() * 1000.);
                            return Ok(answer);
                        }
                        let total: i64 =
                            db.query_row("SELECT count(*) FROM source.chunks", [], |r| r.get(0))?;
                        let failed: i64 = db.query_row(
                            "SELECT count(*) FROM vectors WHERE length(vector)=0",
                            [],
                            |r| r.get(0),
                        )?;
                        Ok(
                            json!({"indexed":vectors.ids.len(),"total":total,"failed":failed,"paused":paused,"model_loaded":worker.is_some(),"error_message":last_error}),
                        )
                    })();
                    if response.is_err() {
                        worker = None;
                    }
                    let _ = writeln!(
                        stream,
                        "{}",
                        response.unwrap_or_else(|e| json!({"error":e.to_string()}))
                    );
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
            if worker.is_some() && last_used.elapsed() > Duration::from_secs(30) {
                worker = None;
            }
            if !paused && Instant::now() >= next_work {
                let begin = Instant::now();
                last_build = begin;
                let indexed = (|| -> Result<bool> {
                    if pending.is_empty() || pending_refreshed.elapsed() >= Duration::from_secs(10)
                    {
                        prune(&db, &mut vectors)?;
                        pending = db.prepare("SELECT c.id,c.path,c.text,d.fingerprint FROM source.chunks c JOIN source.documents d ON d.path=c.path LEFT JOIN vectors v ON v.id=c.id WHERE d.state IN ('ready','partial_limit') AND (v.id IS NULL OR v.fingerprint!=d.fingerprint) ORDER BY c.id DESC LIMIT 32")?
                            .query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?
                            .collect::<rusqlite::Result<std::collections::VecDeque<_>>>()?;
                        pending_refreshed = Instant::now();
                    }
                    let Some((id, path, text, fp)) = pending.pop_front() else {
                        return Ok(false);
                    };
                    if !fresh(&path, &fp) {
                        return Ok(true);
                    }
                    if worker.is_none() {
                        worker = Some(Embedder::load(helper, model)?);
                    }
                    let encoded = worker
                        .as_mut()
                        .unwrap()
                        .encode(&format!("title: none | text: {text}"));
                    last_used = Instant::now();
                    let vector = match encoded {
                        Ok(v) => v,
                        Err(e) if e.to_string().contains("exceeds 2048") => Vec::new(),
                        Err(e) => return Err(e),
                    };
                    let current: Option<String>=db.query_row("SELECT d.fingerprint FROM source.chunks c JOIN source.documents d ON d.path=c.path WHERE c.id=?1 AND c.text=?2 AND d.state IN ('ready','partial_limit')",params![id,text],|r|r.get(0)).optional()?;
                    if current.as_deref() != Some(&fp) || !fresh(&path, &fp) {
                        return Ok(true);
                    }
                    let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
                    db.execute(
                        "INSERT OR REPLACE INTO vectors VALUES(?1,?2,?3)",
                        params![id, fp, bytes],
                    )?;
                    if !vector.is_empty() {
                        vectors.put(id, &vector);
                    }
                    Ok(true)
                })();
                next_work = Instant::now()
                    + match indexed {
                        Ok(true) => {
                            last_error = None;
                            (begin.elapsed() * 3)
                                .clamp(Duration::from_millis(30), Duration::from_millis(500))
                        }
                        Ok(false) => Duration::from_secs(10),
                        Err(e) => {
                            last_error = Some(e.to_string());
                            worker = None;
                            pending.clear();
                            Duration::from_secs(10)
                        }
                    };
            }
            let mut poll = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut poll, 1, 20);
            }
        }
        Ok(())
    })();
    drop(worker);
    let _ = fs::remove_file(socket);
    result
}
