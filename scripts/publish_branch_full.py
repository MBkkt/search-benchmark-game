#!/usr/bin/env python3
"""Build a per-fork `/full`-style dashboard page for a branch bench run.

Usage:
    publish_branch_full.py \
        --main-full   results-full.json \
        --branch-full results-full-branch-<slug>.json \
        --template    web/build/full/index.html \
        --out-dir     web/build/<fork_user>/full \
        --label       "<repo>@<branch>"

A branch/fork run benches the `infino-branch` engine (the dispatched ref), and
on a same-box run `infino-main` alongside it. To make the published page a true
full-benchmark comparison (not a lone column), a branch-only run splices its
infino column into the committed main baseline — which already carries the
competitor engines and main's own infino — as a new engine `infino @<branch>`.
The page then shows the branch side by side with tantivy, lucene, and main's
infino.

The page reuses the prebuilt `/full` index.html verbatim (same JS bundle, which
fetches `./results.json` relative to the page URL). We only rewrite the static
asset paths for the extra directory depth and retitle the page so it is
self-identifying.
"""
import argparse
import json
import re
import sys
from pathlib import Path


# The branch build is benched twice on a same-box run — first and last in the
# engine order — under these two engine keys (see engines/infino-branch-last
# and scripts/user-data-template.sh). Both are the *same* branch build + index;
# the pair brackets the harness's fixed-order position bias.
BRANCH_ENGINE_FIRST = "infino-branch"
BRANCH_ENGINE_LAST = "infino-branch-last"
BRANCH_ENGINES = frozenset({BRANCH_ENGINE_FIRST, BRANCH_ENGINE_LAST})


def branch_engine_key(label: str) -> str:
    """Column label stem for the branch's infino, e.g. `infino @my-branch`.
    The same-box page suffixes it with ` (first)` / ` (last)`."""
    branch = label.split("@", 1)[1] if "@" in label else label
    return f"infino @{branch}"


def is_same_box(branch_full: dict) -> bool:
    """True when the branch run re-benched the baseline engines alongside the
    branch on one instance (the default same-box A/B). Its own results then
    carry `infino-main` / `lucene` / `tantivy` columns from the *same* run, so
    they are the variance-free comparison and must be used directly — splicing
    the branch column into the committed baseline instead would compare it
    against a different nightly on a different box (cross-run variance that
    reads as a spurious regression on unchanged query classes).

    Detected by the presence of a non-branch engine column: the branch's own
    two positions (`infino-branch` first, `infino-branch-last` last) don't
    count."""
    for engines in branch_full.get("results", {}).values():
        if any(k not in BRANCH_ENGINES for k in engines):
            return True
    return False


def relabel_same_box(branch_full: dict, engine_key: str, label: str) -> dict:
    """Same-box path: publish the branch run's own multi-engine results. The
    branch was benched both first and last, so its two columns are relabeled
    `<engine_key> (first)` / `<engine_key> (last)` — self-identifying next to
    the same-run `infino-main` / lucene / tantivy, and showing the reader how
    much of any branch-vs-main delta is measurement position vs. code."""
    first_key = f"{engine_key} (first)"
    last_key = f"{engine_key} (last)"
    for engines in branch_full["results"].values():
        if BRANCH_ENGINE_FIRST in engines:
            engines[first_key] = engines.pop(BRANCH_ENGINE_FIRST)
        if BRANCH_ENGINE_LAST in engines:
            engines[last_key] = engines.pop(BRANCH_ENGINE_LAST)
    details = branch_full.setdefault("details", {})
    details.pop(BRANCH_ENGINE_FIRST, None)
    details.pop(BRANCH_ENGINE_LAST, None)
    common = (
        f"infino built from {label} (this fork's latest branch run), benched "
        f"on the same instance as the infino-main / lucene / tantivy columns here."
    )
    details[first_key] = [
        common,
        "Measured FIRST in the engine order. Engines are benched sequentially, "
        "so the first and last slots see different within-run machine state; "
        "compare this column with the (last) column to gauge how much of any "
        "branch-vs-main delta is measurement position rather than code.",
    ]
    details[last_key] = [
        common,
        "Measured LAST in the engine order, after infino-main / lucene / tantivy "
        "— same branch build and index as the (first) column.",
    ]
    return branch_full


