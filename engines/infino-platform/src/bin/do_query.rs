//! Serve count / top-k queries against a table on the hosted platform.
//!
//! Reads `COMMAND\t<lucene-query>` lines from stdin and prints one
//! `<count>\t<us>` line per query, where `<us>` is the platform's own query
//! time from the `x-infino-query-time` response header. The harness times
//! this engine by that value instead of the round trip, which is mostly
//! network and gateway (see src/client.py).
//!
//! Supported: COUNT, TOP_10/100/1000, TOP_{1,5,10,100,1000}_COUNT, with the
//! same query semantics as the local infino engines (BoolMode::Or, +/- sigils
//! and quoted phrases parsed by infino). TOP_k_COUNT sends a search and a
//! count, and reports the sum of their query times. `*_FF` is answered
//! "UNSUPPORTED".

use std::io::{self, BufRead};

use infino::InfinoError;
use infino_platform::{HostedQueries, QUERY_TIME_HEADER, retry_rate_limited};

fn main() {
    let host = infino_platform::host();
    let table = infino_platform::table_name();
    let conn = infino_platform::connect(&host);

    // Check the row count first, so a partial or wrong table is never benched.
    let held = retry_rate_limited(|| infino_platform::doc_count(&conn, &table))
        .unwrap_or_else(|e| {
            eprintln!("hosted error counting table {table} at {host}: {e}");
            std::process::exit(1);
        });
    match infino_platform::expected_docs() {
        Some(expected) if expected != held => {
            eprintln!("ERROR: hosted table {table} at {host} holds {held} docs, the run expects {expected}");
            std::process::exit(1);
        }
        Some(_) => eprintln!("hosted: querying table {table} ({held} docs) at {host}"),
        None => eprintln!("hosted: querying table {table} ({held} docs) at {host}; row count not checked"),
    }
    // Open through infino's client so a missing table fails here, then query
    // directly: infino's client does not expose the query-time header.
    retry_rate_limited(|| conn.open_table(&table)).expect("open hosted table");
    eprintln!("hosted: timing each query by the {QUERY_TIME_HEADER} response header");
    let index = HostedQueries::new(&host, &table);

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.expect("read line");
        let mut parts = line.splitn(2, '\t');
        let command = parts.next().unwrap_or("");
        let query = parts.next().unwrap_or("");

        let result: Result<(u64, u64), InfinoError> = match command {
            _ if query.split_whitespace().all(|t| t.starts_with('-') || t.trim().is_empty()) => {
                // negation-only: no positive terms to rank
                Ok((0, 0))
            }
            _ if query.trim().is_empty() => Ok((0, 0)),
            "TOP_10" | "TOP_100" | "TOP_1000" => {
                index.bm25_search(query, top_k(command)).map(|us| (1, us))
            }
            "COUNT" => index.count(query),
            // TOP_k_COUNT: fetch the top-k results AND count all matches —
            // two requests, matching what engines like Lucene do for this command.
            "TOP_1_COUNT" | "TOP_5_COUNT" | "TOP_10_COUNT"
            | "TOP_100_COUNT" | "TOP_1000_COUNT" => index
                .bm25_search(query, top_k_count(command))
                .and_then(|search_us| {
                    index.count(query).map(|(n, count_us)| (n, search_us + count_us))
                }),
            _ => {
                println!("UNSUPPORTED");
                continue;
            }
        };
        match result {
            Ok((count, us)) => println!("{count}\t{us}"),
            // Exit rather than print 0, so a failed request is never recorded
            // as a result.
            Err(e) => {
                eprintln!("hosted error for {command:?} {query:?}: {e}");
                std::process::exit(1);
            }
        }
    }
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
