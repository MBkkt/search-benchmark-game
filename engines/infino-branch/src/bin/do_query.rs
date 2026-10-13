//! Serve count / top-k queries against a persisted infino supertable.
//!
//! Reads `COMMAND\t<lucene-query>` lines from stdin and prints one result
//! line per query (stdout is a LineWriter, so each newline flushes).
//!
//! Supported: COUNT, TOP_10/100/1000, TOP_{1,5,10,100,1000}_COUNT.
//! COUNT and TOP_*_COUNT use the native count() path (posting-list traversal,
//! no scoring). The query string passes through verbatim: infino parses
//! the lucene clause sigils natively (`+term` must, `-term` must-not,
//! bare term should) under `BoolMode::Or` as the default operator, and
//! double-quoted runs are exact phrases verified against token
//! positions — the same BooleanQuery + PhraseQuery semantics lucene
//! applies. `*_FF` (fast-field ordering) and UNOPTIMIZED_COUNT are
//! answered "UNSUPPORTED" — see README.md.

use std::env;
use std::io::{self, BufRead};
use std::sync::Arc;

use infino::storage::StorageProvider;
use infino::superfile::fts::reader::{Bm25SearchOptions, BoolMode};
use infino::InfinoError;
use infino::supertable::{Supertable, SupertableReader};
use infino::supertable::reader_cache::{InMemoryReaderCache, SuperfileReaderCache};

use infino_bench::COLUMN;

fn main() {
    let args: Vec<String> = env::args().collect();
    // INFINO_BENCH_INDEX_URI names an index that already exists, which is how a
    // scale run is served: the index is far too large to build per run, and on
    // object storage rather than beside the engine.
    let target = env::var("INFINO_BENCH_INDEX_URI").unwrap_or_else(|_| args[1].clone());

    // A hosted target sends each query to the platform over HTTPS.
    let index = if infino_bench::is_hosted(&target) {
        open_hosted(&target)
    } else {
        Index::Local(open_local(&target))
    };

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.expect("read line");
        let mut parts = line.splitn(2, '\t');
        let command = parts.next().unwrap_or("");
        let query = parts.next().unwrap_or("");

        // Lucene's default operator: bare terms are OR'd. All clause
        // structure — +/- sigils and quoted phrases — rides in the
        // query string itself; infino parses it natively.
        let mode = BoolMode::Or;

        let result = match command {
            _ if query.split_whitespace().all(|t| t.starts_with('-') || t.trim().is_empty()) => {
                // negation-only: no positive terms to rank
                Ok(0usize)
            }
            _ if query.trim().is_empty() => Ok(0usize),
            "TOP_10" | "TOP_100" | "TOP_1000" => index
                .bm25_search(query, top_k(command), search_opts(mode))
                .map(|_| 1),
            // Plain COUNT: native posting-list traversal, no scoring.
            "COUNT" => index.count(query, mode).map(|n| n as usize),
            // TOP_k_COUNT: fetch the top-k results AND count all matches —
            // two passes, matching what engines like Lucene do for this command.
            "TOP_1_COUNT" | "TOP_5_COUNT" | "TOP_10_COUNT"
            | "TOP_100_COUNT" | "TOP_1000_COUNT" => index
                .bm25_search(query, top_k_count(command), search_opts(mode))
                .and_then(|_| index.count(query, mode))
                .map(|n| n as usize),
            _ => {
                println!("UNSUPPORTED");
                continue;
            }
        };
        match result {
            Ok(count) => println!("{count}"),
            // Exit rather than print 0, so a failed request is never
            // recorded as a result.
            Err(e) if index.is_hosted() => {
                eprintln!("hosted error for {command:?} {query:?}: {e}");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("search error for {command:?} {query:?}: {e}");
                println!("0");
            }
        }
    }
}

