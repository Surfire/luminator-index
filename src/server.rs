use crate::{
    Catalog, Index, MatchMode, QueryOptions, WriterLock, footprint, footprint_peak,
    index::{QueryCache, Record},
    native::{Pinyin, Watcher},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Default, Deserialize, Serialize)]
pub struct Request {
    pub query: Option<String>,
    pub limit: Option<usize>,
    pub kind: Option<u8>,
    pub extension: Option<String>,
    pub under: Option<PathBuf>,
    #[serde(default)]
    pub mode: MatchMode,
}

pub fn answer(index: &Index, request: Request) -> Value {
    answer_cached(index, request, &mut QueryCache::default())
}

fn answer_cached(index: &Index, request: Request, cache: &mut QueryCache) -> Value {
    if let Some(query) = request.query {
        let start = Instant::now();
        let results = cache.query(
            index,
            &query,
            &QueryOptions {
                limit: request.limit.unwrap_or(20),
                kind: request.kind,
                extension: request.extension,
                under: request.under,
                mode: request.mode,
            },
        );
        json!({"results":results,"elapsed_ms":start.elapsed().as_secs_f64()*1000.,"examined_candidates":cache.examined})
    } else {
        json!({"entries":index.entries(),"slots":index.slots(),"index_allocated_bytes":index.allocated_bytes(),
            "name_pool_bytes":index.name_bytes(),"physical_footprint_bytes":footprint(),
            "physical_footprint_peak_bytes":footprint_peak(),
            "cloud_directories":index.kinds.iter().filter(|k|**k&135==5).count()})
    }
}

pub fn request(socket: &Path, request: &Request) -> Result<Value> {
    let mut stream = UnixStream::connect(socket).context("cannot connect to daemon")?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).take(64 << 20).read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line)?;
    ensure!(
        reply.get("error").is_none(),
        "daemon error: {}",
        reply["error"]
    );
    Ok(reply)
}

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
struct SocketFile(PathBuf);
impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn serve(db: &Path, socket: &Path) -> Result<()> {
    let _lock = WriterLock::acquire(db)?;
    let catalog = Catalog::open(db, true)?;
    let start = Instant::now();
    let mut index = catalog.load()?;
    let restore_ms = start.elapsed().as_secs_f64() * 1000.;
    ensure!(
        !db.canonicalize()?.starts_with(&index.root),
        "database must live outside the indexed root"
    );
    ensure!(
        !socket.starts_with(&index.root),
        "socket must live outside the indexed root"
    );
    if socket.exists() {
        ensure!(
            UnixStream::connect(socket).is_err(),
            "socket already has a server"
        );
        ensure!(
            fs::symlink_metadata(socket)?.file_type().is_socket(),
            "refusing to remove a non-socket path"
        );
        fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    let _socket = SocketFile(socket.to_path_buf());
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let pinyin = Pinyin::new()?;
    if !catalog.history_valid(&index)? {
        index = catalog.scan(&index.root)?.0;
    }
    STOP.store(false, Ordering::Relaxed);
    unsafe {
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
    }
    eprintln!(
        "{}",
        json!({"ready":true,"entries":index.entries(),"restore_ms":restore_ms,"startup_ms":start.elapsed().as_secs_f64()*1000.})
    );
    // Copy once at startup; subsequent publications contain only committed changed records.
    let mut writer_index = index.clone();
    let (send, receive) = mpsc::sync_channel(1);
    thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let result = (|| -> Result<()> {
                let watcher = Watcher::start(&writer_index.root, catalog.cursor()?)?;
                while !STOP.load(Ordering::Relaxed) {
                    let first = match watcher.events.recv_timeout(Duration::from_millis(20)) {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(error) => return Err(error.into()),
                    };
                    let mut events = vec![first];
                    events.extend(watcher.events.try_iter().take(19_999));
                    let stats = catalog.apply_events(&mut writer_index, &events, &pinyin)?;
                    let changes = catalog.take_changes(&writer_index);
                    eprintln!("{}", json!({"update":stats}));
                    if !changes.is_empty() && send.send(Ok(changes)).is_err() {
                        break;
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = send.send(Err(error));
            }
        });
        let result = serve_queries(listener, &mut index, &receive);
        STOP.store(true, Ordering::Relaxed);
        drop(receive);
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("index writer panicked"))?;
        result
    })
}

fn serve_queries(
    listener: UnixListener,
    index: &mut Index,
    receive: &mpsc::Receiver<Result<Vec<Record<'static>>>>,
) -> Result<()> {
    let mut cache = QueryCache::default();
    while !STOP.load(Ordering::Relaxed) {
        match receive.try_recv() {
            Ok(changes) => {
                cache.clear();
                for record in changes? {
                    index.put(&record)?;
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => anyhow::bail!("index writer stopped"),
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                // BSD sockets can inherit O_NONBLOCK from their listening socket.
                // A JSON line may arrive in multiple writes, so this connection must wait.
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                stream.set_write_timeout(Some(Duration::from_secs(1)))?;
                let mut line = String::new();
                let reply = (|| -> Result<Value> {
                    BufReader::new(&mut stream)
                        .take(65_537)
                        .read_line(&mut line)?;
                    ensure!(line.len() <= 65_536, "request too large");
                    Ok(answer_cached(
                        index,
                        serde_json::from_str(&line)?,
                        &mut cache,
                    ))
                })()
                .unwrap_or_else(|e| json!({"error":e.to_string()}));
                // A client closing its view/pipe must not stop the shared engine.
                if serde_json::to_writer(&mut stream, &reply).is_ok() {
                    let _ = stream.write_all(b"\n");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Wake immediately for a query instead of imposing a 5 ms sleep.
                let mut fd = libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                unsafe {
                    libc::poll(&mut fd, 1, 5);
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

use std::os::unix::fs::FileTypeExt;
