//! Shared configuration for the build and query binaries.
//!
//! `build_index` and `do_query` MUST construct byte-identical
//! `SupertableOptions` — the supertable stamps a digest of the options
//! into the manifest at commit time and verifies it on `open`. Centralizing
//! the schema / FTS column / tokenizer / pool config here guarantees they
//! agree.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema};
use infino::storage::{GcsStorageProvider, LocalFsStorageProvider, StorageProvider};
use infino::superfile::builder::FtsConfig;
use infino::supertable::manifest::list::PartitionStrategy;
use infino::supertable::reader_cache::{DiskCacheConfig, DiskCacheStore};
use infino::supertable::{Consistency, SupertableOptions};
#[cfg(feature = "hosted")]
use infino::{ConnectOptions, Connection, FtsField, IndexSpec};

/// The single indexed full-text column.
pub const COLUMN: &str = "text";

/// Whether `target` is a hosted platform database (`https://host/<database>`).
/// infino itself rejects `http://` for any host but localhost.
pub fn is_hosted(target: &str) -> bool {
    target.starts_with("https://") || target.starts_with("http://")
}

/// Exit with an error: this build cannot query the hosted platform.
#[cfg(not(feature = "hosted"))]
pub fn no_hosted_support(target: &str) -> ! {
    eprintln!(
        "{target} is a hosted database, and this build has no hosted client; \
         serve it through engines/infino-platform"
    );
    std::process::exit(1);
}

/// The hosted table to use: `INFINO_BENCH_TABLE` if set, else
/// `<corpus>_<scale>` on a scale run (`webcrawl_1b`), else `sbg`.
#[cfg(feature = "hosted")]
pub fn hosted_table() -> String {
    if let Some(name) = std::env::var("INFINO_BENCH_TABLE").ok().filter(|s| !s.is_empty()) {
        return name;
    }
    match (
        std::env::var("INFINO_BENCH_CORPUS_NAME"),
        std::env::var("INFINO_BENCH_SCALE"),
    ) {
        (Ok(corpus), Ok(scale)) => format!("{corpus}_{scale}").to_lowercase(),
        _ => "sbg".to_string(),
    }
}

/// Connect to a hosted database. infino reads the API key from
/// `INFINO_API_KEY`. Accepts `https://host/v1/db` or `https://host/db`; infino
/// expects the latter.
#[cfg(feature = "hosted")]
pub fn connect_hosted(target: &str) -> Connection {
    let uri = target.replacen("/v1/", "/", 1);
    infino::connect_with(&uri, ConnectOptions::default()).expect("connect to hosted database")
}

/// The hosted table's full-text index; matches the FTS config in `options`.
#[cfg(feature = "hosted")]
pub fn index_spec() -> IndexSpec {
    IndexSpec::new().fts(
        FtsField::new(COLUMN)
            .analyzer("standard")
            .positions(true)
            .stored(false),
    )
}

/// Number of rows in the hosted table.
#[cfg(feature = "hosted")]
pub fn hosted_doc_count(conn: &Connection, table: &str) -> Result<u64, infino::InfinoError> {
    use arrow_array::{Array, Int64Array, UInt64Array};
    let batches = conn.query_sql(&format!("SELECT COUNT(*) FROM \"{table}\""))?;
    let column = batches
        .first()
        .filter(|b| b.num_rows() == 1)
        .map(|b| b.column(0).clone())
        .ok_or_else(|| infino::InfinoError::Backend("COUNT(*) returned no row".into()))?;
    if let Some(a) = column.as_any().downcast_ref::<Int64Array>() {
        return Ok(a.value(0) as u64);
    }
    if let Some(a) = column.as_any().downcast_ref::<UInt64Array>() {
        return Ok(a.value(0));
    }
    Err(infino::InfinoError::Backend(format!(
        "COUNT(*) returned {:?}",
        column.data_type()
    )))
}

