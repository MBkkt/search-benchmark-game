#!/usr/bin/env bash
# Build infino's index of a registry corpus at a scale, into object storage.
#
#   make index-scale \
#       INDEX_URI=gs://<bucket>/<prefix> \
#       CACHE_DIR=/<local disk>/<cache> \
#       CORPUS_NAME=webcrawl SCALE=1B
#
# The corpus comes from src/corpora.py, so the index holds exactly the first
# SCALE documents of the corpus the page names. INDEX_URI must be empty: a
# build never deletes what is already there, because what is there may be the
# index behind a published page. `make bench-scale` with the same arguments
# then queries the result, and its page carries what this build cost.
#
# Local disk holds the ingest spill (SPILL_DIR) and the read cache (CACHE_DIR);
# neither is authoritative. A build that would not fit fails before starting.
set -euo pipefail

: "${INDEX_URI:?INDEX_URI is required (e.g. gs://bucket/prefix)}"
: "${CORPUS_NAME:?CORPUS_NAME is required (e.g. webcrawl)}"
: "${SCALE:?SCALE is required (e.g. 1B)}"
: "${CACHE_DIR:?CACHE_DIR is required (a local disk directory)}"
SPILL_DIR="${SPILL_DIR:-$(dirname "$CACHE_DIR")/spill}"
CACHE_BUDGET_GB="${CACHE_BUDGET_GB:-4096}"
# A 44-core build box keeps 16 writer threads busy without running out of the
# memory each one's in-flight segment holds.
WRITER_THREADS="${WRITER_THREADS:-16}"

REPO="$(cd "$(dirname "$0")/.." && pwd)"
ENGINE="$REPO/engines/infino-branch"
STATS="$REPO/build-$CORPUS_NAME-$SCALE.json"

python3 "$REPO/src/corpora.py" docs --corpus "$CORPUS_NAME" --scale "$SCALE" >/dev/null

if [ -n "$(gcloud storage ls "$INDEX_URI/**" 2>/dev/null | head -1)" ]; then
  echo "index-scale: $INDEX_URI already holds objects; refusing to build over them" >&2
  exit 1
fi

python3 "$REPO/src/corpora.py" check-disk --corpus "$CORPUS_NAME" --scale "$SCALE" \
  --dir "$CACHE_DIR" --dir "$SPILL_DIR"

make -C "$ENGINE" compile

echo "--- Building $CORPUS_NAME at $SCALE into $INDEX_URI ---"
rm -f "$STATS"
python3 "$REPO/src/corpora.py" stream --corpus "$CORPUS_NAME" --scale "$SCALE" \
  | TMPDIR="$SPILL_DIR" \
    INFINO_BENCH_INDEX_URI="$INDEX_URI" \
    INFINO_BENCH_CACHE_DIR="$CACHE_DIR" \
    INFINO_BENCH_CACHE_BUDGET_GB="$CACHE_BUDGET_GB" \
    INFINO_BENCH_VERIFY_CRC=0 \
    INFINO_BENCH_WRITER_THREADS="$WRITER_THREADS" \
    INFINO_BENCH_MULTI_SUPERFILE=1 \
    INFINO_BENCH_BUILD_STATS="$STATS" \
    "$ENGINE/target/release/build_index" "$INDEX_URI"

# What the index occupies in the store of record, beside what it cost.
INDEX_BYTES="$(gcloud storage du -s "$INDEX_URI" | awk '{print $1}')"
python3 - "$STATS" "$INDEX_URI" "$INDEX_BYTES" <<'EOF'
import json, sys
stats_path, uri, index_bytes = sys.argv[1:4]
stats = json.load(open(stats_path))
stats["index"] = uri
stats["index_bytes"] = int(index_bytes)
json.dump(stats, open(stats_path, "w"))
print("--- Built: %s ---" % json.dumps(stats))
EOF
