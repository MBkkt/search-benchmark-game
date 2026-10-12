This is an example of the commands that had to be executed to run this
benchmark on a fresh new machine running Amazon Linux 2023.

First install some packages:
```
sudo yum install git make gcc gcc-c++ docker
sudo usermod -a -G docker ec2-user
```

At this point you need to log off and on again for the group change to be
effective.

```
sudo systemctl start docker
```

Then install Rust and Java.

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"

wget https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.8%2B9/OpenJDK21U-jdk_x64_linux_hotspot_21.0.8_9.tar.gz
tar -xvzf OpenJDK21U-jdk_x64_linux_hotspot_21.0.8_9.tar.gz
export JAVA_HOME="$PWD/jdk-21.0.8+9/"
export PATH="$PATH:$PWD/jdk-21.0.8+9/bin"
```

All dependencies are installed, we can check out the benchmark and run it.

```
git clone --recurse-submodules git@github.com:quickwit-oss/search-benchmark-game.git
cd search-benchmark-game
```

At this point you may want to edit the Makefile to customize tasks that need to run and

```
make corpus
make compile
make index
make bench
```

You're done, make sure to note the Java / Rust / kernel versions and copy the
results.json file on another machine before shutting this one down.


## The nightly bench box

The nightly workflow (`.github/workflows/nightly-bench.yml`) runs on a
throwaway GCP `c3-highcpu-8` in `infino-dev-ci`, created and deleted per run.
It can be switched back to the original AWS `c7i.2xlarge` by dispatching with
`cloud: aws`; see README's benchmark-environment section for why the two are
not interchangeable for absolute numbers.

The split is deliberate and worth keeping:

- `scripts/bench-cloud.sh` — everything the **runner** does (launch, poll,
  fetch, tear down), dispatching on `$BENCH_CLOUD`.
- `scripts/cloud-shim-{gcp,aws}.sh` — everything the **box** does that differs
  (packages, object storage, the done-signal).
- `scripts/user-data-template.sh` — the bench itself, cloud-neutral. A change
  to how the bench runs belongs here and needs no cloud-specific edit.

One-time GCP setup is `scripts/provision-gcp.sh` (idempotent; re-run it to
check the project still matches).

The bench bucket holds a prebuilt `corpus.json` so every run indexes identical
input. A fresh bucket does not need seeding by hand: the first run finds the
object missing, builds the corpus from the public source and uploads it, which
costs that one run about half an hour. To skip that, seed it first:

```bash
make corpus                                        # downloads + transforms; slow
gcloud storage cp corpus.json gs://sbg-bench-corpus/corpus.json
```

## Scale runs

A scale page answers a different question from the nightly: how infino behaves
over a corpus far larger than Wikipedia, at a fixed document count. Scale runs
are started by hand on a large box (the published pages came from a 44-vCPU,
85 GB host with a 16 TB local disk), never by the nightly, and publish to

```
https://infino-ai.github.io/search-benchmark-game/<fork>/<corpus>/<scale>/full
```

`<fork>` is the owner of the infino repo the run built, `<corpus>` a name from
`src/corpora.py` and `<scale>` one of `1M 100M 1B 10B 100B 1T`. Every corpus,
its source and its document count is declared in `src/corpora.py`, which is
also the only scale parser: a scale larger than the corpus is refused there.
`webcrawl` is 2,100,000,000 FineWeb documents in 2,414 zstd shards in GCS, read
in name order, so the first N documents are the same N at every scale. Set
`CORPUS_MIRROR` to a directory holding some of the shards to read those from
local disk instead; it changes nothing about which documents are read.

```bash
# 1. Build infino's index in object storage. Refuses a non-empty INDEX_URI,
#    and fails up front if CACHE_DIR's disk cannot hold the spill and cache.
make index-scale INDEX_URI=gs://<bucket>/webcrawl/1B CACHE_DIR=/data/cache-1b \
     CORPUS_NAME=webcrawl SCALE=1B

# 2. Query it: 962 queries, five commands, one query using every core.
make bench-scale INDEX_URI=gs://<bucket>/webcrawl/1B CACHE_DIR=/data/cache-1b \
     CORPUS_NAME=webcrawl SCALE=1B

# 3. Publish results-webcrawl-1B.json at /<fork>/webcrawl/1B/full.
python3 scripts/publish_scale_page.py results-webcrawl-1B.json --fork infino-ai
```

The page states the corpus, document count, query set, start time and, when
step 1 built the index, what the build cost.

A Lucene column needs Lucene's index of the same documents. `make corpus
CORPUS_NAME=webcrawl SCALE=<scale>` writes them to `corpus.json`, then
`LUCENE_FORCE_MERGE=0 make -C engines/lucene-10.5.0 index` builds it, leaving
the segments as Lucene's merge policy makes them (infino's index at scale is
many superfiles, so a force-merged Lucene would be compared in a different
shape). Pass `SCALE_ENGINES="infino-branch lucene-10.5.0"` to step 2. Where
the two columns cannot be measured in one run, `scripts/merge_runs.py` splices
them and says so on the page.