/// Run `call`, retrying up to 5 times, 1s apart, when the platform answers
/// 429 (rate limited). A 429 is rejected before any work is done, so retrying
/// is safe even for appends. Other errors are returned as is: the request may
/// have been applied, and repeating an append could add rows twice.
#[cfg(feature = "hosted")]
pub fn retry_rate_limited<T>(
    mut call: impl FnMut() -> Result<T, infino::InfinoError>,
) -> Result<T, infino::InfinoError> {
    const ATTEMPTS: usize = 5;
    let mut attempt = 1;
    loop {
        match call() {
            Err(e) if attempt < ATTEMPTS && e.to_string().contains("server returned 429") => {
                eprintln!("rate limited (attempt {attempt} of {ATTEMPTS}); retrying in 1s");
                std::thread::sleep(std::time::Duration::from_secs(1));
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// The response header the platform reports its query time in, in ms. Hosted
/// latency is taken from it, so the column measures the platform's query
/// engine rather than the network and gateway in front of it.
#[cfg(feature = "hosted")]
pub const QUERY_TIME_HEADER: &str = "x-infino-query-time";

/// Queries against a hosted table, sent directly rather than through infino's
/// client, which does not expose response headers. The request bodies are the
/// ones infino's remote client sends for `bm25_search` and `count`.
#[cfg(feature = "hosted")]
pub struct HostedQueries {
    agent: ureq::Agent,
    base: String,
    database: String,
    table: String,
    bearer: String,
}

#[cfg(feature = "hosted")]
impl HostedQueries {
    /// `target` is `https://host/v1/db` or `https://host/db`.
    pub fn new(target: &str, table: &str) -> Self {
        let uri = target.replacen("/v1/", "/", 1);
        let uri = uri.trim_end_matches('/');
        let (base, database) = uri.rsplit_once('/').expect("hosted target names a database");
        let key = std::env::var("INFINO_API_KEY").expect("INFINO_API_KEY is required");
        HostedQueries {
            agent: ureq::AgentBuilder::new().build(),
            base: base.to_string(),
            database: database.to_string(),
            table: table.to_string(),
            bearer: format!("Bearer {key}"),
        }
    }

    /// Run a top-k search; returns the platform's query time in microseconds.
    pub fn bm25_search(&self, query: &str, k: usize) -> Result<u64, infino::InfinoError> {
        let body = serde_json::json!({
            "table_name": self.table, "field_name": COLUMN, "query": query,
            "k": k, "mode": "or", "projection": null,
        });
        let response = self.post("bm25_search", &body)?;
        let micros = query_time_micros(&response)?;
        // Drain the Arrow body so the connection can be reused.
        std::io::Read::read_to_end(&mut response.into_reader(), &mut Vec::new())
            .map_err(|e| infino::InfinoError::Backend(format!("bm25_search: read body: {e}")))?;
        Ok(micros)
    }

    /// Count matches; returns the count and the platform's query time in
    /// microseconds.
    pub fn count(&self, query: &str) -> Result<(u64, u64), infino::InfinoError> {
        let body = serde_json::json!({
            "table_name": self.table, "field_name": COLUMN, "query": query, "mode": "or",
        });
        let response = self.post("count", &body)?;
        let micros = query_time_micros(&response)?;
        let json: serde_json::Value = response
            .into_json()
            .map_err(|e| infino::InfinoError::Backend(format!("count: read body: {e}")))?;
        let count = json
            .get("count")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| infino::InfinoError::Backend("count response missing `count`".into()))?;
        Ok((count, micros))
    }

    fn post(&self, op: &str, body: &serde_json::Value) -> Result<ureq::Response, infino::InfinoError> {
        let url = format!("{}/v1/{op}/{}", self.base, self.database);
        retry_rate_limited(|| {
            self.agent
                .post(&url)
                .set("Authorization", &self.bearer)
                .set("Accept", "application/vnd.apache.arrow.stream")
                .send_json(body.clone())
                .map_err(|e| match e {
                    ureq::Error::Status(code, r) => {
                        let trace = r.header("x-infino-trace-id").unwrap_or("-").to_string();
                        let body = r.into_string().unwrap_or_default();
                        infino::InfinoError::Backend(format!(
                            "{op}: server returned {code} (trace {trace}): {body}"
                        ))
                    }
                    ureq::Error::Transport(t) => {
                        infino::InfinoError::Backend(format!("{op}: transport error: {t}"))
                    }
                })
        })
    }
}

/// The platform's query time for one response, in microseconds. A response
/// without the header is an error: falling back to wall time would mix two
/// different measurements in one column.
#[cfg(feature = "hosted")]
fn query_time_micros(response: &ureq::Response) -> Result<u64, infino::InfinoError> {
    let raw = response.header(QUERY_TIME_HEADER).ok_or_else(|| {
        infino::InfinoError::Backend(format!("response has no {QUERY_TIME_HEADER} header"))
    })?;
    let ms: f64 = raw.trim().parse().map_err(|_| {
        infino::InfinoError::Backend(format!("{QUERY_TIME_HEADER} is not a number: {raw:?}"))
    })?;
    Ok((ms * 1000.0).round() as u64)
}

/// The number of documents the hosted table should hold: from
/// `INFINO_BENCH_EXPECTED_DOCS`, else the scale (`1B` = 1,000,000,000), else
/// the line count of `CORPUS`. `None` if none of these is available.
#[cfg(feature = "hosted")]
pub fn expected_docs() -> Option<u64> {
    if let Some(n) = std::env::var("INFINO_BENCH_EXPECTED_DOCS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
    {
        return Some(n);
    }
    if let Ok(scale) = std::env::var("INFINO_BENCH_SCALE") {
        return match scale.as_str() {
            "1M" => Some(1_000_000),
            "100M" => Some(100_000_000),
            "1B" => Some(1_000_000_000),
            "10B" => Some(10_000_000_000),
            "100B" => Some(100_000_000_000),
            "1T" => Some(1_000_000_000_000),
            _ => None,
        };
    }
    let corpus = std::env::var("CORPUS").ok()?;
    let file = std::fs::File::open(corpus).ok()?;
    use std::io::BufRead;
    let lines = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .count();
    Some(lines as u64)
}

/// User schema (the `_id` column is auto-injected by the supertable).
pub fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new(
        COLUMN,
        DataType::LargeUtf8,
        false,
    )]))
}

