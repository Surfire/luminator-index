use luminator_index::{Catalog, content};
use std::{fs, path::Path};
use tempfile::TempDir;

#[test]
fn content_is_opt_in_separate_and_updates_without_touching_names() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    fs::create_dir(&root).unwrap();
    let file = root.join("ordinary.txt");
    fs::write(&file, "项目采用中文全文检索方案 café\n边界测试").unwrap();
    let names = temp.path().join("names.sqlite");
    Catalog::open(&names, true).unwrap().scan(&root).unwrap();
    let names_before = fs::read(&names).unwrap();
    let db = temp.path().join("content.sqlite");
    assert!(content::scan(&db, &root, Path::new("unused"), false).is_err());
    assert!(!db.exists());
    assert!(content::scan(&names, &root, Path::new("unused"), true).is_err());
    content::scan(&db, &root, Path::new("unused"), true).unwrap();
    for q in ["中文全文", "全文检索", "全", "cafe\u{301}", "方案café"] {
        assert_eq!(
            content::query(&db, Some(q), 20).unwrap()["results"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "{q}"
        );
    }
    assert!(
        content::query(&db, Some("检索全文"), 20).unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    fs::write(&file, "替换之后的增量更新").unwrap();
    assert!(
        content::query(&db, Some("中文全文"), 20).unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    content::scan(&db, &root, Path::new("unused"), true).unwrap();
    assert_eq!(
        content::query(&db, Some("增量更新"), 20).unwrap()["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let moved = root.join("renamed.md");
    fs::rename(&file, &moved).unwrap();
    content::scan(&db, &root, Path::new("unused"), true).unwrap();
    assert_eq!(
        content::query(&db, Some("增量更新"), 20).unwrap()["results"][0]["path"],
        moved.canonicalize().unwrap().to_str().unwrap()
    );
    fs::remove_file(&moved).unwrap();
    content::scan(&db, &root, Path::new("unused"), true).unwrap();
    assert_eq!(content::query(&db, None, 20).unwrap()["chunks"], 0);
    assert_eq!(fs::read(names).unwrap(), names_before);
}

#[test]
fn overlapping_chunks_preserve_max_query_and_single_result_per_file() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    fs::create_dir(&root).unwrap();
    let phrase = "乙".repeat(128);
    fs::write(
        root.join("a.txt"),
        format!("{}{}{}", "甲".repeat(1000), phrase, "丙".repeat(2200)),
    )
    .unwrap();
    let db = temp.path().join("body.sqlite");
    content::scan(&db, &root, Path::new("unused"), true).unwrap();
    assert_eq!(
        content::query(&db, Some(&phrase), 20).unwrap()["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        content::query(&db, Some("丙"), 20).unwrap()["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(content::query(&db, Some(&"甲".repeat(129)), 20).is_err());
}
