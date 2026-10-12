#!/usr/bin/env python3
"""What produced a results.json: the corpus, the queries, and when it ran.

A results file on its own says how fast each engine was and nothing about what
was measured. Two pages can differ because the corpus differs or because the
query set differs, and none of that was previously recorded anywhere.

One run, one answer: this is per-run metadata, not per-engine.

Nothing in here is allowed to fail a bench run. A run costs hours; this is a
label on it. Every lookup degrades to an absent field rather than raising, and
the page renders whatever is present.
"""

import json
import os
import subprocess
from datetime import datetime, timezone
from os import path

from corpora import SCALES


def _line_count(file_path):
    """Documents in a corpus, or queries in a query set: one per line."""
    try:
        count = 0
        with open(file_path, "rb") as f:
            for _ in f:
                count += 1
        return count
    except OSError:
        return None


def run_provenance(query_path, commands, started_utc, engines=None):
    """The `run` block written beside details/index_sizes/results."""
    run = {
        "started_utc": started_utc,
        "queries": {"name": path.basename(query_path), "count": _line_count(query_path)},
    }
    if commands:
        run["commands"] = list(commands)

    builds = _engine_builds(engines)
    if builds:
        run["engines"] = builds
    reader = _reader_settings()
    if reader:
        run["reader"] = reader

    # A scale run queries an index that already exists, so there is no
    # corpus.json to measure: the corpus is named rather than counted, and the
    # document count comes from the scale that built it. Counting lines of
    # whatever CORPUS happens to point at would be wrong here, and on a
    # compressed file it is not even a document count.
    index_uri = os.environ.get("INFINO_BENCH_INDEX_URI")
    if index_uri:
        corpus = {"index": index_uri}
        name = os.environ.get("INFINO_BENCH_CORPUS_NAME")
        if name:
            corpus["name"] = name
        scale = os.environ.get("INFINO_BENCH_SCALE")
        if scale:
            corpus["scale"] = scale
            if scale in SCALES:
                corpus["docs"] = SCALES[scale]
        run["corpus"] = corpus
        build = _build_stats()
        if build:
            run["build"] = build
        return run

    corpus_path = os.environ.get("CORPUS")
    if corpus_path:
        # The registry name (corpora.py) when the run came through the
        # Makefile, which always sets one; the file name otherwise.
        name = os.environ.get("CORPUS_NAME") or path.basename(corpus_path)
        corpus = {"name": name, "path": corpus_path}
        docs = _line_count(corpus_path)
        if docs is not None:
            corpus["docs"] = docs
        try:
            corpus["bytes"] = os.stat(corpus_path).st_size
        except OSError:
            pass
        run["corpus"] = corpus

    return run


def _engine_commit(engine_dir):
    """The commit of the infino checkout this engine path-depends on.

    Only the infino engines are reported, and only those built from a
    checkout. Every other engine here is pinned: a released version from a
    registry, or a jar at a fixed version. Their bytes cannot move between
    runs, so there is no commit to attribute and nothing a reader would do
    with one. An engine built from a working tree can be any commit at all,
    and nothing else in a results file says which.
    """
    manifest = path.join(engine_dir, "Cargo.toml")
    try:
        with open(manifest) as f:
            lines = [l for l in f if l.startswith("infino ") or l.startswith("infino=")]
    except OSError:
        return None
    dep_path = None
    for line in lines:
        marker = 'path = "'
        if marker in line:
            dep_path = line.split(marker, 1)[1].split('"', 1)[0]
            break
    if not dep_path:
        return None
    checkout = path.normpath(path.join(engine_dir, dep_path))
    try:
        done = subprocess.run(
            ["git", "-C", checkout, "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    sha = done.stdout.strip()
    if done.returncode != 0 or not sha:
        return None
    out = {"commit": sha, "checkout": checkout}
    try:
        dirty = subprocess.run(
            ["git", "-C", checkout, "status", "--porcelain"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        if dirty.returncode == 0 and dirty.stdout.strip():
            out["dirty"] = True
    except (OSError, subprocess.SubprocessError):
        pass
    return out


def _engine_builds(engines=None):
    """Per infino engine a run used, the commit it was built from.

    Restricted to the engines a run actually used. Reporting every engine
    directory that happens to sit on the host would put a commit beside an
    engine that answered no queries, which is worse than saying nothing.
    """
    root = path.dirname(path.dirname(path.abspath(__file__)))
    engines_dir = path.join(root, "engines")
    out = {}
    if engines:
        names = sorted(engines)
    else:
        try:
            names = sorted(os.listdir(engines_dir))
        except OSError:
            return out
    for name in names:
        built = _engine_commit(path.join(engines_dir, name))
        if built:
            out[name] = built
    return out


def _reader_settings():
    """Run settings that change what is measured without changing the code.

    The disk-cache budget is the one that bites: left unset, the engine uses
    its own modest default, and an index far larger than that is re-fetched
    from object storage on every query. The run still succeeds and the numbers
    describe a cache miss rather than the engine.
    """
    out = {}
    for key, name in (
        ("INFINO_BENCH_CACHE_BUDGET_GB", "cache_budget_gb"),
        ("INFINO_BENCH_CACHE_DIR", "cache_dir"),
        ("BENCH_QUERY_THREADS", "query_threads"),
        ("INFINO_BENCH_QUERY_MODE", "query_mode"),
    ):
        value = os.environ.get(key)
        if value:
            out[name] = value
    if os.environ.get("INFINO_BENCH_INDEX_URI") and "cache_budget_gb" not in out:
        out["cache_budget_gb"] = "unset"
    return out


def describe(run):
    """One human-readable line per fact worth seeing before a run starts."""
    lines = []
    for engine, built in sorted(run.get("engines", {}).items()):
        dirty = ", dirty" if built.get("dirty") else ""
        lines.append("%s: built from %s%s" % (engine, built["commit"], dirty))
    reader = run.get("reader", {})
    if reader:
        lines.append("reader: " + ", ".join(
            "%s=%s" % (k, v) for k, v in sorted(reader.items())
        ))
    if reader.get("cache_budget_gb") == "unset":
        lines.append(
            "warning: no disk-cache budget set, so the engine's own default "
            "applies. An index larger than that default is re-fetched from "
            "object storage on every query, and these timings will describe "
            "that rather than the engine."
        )
    return lines


def _build_stats():
    """What building the queried index cost, when the run built it.

    A scale build writes these beside its results (scripts/index-scale.sh);
    a run against an index built some other way has none, and says nothing.
    Stats left behind by a build of some other index are not this run's.
    """
    stats_path = os.environ.get("INFINO_BENCH_BUILD_STATS")
    if not stats_path:
        return None
    try:
        with open(stats_path) as f:
            stats = json.load(f)
    except (OSError, ValueError):
        return None
    if stats.get("index") != os.environ.get("INFINO_BENCH_INDEX_URI"):
        return None
    return stats


def collect(query_path, commands, started_utc, engines=None):
    """`run_provenance`, with any failure reduced to an error string.

    The caller is a bench run that has already spent hours; it must finish and
    write its results whatever happens here.
    """
    try:
        return run_provenance(query_path, commands, started_utc, engines)
    except Exception as e:  # noqa: BLE001 - deliberately total
        return {"error": "run metadata collection failed: %s" % e}


def utc_now():
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


if __name__ == "__main__":
    print(json.dumps(
        collect(os.environ.get("QUERIES", "queries.txt"), None, utc_now()),
        indent=2,
    ))
