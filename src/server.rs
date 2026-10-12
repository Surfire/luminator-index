use crate::{
    Catalog, Index, QueryOptions, WriterLock, footprint, footprint_peak,
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
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
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
}

pub fn answer(index: &Index, request: Request) -> Value {
    if let Some(query) = request.query {
        let start = Instant::now();
        let results = index.query(
            &query,
            &QueryOptions {
                limit: request.limit.unwrap_or(20),
                kind: request.kind,
                extension: request.extension,
                under: request.under,
            },
        );
        json!({"results":results,"elapsed_ms":start.elapsed().as_secs_f64()*1000.})
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
    let watcher = Watcher::start(&index.root, catalog.cursor()?)?;
    STOP.store(false, Ordering::Relaxed);
    unsafe {
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
    }
    eprintln!(
        "{}",
        json!({"ready":true,"entries":index.entries(),"restore_ms":restore_ms,"startup_ms":start.elapsed().as_secs_f64()*1000.})
    );
    while !STOP.load(Ordering::Relaxed) {
        let events: Vec<_> = watcher.events.try_iter().take(20_000).collect();
        if !events.is_empty() {
            match catalog.apply_events(&mut index, &events, &pinyin) {
                Ok(stats) => eprintln!("{}", json!({"update":stats})),
                Err(error) => {
                    eprintln!("update failed: {error:#}");
                    return Err(error);
                }
            }
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
                    Ok(answer(&index, serde_json::from_str(&line)?))
                })()
                .unwrap_or_else(|e| json!({"error":e.to_string()}));
                // A client closing its view/pipe must not stop the shared engine.
                if serde_json::to_writer(&mut stream, &reply).is_ok() {
                    let _ = stream.write_all(b"\n");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

use std::os::unix::fs::FileTypeExt;
