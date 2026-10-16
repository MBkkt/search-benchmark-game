CORPUS := $(shell pwd)/corpus.json
export

# Which corpus corpus.json holds. Every corpus, its source and its document
# count is declared in src/corpora.py; wikipedia is the nightly's.
CORPUS_NAME ?= wikipedia

COMMANDS ?= TOP_10 TOP_100 TOP_1000 TOP_100_COUNT COUNT

# infino-0.11.2 is the published crate (what a user gets from crates.io);
# infino-main is the tip of infino-ai/infino main. Branch runs swap in
# infino-branch (see scripts/user-data-template.sh).
# lucene-10.5.0 stays ahead of lucene-10.5.0-bp: the web page takes the first
# lucene-named column as the 1.00 baseline, so the plain build is the baseline
# and the doc-reordered build reads as a ratio against it.
ENGINES ?= infino-0.11.2 infino-main tantivy-0.26 lucene-10.5.0 lucene-10.5.0-bp iresearch-26.03.1
# Hosted engines appended last, e.g. PLATFORM_ENGINES=infino-platform. Off by
# default; the nightly sets it only on a dispatch with `platform: compare`. An
# ENGINES given on the command line replaces the whole list and this is not
# appended.
PLATFORM_ENGINES ?=
ENGINES += $(PLATFORM_ENGINES)
QUERIES ?= queries.txt
PORT ?= 8080
WARMUP_TIME ?= 60
NUM_ITER ?= 10

# turbopuffer's published snapshot we merge against (their `turbopuffer` column).
# fetch-tpuf refreshes this before bench runs; the static file is the fallback.
TPUF_RESULTS ?= data/turbopuffer-latest.json

help:
	@grep '^[^#[:space:]].*:' Makefile

all: index

corpus:
	@echo "--- Fetching $(CORPUS_NAME)$(if $(SCALE), at $(SCALE)) into $(CORPUS) ---"
	@python3 src/corpora.py stream --corpus $(CORPUS_NAME) $(if $(SCALE),--scale $(SCALE)) > $(CORPUS).part
	@mv $(CORPUS).part $(CORPUS)

clean:
	@echo "--- Cleaning directories ---"
	@rm -fr results
	@for engine in $(ENGINES); do cd ${shell pwd}/engines/$$engine && make clean ; done

index:
	@echo "--- Indexing corpus ---"
	@for engine in $(ENGINES); do cd ${shell pwd}/engines/$$engine && make index || exit 1; done

fetch-tpuf:
	@python3 scripts/fetch_tpuf_latest.py

# Default benchmark = turbopuffer comparison: run infino/tantivy/lucene on
# turbopuffer's exact query set + the commands all three support, then merge
# turbopuffer's published column into results.json.
bench: QUERIES := queries-tpuf.txt
bench: COMMANDS := TOP_10 TOP_100 TOP_1000 COUNT
bench: fetch-tpuf
	@echo "--- Benchmarking (turbopuffer comparison: $(ENGINES)) ---"
	@rm -fr results && mkdir results
	@python3 src/client.py $(QUERIES) $(ENGINES)
	@echo "--- Merging turbopuffer published column ($(TPUF_RESULTS)) ---"
	@python3 scripts/merge_turbopuffer.py results.json $(TPUF_RESULTS) results.json

# Full standard benchmark = the 962-query set with the full command list.
# Outputs results-full.json so it doesn't collide with the tpuf results.json.
bench-full: QUERIES := queries-full.txt
bench-full: COMMANDS := TOP_10 TOP_100 TOP_1000 TOP_100_COUNT COUNT
bench-full:
	@echo "--- Benchmarking (full 962-query standard: $(ENGINES)) ---"
	@rm -fr results && mkdir results
	@python3 src/client.py $(QUERIES) $(ENGINES)
	@mv results.json results-full.json

