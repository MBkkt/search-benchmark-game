//! Shared code for the build and query binaries: the hosted table's name and
//! schema, the connection, and the query requests.
//!
//! This engine has no local index. `build_index` loads the corpus into a
//! table on the hosted platform, and `do_query` sends each query to it over
//! HTTPS, timing it by the platform's own query-time header.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use arrow_schema::{DataType, Field, Schema};
use infino::{ConnectOptions, Connection, FtsField, IndexSpec, InfinoError};

/// The single indexed full-text column.
pub const COLUMN: &str = "text";

/// The response header the platform reports its query time in, in ms. Latency
/// is taken from it, so the column measures the platform's query engine
/// rather than the network and gateway in front of it.
pub const QUERY_TIME_HEADER: &str = "x-infino-query-time";

/// The database URI, from `INFINO_HOST`: `https://host/v1/db` or
/// `https://host/db`.
pub fn host() -> String {
    std::env::var("INFINO_HOST")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            eprintln!("INFINO_HOST is required (https://<gateway>/v1/<database>)");
            std::process::exit(1);
        })
}

/// User schema (the `_id` column is auto-injected by the platform).
pub fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new(
        COLUMN,
        DataType::LargeUtf8,
        false,
    )]))
}

/// The table's full-text index. Same setup as the local infino engines:
/// standard analyzer, positions on, text not stored.
pub fn index_spec() -> IndexSpec {
    IndexSpec::new().fts(
        FtsField::new(COLUMN)
            .analyzer("standard")
            .positions(true)
            .stored(false),
    )
}

