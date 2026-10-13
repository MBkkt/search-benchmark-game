//! Build an infino supertable from newline-delimited JSON, then compact it.
//!
//! Each input line is `{"id": "...", "text": "...", "sort_field": <u64>}`.
//! Only `text` is indexed. Docs are streamed in 50 k-doc batches; a 4 GiB
//! auto-flush threshold causes the writer to commit several segments
//! incrementally (bounded build memory). After ingest, `optimize()` compacts
//! all segments into one, matching the single-segment shape that tantivy and
//! Lucene produce — so query-path fan-out overhead is equivalent.
//!
//! A scale build (scripts/index-scale.sh) sets `INFINO_BENCH_MULTI_SUPERFILE=1`:
//! past a few million documents the index no longer fits one superfile, and
//! the comparison there is between whole tables, not segments. It also sets
//! `INFINO_BENCH_BUILD_STATS` to a path, which receives what the build cost.

use std::env;
use std::io::{self, BufRead};
use std::sync::Arc;
use std::fs;
use std::time::{Duration, Instant};

use arrow_array::{LargeStringArray, RecordBatch};
use infino::storage::StorageProvider;
use infino::supertable::Supertable;
use infino::{CompactionSettings, GcSettings, OptimizeOptions};
use serde::Deserialize;
use serde_json::json;

/// Large enough that the entire Wikipedia BM25 index fits in one output segment.
const COMPACT_TARGET_MB: u64 = 8 * 1024;

const BATCH: usize = 50_000;

#[derive(Deserialize)]
struct Doc {
    text: String,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    // Same resolution as do_query, so the two agree on where the index lives
    // as well as on the options they stamp into it.
    let target = env::var("INFINO_BENCH_INDEX_URI").unwrap_or_else(|_| args[1].clone());
    if infino_bench::is_hosted(&target) {
        #[cfg(feature = "hosted")]
        {
            build_hosted(&target);
            return;
        }
        #[cfg(not(feature = "hosted"))]
        infino_bench::no_hosted_support(&target);
    }
    let storage: Arc<dyn StorageProvider> = infino_bench::storage_for(&target);
    let st = Supertable::create(infino_bench::options(storage)).expect("create supertable");
    let mut writer = st.writer().expect("acquire writer");
    let schema = infino_bench::schema();

    let started = Instant::now();
    let mut buf: Vec<String> = Vec::with_capacity(BATCH);
    let mut total: u64 = 0;
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.expect("read line");
        if line.trim().is_empty() {
            continue;
        }
        let doc: Doc = serde_json::from_str(&line).expect("parse json");
        buf.push(doc.text);
        if buf.len() == BATCH {
            total += buf.len() as u64;
            append(&mut writer, &schema, &mut buf);
            if total % 1_000_000 == 0 {
                eprintln!("{total}");
            }
        }
    }
    if !buf.is_empty() {
        total += buf.len() as u64;
        append(&mut writer, &schema, &mut buf);
    }

    writer.commit().expect("commit");
    drop(writer);
    let ingest_secs = started.elapsed().as_secs_f64();
    eprintln!("indexed {total} docs into the supertable in {ingest_secs:.0}s");

    let compact_started = Instant::now();
    eprintln!("compacting…");
    st.optimize(
        &OptimizeOptions::compact(CompactionSettings {
            target_superfile_size_mb: COMPACT_TARGET_MB,
            min_fill_percent: 1,
            max_memory_mb: COMPACT_TARGET_MB + 2048,
            ..Default::default()
        })
        .with_gc(GcSettings {
            safety_gap: Duration::ZERO,
        }),
    )
    .expect("optimize");
    let compact_secs = compact_started.elapsed().as_secs_f64();
    eprintln!("compact done in {compact_secs:.0}s");

    // Report how many superfiles the table compacted to. The query path fans
    // out one work unit per superfile, so single-threaded latency (and the
    // fairness of the comparison against tantivy/lucene's single force-merged
    // segment) hinges on this being 1.
    let reader = st.reader().expect("open reader after compact");
    let n_superfiles = reader.manifest().superfiles.len();
    eprintln!("SUPERFILE_COUNT after compact: {n_superfiles}");
    if let Ok(stats_path) = env::var("INFINO_BENCH_BUILD_STATS") {
        let stats = json!({
            "docs": total,
            "ingest_secs": ingest_secs.round(),
            "compact_secs": compact_secs.round(),
            "superfiles": n_superfiles,
        });
        fs::write(&stats_path, stats.to_string()).expect("write build stats");
    }
    let multi_superfile = env::var("INFINO_BENCH_MULTI_SUPERFILE").as_deref() == Ok("1");
    if n_superfiles != 1 && !multi_superfile {
        // Fail hard: if compaction didn't reach a single superfile (e.g. it ran
        // out of memory and left the ingest segments in place), the query path
        // would fan out over several units single-threaded and publish a
        // silently-degraded, unfair infino result. Better to fail the build
        // (and the whole run, via the Makefile's `|| exit 1`) than bench it.
        eprintln!(
            "ERROR: expected a single compacted superfile, got {n_superfiles} — \
             refusing to bench a multi-superfile index (it would fan out \
             {n_superfiles} units single-threaded). Failing the build."
        );
        std::process::exit(1);
    }
}

