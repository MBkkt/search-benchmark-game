//! Ingestion benchmark binary: the SBG `build_index` path with the storage
//! backend selectable (GCS or local FS), phase timers, and the build knobs
//! exposed through the environment so one binary can sweep a configuration
//! without a rebuild.
//!
//! Storage (index):
//!   BENCH_GCS_BUCKET=<bucket> [BENCH_GCS_PREFIX=<prefix>]   → GCS
//!   otherwise                                               → local dir argv[1]
//! The reader disk cache always stays on local disk (under `$TMPDIR`).
//!
//! Knobs (all optional):
//!   BENCH_WRITER_THREADS  writer-pool threads   (default: the shared lib's 4)
//!   BENCH_BATCH           rows per RecordBatch  (default 50_000)
//!   BENCH_FLUSH_MB        commit auto-flush MiB (default 4096)
//!   BENCH_SKIP_COMPACT=1  stop after commit, report ingest only
//!   BENCH_COMPACT_MB      compaction target MiB (default 8192)
//!   BENCH_TRACE=info|debug  install a tracing subscriber so the engine's own
//!                         `[optphase]` (info) and `[fts-finish]` (debug)
//!                         phase lines reach stderr. Off by default so a
//!                         timing run pays nothing for logging.

use std::env;
use std::io::{self, BufRead};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow_array::{LargeStringArray, RecordBatch};
use infino::storage::{
    GcsStorageProvider, LocalFsStorageProvider, PrefixedStorageProvider, StorageProvider,
};
use infino::supertable::Supertable;
use infino::{CompactionSettings, GcSettings, OptimizeOptions};
use serde::Deserialize;

#[derive(Deserialize)]
struct Doc {
    text: String,
}

fn envnum<T: std::str::FromStr>(k: &str) -> Option<T> {
    env::var(k).ok().and_then(|v| v.parse().ok())
}

