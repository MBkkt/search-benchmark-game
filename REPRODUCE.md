# Benchmark runbook

End-to-end steps to reproduce infino vs tantivy vs lucene vs turbopuffer and
publish to <https://infino-ai.github.io/search-benchmark-game>.

---

## Step 1 — Provision the box (from your Mac)

```bash
cd ~/code/infino-ai/search-benchmark-game/terraform
terraform init        # first time only
terraform apply       # type: yes
# prints: public_ip and ssh command
```

Note the `public_ip`. The key is written to `terraform/sbg-bench-key.pem`.

---

## Step 2 — Push the harness to the box (from your Mac)

`infino-0.11.2` pulls the infino crate from crates.io, but the other two infino
engines are path deps and need a checkout each on the box:

| engine | path dep | put on the box at |
|---|---|---|
| `infino-main` | `../../../infino-main` | `~/infino-main`, at `main` |
| `infino-branch` | `../../../infino` | `~/infino`, at the ref you want measured |

`infino-main` is in the default `ENGINES`, so its checkout is required for a
plain run; `infino` is only needed if you add `infino-branch`.

```bash
# set this to whatever terraform printed
export BOX=ec2-user@<public_ip>
export KEY=~/code/infino-ai/search-benchmark-game/terraform/sbg-bench-key.pem

# search-benchmark-game repo
rsync -avz -e "ssh -i $KEY" \
  --exclude '.git' --exclude target --exclude idx \
  --exclude corpus.json --exclude node_modules \
  ~/code/infino-ai/search-benchmark-game/ $BOX:~/search-benchmark-game/

# main checkout — the `infino-main` engine's path dep
rsync -avz -e "ssh -i $KEY" \
  --exclude '.git' --exclude target \
  ~/code/infino-ai/infino/ $BOX:~/infino-main/

# the ref under test — the `infino-branch` engine's path dep (skip if you
# are not benching that column)
rsync -avz -e "ssh -i $KEY" \
  --exclude '.git' --exclude target \
  ~/code/infino-ai/infino/ $BOX:~/infino/
```

---

## Step 3 — Set up toolchains (on the box, one-time)

```bash
ssh -i $KEY $BOX
# ---- everything below runs ON THE BOX ----

tmux new -s bench      # run inside tmux so a disconnect doesn't kill the bench

cd ~/search-benchmark-game
scripts/setup-aws.sh   # installs Rust 1.95, Temurin 21.0.8+9, build deps

source "$HOME/.cargo/env"
export JAVA_HOME="$HOME/jdk-21.0.8+9"
export PATH="$PATH:$JAVA_HOME/bin"
```

---

## Step 4 — Run the benchmark (on the box)

```bash
# still on the box, inside tmux, toolchains exported:
./scripts/run-bench.sh
# => downloads corpus (first time, ~8 GB), compiles all engines, indexes,
#    benchmarks 3 engines × 4 commands × 31 queries × 10 iterations,
#    merges turbopuffer's published column, writes results.json
```

If you get disconnected: `ssh -i $KEY $BOX` then `tmux a -t bench`.

When it finishes you'll see:
```
Done. Result: /home/ec2-user/search-benchmark-game/results.json
```

---

## Step 5 — Pull results and publish (from your Mac)

```bash
# copy results off the box
scp -i $KEY $BOX:~/search-benchmark-game/results.json \
  ~/code/infino-ai/search-benchmark-game/results.json

# verify the merge — should list all 4 engines per command
python3 -c "
import json
d = json.load(open('results.json'))
for cmd, engines in d['results'].items():
    print(cmd, '->', list(engines.keys()))
"

# publish
cd ~/code/infino-ai/search-benchmark-game
git add results.json
git commit -m "bench: $(date -u +%F) turbopuffer comparison"
git push
# GitHub Actions deploys to https://infino-ai.github.io/search-benchmark-game
# in ~1-2 minutes
```

---

## Step 6 — Destroy the box (from your Mac)

```bash
cd ~/code/infino-ai/search-benchmark-game/terraform
terraform destroy     # type: yes
```

Do this as soon as results.json is safely pushed. The box bills hourly.

---

## Subsequent runs (box already set up)

On second and later runs the corpus is cached on the box (don't `make clean`).
Only re-provision if the box was destroyed. If it still exists, skip steps 1–3
and go straight to step 4.

If the infino or search-benchmark-game code changed since the last run,
re-rsync (step 2) before running the bench.

---

## Bench modes

| command | query set | what it runs |
|---|---|---|
| `./scripts/run-bench.sh` | `queries-tpuf.txt` (31 queries) | turbopuffer comparison: TOP_10/100/1000 + COUNT |
| `./scripts/run-bench.sh full` | `queries-full.txt` (962 queries) | full standard benchmark |

---

## Refreshing turbopuffer's numbers

If turbopuffer publishes a newer snapshot:

```bash
NEW=2026-XX-XXT...
curl -s "https://turbopuffer.github.io/search-benchmark-game/$NEW/results.json" \
  -o data/turbopuffer-$NEW.json

# update queries-tpuf.txt from the new snapshot
python3 -c "
import json, sys
d = json.load(open('data/turbopuffer-$NEW.json'))
entries = d['results']['TOP_10']['turbopuffer']
for e in entries:
    print(json.dumps({'query': e['query'], 'tags': e['tags']}))
" > queries-tpuf.txt

# point the Makefile at the new file
sed -i '' "s|TPUF_RESULTS ?=.*|TPUF_RESULTS ?= data/turbopuffer-$NEW.json|" Makefile

git add data/turbopuffer-$NEW.json queries-tpuf.txt Makefile
git commit -m "bench: update turbopuffer snapshot to $NEW"
git push
```
