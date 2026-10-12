use luminator_index::{
    Catalog, QueryOptions, WriterLock,
    server::{self, Request},
};
use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn options(limit: usize) -> QueryOptions {
    QueryOptions {
        limit,
        ..Default::default()
    }
}

#[test]
fn chinese_pinyin_unicode_filters_and_uncapped_ranking() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("files");
    fs::create_dir(&root).unwrap();
    for name in [
        "沐元健康.pdf",
        "医院合作分工方案.docx",
        "Cafe\u{301}.md",
        "Straße.txt",
        "MIXEDCase.TXT",
    ] {
        fs::write(root.join(name), []).unwrap();
    }
    fs::create_dir(root.join("沐元健康目录")).unwrap();
    for n in 0..300 {
        fs::write(root.join(format!("archive_target_{n:03}.txt")), []).unwrap();
    }
    fs::write(root.join("target.txt"), []).unwrap();
    let db = Catalog::open(&temp.path().join("index.sqlite"), true).unwrap();
    let (index, stats) = db.scan(&root).unwrap();
    assert_eq!(stats.skipped, 0);
    for q in [
        "沐元健康",
        "muyuanjiankang",
        "myjk",
        "合作分工",
        "CAFÉ",
        "cafe\u{301}",
        "straße",
        "mixedcase",
    ] {
        assert!(!index.query(q, &options(20)).is_empty(), "{q}");
    }
    assert_eq!(
        index.query("target", &options(1))[0]
            .path
            .file_name()
            .unwrap(),
        "target.txt"
    );
    let hits = index.query(
        "myjk",
        &QueryOptions {
            limit: 20,
            kind: Some(0),
            extension: Some("PDF".into()),
            under: None,
        },
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].match_kind, "pinyin");
    assert!(index.query("", &options(20)).is_empty());
    assert!(index.query("myjk", &options(0)).is_empty());
    let loaded = db.load().unwrap();
    assert_eq!(loaded.query("myjk", &options(20)).len(), 2);
    assert_eq!(loaded.query("files/医院", &options(20)).len(), 1);
    for q in [
        "files/",
        "files/沐元",
        "/医院",
        "files/CAFÉ",
        "/",
        "files/missing",
    ] {
        let expected = (1..loaded.slots() as u32)
            .map(|id| loaded.path(id))
            .filter(|p| {
                luminator_index::index::normalize(&p.to_string_lossy())
                    .contains(&luminator_index::index::normalize(q))
            })
            .collect::<std::collections::HashSet<_>>();
        let actual = loaded
            .query(q, &options(1000))
            .into_iter()
            .map(|hit| hit.path)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(actual, expected, "literal path query: {q}");
    }
}

#[test]
fn refresh_rename_move_replace_delete_and_symlinks() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("files");
    fs::create_dir(&root).unwrap();
    fs::create_dir_all(root.join("旧目录/内部")).unwrap();
    fs::create_dir(root.join("目的地")).unwrap();
    fs::write(root.join("旧目录/内部/健康报告.txt"), b"a").unwrap();
    symlink(root.join("旧目录"), root.join("链接")).unwrap();
    let db = Catalog::open(&temp.path().join("index.sqlite"), true).unwrap();
    let (index, _) = db.scan(&root).unwrap();
    let id = index.query("健康报告", &options(20))[0].id;
    fs::rename(root.join("旧目录"), root.join("目的地/新目录")).unwrap();
    let (index, _) = db.scan(&root).unwrap();
    let hit = &index.query("健康报告", &options(20))[0];
    assert_eq!(hit.id, id);
    assert!(hit.path.ends_with("目的地/新目录/内部/健康报告.txt"));
    assert!(index.query("旧目录", &options(20)).is_empty());
    fs::write(&hit.path, b"different-size").unwrap();
    let (index, stats) = db.scan(&root).unwrap();
    assert!(stats.changed > 0);
    assert_eq!(index.query("健康报告", &options(20))[0].size, 14);
    fs::hard_link(
        root.join("目的地/新目录/内部/健康报告.txt"),
        root.join("硬链接健康报告.txt"),
    )
    .unwrap();
    let replacement = root.join("replacement");
    fs::write(&replacement, b"replacement").unwrap();
    fs::rename(replacement, root.join("目的地/新目录/内部/健康报告.txt")).unwrap();
    let (index, _) = db.scan(&root).unwrap();
    let hits = index.query("健康报告", &options(20));
    assert_eq!(hits.len(), 2);
    assert_ne!(hits[0].id, hits[1].id);
    assert_eq!(index.query("硬链接健康报告", &options(20))[0].size, 14);
    fs::remove_dir_all(root.join("目的地/新目录")).unwrap();
    fs::remove_file(root.join("硬链接健康报告.txt")).unwrap();
    let (index, _) = db.scan(&root).unwrap();
    assert!(index.query("健康报告", &options(20)).is_empty());
    assert_eq!(index.query("链接", &options(20)).len(), 1);
    assert_eq!(db.load().unwrap().entries(), index.entries());
}