# A scale run queries an index that already exists on object storage, rather
# than building one: at a billion documents the index is terabytes and takes
# hours to build, so it is built once out of band and served many times.
#
#   make bench-scale \
#       INDEX_URI=gs://<bucket>/<prefix> \
#       CACHE_DIR=/<local disk>/<cache> \
#       CORPUS_NAME=webcrawl SCALE=1B
#
# INDEX_URI is the store of record. CACHE_DIR is a local disk cache in front of
# it and holds no authority: deleting it costs fetches, never data.
#
# Add infino-platform to SCALE_ENGINES for a hosted column. It uses INFINO_HOST
# and INFINO_API_KEY, not INDEX_URI (see engines/infino-platform/Makefile).
SCALE_ENGINES ?= infino-branch
CACHE_BUDGET_GB ?= 4096
# A scale run uses the host's cores for one query. The nightly does not set
# this and stays single-threaded, which is what its comparison is built on.
QUERY_THREADS ?= $(shell nproc 2>/dev/null || echo 1)

# Builds the index a scale run queries, from the corpus registry. Same
# arguments as bench-scale; see scripts/index-scale.sh.
index-scale:
	@test "$(origin CORPUS_NAME)" != file || { echo "CORPUS_NAME is required (e.g. webcrawl)"; exit 1; }
	@scripts/index-scale.sh

bench-scale: QUERIES := queries-full.txt
bench-scale: COMMANDS := TOP_10 TOP_100 TOP_1000 TOP_100_COUNT COUNT
bench-scale:
	@test -n "$(INDEX_URI)" || { echo "INDEX_URI is required (e.g. gs://bucket/prefix)"; exit 1; }
	@test "$(origin CORPUS_NAME)" != file || { echo "CORPUS_NAME is required (e.g. webcrawl)"; exit 1; }
	@test -n "$(SCALE)" || { echo "SCALE is required (e.g. 1B)"; exit 1; }
	@python3 src/corpora.py docs --corpus $(CORPUS_NAME) --scale $(SCALE) > /dev/null
	@echo "--- Benchmarking $(CORPUS_NAME) at $(SCALE) against $(INDEX_URI) ---"
	@# The index is not on local disk, so its size has to be asked for rather
	@# than measured. Failure is fine: the page omits what it does not know.
	$(eval INDEX_BYTES := $(if $(filter gs://%,$(INDEX_URI)),$(shell gsutil du -s "$(INDEX_URI)" 2>/dev/null | awk '{print $$1}')))
	@rm -fr results && mkdir results
	@INFINO_BENCH_INDEX_URI="$(INDEX_URI)" \
	 INFINO_BENCH_CORPUS_NAME="$(CORPUS_NAME)" \
	 INFINO_BENCH_SCALE="$(SCALE)" \
	 INFINO_BENCH_CACHE_DIR="$(CACHE_DIR)" \
	 INFINO_BENCH_CACHE_BUDGET_GB="$(CACHE_BUDGET_GB)" \
	 INFINO_BENCH_VERIFY_CRC=0 \
	 INFINO_BENCH_QUERY_MODE=disk \
	 BENCH_QUERY_THREADS=$(QUERY_THREADS) \
	 RESULTS_PATH=results-$(CORPUS_NAME)-$(SCALE).json \
	 INFINO_BENCH_BUILD_STATS=$(if $(wildcard build-$(CORPUS_NAME)-$(SCALE).json),build-$(CORPUS_NAME)-$(SCALE).json) \
	 INFINO_BENCH_INDEX_BYTES="$(INDEX_BYTES)" \
	 python3 src/client.py $(QUERIES) $(SCALE_ENGINES)
	@echo "--- Wrote results-$(CORPUS_NAME)-$(SCALE).json ---"

compile:
	@echo "--- Compiling binaries ---"
	@for engine in $(ENGINES); do cd ${shell pwd}/engines/$$engine && make compile || exit 1; done

serve:
	@echo "--- Serving results ---"
	@cp results.json web/build/results.json
	@cd web/build && python3 -m http.server $(PORT)
