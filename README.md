
# Welcome to Search Benchmark, the Game!

This repository is standardized benchmark for comparing the speed of various
aspects of search engine technologies.

The results are available at:

- **[full benchmark](https://infino-ai.github.io/search-benchmark-game/)** — 962-query standard set (infino, tantivy, lucene, lucene with doc reordering, iresearch), updated nightly. Also served at `/full`.
- **[turbopuffer comparison](https://infino-ai.github.io/search-benchmark-game/tpuf)** — infino vs tantivy vs lucene vs turbopuffer on turbopuffer's 31-query set, updated nightly
- **per-fork branch page** — `https://infino-ai.github.io/search-benchmark-game/<fork_user>/full`, the same full benchmark with the latest branch run from a public infino fork spliced in as an extra infino column. Produced by dispatching the nightly workflow with `infino_repo`/`infino_branch` inputs; each fork's page is overwritten by that fork's next run.

This benchmark is both
- **for users** to make it easy for users to compare different libraries
- **for library** developers to identify optimization opportunities by comparing
their implementation to other implementations.

Currently, the benchmark includes infino, tantivy, Lucene (plain and
doc-reordered), and iresearch
(plus turbopuffer's published numbers on the `/tpuf` page).
It is reasonably simple to add another engine.

You are free to communicate about the results of this benchmark **in
a reasonable manner**.
For instance, twisting this benchmark in marketing material to claim that your search engine is 31x faster than Lucene,
because your product was 31x on one of the test is not tolerated. If this happens, the benchmark will publicly
host a wall of shame.
Bullshit claims about performance are a plague in the database world.


## The benchmark

Different search engine implementation are benched over different real-life tests.
The corpus used is the English wikipedia. Stemming is disabled. Queries have been derived
 from the [AOL query dataset](https://en.wikipedia.org/wiki/AOL_search_data_leak)
 (but do not contain any personal information).

Out of a random sample of query, we filtered queries that had at least two terms and yield at least 1 hit when searches as
a phrase query.

For each of these query, we then run them as :
- `intersection`
- `unions`
- `phrase queries`

with the following collection options :
- `COUNT` only count documents, no need to score them
- `TOP 10` : Identify the 10 documents with the best BM25 score.
- `TOP 10 + COUNT`: Identify the 10  documents with the best BM25 score, and count the matching documents.

We also reintroduced artificially a couple of term queries with different term frequencies.

All tests are run once in order to make sure that
- all of the data is loaded and in page cache
- Java's JIT already kicked in.

Test are run in a single thread.
Out of 10 runs, we only retain the best score, so Garbage Collection likely does not matter.

### Benchmark environment

The local results (infino, tantivy, lucene) were generated on:

| | |
|---|---|
| Instance | GCP **c3-highcpu-8** (8 vCPU, 16 GiB RAM), us-central1-a |
| CPU | Intel Xeon Platinum 8481C @ 2.70 GHz (Sapphire Rapids) |
| OS | Rocky Linux 9 (tracks the `rocky-linux-9` image family) |
| Rust | 1.95.0 |
| JDK | Adoptium Temurin 21.0.8+9 |

Runs before 2026-10-06 used an AWS **c7i.2xlarge** (8 vCPU, 16 GiB, Intel Xeon
Platinum 8488C, Amazon Linux 2023) in us-east-1. That instance type was chosen
to match the one turbopuffer used in their published benchmark; the c3-highcpu-8
is its closest equivalent — same Sapphire Rapids generation, same core count and
memory — but it is a different SKU on a different cloud, so **absolute timings
are not comparable across that date**, and a step in the published series at
that point is the hardware change rather than an engine regression. The
c7i.2xlarge path is still supported: dispatch the nightly workflow with
`cloud: aws`.

### How turbopuffer numbers are sourced

We do not re-run turbopuffer ourselves — instead we take the numbers directly
from turbopuffer's own published benchmark snapshot
(`data/turbopuffer-2026-05-20.json`, sourced from
`turbopuffer.github.io/search-benchmark-game`).

After running infino, tantivy, and lucene locally, `scripts/merge_turbopuffer.py`
splices turbopuffer's per-query durations into our `results.json` for the
commands that appear in both files. Only the 31 queries in `queries-tpuf.txt`
(derived verbatim from the turbopuffer snapshot) are used for this comparison —
query alignment is verified by exact string match, so any drift is surfaced as
a warning.

### Apples-to-apples considerations

The comparison is **methodologically equivalent**:

| | infino / tantivy / lucene | turbopuffer |
|---|---|---|
| Hardware | GCP c3-highcpu-8, us-central1-a | AWS c7i.2xlarge (per their published benchmark) |
| Benchmark harness | subprocess stdin/stdout | local HTTP server on the same box |
| Latency measured | wall time including IPC overhead | wall time including localhost HTTP overhead |
| Query set | turbopuffer's exact 31 queries | same 31 queries |
| Commands compared | TOP_10, TOP_100, TOP_1000, COUNT | same |

Turbopuffer's benchmark engine starts a **local** turbopuffer server process
on their box and queries it via `http://localhost:3001` — no external network
call, so the communication overhead difference (stdin/stdout vs localhost HTTP)
is negligible.

The one thing that is **not** matched is the machine. Turbopuffer's published
numbers were measured on an AWS c7i.2xlarge; infino, tantivy and lucene are now
measured on a GCP c3-highcpu-8 of the same generation, core count and memory.
Methodology, corpus and query set are identical, but a cross-cloud hardware
difference of a few percent is folded into every ratio on the tpuf page and
should not be read as an engine difference. To compare on turbopuffer's exact
hardware, dispatch the nightly with `cloud: aws`.

## Engine specific detail

### Lucene

- Query cache is disabled.
- GC should not influence the results as we pick the best out of 5 runs.
- The `-bp` variant implements document reordering via the bipartite graph partitioning algorithm, also called recursive graph bisection.

### Tantivy

- Tantivy returns slightly more results because its tokenizer handles apostrophes differently.
- Tantivy and Lucene both use BM25 and should return almost identical scores.

### infino

infino appears as up to three columns, one per source of the engine code. They
are the same benchmark harness (`engines/infino-*` differ only in where the
crate comes from):

| engine | source | benched on |
|---|---|---|
| `infino-0.11.9` | the published crate, pinned to the 0.11.9 release | every nightly |
| `infino-main` | `infino-ai/infino` at `main`, path-depped at `../../../infino-main` | every nightly, and as the baseline on a branch run |
| `infino-branch` | the repo/ref dispatched into the workflow, path-depped at `../../../infino` | branch and fork runs only |

`infino-branch-last` is not a fourth build: it re-benches the `infino-branch`
binary and index in the last engine slot, so a fork page can show how much of a
branch-vs-main delta is measurement position rather than code.

`infino-platform` benchmarks infino on the hosted infino platform. Each query is
an HTTPS request, and its latency is the platform's own query time from the
`x-infino-query-time` response header, so the network and the platform's
gateway are not counted. It is a separate engine with its own code, built
against the published infino 0.11.9 crate so the client matches the platform's
release; it shares nothing with the local infino engines.

Set `INFINO_HOST` (`https://<gateway>/v1/<database>`) and `INFINO_API_KEY`, in
the environment or in an uncommitted `.env` of plain `KEY=value` lines. The
table is `INFINO_BENCH_TABLE`, defaulting to `<corpus>_<scale>` on a scale run
(`webcrawl_1b`) and `sbg` otherwise.

- `make index` loads the corpus and waits until every row is visible. An
  existing table is reused if its row count matches the corpus; otherwise the
  load fails, and the table must be dropped by hand.
- Before benching, the engine checks the table's row count against the corpus
  (or the scale) and fails on a mismatch.
- The platform allows 20 requests a second per account. The engine paces
  itself at `INFINO_BENCH_REQUESTS_PER_SEC` (default 18); the pause is never
  part of a measured query time.
  A rate-limited request is retried after 1s, and a gateway error
  (502/503/504) or dropped connection up to 3 times; any other error stops the
  engine.
- If an engine exits, or this engine doesn't answer within
  `QUERY_TIMEOUT_SECS` (default 600), it is dropped from the results and listed
  under `run.failed_engines`. A failed local engine makes the run exit non-zero;
  a failed hosted engine only loses its own column.
- Accounts have a row limit (10M by default), so 100M and 1B tables need a
  higher limit.

```sh
make index bench-full ENGINES=infino-platform CORPUS=$PWD/corpus.json
```

The nightly does not bench this column on its own schedule. It's opt-in, through
the workflow's `platform` dispatch input:

- `off` (the default, and always the scheduled nightly): no platform column.
- `compare`: adds `infino-platform` as the last engine of that run, next to
  whatever else the run benches. On a main run (`infino_branch` = `main`) the
  results publish as the official results, platform column included, until the
  next scheduled nightly replaces them. On a branch run the column shows on
  the fork page, same-box or not.
- `only`: benches `infino-platform` alone, publishes nothing, and writes
  per-command latency to the job summary. A quick check that the column works
  on the CI box.

```sh
gh workflow run nightly-bench.yml --ref master -f platform=compare
gh workflow run nightly-bench.yml --ref <branch> -f platform=only
```

`compare` and `only` read the platform's credentials from the cloud's secret
store and fail the run if they are missing or the build or table step fails.
A column that fails mid-bench fails an `only` run; on a `compare` run it is
dropped and listed under `run.failed_engines`, and the other engines' results
stand. Both bench table `sbg_nightly`, which is already loaded and is reused
while its row count matches the corpus. The column adds about an hour to a
run, mostly the paced queries.

One-time setup, GCP (the default cloud). Secret Manager must be enabled in
`infino-dev-ci`:

```sh
printf '%s' 'https://<gateway>/v1/<database>' | gcloud secrets create sbg-infino-host \
  --project infino-dev-ci --data-file=-
printf '%s' '<api key>' | gcloud secrets create sbg-infino-api-key \
  --project infino-dev-ci --data-file=-
for s in sbg-infino-host sbg-infino-api-key; do
  gcloud secrets add-iam-policy-binding "$s" --project infino-dev-ci \
    --member serviceAccount:sbg-bench-vm@infino-dev-ci.iam.gserviceaccount.com \
    --role roles/secretmanager.secretAccessor
done
```

On AWS the same two values are SSM `SecureString` parameters
`/sbg-bench/sbg-infino-host` and `/sbg-bench/sbg-infino-api-key`, readable by
the bench box's role (`ssm:GetParameter` and `kms:Decrypt`).


infino is benchmarked on its **optimized paths only**. Commands without a
first-class implementation return `UNSUPPORTED` rather than falling back to
a slower workaround — so every reported number reflects infino's actual engine.

| Command | Status |
|---|---|
| `TOP_10`, `TOP_100`, `TOP_1000` (union / intersection) | ✅ benchmarked |
| `COUNT`, `TOP_*_COUNT` | ✅ benchmarked — native count path (posting-list traversal, no scoring) |
| Mixed must/should (`+a b`) | ✅ benchmarked — native lucene `BooleanQuery` clause semantics (`+must`, bare should, `-must-not`) |
| Negation (`-term`) | ✅ benchmarked — native must-not exclusion |
| Phrase queries (`"a b"`) | ✅ benchmarked — exact adjacency over positional postings |
| `TOP_*_FILTER_%` | ❌ UNSUPPORTED — results are score-ordered only |

Tokenization: infino's `standard` analyzer (UAX #29 word segmentation, Unicode lowercase, no stemming) — the same split as Lucene's `StandardTokenizer` + `LowerCaseFilter`; on this pre-transformed corpus it reduces to whitespace splitting. BM25 with Lucene defaults (`k1 = 1.2`, `b = 0.75`).

The index is built as multiple on-disk segments and then fully loaded into
memory before benchmarking begins, so the query path is synchronous with no
per-query I/O or async overhead.

Compacting the segments into one reorders the documents by recursive graph
bisection, as the Lucene `-bp` variant does. infino has no setting to turn it
off, so there is no plain infino column to compare against.


# Reproducing

These instructions will get you a copy of the project up and running on your local machine.

### Prerequisites

The lucene benchmarks requires Java, the most recent version is recommended.
The tantivy benchmarks and benchmark driver code requires Cargo. This can be installed using [rustup](https://www.rustup.rs/).

### Installing

Clone this repo.

```
git clone git@github.com:tantivy-search/search-benchmark-game.git
```

## Running

Checkout the [Makefile](Makefile) for all available commands. You can adjust the `ENGINES` parameter for a different set of engines.

Run `make corpus` to download and unzip the corpus used in the benchmark.
```
make corpus
```

Run `make index` to create the indices for the engines.

```
make index
```

Run `make compile` to compile the query execution layer.
Run `make bench` to build the different project and run the benches.
This command may take more than 30mn.

```
make bench
```

The results are outputted in a `results.json` file.

You can then check your results out by running:

```
make serve
```

And open the following in your browser: [http://localhost:8080/](http://localhost:8080/)

For a quick text summary from `results.json` without opening the browser:

```
python tools/analyze.py <engine> <command> [--filter <tag>]
```

- `<engine>` — e.g. `tantivy-main`, `tantivy-0.26`, `iresearch-26.03.1`
- `<command>` — `TOP_100`, `TOP_100_COUNT`, or `COUNT`
- `--filter` — optional tag filter: `union`, `intersection`, or `intersection_union`

Examples:
```
python tools/analyze.py tantivy-main TOP_100_COUNT
python tools/analyze.py tantivy-main TOP_100 --filter union
```

It prints query count, sample count, and avg/min/max/p50/p95/p99 latencies (in µs).

# Adding another search engine

See `CONTRIBUTE.md`.
