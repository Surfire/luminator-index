use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use luminator_index::{
    Catalog, MatchMode, WriterLock, benchmark,
    server::{self, Request},
};
use std::{fs, path::PathBuf, time::Instant};

#[derive(Parser)]
#[command(version, about = "Local filename and opt-in document text search.")]
struct Args {
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    File,
    Directory,
    Symlink,
}
#[derive(Subcommand)]
enum Command {
    /// Optional local-only document text indexing and search.
    Content {
        #[command(subcommand)]
        command: ContentCommand,
    },
    /// Create or refresh an index, reading filename metadata only.
    Scan { root: PathBuf },
    /// Query a saved index, or the live daemon with --socket.
    Query {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        kind: Option<Kind>,
        #[arg(long)]
        extension: Option<String>,
        #[arg(long)]
        under: Option<PathBuf>,
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "auto")]
        mode: MatchMode,
    },
    /// Watch FSEvents and answer local JSON queries; stays in the foreground.
    Serve {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Show index size and memory use.
    Status {
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Measure warm queries, separately reporting catalog restore time.
    Benchmark {
        #[arg(long)]
        queries: PathBuf,
        #[arg(long, default_value_t = 20)]
        repeats: usize,
    },
    /// Create an empty synthetic file tree. Refuses an existing root.
    Fixture {
        root: PathBuf,
        #[arg(long, default_value_t = 1_000_000)]
        entries: usize,
    },
}
#[derive(Subcommand)]
enum ContentCommand {
    Scan {
        #[arg(long)]
        names_db: PathBuf,
        #[arg(long)]
        helper: PathBuf,
        #[arg(long)]
        enable: bool,
    },
    Serve {
        #[arg(long)]
        names_db: PathBuf,
        #[arg(long)]
        helper: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        enable: bool,
    },
    Query {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Status,
}
fn db_path(db: Option<PathBuf>) -> Result<PathBuf> {
    let db = db.context("--db PATH is required for this command")?;
    let db = if db.is_absolute() {
        db
    } else {
        std::env::current_dir()?.join(db)
    };
    fs::create_dir_all(db.parent().context("database needs a parent directory")?)?;
    Ok(db
        .parent()
        .unwrap()
        .canonicalize()?
        .join(db.file_name().context("database filename missing")?))
}
fn absolute(path: PathBuf) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
fn main() -> Result<()> {
    let args = Args::parse();
    let value = match args.command {
        Command::Content { command } => {
            let path = db_path(args.db)?;
            match command {
                ContentCommand::Scan {
                    names_db,
                    helper,
                    enable,
                } => luminator_index::content::scan(
                    &path,
                    &luminator_index::content::root_from_names(&names_db)?,
                    &helper,
                    enable,
                )?,
                ContentCommand::Serve {
                    names_db,
                    helper,
                    socket,
                    enable,
                } => {
                    luminator_index::content::serve(
                        &path,
                        &luminator_index::content::root_from_names(&names_db)?,
                        &helper,
                        &absolute(socket)?,
                        enable,
                    )?;
                    return Ok(());
                }
                ContentCommand::Query { query, limit } => {
                    luminator_index::content::query(&path, Some(&query), limit)?
                }
                ContentCommand::Status => luminator_index::content::query(&path, None, 20)?,
            }
        }
        Command::Fixture { root, entries } => benchmark::fixture(&root, entries)?,
        Command::Scan { root } => {
            let db = db_path(args.db)?;
            let root = root.canonicalize()?;
            ensure!(
                !db.starts_with(&root),
                "database must live outside the indexed root"
            );
            let _lock = WriterLock::acquire(&db)?;
            let catalog = Catalog::open(&db, true)?;
            let (index, stats) = catalog.scan(&root)?;
            serde_json::json!({"entries":index.entries(),"update":stats,"index_allocated_bytes":index.allocated_bytes(),"physical_footprint_bytes":luminator_index::footprint(),"physical_footprint_peak_bytes":luminator_index::footprint_peak()})
        }
        Command::Query {
            query,
            limit,
            kind,
            extension,
            under,
            socket,
            mode,
        } => {
            ensure!(limit <= 10_000, "limit must be at most 10000");
            let request = Request {
                query: Some(query),
                limit: Some(limit),
                kind: kind.map(|k| match k {
                    Kind::File => 0,
                    Kind::Directory => 1,
                    Kind::Symlink => 2,
                }),
                extension,
                under: under.map(absolute).transpose()?,
                mode,
            };
            if let Some(socket) = socket {
                server::request(&socket, &request)?
            } else {
                let start = Instant::now();
                let catalog = Catalog::open(&db_path(args.db)?, false)?;
                let index = catalog.load()?;
                let restore_ms = start.elapsed().as_secs_f64() * 1000.;
                let mut value = server::answer(&index, request);
                value["restore_ms"] = serde_json::json!(restore_ms);
                value
            }
        }
        Command::Status { socket } => {
            if let Some(socket) = socket {
                server::request(&socket, &Request::default())?
            } else {
                let catalog = Catalog::open(&db_path(args.db)?, false)?;
                let index = catalog.load()?;
                server::answer(&index, Request::default())
            }
        }
        Command::Serve { socket } => {
            server::serve(&db_path(args.db)?, &absolute(socket)?)?;
            return Ok(());
        }
        Command::Benchmark { queries, repeats } => {
            benchmark::run(&db_path(args.db)?, &queries, repeats)?
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
