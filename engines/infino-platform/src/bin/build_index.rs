//! Load newline-delimited JSON into a table on the hosted platform.
//!
//! Each input line is `{"id": "...", "text": "...", "sort_field": <u64>}`.
//! Only `text` is indexed. The platform builds and compacts the table itself,
//! so there is no `optimize` step here. After loading, this waits until the
//! table reports every document, so the bench never runs on part of the
//! corpus.
//!
//! If the table already exists, it is reused when its row count matches the
//! corpus. Otherwise this exits with an error: loading again would add rows
//! twice, and the table is left for a person to drop.

use std::env;
use std::io::{self, BufRead};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow_array::{LargeStringArray, RecordBatch};
use infino_platform::retry_rate_limited;
use serde::Deserialize;

// A batch ends at whichever limit comes first. The byte limit keeps a request
// well under the platform's 128 MiB body limit.
const BATCH: usize = 5_000;
const BATCH_BYTES: usize = 32 * 1024 * 1024;

#[derive(Deserialize)]
struct Doc {
    text: String,
}

fn main() {
    let host = infino_platform::host();
    let mut docs = io::stdin()
        .lock()
        .lines()
        .map(|l| l.expect("read line"))
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Doc>(&l).expect("parse json").text)
        .peekable();

    let conn = infino_platform::connect(&host);
    match retry_rate_limited(|| conn.create_database()) {
        Ok(()) | Err(infino::InfinoError::AlreadyExists(_)) => {}
        Err(e) => panic!("create database: {e}"),
    }
    let name = infino_platform::table_name();
    let tables = retry_rate_limited(|| conn.list_tables()).expect("list hosted tables");
    if tables.contains(&name) {
        let expected = docs.count() as u64;
        let held = retry_rate_limited(|| infino_platform::doc_count(&conn, &name))
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

    let table = retry_rate_limited(|| {
        conn.create_table(&name, infino_platform::schema(), infino_platform::index_spec())
    })
    .expect("create hosted table");
    let schema = infino_platform::schema();

    let mut sent: u64 = 0;
    let mut buf: Vec<String> = Vec::with_capacity(BATCH);
    while docs.peek().is_some() {
        let mut bytes = 0;
        while let Some(doc) = docs.peek() {
            if buf.len() == BATCH || (!buf.is_empty() && bytes + doc.len() > BATCH_BYTES) {
                break;
            }
            bytes += doc.len();
            buf.push(docs.next().expect("peeked"));
        }
        let arr = LargeStringArray::from(buf.iter().map(String::as_str).collect::<Vec<_>>());
        let batch =
            RecordBatch::try_new(schema.clone(), vec![Arc::new(arr)]).expect("record batch");
        retry_rate_limited(|| table.append(&batch)).unwrap_or_else(|e| {
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

    // Appended rows may become visible only after the append returns.
    let wait_secs: u64 = env::var("INFINO_BENCH_LOAD_WAIT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        let held = retry_rate_limited(|| infino_platform::doc_count(&conn, &name))
            .expect("count hosted table");
        if held == expected {
            eprintln!("indexed {expected} docs into hosted table {name}");
            return;
        }
        if Instant::now() >= deadline {
            eprintln!("ERROR: hosted table {name} holds {held} of {expected} docs after {wait_secs}s");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}
