#!/bin/bash
# Bench-box bootstrap for the nightly CI bench.
#
# Cloud-neutral: everything that differs between clouds lives in
# scripts/cloud-shim-{gcp,aws}.sh, which the workflow splices in at the
# shim marker below. The __*__ run parameters are substituted by the GitHub
# Actions workflow at launch time.
exec >> /var/log/sbg-bench.log 2>&1

mkdir -p /run/sbg

# The shim is written to a file rather than inlined so that bench.sh — which
# runs as the unprivileged bench user, not root — can source the same
# definitions for its corpus fetch and results upload.
cat > /run/sbg/cloud-shim.sh << 'SHIM_EOF'
__CLOUD_SHIM__
SHIM_EOF
chmod 644 /run/sbg/cloud-shim.sh
source /run/sbg/cloud-shim.sh

signal_done() { cloud_signal "$1"; }
trap 'echo "=== disk usage at exit ==="; df -h; signal_done error' EXIT

cloud_install_deps

# The bench user pre-exists on some images (ec2-user) and not others.
id -u "$BENCH_USER" &>/dev/null || useradd -m "$BENCH_USER"
usermod -aG docker "$BENCH_USER"
BENCH_HOME=$(getent passwd "$BENCH_USER" | cut -d: -f6)

# write the GitHub token to tmpfs so bench.sh can read it for git clone
printf '%s' '__GH_TOKEN__' > /run/sbg/gh-token
chmod 644 /run/sbg/gh-token   # the bench user needs to read this

# write per-user bench script
cat > /tmp/bench.sh << 'BENCH_EOF'
#!/bin/bash
set -euo pipefail

source /run/sbg/cloud-shim.sh

# Index builds open far more files than a stock limit allows (they die with
# TooManyOpenFiles). The shim raises the hard limit where the distro needs it.
ulimit -n 65535 || true

# Index-build spill scratch goes to $TMPDIR. Where /tmp is a tmpfs it is sized
# to a fraction of RAM, which a positional index build overflows, and tmpfs
# pages compete with the writer's own memory. Point scratch at the data disk.
mkdir -p "$HOME/tmp"
export TMPDIR="$HOME/tmp"

# __INFINO_BRANCH__, __INFINO_REPO__, and __SBG_BRANCH__ are substituted by the
# GitHub Actions workflow at launch time. SBG_BRANCH is the ref the workflow was
# dispatched on, so the harness code that runs is the same code that launched it.
INFINO_BRANCH="__INFINO_BRANCH__"
INFINO_REPO="__INFINO_REPO__"
SBG_BRANCH="__SBG_BRANCH__"
# When "true", also bench lucene/tantivy alongside the branch and its main
# baseline on THIS instance, so branch-vs-lucene is free of cross-instance
# variance too. On by default (the workflow input); 'false' trims the run to
# the branch column alone, compared cross-run against the committed baseline.
# It has no effect on a main run — see IS_BRANCH_RUN below.
SAME_BOX="__SAME_BOX__"

# A branch/fork run is anything other than the official infino-ai/infino main.
# A main run has no branch column: infino-main already covers that code.
IS_BRANCH_RUN=false
if [ "$INFINO_BRANCH" != "main" ] || [ "$INFINO_REPO" != "infino-ai/infino" ]; then
  IS_BRANCH_RUN=true
fi

# Rust toolchain always needed (infino + tantivy; rust-toolchain.toml pins version)
if ! command -v rustup &>/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
source "$HOME/.cargo/env"
# pre-install the pinned version so the first cargo build doesn't stall
rustup toolchain install 1.95.0

# JDK 21 only needed for lucene — benched on the official main nightly and on
# same-box runs; skipped on fast branch/fork runs (the branch column only).
if { [ "$INFINO_BRANCH" = "main" ] && [ "$INFINO_REPO" = "infino-ai/infino" ]; } \
  || [ "$SAME_BOX" = "true" ]; then
  if [ ! -d "$HOME/jdk-21.0.8+9" ]; then
    wget -q \
      "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.8%2B9/OpenJDK21U-jdk_x64_linux_hotspot_21.0.8_9.tar.gz" \
      -O /tmp/jdk.tar.gz
    tar xzf /tmp/jdk.tar.gz -C "$HOME" && rm /tmp/jdk.tar.gz
  fi
  export JAVA_HOME="$HOME/jdk-21.0.8+9"
  export PATH="$PATH:$JAVA_HOME/bin"
fi

GH_TOKEN=$(cat /run/sbg/gh-token)