/// Number of writer-pool threads (also the number of segments produced per
/// commit, since a commit shards across `min(pool_threads, rows)`).
///
/// Tuned for c7i.2xlarge (8 vCPU, 16 GiB). Capped at 4: fewer threads → fewer
/// segments per commit (~12 total vs ~24 at 8) → less query-time fan-out, and
/// lower build peak memory (4 parallel shard builds instead of 8). Build is a
/// little slower but stays well within 16 GiB. A post-ingest `optimize`
/// (see `build_index`) then compacts every segment into one superfile.
pub fn writer_threads() -> usize {
    // A scale run on a large host wants more; INFINO_BENCH_WRITER_THREADS
    // overrides. The pool is not part of the options digest, so changing it
    // never stops an existing index from opening.
    if let Some(n) = std::env::var("INFINO_BENCH_WRITER_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(4)
}

/// Threads for one query's CPU work, from `BENCH_QUERY_THREADS`. Default 1.
pub fn query_threads() -> usize {
    std::env::var("BENCH_QUERY_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|n| n.max(1))
        .unwrap_or(1)
}

/// Storage for `target`: `gs://bucket/prefix` keeps the superfiles in object
/// storage with local disk as cache only; anything else is a filesystem path.
///
/// GCS credentials resolve through the ambient instance metadata chain, so the
/// host's service account is what grants access and nothing is read from the
/// process environment.
pub fn storage_for(target: &str) -> Arc<dyn StorageProvider> {
    match target.strip_prefix("gs://") {
        Some(rest) => {
            let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
            Arc::new(
                GcsStorageProvider::new_with_prefix(bucket, prefix, &HashMap::new())
                    .expect("build GCS storage provider"),
            )
        }
        None => Arc::new(LocalFsStorageProvider::new(target).expect("open local storage")),
    }
}

/// Options shared by builder and reader.
///
/// Bounded-memory build: a multi-thread writer pool builds segments in parallel
/// and a `with_commit_threshold_size_mb` auto-flush caps the in-memory write
/// buffer, so the corpus is committed in several rounds rather than held whole
/// in RAM. This emits multiple superfiles; `build_index`'s post-ingest
/// `optimize` compacts them into one.
pub fn options(storage: Arc<dyn StorageProvider>) -> SupertableOptions {
    let writer_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(writer_threads())
            .build()
            .expect("build writer pool"),
    );
    // BENCH_QUERY_THREADS sizes the pool that runs a query's CPU work: page
    // decode, scoring, rerank. Default 1 — which is the nightly — keeps the
    // single-threaded measurement the standard benchmark is built around.
    //
    // A scale run sets it to the host's cores. A machine answering queries over
    // a billion documents uses what it has, and one core of a 44-core host
    // describes nothing anyone would deploy.
    let reader_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(query_threads())
            .build()
            .expect("build reader pool"),
    );

    let disk_cache = DiskCacheStore::new(
        Arc::clone(&storage),
        DiskCacheConfig {
            cache_root: std::env::var("INFINO_BENCH_CACHE_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::env::temp_dir().join("infino-bench-disk-cache")),
            // A scale index is far larger than RAM, so the cache holds it on
            // local disk and the OS page cache decides residency.
            disk_budget_bytes: std::env::var("INFINO_BENCH_CACHE_BUDGET_GB")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|gb| gb * 1024 * 1024 * 1024)
                .unwrap_or(DiskCacheConfig::default().disk_budget_bytes),
            // No idle MADV_DONTNEED sweep while benching: it would inject
            // cold-page spikes that belong to the sweeper, not the engine.
            mmap_cold_threshold_secs: 0,
            // Verifying the CRC of every cached superfile at open costs tens of
            // minutes on a multi-terabyte index.
            verify_crc_on_open: std::env::var("INFINO_BENCH_VERIFY_CRC")
                .map(|v| v != "0")
                .unwrap_or(true),
            ..Default::default()
        },
        Arc::new(HashSet::new),
    )
    .expect("build disk cache");

    SupertableOptions::new(
        schema(),
        // Token positions on (phrase queries are first-class); the text is
        // index-only (stored(false)), matching how the other engines build
        // the SBG index — Lucene does not store the body either. The
        // default `standard` analyzer (UAX #29 + Unicode lowercase) is the
        // Lucene-parity tokenizer, same as the infino-0.11.2 engine.
        vec![FtsConfig::new(COLUMN)
            .positions(true)
            .stored(false)],
        vec![],
    )
    .expect("valid supertable options")
    .with_partition_strategy(PartitionStrategy::Hash {
        column: "_id".to_string(),
        n_buckets: 1,
    })
    .with_writer_pool(writer_pool)
    .with_reader_pool(reader_pool)
    // Snapshot read consistency: the bench index is built once and read many
    // times in-process, so pin the manifest at open and never pay the
    // per-query pointer re-check that the default BoundedStaleness policy does.
    .with_read_consistency(Consistency::Snapshot)
    // Auto-flush every 4 GiB of buffered rows so each commit's peak memory
    // stays bounded (buffer + that chunk's index) instead of the whole corpus
    // at once. Ingest emits several superfiles; `build_index`'s post-ingest
    // `optimize` compacts them into one.
    .with_commit_threshold_size_mb(4096)
    .with_storage(storage)
    .with_disk_cache(disk_cache)
}
