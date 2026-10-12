#!/usr/bin/env python3
"""The corpora a run can index, and the scale vocabulary, in one place.

Every corpus the harness can index is declared here: where its source lives,
how it becomes `corpus.json` lines (`{"id", "text", "sort_field"}`), how many
documents it holds, and which query set belongs to it. The Makefile, the
scale build and the run metadata all read this module, so a page's URL, the
index it was built from and the document count on the page cannot disagree.

    corpora.py stream --corpus NAME [--scale S]     corpus.json lines on stdout
    corpora.py docs --corpus NAME --scale S         the scale as a document count
    corpora.py check-disk --corpus NAME --scale S --dir D [--dir D2 ...]
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
from os import path

HERE = path.dirname(path.abspath(__file__))
REPO = path.dirname(HERE)

# Scale shorthand to a document count. The vocabulary runs past what any one
# box can build today: a scale the hardware cannot reach fails the disk check,
# not the parser, because only one of those two is permanent.
SCALES = {
    "1M": 1_000_000,
    "100M": 100_000_000,
    "1B": 1_000_000_000,
    "10B": 10_000_000_000,
    "100B": 100_000_000_000,
    "1T": 1_000_000_000_000,
}

# Bytes a byte of local disk must hold per indexed document during a scale
# build and the query run after it: the ingest spill and compaction scratch,
# then the read cache in front of the object store. Measured on webcrawl,
# whose index is ~1,150 bytes per document at 1B (1.04 TiB); a build holds
# about twice that at its peak.
WEBCRAWL_LOCAL_BYTES_PER_DOC = 2_300

WIKIPEDIA_SRC = "https://www.dropbox.com/s/wwnfnu441w1ec9p/wiki-articles.json.bz2"
WEBCRAWL_SRC = "gs://vinay-fts-scale-test-gcp/corpus/webcrawl"
WEBCRAWL_FILES = 2414

CORPORA = {
    # English Wikipedia, as the search-benchmark-game has always indexed it.
    # One bz2 file over HTTP, reshaped by corpus_transform.py. Indexed whole.
    "wikipedia": {
        "docs": 5_032_104,
        "queries": "queries-full.txt",
        # The nightly's comparison is over the whole of it; a prefix of
        # Wikipedia is not a scale anyone publishes.
        "scales": False,
        "local_bytes_per_doc": None,
    },
    # FineWeb (HuggingFaceFW/fineweb, the ten newest dumps), already in
    # corpus.json form: text lowercased with every non-letter run folded to
    # one space, the same transform as corpus_transform.py. 2,414 zstd shards
    # of roughly 870k documents each, read in name order, so the first N
    # documents of the corpus are the same N at every scale.
    "webcrawl": {
        "docs": 2_100_000_000,
        "queries": "queries-full.txt",
        "scales": True,
        "local_bytes_per_doc": WEBCRAWL_LOCAL_BYTES_PER_DOC,
    },
}

# Read size when copying a shard through, and when counting its lines.
CHUNK_BYTES = 8 << 20


class CorpusError(Exception):
    pass


def corpus(name):
    try:
        return CORPORA[name]
    except KeyError:
        raise CorpusError("unknown corpus %r (known: %s)" % (name, ", ".join(sorted(CORPORA))))


def scale_docs(name, scale):
    """The document count `scale` stands for, checked against the corpus."""
    if not corpus(name)["scales"]:
        raise CorpusError("%s is indexed whole; it takes no scale" % name)
    if scale not in SCALES:
        raise CorpusError("scale must be one of %s (got %r)" % (" ".join(SCALES), scale))
    docs = SCALES[scale]
    available = corpus(name)["docs"]
    if docs > available:
        raise CorpusError(
            "%s holds %s documents; %s needs %s"
            % (name, format(available, ","), scale, format(docs, ","))
        )
    return docs


def _wikipedia(out):
    fetch = subprocess.Popen(["curl", "-#", "-L", WIKIPEDIA_SRC], stdout=subprocess.PIPE)
    unzip = subprocess.Popen(["bunzip2", "-c"], stdin=fetch.stdout, stdout=subprocess.PIPE)
    fetch.stdout.close()
    shape = subprocess.Popen(
        [sys.executable, path.join(REPO, "corpus_transform.py")], stdin=unzip.stdout, stdout=out
    )
    unzip.stdout.close()
    codes = [shape.wait(), unzip.wait(), fetch.wait()]
    if any(codes):
        raise CorpusError("wikipedia fetch failed (transform, bunzip2, curl exit %s)" % codes)


def _shard_source(i):
    """Where shard `i` is read from: a local mirror when one holds it, else GCS.

    CORPUS_MIRROR names a directory holding some or all of the shards under
    their published names. It is a cache of the same bytes and changes nothing
    about which documents are read.
    """
    name = "%05d.jsonl.zst" % i
    mirror = os.environ.get("CORPUS_MIRROR")
    if mirror and path.isfile(path.join(mirror, name)):
        return ["zstd", "-dcq", path.join(mirror, name)], None
    fetch = ["gcloud", "storage", "cat", "%s/%s" % (WEBCRAWL_SRC, name)]
    return ["zstd", "-dcq"], fetch


def _webcrawl(out, limit):
    remaining = CORPORA["webcrawl"]["docs"] if limit is None else limit
    for i in range(WEBCRAWL_FILES):
        if remaining == 0:
            return
        unzip_cmd, fetch_cmd = _shard_source(i)
        fetch = subprocess.Popen(fetch_cmd, stdout=subprocess.PIPE) if fetch_cmd else None
        unzip = subprocess.Popen(
            unzip_cmd, stdin=fetch.stdout if fetch else None, stdout=subprocess.PIPE
        )
        if fetch:
            fetch.stdout.close()
        while remaining:
            chunk = unzip.stdout.read(CHUNK_BYTES)
            if not chunk:
                break
            lines = chunk.count(b"\n")
            if lines >= remaining:
                cut = -1
                for _ in range(remaining):
                    cut = chunk.index(b"\n", cut + 1)
                out.write(chunk[: cut + 1])
                remaining = 0
                break
            out.write(chunk)
            remaining -= lines
        stopped_early = remaining == 0
        for proc in (unzip, fetch):
            if proc is None:
                continue
            if stopped_early:
                proc.kill()
            code = proc.wait()
            if code and not stopped_early:
                raise CorpusError("webcrawl shard %05d: %s exited %d" % (i, proc.args[0], code))
    if remaining:
        raise CorpusError("webcrawl ran out of shards %s documents short" % format(remaining, ","))


def stream(name, scale, out):
    """Write the corpus, or its first `scale` documents, as corpus.json lines."""
    corpus(name)
    limit = scale_docs(name, scale) if scale else None
    if name == "wikipedia":
        out.flush()
        _wikipedia(out)
    else:
        _webcrawl(out, limit)
    out.flush()


def check_disk(name, scale, dirs):
    """Fail before a build starts, with the number, if local disk is short.

    Runs are started by hand, and a run that fills the disk fails hours in
    rather than up front. The directories are the build's spill and cache;
    when they share a filesystem its free space is counted once.
    """
    per_doc = corpus(name)["local_bytes_per_doc"]
    if per_doc is None:
        return
    need = scale_docs(name, scale) * per_doc
    by_fs = {}
    for d in dirs:
        os.makedirs(d, exist_ok=True)
        by_fs.setdefault(os.stat(d).st_dev, d)
    free = sum(shutil.disk_usage(d).free for d in by_fs.values())
    if free < need:
        raise CorpusError(
            "%s at %s needs ~%.0f GiB of local disk for spill and cache; %s has %.0f GiB free"
            % (name, scale, need / 2**30, " + ".join(by_fs.values()), free / 2**30)
        )


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("stream")
    s.add_argument("--corpus", required=True)
    s.add_argument("--scale")
    d = sub.add_parser("docs")
    d.add_argument("--corpus", required=True)
    d.add_argument("--scale", required=True)
    c = sub.add_parser("check-disk")
    c.add_argument("--corpus", required=True)
    c.add_argument("--scale", required=True)
    c.add_argument("--dir", action="append", required=True)
    i = sub.add_parser("info")
    i.add_argument("--corpus", required=True)
    args = p.parse_args()
    try:
        if args.cmd == "stream":
            stream(args.corpus, args.scale, sys.stdout.buffer)
        elif args.cmd == "docs":
            print(scale_docs(args.corpus, args.scale))
        elif args.cmd == "check-disk":
            check_disk(args.corpus, args.scale, args.dir)
        else:
            print(json.dumps(corpus(args.corpus)))
    except CorpusError as e:
        sys.exit("corpora: %s" % e)
    except BrokenPipeError:
        # The reader stopped early (`| head`); that is its call, not an error.
        os._exit(0)


if __name__ == "__main__":
    main()