#[test]
fn writer_is_exclusive_and_root_cannot_be_silently_changed() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("index.sqlite");
    let _lock = WriterLock::acquire(&path).unwrap();
    assert!(WriterLock::acquire(&path).is_err());
    let db = Catalog::open(&path, true).unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    db.scan(&a).unwrap();
    assert!(db.scan(&b).is_err());
}

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let _ = self.0.wait();
    }
}
fn start_daemon(db: &Path, socket: &Path, log: &Path) -> Daemon {
    let child = Command::new(env!("CARGO_BIN_EXE_luminator"))
        .args(["--db"])
        .arg(db)
        .args(["serve", "--socket"])
        .arg(socket)
        .stdout(Stdio::null())
        .stderr(fs::File::create(log).unwrap())
        .spawn()
        .unwrap();
    let daemon = Daemon(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    while server::request(socket, &Request::default()).is_err() {
        assert!(
            Instant::now() < deadline,
            "daemon did not start: {}",
            fs::read_to_string(log).unwrap()
        );
        thread::sleep(Duration::from_millis(20));
    }
    daemon
}
fn wait_hit(socket: &Path, q: &str, expected: Option<&Path>) -> Duration {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(8);
    loop {
        let reply = server::request(
            socket,
            &Request {
                query: Some(q.into()),
                ..Default::default()
            },
        )
        .unwrap();
        let hits = reply["results"]
            .as_array()
            .unwrap_or_else(|| panic!("invalid query reply: {reply}"));
        let found = hits
            .iter()
            .any(|h| expected.is_some_and(|p| h["path"] == p.to_str().unwrap()));
        if expected.is_some() && found || expected.is_none() && hits.is_empty() {
            return start.elapsed();
        }
        assert!(
            Instant::now() < deadline,
            "event never became visible: {reply}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn fsevents_live_updates_and_offline_history_recovery() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("files");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let dbpath = temp.path().join("index.sqlite");
    let socket = temp.path().join("engine.sock");
    let log = temp.path().join("daemon.log");
    let db = Catalog::open(&dbpath, true).unwrap();
    db.scan(&root).unwrap();
    drop(db);
    let daemon = start_daemon(&dbpath, &socket, &log);
    // A real client can send its JSON frame in several writes.
    {
        use std::io::{BufRead, BufReader, Write};
        let mut client = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        client.write_all(b"{\"query\":").unwrap();
        thread::sleep(Duration::from_millis(30));
        client.write_all(b"\"nothing\"}\n").unwrap();
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(
            reply["results"].is_array(),
            "fragmented request failed: {reply}"
        );
    }
    let a = root.join("新增健康.txt");
    fs::write(&a, []).unwrap();
    assert!(wait_hit(&socket, "xzjk", Some(&a)).as_secs_f64() < 2.);
    let b = root.join("改名健康.txt");
    fs::rename(&a, &b).unwrap();
    wait_hit(&socket, "gmjk", Some(&b));
    wait_hit(&socket, "xzjk", None);
    fs::create_dir_all(root.join("nested/deeper")).unwrap();
    let nested = root.join("nested/deeper/深处报告.pdf");
    fs::write(&nested, []).unwrap();
    wait_hit(&socket, "深处报告", Some(&nested));
    fs::rename(root.join("nested"), root.join("moved")).unwrap();
    let moved = root.join("moved/deeper/深处报告.pdf");
    wait_hit(&socket, "深处报告", Some(&moved));
    fs::remove_file(&b).unwrap();
    wait_hit(&socket, "gmjk", None);
    drop(daemon);
    let offline = root.join("离线新增.txt");
    fs::write(&offline, []).unwrap();
    fs::remove_dir_all(root.join("moved")).unwrap();
    let _daemon = start_daemon(&dbpath, &socket, &log);
    wait_hit(&socket, "lxxz", Some(&offline));
    wait_hit(&socket, "深处报告", None);
}