def merge(main_full: dict, branch_full: dict, engine_key: str, label: str) -> dict:
    """Return main_full with the branch's infino column added as `engine_key`.

    Fallback for a branch-only run (same-box A/B off): the run produced no
    same-instance baseline, so the branch column is compared against the
    committed baseline. This is a *cross-run* comparison — unchanged query
    classes can drift by the box-to-box delta between the two nightlies."""
    branch_results = branch_full["results"]
    for mode, engines in main_full["results"].items():
        branch_col = branch_results.get(mode, {}).get(BRANCH_ENGINE_FIRST)
        if branch_col is None:
            print(f"  warning [{mode}]: branch run has no {BRANCH_ENGINE_FIRST} "
                  f"column; skipping this mode for the branch", file=sys.stderr)
            continue
        engines[engine_key] = branch_col
    main_full.setdefault("details", {})[engine_key] = [
        f"infino built from {label} (this fork's latest branch run; compared "
        f"against the committed baseline — cross-run, not same-box)."
    ]
    return main_full


# A page's reference to the shared bundle, at whatever depth it was written for:
# `./static/` at the site root, `../static/` one level down, and so on.
STATIC_REF = re.compile(r"""(["'])(?:\./|(?:\.\./)+)static/""")


def relocate(html: str, depth: int) -> str:
    """Point a page's asset references at the site root from `depth` levels down.

    Getting this wrong costs the page its stylesheet and script, so callers
    compute the depth from the page's path rather than assume it.
    """
    prefix = "../" * depth if depth else "./"
    return STATIC_REF.sub(lambda m: m.group(1) + prefix + "static/", html)


def retitle(html: str, label: str) -> str:
    """Name the page after what it shows, in the tab and in the heading."""
    html = re.sub(
        r"<title>.*?</title>",
        f"<title>Search benchmark — {label}</title>",
        html,
        count=1,
    )
    return re.sub(
        r"<h1>.*?</h1>",
        f"<h1>Search Benchmark, the Game — {label}</h1>",
        html,
        count=1,
    )


def render_index(template_html: str, label: str, extra_depth: int = 1) -> str:
    """Adapt the prebuilt /full page for deeper nesting + retitle.

    /full sits one level under the site root. A page nested deeper needs one
    `../` per extra level: `<fork>/full` is one deeper,
    `<fork>/<corpus>/<scale>/full` is three.
    """
    return retitle(relocate(template_html, extra_depth + 1), label)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--main-full", required=True)
    p.add_argument("--branch-full", required=True)
    p.add_argument("--template", required=True)
    p.add_argument("--out-dir", required=True)
    p.add_argument(
        "--site-root",
        default="web/build",
        help="site root the out-dir is nested under; the extra asset depth is "
        "measured from it rather than assumed",
    )
    p.add_argument("--label", required=True)
    args = p.parse_args()

    branch_full = json.load(open(args.branch_full))
    engine_key = branch_engine_key(args.label)

    if is_same_box(branch_full):
        # The branch run re-benched all engines on one instance: use its own
        # results so the page is a true same-box comparison.
        merged = relabel_same_box(branch_full, engine_key, args.label)
    else:
        # Branch-only run: splice the lone infino column into the committed
        # baseline (cross-run — see merge()'s docstring).
        main_full = json.load(open(args.main_full))
        merged = merge(main_full, branch_full, engine_key, args.label)

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    json.dump(merged, open(out_dir / "results.json", "w"))
    # `<fork>/full` is one level deeper than `/full`; `<fork>/<corpus>/<scale>/full`
    # is three. Measured, not assumed.
    out_parts = Path(args.out_dir).resolve().relative_to(Path(args.site_root).resolve()).parts
    extra_depth = max(0, len(out_parts) - 1)
    index_html = render_index(Path(args.template).read_text(), args.label, extra_depth)
    (out_dir / "index.html").write_text(index_html)

    print(f"published {args.label} -> {out_dir}/ (engine column: {engine_key})")


if __name__ == "__main__":
    main()