# Two infino checkouts, one per path-dep engine (public repo and public forks
# — no token needed):
#   $HOME/infino-main -> engines/infino-main, ALWAYS infino-ai/infino at main,
#                        so main is a constant column no matter what is under
#                        test. Cloned unconditionally: cheap next to the bench,
#                        and it keeps a hand-run `make ENGINES=infino-main` on
#                        the box working even on a run that skips that column.
#   $HOME/infino      -> engines/infino-branch, the dispatched repo/ref. Only
#                        cloned for a branch/fork run; a main run has nothing to
#                        put in that column that infino-main isn't already.
# (engines/infino-0.11.2 needs neither — it builds the published crate.)
git clone "https://github.com/infino-ai/infino.git" "$HOME/infino-main"
git -C "$HOME/infino-main" checkout main

if [ "$IS_BRANCH_RUN" = "true" ]; then
  git clone "https://github.com/${INFINO_REPO}.git" "$HOME/infino"
  git -C "$HOME/infino" checkout "$INFINO_BRANCH"
fi

git clone "https://x-access-token:${GH_TOKEN}@github.com/infino-ai/search-benchmark-game.git" \
  "$HOME/search-benchmark-game"
git -C "$HOME/search-benchmark-game" checkout "$SBG_BRANCH"

# The iresearch (SereneDB) engine is only benched on the official main nightly
# (it's in the Makefile default ENGINES, and branch/fork runs override ENGINES).
# Its source is a public submodule pinned via an SSH URL; this box authenticates
# over HTTPS, so rewrite git@ -> https and fetch it plus its nested third_party
# recursively. Skipped on branch/fork runs to avoid the large checkout.
if [ "$IS_BRANCH_RUN" = "false" ]; then
  git config --global url."https://github.com/".insteadOf "git@github.com:"
  git -C "$HOME/search-benchmark-game" submodule update --init --recursive \
    engines/iresearch-26.03.1/serenedb
fi

cd "$HOME/search-benchmark-game"

# The bench bucket holds a prebuilt corpus so every run indexes byte-identical
# input. If it is missing — a freshly provisioned bucket — build it from the
# public source and publish it, so the next run is fast and gets exactly these
# bytes. Self-seeding beats a manual setup step that is only ever done once and
# is therefore always forgotten.
if cloud_get corpus.json corpus.json; then
  echo "corpus: fetched from the bench bucket"
else
  echo "corpus: NOT in the bench bucket — building from source and seeding it."
  echo "corpus: this adds roughly half an hour to THIS run only."
  make corpus
  cloud_put corpus.json corpus.json
fi

# Engine selection:
#   - same-box branch run (default): branch (infino-branch) + main baseline
#     (infino-main) + lucene + tantivy, all on this instance;
#   - fast branch run (same_box=false): infino-branch alone (~30 min saved),
#     compared cross-run against the committed main baseline;
#   - official main nightly: the default full set from the Makefile
#     (infino-0.11.2 + infino-main + the competitor engines).
MAKE_ARGS=()
if [ "$IS_BRANCH_RUN" = "true" ] && [ "$SAME_BOX" = "true" ]; then
  # Branch benched both FIRST and LAST (infino-branch ... infino-branch-last):
  # the engines are measured sequentially in this order, so pinning the branch
  # to a single position biases branch-vs-main by whatever within-run state the
  # other engines leave behind. `infino-branch-last` re-benches the same branch
  # build + index in the last slot (no extra compile/index), so the fork page
  # can show both positions and separate real deltas from measurement-position
  # bias.
  MAKE_ARGS+=(ENGINES="infino-branch infino-main tantivy-0.26 lucene-10.5.0 infino-branch-last")
elif [ "$IS_BRANCH_RUN" = "true" ]; then
  MAKE_ARGS+=(ENGINES=infino-branch)
fi

# compile + index once, then run both bench modes without re-indexing.
# bench-full runs first: writes results.json then renames to results-full.json.
# bench runs second: writes a fresh results.json (turbopuffer comparison).
make "${MAKE_ARGS[@]}" compile
make "${MAKE_ARGS[@]}" index
make "${MAKE_ARGS[@]}" bench-full   # full 962-query standard → results-full.json
make "${MAKE_ARGS[@]}" bench        # turbopuffer comparison  → results.json

# only upload to canonical keys for the official repo on main; everything else
# uses a run-specific key so nightly results are never clobbered
if [ "$INFINO_BRANCH" = "main" ] && [ "$INFINO_REPO" = "infino-ai/infino" ]; then
  cloud_put results.json      results.json
  cloud_put results-full.json results-full.json
else
  RUN_SLUG="${INFINO_REPO//\//-}-${INFINO_BRANCH//\//-}"
  cloud_put results.json      "results-branch-${RUN_SLUG}.json"
  cloud_put results-full.json "results-full-branch-${RUN_SLUG}.json"
fi
BENCH_EOF

chmod +x /tmp/bench.sh

# -H sets HOME to the bench user's home so rustup, cargo and git all install
# and resolve paths under the correct home directory
if sudo -H -u "$BENCH_USER" bash /tmp/bench.sh; then
  trap - EXIT
  signal_done ok
else
  exit 1   # EXIT trap fires → signal_done error + log upload
fi
