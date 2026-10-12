use crate::{Catalog, QueryOptions, footprint_peak};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    path::Path,
    time::Instant,
};

#[derive(Deserialize)]
struct Case {
    query: String,
    category: String,
}
#[derive(Serialize)]
struct Measurement {
    query: String,
    category: String,
    results: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}
fn percentile(values: &mut [f64], fraction: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() as f64 * fraction).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1)]
}

pub fn run(db: &Path, queries: &Path, repeats: usize) -> Result<Value> {
    ensure!(repeats >= 20, "use at least 20 repeats");
    let cases: Vec<Case> = serde_json::from_reader(fs::File::open(queries)?)?;
    ensure!(cases.len() >= 50, "use at least 50 queries");
    ensure!(
        cases
            .iter()
            .all(|c| ["normal", "high_fanout", "path"].contains(&c.category.as_str())),
        "unknown category"
    );
    let start = Instant::now();
    let catalog = Catalog::open(db, false)?;
    let index = catalog.load()?;
    let restore_ms = start.elapsed().as_secs_f64() * 1000.;
    let opts = QueryOptions {
        limit: 20,
        ..Default::default()
    };
    let mut measurements = Vec::new();
    let (mut normal, mut fanout, mut paths) = (Vec::new(), Vec::new(), Vec::new());
    for case in cases {
        let count = index.query(&case.query, &opts).len();
        let mut times = Vec::new();
        for _ in 0..repeats {
            let start = Instant::now();
            let hits = index.query(&case.query, &opts);
            std::hint::black_box(hits);
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        match case.category.as_str() {
            "normal" => normal.extend_from_slice(&times),
            "high_fanout" => fanout.extend_from_slice(&times),
            _ => paths.extend_from_slice(&times),
        }
        let p50 = percentile(&mut times, 0.5);
        let p95 = percentile(&mut times, 0.95);
        measurements.push(Measurement {
            query: case.query,
            category: case.category,
            results: count,
            p50_ms: p50,
            p95_ms: p95,
            max_ms: *times.last().unwrap(),
        });
    }
    let summary = |values: &mut Vec<f64>| -> Value {
        if values.is_empty() {
            Value::Null
        } else {
            let p50 = percentile(values, 0.5);
            let p95 = percentile(values, 0.95);
            json!({"samples":values.len(),"p50_ms":p50,"p95_ms":p95,"max_ms":values.last()})
        }
    };
    Ok(
        json!({"entries":index.entries(),"slots":index.slots(),"restore_ms":restore_ms,
        "index_allocated_bytes":index.allocated_bytes(),"name_pool_bytes":index.name_bytes(),"physical_footprint_peak_bytes":footprint_peak(),
        "database_bytes":fs::metadata(db)?.len(),"normal":summary(&mut normal),"high_fanout":summary(&mut fanout),
        "path":summary(&mut paths),"measurements":measurements}),
    )
}

pub fn fixture(root: &Path, entries: usize) -> Result<Value> {
    ensure!(
        entries > 0 && !root.exists(),
        "fixture root must not exist and entries must be positive"
    );
    let start = Instant::now();
    fs::create_dir_all(root)?;
    let mut dir = root.to_path_buf();
    for i in 0..entries {
        if i % 1000 == 0 {
            dir = root.join(format!("group_{:05}", i / 1000));
            fs::create_dir(&dir)?;
        } else {
            let name = match i % 20 {
                0 | 1 => format!("健康合作方案_{i:07}.pdf"),
                2 | 3 => format!("清泉资料_{i:07}.docx"),
                4 => format!("Cafe\u{301}_notes_{i:07}.md"),
                n => format!(
                    "{}_{i:07}.{}",
                    ["report", "invoice", "snapshot", "archive"][n % 4],
                    ["txt", "pdf", "json", "zip"][n % 4]
                ),
            };
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(name))?;
        }
        if (i + 1) % 100_000 == 0 {
            eprintln!("created {} entries", i + 1);
        }
    }
    Ok(json!({"entries":entries,"creation_ms":start.elapsed().as_secs_f64()*1000.}))
}
