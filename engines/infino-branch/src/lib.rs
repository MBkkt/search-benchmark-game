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
use infino::config::CrcVerification;
use infino::storage::{GcsStorageProvider, LocalFsStorageProvider, StorageProvider};
use infino::superfile::builder::FtsConfig;
use infino::supertable::manifest::list::PartitionStrategy;
use infino::supertable::reader_cache::{DiskCacheConfig, DiskCacheStore};
use infino::supertable::{Consistency, SupertableOptions};

/// The single indexed full-text column.
pub const COLUMN: &str = "text";

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
            // Unset benches the engine's own default. A scale run sets 0: even
            // verifying once, as superfiles land in the cache, is a full pass
            // over a multi-terabyte index. Any other value verifies on every
            // open, the strictest and slowest policy.
            crc_verification: match std::env::var("INFINO_BENCH_VERIFY_CRC").as_deref() {
                Ok("0") => CrcVerification::Never,
                Ok(_) => CrcVerification::Always,
                Err(_) => CrcVerification::default(),
            },
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
        // Lucene-parity tokenizer, same as the infino-0.11.9 engine.
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