/// Open a local path or `gs://bucket/prefix` index.
fn open_local(target: &str) -> SupertableReader {
    let storage: Arc<dyn StorageProvider> = infino_bench::storage_for(target);

    // Default: an in-memory reader tier, preloaded after open, so the query
    // path resolves readers SYNCHRONOUSLY from tier-1 (`store.reader`) and
    // never touches the async disk-cache path — no per-query tokio runtime
    // build on the rayon fan-out workers.
    //
    // INFINO_BENCH_QUERY_MODE=disk drops both: readers come from the
    // mmap-backed disk-cache tier, so the index need not fit in memory. A
    // terabyte-scale index has no other option.
    let disk_mode = env::var("INFINO_BENCH_QUERY_MODE").as_deref() == Ok("disk");
    let store: Arc<dyn SuperfileReaderCache> = Arc::new(InMemoryReaderCache::new());
    let mut opts = infino_bench::options(Arc::clone(&storage));
    if !disk_mode {
        opts = opts.with_store(Arc::clone(&store));
    }

    // Supertable::open is sync (bridges internally to async storage I/O).
    // We still need a runtime for the preload loop below.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let st = Supertable::open(opts).expect("open supertable");
    let reader = st.reader().expect("reader");

    // Preload all segments into the in-memory tier. Skipped in disk mode,
    // where the whole point is that the index never has to fit in memory.
    let uris: Vec<_> = reader.manifest().superfiles.iter().map(|e| e.uri).collect();
    if disk_mode {
        eprintln!(
            "disk mode: {} segments served from the mmap disk-cache tier (no preload)",
            uris.len()
        );
    } else {
        eprintln!("preloading {} segments into memory", uris.len());
        rt.block_on(async {
            for uri in uris {
                let path = uri.storage_path();
                let (bytes, _meta) = storage.get(&path).await.expect("fetch segment bytes");
                store.insert(uri, bytes).expect("insert segment into store");
            }
        });
    }
    reader
}

/// The index being queried: opened in this process, or a hosted table.
enum Index {
    Local(SupertableReader),
    #[cfg(feature = "hosted")]
    Hosted(infino::Supertable),
}

/// Open the hosted table. Exits if its row count differs from the expected
/// document count, so a partial or wrong table is never benched.
#[cfg(feature = "hosted")]
fn open_hosted(target: &str) -> Index {
    let table = infino_bench::hosted_table();
    let conn = infino_bench::connect_hosted(target);
    let held = infino_bench::retry_rate_limited(|| infino_bench::hosted_doc_count(&conn, &table))
        .unwrap_or_else(|e| {
            eprintln!("hosted error counting table {table} at {target}: {e}");
            std::process::exit(1);
        });
    match infino_bench::expected_docs() {
        Some(expected) if expected != held => {
            eprintln!(
                "ERROR: hosted table {table} at {target} holds {held} docs, the run expects {expected}"
            );
            std::process::exit(1);
        }
        Some(_) => eprintln!("hosted: querying table {table} ({held} docs) at {target}"),
        None => eprintln!(
            "hosted: querying table {table} ({held} docs) at {target}; \
             row count not checked"
        ),
    }
    Index::Hosted(
        infino_bench::retry_rate_limited(|| conn.open_table(&table)).expect("open hosted table"),
    )
}

#[cfg(not(feature = "hosted"))]
fn open_hosted(target: &str) -> Index {
    infino_bench::no_hosted_support(target)
}

impl Index {
    fn bm25_search(&self, query: &str, k: usize, opts: Bm25SearchOptions) -> Result<(), InfinoError> {
        match self {
            Index::Local(r) => r.bm25_search(COLUMN, query, k, opts, None).map(|_| ())?,
            #[cfg(feature = "hosted")]
            Index::Hosted(t) => infino_bench::retry_rate_limited(|| {
                t.bm25_search(COLUMN, query, k, opts, None).map(|_| ())
            })?,
        }
        Ok(())
    }

    fn count(&self, query: &str, mode: BoolMode) -> Result<u64, InfinoError> {
        match self {
            Index::Local(r) => Ok(r.count(COLUMN, query, mode)? as u64),
            #[cfg(feature = "hosted")]
            Index::Hosted(t) => infino_bench::retry_rate_limited(|| t.count(COLUMN, query, mode)),
        }
    }

    fn is_hosted(&self) -> bool {
        !matches!(self, Index::Local(_))
    }
}

/// Search options for every ranked command, in one place so the two
/// call sites cannot drift apart on the statistics scope.
///
/// Statistics scope and BM25 parameters are left at the engine's
/// defaults: table-wide idf, the only scope infino keeps once per-superfile
/// idf is removed, and the column's declared k1/b. This harness measures
/// the standard scoring configuration.
fn search_opts(mode: BoolMode) -> Bm25SearchOptions {
    Bm25SearchOptions::new().with_mode(mode)
}

fn top_k(command: &str) -> usize {
    match command {
        "TOP_10" => 10,
        "TOP_100" => 100,
        "TOP_1000" => 1000,
        _ => 10,
    }
}

fn top_k_count(command: &str) -> usize {
    match command {
        "TOP_1_COUNT" => 1,
        "TOP_5_COUNT" => 5,
        "TOP_10_COUNT" => 10,
        "TOP_100_COUNT" => 100,
        "TOP_1000_COUNT" => 1000,
        _ => 10,
    }
}

