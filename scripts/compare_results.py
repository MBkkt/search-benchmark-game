#!/usr/bin/env python3
"""
Compare infino's performance between two results.json files.

Usage:
  compare_results.py <baseline.json> <experiment.json> [--label <name>]

Outputs GitHub-flavored markdown for GITHUB_STEP_SUMMARY.
"""
import json
import math
import sys


def median(durations):
    if not durations:
        return None
    s = sorted(durations)
    return s[len(s) // 2]


def fmt_bytes(n):
    if n is None:
        return "n/a"
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if abs(n) < 1024 or unit == "TiB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.2f} {unit}"
        n /= 1024.0


def load_index_sizes(path):
    with open(path) as f:
        return json.load(f).get("index_sizes", {}) or {}


# Which infino column to read on each side, most-preferred first.
#
# The baseline is the committed nightly, which carries both `infino-main` (the
# development line) and `infino-0.11.9` (the published crate); main is the right
# thing to judge a branch against. The experiment is a branch run, whose own
# column is `infino-branch` — on a same-box run it also carries `infino-main`,
# and preferring the branch column there is what makes this branch-vs-main
# rather than main-vs-main. `infino-0.6` / `infino-0.11.9` trail both lists so a
# comparison against a baseline committed before these renames still resolves
# a column instead of silently printing an empty table.
BASELINE_ENGINES = ("infino-main", "infino-0.11.9", "infino-0.6")
EXPERIMENT_ENGINES = ("infino-branch", "infino-main", "infino-0.11.9", "infino-0.6")


def pick_engine(results, preference):
    """First column in `preference` that any metric in `results` carries."""
    present = {engine for engines in results.values() for engine in engines}
    for key in preference:
        if key in present:
            return key
    return None


def load_infino(path, preference):
    """Return ({metric: {query: median_us}}, engine_key) for one results file."""
    with open(path) as f:
        data = json.load(f)
    results = data.get("results", data)
    engine = pick_engine(results, preference)
    out = {}
    for metric, engines in results.items():
        infino = engines.get(engine, []) if engine else []
        out[metric] = {q["query"]: median(q["duration"]) for q in infino if q.get("duration")}
    return out, engine


def compare(baseline_path, experiment_path, label):
    baseline, baseline_engine = load_infino(baseline_path, BASELINE_ENGINES)
    experiment, experiment_engine = load_infino(experiment_path, EXPERIMENT_ENGINES)

    lines = [
        f"## infino: `{label}` vs main\n",
        f"`{experiment_engine or 'none'}` (this run) vs "
        f"`{baseline_engine or 'none'}` (committed baseline)\n",
    ]
    if experiment_engine is None or baseline_engine is None:
        lines.append("No infino column on one side — nothing to compare.\n")
        print("\n".join(lines))
        return

    all_ratios = []

    for metric in sorted(baseline.keys()):
        if metric not in experiment:
            continue
        b_queries = baseline[metric]
        e_queries = experiment[metric]

        rows = []
        for query in sorted(b_queries.keys()):
            if query not in e_queries:
                continue
            b_us = b_queries[query]
            e_us = e_queries[query]
            if b_us is None or e_us is None or b_us == 0:
                continue
            ratio = e_us / b_us
            all_ratios.append(ratio)
            pct = (ratio - 1) * 100
            sign = "+" if pct > 0 else ""
            flag = " ⚠️" if pct > 5 else (" ✅" if pct < -5 else "")
            rows.append((query, b_us, e_us, pct, sign, flag))

        if not rows:
            continue

        lines.append(f"### {metric}\n")
        lines.append("| query | main µs | branch µs | Δ% |")
        lines.append("|---|---:|---:|---:|")
        for query, b_us, e_us, pct, sign, flag in sorted(rows, key=lambda r: r[3]):
            lines.append(f"| `{query}` | {b_us} | {e_us} | {sign}{pct:.1f}%{flag} |")
        lines.append("")

    b_sizes = load_index_sizes(baseline_path)
    e_sizes = load_index_sizes(experiment_path)
    if b_sizes or e_sizes:
        lines.append("### Index size\n")
        lines.append("| engine | main | branch | Δ% |")
        lines.append("|---|---:|---:|---:|")

        def size_row(label, b, e):
            if b and e:
                pct = (e / b - 1) * 100
                delta = f"{'+' if pct > 0 else ''}{pct:.2f}%"
            else:
                delta = "—"
            lines.append(f"| {label} | {fmt_bytes(b)} | {fmt_bytes(e)} | {delta} |")

        # The two infino columns carry different names on each side
        # (`infino-branch` here, `infino-main` in the baseline), so pair them
        # explicitly first; everything else pairs by name, in the ENGINES /
        # results.json order (branch first, then any baseline-only engine) so
        # this matches the latency ordering rather than alphabetizing.
        infino_label = (
            experiment_engine if experiment_engine == baseline_engine
            else f"{experiment_engine} vs {baseline_engine}"
        )
        size_row(infino_label,
                 b_sizes.get(baseline_engine), e_sizes.get(experiment_engine))
        paired = {baseline_engine, experiment_engine}
        for engine in dict.fromkeys(list(e_sizes) + list(b_sizes)):
            if engine in paired:
                continue
            size_row(engine, b_sizes.get(engine), e_sizes.get(engine))
        lines.append("")

    if all_ratios:
        gmean = math.exp(sum(math.log(r) for r in all_ratios) / len(all_ratios))
        gpct = (gmean - 1) * 100
        sign = "+" if gpct > 0 else ""
        verdict = "faster ✅" if gpct < -1 else ("slower ⚠️" if gpct > 1 else "no change")
        lines.append(f"**Geometric mean: {sign}{gpct:.1f}% ({verdict} vs main)**\n")

    print("\n".join(lines))


if __name__ == "__main__":
    args = sys.argv[1:]
    label = "experiment"
    if "--label" in args:
        idx = args.index("--label")
        label = args[idx + 1]
        args = args[:idx] + args[idx + 2:]

    if len(args) < 2:
        print("Usage: compare_results.py <baseline.json> <experiment.json> [--label <name>]",
              file=sys.stderr)
        sys.exit(1)

    compare(args[0], args[1], label)