/// The table to use: `INFINO_BENCH_TABLE` if set, else `<corpus>_<scale>` on
/// a scale run (`webcrawl_1b`), else `sbg`.
pub fn table_name() -> String {
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

/// Connect to the database. infino reads the API key from `INFINO_API_KEY`,
/// and expects `https://host/db` rather than `https://host/v1/db`.
pub fn connect(host: &str) -> Connection {
    let uri = host.replacen("/v1/", "/", 1);
    infino::connect_with(&uri, ConnectOptions::default()).expect("connect to hosted database")
}

/// Number of rows in `table`.
pub fn doc_count(conn: &Connection, table: &str) -> Result<u64, InfinoError> {
    use arrow_array::{Array, Int64Array, UInt64Array};
    let batches = conn.query_sql(&format!("SELECT COUNT(*) FROM \"{table}\""))?;
    let column = batches
        .first()
        .filter(|b| b.num_rows() == 1)
        .map(|b| b.column(0).clone())
        .ok_or_else(|| InfinoError::Backend("COUNT(*) returned no row".into()))?;
    if let Some(a) = column.as_any().downcast_ref::<Int64Array>() {
        return Ok(a.value(0) as u64);
    }
    if let Some(a) = column.as_any().downcast_ref::<UInt64Array>() {
        return Ok(a.value(0));
    }
    Err(InfinoError::Backend(format!(
        "COUNT(*) returned {:?}",
        column.data_type()
    )))
}

/// Run `call`, retrying up to 5 times, 1s apart, when the platform answers
/// 429 (rate limited). A 429 is rejected before any work is done, so retrying
/// is safe even for appends. Other errors are returned as is: the request may
/// have been applied, and repeating an append could add rows twice.
pub fn retry_rate_limited<T>(
    mut call: impl FnMut() -> Result<T, InfinoError>,
) -> Result<T, InfinoError> {
    const ATTEMPTS: usize = 5;
    let mut attempt = 1;
    loop {
        match call() {
            Err(e) if attempt < ATTEMPTS && e.to_string().contains("server returned 429") => {
                eprintln!("rate limited (attempt {attempt} of {ATTEMPTS}); retrying in 1s");
                std::thread::sleep(Duration::from_secs(1));
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// The number of documents the table should hold: from
/// `INFINO_BENCH_EXPECTED_DOCS`, else the scale (`1B` = 1,000,000,000), else
/// the line count of `CORPUS`. `None` if none of these is available.
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

/// Queries against the hosted table, sent directly rather than through
/// infino's client, which does not expose response headers. The request
/// bodies are the ones infino's remote client sends for `bm25_search` and
/// `count`.
///
/// Requests are paced to stay under the platform's per-account rate limit.
/// The pause comes before a request is sent, and the harness times the query
/// by the platform's reported query time, so pacing never shows up in a
/// measurement.
pub struct HostedQueries {
    agent: ureq::Agent,
    base: String,
    database: String,
    table: String,
    bearer: String,
    interval: Duration,
    last_sent: Mutex<Option<Instant>>,
}

impl HostedQueries {
    /// `host` is `https://host/v1/db` or `https://host/db`. The pace is
    /// `INFINO_BENCH_REQUESTS_PER_SEC` (default 18; the platform allows 20).
    pub fn new(host: &str, table: &str) -> Self {
        let uri = host.replacen("/v1/", "/", 1);
        let uri = uri.trim_end_matches('/');
        let (base, database) = uri.rsplit_once('/').expect("INFINO_HOST names a database");
        let key = std::env::var("INFINO_API_KEY").expect("INFINO_API_KEY is required");
        let per_sec: f64 = std::env::var("INFINO_BENCH_REQUESTS_PER_SEC")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|n: &f64| *n > 0.0)
            .unwrap_or(18.0);
        HostedQueries {
            agent: ureq::AgentBuilder::new().build(),
            base: base.to_string(),
            database: database.to_string(),
            table: table.to_string(),
            bearer: format!("Bearer {key}"),
            interval: Duration::from_secs_f64(1.0 / per_sec),
            last_sent: Mutex::new(None),
        }
    }

    /// Run a top-k search; returns the platform's query time in microseconds.
    pub fn bm25_search(&self, query: &str, k: usize) -> Result<u64, InfinoError> {
        let body = serde_json::json!({
            "table_name": self.table, "field_name": COLUMN, "query": query,
            "k": k, "mode": "or", "projection": null,
        });
        let response = self.post("bm25_search", &body)?;
        let micros = query_time_micros(&response)?;
        // Drain the Arrow body so the connection can be reused.
        std::io::Read::read_to_end(&mut response.into_reader(), &mut Vec::new())
            .map_err(|e| InfinoError::Backend(format!("bm25_search: read body: {e}")))?;
        Ok(micros)
    }

    /// Count matches; returns the count and the platform's query time in
    /// microseconds.
    pub fn count(&self, query: &str) -> Result<(u64, u64), InfinoError> {
        let body = serde_json::json!({
            "table_name": self.table, "field_name": COLUMN, "query": query, "mode": "or",
        });
        let response = self.post("count", &body)?;
        let micros = query_time_micros(&response)?;
        let json: serde_json::Value = response
            .into_json()
            .map_err(|e| InfinoError::Backend(format!("count: read body: {e}")))?;
        let count = json
            .get("count")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| InfinoError::Backend("count response missing `count`".into()))?;
        Ok((count, micros))
    }

    /// POST one query. A gateway error (502/503/504) or a dropped connection
    /// is retried up to 3 times, 1s apart: these are reads, so repeating one is
    /// safe, and one lost upstream connection would otherwise end the column.
    /// 429s are retried by `retry_rate_limited`.
    fn post(&self, op: &str, body: &serde_json::Value) -> Result<ureq::Response, InfinoError> {
        const TRANSIENT_RETRIES: usize = 3;
        let mut retries = 0;
        loop {
            match self.post_once(op, body) {
                Err(e) if retries < TRANSIENT_RETRIES && is_transient(&e) => {
                    retries += 1;
                    eprintln!("{e}; retry {retries} of {TRANSIENT_RETRIES} in 1s");
                    std::thread::sleep(Duration::from_secs(1));
                }
                result => return result,
            }
        }
    }

    fn post_once(&self, op: &str, body: &serde_json::Value) -> Result<ureq::Response, InfinoError> {
        let url = format!("{}/v1/{op}/{}", self.base, self.database);
        retry_rate_limited(|| {
            self.pace();
            self.agent
                .post(&url)
                .set("Authorization", &self.bearer)
                .set("Accept", "application/vnd.apache.arrow.stream")
                .send_json(body.clone())
                .map_err(|e| match e {
                    ureq::Error::Status(code, r) => {
                        let trace = r.header("x-infino-trace-id").unwrap_or("-").to_string();
                        let body = r.into_string().unwrap_or_default();
                        InfinoError::Backend(format!(
                            "{op}: server returned {code} (trace {trace}): {body}"
                        ))
                    }
                    ureq::Error::Transport(t) => {
                        InfinoError::Backend(format!("{op}: transport error: {t}"))
                    }
                })
        })
    }

    /// Wait until `interval` has passed since the previous request.
    fn pace(&self) {
        let mut last = self.last_sent.lock().expect("pace lock");
        if let Some(prev) = *last {
            let since = prev.elapsed();
            if since < self.interval {
                std::thread::sleep(self.interval - since);
            }
        }
        *last = Some(Instant::now());
    }
}

/// Whether `e` is a gateway error or a dropped connection, as `post_once`
/// words them.
fn is_transient(e: &InfinoError) -> bool {
    let msg = e.to_string();
    ["server returned 502", "server returned 503", "server returned 504", "transport error"]
        .iter()
        .any(|needle| msg.contains(needle))
}

/// The platform's query time for one response, in microseconds. A response
/// without the header is an error: falling back to wall time would mix two
/// different measurements in one column.
fn query_time_micros(response: &ureq::Response) -> Result<u64, InfinoError> {
    let raw = response.header(QUERY_TIME_HEADER).ok_or_else(|| {
        InfinoError::Backend(format!("response has no {QUERY_TIME_HEADER} header"))
    })?;
    let ms: f64 = raw.trim().parse().map_err(|_| {
        InfinoError::Backend(format!("{QUERY_TIME_HEADER} is not a number: {raw:?}"))
    })?;
    Ok((ms * 1000.0).round() as u64)
}