/// Load the corpus into a hosted table, then wait until the table reports
/// every document. The platform compacts the table itself, so there is no
/// `optimize` step here.
///
/// If the table already exists, it is reused when its row count matches the
/// corpus. Otherwise this exits with an error: loading again would add rows
/// twice, and the table is left for a person to drop.
#[cfg(feature = "hosted")]
fn build_hosted(target: &str) {
    // A batch ends at whichever limit comes first. The byte limit keeps a
    // request well under the platform's 128 MiB body limit.
    const HOSTED_BATCH: usize = 5_000;
    const HOSTED_BATCH_BYTES: usize = 32 * 1024 * 1024;

    let mut docs = io::stdin()
        .lock()
        .lines()
        .map(|l| l.expect("read line"))
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Doc>(&l).expect("parse json").text)
        .peekable();

    let conn = infino_bench::connect_hosted(target);
    match infino_bench::retry_rate_limited(|| conn.create_database()) {
        Ok(()) | Err(infino::InfinoError::AlreadyExists(_)) => {}
        Err(e) => panic!("create database: {e}"),
    }
    let name = infino_bench::hosted_table();
    let tables = infino_bench::retry_rate_limited(|| conn.list_tables()).expect("list hosted tables");
    if tables.contains(&name) {
        let expected = docs.count() as u64;
        let held = infino_bench::retry_rate_limited(|| infino_bench::hosted_doc_count(&conn, &name))
            .expect("count hosted table");
        if held == expected {
            eprintln!("hosted table {name} already holds all {expected} docs; nothing to load");
            return;
        }
        eprintln!(
            "ERROR: hosted table {name} has {held} docs but the corpus has {expected}. \
             Drop the table, or set INFINO_BENCH_TABLE, and re-run."
        );
        std::process::exit(1);
    }

    let table = infino_bench::retry_rate_limited(|| {
        conn.create_table(&name, infino_bench::schema(), infino_bench::index_spec())
    })
    .expect("create hosted table");
    let schema = infino_bench::schema();

    let mut sent: u64 = 0;
    let mut buf: Vec<String> = Vec::with_capacity(HOSTED_BATCH);
    while docs.peek().is_some() {
        let mut bytes = 0;
        while let Some(doc) = docs.peek() {
            if buf.len() == HOSTED_BATCH || (!buf.is_empty() && bytes + doc.len() > HOSTED_BATCH_BYTES) {
                break;
            }
            bytes += doc.len();
            buf.push(docs.next().expect("peeked"));
        }
        let arr = LargeStringArray::from(buf.iter().map(String::as_str).collect::<Vec<_>>());
        let batch =
            RecordBatch::try_new(schema.clone(), vec![Arc::new(arr)]).expect("record batch");
        infino_bench::retry_rate_limited(|| table.append(&batch)).unwrap_or_else(|e| {
            eprintln!(
                "ERROR: append failed after {sent} docs: {e}. \
                 Drop {name} before re-running."
            );
            std::process::exit(1);
        });
        sent += buf.len() as u64;
        buf.clear();
        eprintln!("{sent}");
    }
    let expected = sent;

    // Appended rows may become visible only after the append returns. Wait
    // until all of them are, so the bench never runs on part of the corpus.
    let wait_secs: u64 = env::var("INFINO_BENCH_LOAD_WAIT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    let deadline = std::time::Instant::now() + Duration::from_secs(wait_secs);
    loop {
        let held = infino_bench::retry_rate_limited(|| infino_bench::hosted_doc_count(&conn, &name))
            .expect("count hosted table");
        if held == expected {
            eprintln!("indexed {expected} docs into hosted table {name}");
            return;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!(
                "ERROR: hosted table {name} holds {held} of {expected} docs after {wait_secs}s"
            );
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn append(
    writer: &mut infino::supertable::SupertableWriter,
    schema: &Arc<arrow_schema::Schema>,
    buf: &mut Vec<String>,
) {
    let arr = LargeStringArray::from(buf.iter().map(String::as_str).collect::<Vec<_>>());
    let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(arr)]).expect("record batch");
    writer.append(&batch).expect("append batch");
    buf.clear();
}