fn main() {
    if let Ok(level) = env::var("BENCH_TRACE") {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(format!("infino={level}")))
            .with_writer(std::io::stderr)
            .init();
    }
    let args: Vec<String> = env::args().collect();
    let batch_rows: usize = envnum("BENCH_BATCH").unwrap_or(50_000);
    let flush_mb: u64 = envnum("BENCH_FLUSH_MB").unwrap_or(4096);
    let compact_mb: u64 = envnum("BENCH_COMPACT_MB").unwrap_or(8 * 1024);
    let threads: Option<usize> = envnum("BENCH_WRITER_THREADS");
    let skip_compact = env::var("BENCH_SKIP_COMPACT").ok().as_deref() == Some("1");

    let (storage, where_): (Arc<dyn StorageProvider>, String) = match env::var("BENCH_GCS_BUCKET") {
        Ok(bucket) if !bucket.is_empty() => {
            let prefix = env::var("BENCH_GCS_PREFIX").unwrap_or_default();
            // `GcsStorageProvider::new_with_prefix` takes the crate-private
            // `StorageOptions`, so it is not callable from outside the crate;
            // the public prefix wrapper gives the same namespacing.
            let gcs: Arc<dyn StorageProvider> =
                Arc::new(GcsStorageProvider::new(&bucket).expect("open GCS storage"));
            let p: Arc<dyn StorageProvider> = if prefix.is_empty() {
                gcs
            } else {
                Arc::new(PrefixedStorageProvider::new(gcs, prefix.clone()))
            };
            (p, format!("gs://{bucket}/{prefix}"))
        }
        _ => {
            let dir = &args[1];
            let p: Arc<dyn StorageProvider> =
                Arc::new(LocalFsStorageProvider::new(dir).expect("open local storage"));
            (p, dir.clone())
        }
    };

    let mut opts = infino_bench::options(Arc::clone(&storage))
        .with_commit_threshold_size_mb(flush_mb)
        .with_storage(storage);
    if let Some(n) = threads {
        let pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .expect("writer pool"),
        );
        opts = opts.with_writer_pool(pool);
    }
    eprintln!(
        "[cfg] storage={} writer_threads={} batch={} flush_mb={} compact_mb={} skip_compact={}",
        where_,
        threads
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("{}(default)", infino_bench::writer_threads())),
        batch_rows,
        flush_mb,
        compact_mb,
        skip_compact
    );

    let st = Supertable::create(opts).expect("create supertable");
    let mut writer = st.writer().expect("acquire writer");
    let schema = infino_bench::schema();

    let mut buf: Vec<String> = Vec::with_capacity(batch_rows);
    let mut total: u64 = 0;
    let (mut t_read, mut t_parse, mut t_arrow, mut t_append) = (
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
    );
    let mut bytes: u64 = 0;

    let t_total = Instant::now();
    let stdin = io::stdin();
    let mut lock = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        let t = Instant::now();
        let n = lock.read_line(&mut line).expect("read line");
        t_read += t.elapsed();
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if line.trim().is_empty() {
            continue;
        }
        let t = Instant::now();
        let doc: Doc = serde_json::from_str(&line).expect("parse json");
        t_parse += t.elapsed();
        buf.push(doc.text);
        if buf.len() == batch_rows {
            total += buf.len() as u64;
            append(&mut writer, &schema, &mut buf, &mut t_arrow, &mut t_append);
            if total % 1_000_000 == 0 {
                eprintln!("{total}");
            }
        }
    }
    if !buf.is_empty() {
        total += buf.len() as u64;
        append(&mut writer, &schema, &mut buf, &mut t_arrow, &mut t_append);
    }

    let t = Instant::now();
    writer.commit().expect("commit");
    drop(writer);
    let t_commit = t.elapsed();
    let ingest_wall = t_total.elapsed();
    eprintln!("indexed {total} docs into the supertable");
    eprintln!(
        "[phase] ingest_wall={:.1}s read={:.1}s parse={:.1}s arrow={:.1}s append={:.1}s \
         final_commit={:.1}s corpus={:.2}GiB",
        ingest_wall.as_secs_f64(),
        t_read.as_secs_f64(),
        t_parse.as_secs_f64(),
        t_arrow.as_secs_f64(),
        t_append.as_secs_f64(),
        t_commit.as_secs_f64(),
        bytes as f64 / (1u64 << 30) as f64,
    );

    if skip_compact {
        eprintln!("[phase] compact skipped");
        eprintln!("[phase] total={:.1}s", t_total.elapsed().as_secs_f64());
        return;
    }

    eprintln!("compacting…");
    let t = Instant::now();
    st.optimize(
        &OptimizeOptions::compact(CompactionSettings {
            target_superfile_size_mb: compact_mb,
            min_fill_percent: 1,
            max_merge_input_mb: compact_mb + 2048,
            ..Default::default()
        })
        .with_gc(GcSettings {
            safety_gap: Duration::ZERO,
        }),
    )
    .expect("optimize");
    eprintln!("compact done");
    eprintln!("[phase] compact={:.1}s", t.elapsed().as_secs_f64());

    let reader = st.reader().expect("open reader after compact");
    let n = reader.manifest().superfiles.len();
    eprintln!("SUPERFILE_COUNT after compact: {n}");
    eprintln!("[phase] total={:.1}s", t_total.elapsed().as_secs_f64());
}

fn append(
    writer: &mut infino::supertable::SupertableWriter,
    schema: &Arc<arrow_schema::Schema>,
    buf: &mut Vec<String>,
    t_arrow: &mut Duration,
    t_append: &mut Duration,
) {
    let t = Instant::now();
    let arr = LargeStringArray::from(buf.iter().map(String::as_str).collect::<Vec<_>>());
    let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(arr)]).expect("record batch");
    *t_arrow += t.elapsed();
    let t = Instant::now();
    writer.append(&batch).expect("append batch");
    *t_append += t.elapsed();
    buf.clear();
}
