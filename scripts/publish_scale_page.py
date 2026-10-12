#!/usr/bin/env python3
"""Publish a scale run's results as /<fork>/<corpus>/<scale>/full.

Usage:
    publish_scale_page.py <results.json> --fork <owner> \
        [--template web/build/full/index.html] [--site-root web/build]

The corpus and scale come from the results file's own run block, so the URL
cannot name a different corpus or document count from the run it shows. The
page is the /full page relabelled for its depth; the Pages deploy restamps it
against the current bundle like every other page.
"""
import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from corpora import CorpusError, scale_docs  # noqa: E402
from publish_branch_full import render_index  # noqa: E402


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("results")
    p.add_argument("--fork", required=True, help="owner of the infino repo the run built")
    p.add_argument("--template", default="web/build/full/index.html")
    p.add_argument("--site-root", default="web/build")
    args = p.parse_args()

    results = json.load(open(args.results))
    corpus = (results.get("run") or {}).get("corpus") or {}
    name, scale = corpus.get("name"), corpus.get("scale")
    if not name or not scale:
        sys.exit("%s has no corpus name and scale in its run block; not a scale run" % args.results)
    try:
        docs = scale_docs(name, scale)
    except CorpusError as e:
        sys.exit("publish_scale_page: %s" % e)
    if corpus.get("docs") != docs:
        sys.exit("run block says %s documents; %s at %s is %s" % (corpus.get("docs"), name, scale, docs))

    out_dir = Path(args.site_root) / args.fork / name / scale / "full"
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "results.json").write_text(json.dumps(results))
    depth = len(out_dir.relative_to(args.site_root).parts) - 1
    label = "%s %s" % (name, scale)
    (out_dir / "index.html").write_text(render_index(Path(args.template).read_text(), label, depth))
    print("published %s -> %s/" % (label, out_dir))


if __name__ == "__main__":
    main()
