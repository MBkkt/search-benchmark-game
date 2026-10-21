import subprocess
import os
from os import path
import time
import json
import random
import queue
import signal
import threading
from collections import defaultdict

from run_provenance import HOSTED_ENGINES

COMMANDS = os.environ['COMMANDS'].split(' ')

# Seconds to wait for a hosted engine's answer before treating it as hung. The
# first answer also covers engine startup, so it gets longer.
QUERY_TIMEOUT_SECS = float(os.environ.get("QUERY_TIMEOUT_SECS", "600"))
STARTUP_TIMEOUT_SECS = float(os.environ.get("STARTUP_TIMEOUT_SECS", "3600"))

class EngineFailed(Exception):
    """The engine exited or did not answer in time."""

class SearchClient:

    def __init__(self, engine):
        self.engine = engine
        dirname = os.path.split(os.path.abspath(__file__))[0]
        dirname = path.dirname(dirname)
        dirname = path.join(dirname, "engines")
        cwd = path.join(dirname, engine)
        print(cwd)
        self.process = subprocess.Popen(["make", "--no-print-directory", "serve"],
            cwd=cwd,
            stdout=subprocess.PIPE,
            stdin=subprocess.PIPE,
            # A separate process group, so kill() also stops the engine
            # `make` started.
            start_new_session=True)
        # Hosted engines read answers on a thread so a wait can time out.
        # Local engines read directly: the thread adds ~10us to every timing.
        self.answers = None
        self.started = False
        if engine in HOSTED_ENGINES:
            self.answers = queue.Queue()
            threading.Thread(target=self._read_answers, daemon=True).start()

    def _read_answers(self):
        for line in self.process.stdout:
            self.answers.put(line)
        self.answers.put(None)

    def query(self, query, command):
        query_line = "%s\t%s\n" % (command, query)
        try:
            self.process.stdin.write(query_line.encode("utf-8"))
            self.process.stdin.flush()
        except BrokenPipeError:
            raise EngineFailed("%s exited before %s %r" % (self.engine, command, query))
        if self.answers is None:
            recv = self.process.stdout.readline() or None
        else:
            timeout = QUERY_TIMEOUT_SECS if self.started else max(QUERY_TIMEOUT_SECS, STARTUP_TIMEOUT_SECS)
            try:
                recv = self.answers.get(timeout=timeout)
            except queue.Empty:
                raise EngineFailed("%s gave no answer to %s %r within %gs" % (self.engine, command, query, timeout))
        if recv is None:
            raise EngineFailed("%s exited during %s %r" % (self.engine, command, query))
        self.started = True
        recv = recv.strip()
        if recv == b"UNSUPPORTED":
            return None
        if self.answers is None:
            return int(recv)
        # A hosted engine answers "<count>\t<us>": the platform's own query
        # time, which is what its column reports (see drive()).
        count, reported_us = recv.split(b"\t")
        return int(count), int(reported_us)

    def close(self):
        try:
            self.process.stdin.close()
        except BrokenPipeError:
            pass
        try:
            self.process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.kill()

    def kill(self):
        try:
            os.killpg(self.process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.process.wait()

def drive(queries, client, command):
    for query in queries:
        start = time.monotonic()
        count = client.query(query.query, command)
        stop = time.monotonic()
        duration = int((stop - start) * 1e6)
        # A hosted engine is timed by the platform's reported query time, not
        # the round trip, which is mostly network and gateway.
        if isinstance(count, tuple):
            count, duration = count
        yield (query, count, duration)

class Query(object):
    def __init__(self, query, tags):
        self.query = query
        self.tags = tags

def read_queries(query_path):
    for q in open(query_path):
        c = json.loads(q)
        yield Query(c["query"], c["tags"])

# Print progress, borrowed from https://stackoverflow.com/questions/3173320/text-progress-bar-in-terminal-with-block-characters
def printProgressBar (progress, prefix = '', suffix = '', decimals = 1, length = 100, fill = '█', printEnd = "\r"):
    """
    Call in a loop to create terminal progress bar
    @params:
        progress    - Required  : current progress in [0,1] (Float)
        prefix      - Optional  : prefix string (Str)
        suffix      - Optional  : suffix string (Str)
        decimals    - Optional  : positive number of decimals in percent complete (Int)
        length      - Optional  : character length of bar (Int)
        fill        - Optional  : bar fill character (Str)
        printEnd    - Optional  : end character (e.g. "\r", "\r\n") (Str)
    """
    percent = ("{0:." + str(decimals) + "f}").format(100 * progress)
    filledLength = int(length * progress)
    bar = fill * filledLength + '-' * (length - filledLength)
    print(f'\r{prefix} |{bar}| {percent}% {suffix}', end = printEnd)
    # Print New Line on Complete
    if progress >= 1:
        print()

def index_size_bytes(idx_path):
    """Total on-disk bytes of an engine's built index. Resolves the
    `idx` symlink (some engines point it at an index dir elsewhere) and
    sums every file under it. Returns None when the dir is absent."""
    root = os.path.realpath(idx_path)
    if not os.path.isdir(root):
        return None
    total = 0
    for dirpath, _dirs, files in os.walk(root):
        for fn in files:
            try:
                total += os.path.getsize(os.path.join(dirpath, fn))
            except OSError:
                pass
    return total


WARMUP_TIME = int(os.environ.get('WARMUP_TIME', '60'))
NUM_ITER = int(os.environ.get('NUM_ITER', '10'))

if __name__ == "__main__":
    import sys
    import run_provenance
    random.seed(2)
    # Stamped before the first engine starts, so it dates the run rather than
    # the moment the results happened to be written.
    started_utc = run_provenance.utc_now()
    query_path = sys.argv[1]
    engines = sys.argv[2:]
    queries = list(read_queries(query_path))

    # Say what is about to be measured, before hours are spent measuring it:
    # which engine build answers, and the settings that quietly change what the
    # answer means. Never fatal, like everything else in run_provenance.
    for line in run_provenance.describe(
        run_provenance.collect(query_path, None, started_utc, engines)
    ):
        print("--- %s" % line, file=sys.stderr)

    details = {}
    for engine in engines:
      dirname = os.path.split(os.path.abspath(__file__))[0]
      dirname = path.dirname(dirname)
      dirname = path.join(dirname, "engines")
      details_file = path.join(dirname, engine, "details.json")
      if os.path.exists(details_file):
        with open(details_file, "r") as f:
          details[engine] = json.loads(f.read())
      else:
        details[engine] = []

    # Record each engine's built-index size so the comparison table can
    # show the storage cost alongside latency. Indexing runs before the
    # bench, so `engines/<engine>/idx` exists here.
    # A scale run's index is not under engines/<engine>/idx at all: it lives
    # wherever INFINO_BENCH_INDEX_URI points, typically object storage. The
    # local path is then either absent or — worse — a stale symlink to some
    # earlier run's index, which measures confidently and reports the wrong
    # number. So a declared size wins whenever the local path cannot be read.
    # A hosted table's size can't be measured from here, and the declared
    # size is for a different index, so hosted engines report none.
    declared = os.environ.get("INFINO_BENCH_INDEX_BYTES")
    index_sizes = {}
    for engine in engines:
      measured = index_size_bytes(path.join(dirname, engine, "idx"))
      if measured is None and declared and engine not in HOSTED_ENGINES:
        try:
          measured = int(declared)
        except ValueError:
          measured = None
      index_sizes[engine] = measured

    results = {}
    # An engine that fails is removed from all results, and the run block
    # records why. The other engines carry on.
    failed = {}
    for command in COMMANDS:
        results_commands = {}
        for engine in engines:
            if engine in failed:
                continue
            engine_results = []
            query_idx = {}
            for query in queries:
                query_result = {
                    "query": query.query,
                    "tags": query.tags,
                    "count": 0,
                    "duration": []
                }
                query_idx[query.query] = query_result
                engine_results.append(query_result)
            print("======================")
            print("BENCHMARKING %s %s" % (engine, command))
            search_client = SearchClient(engine)
            try:
                queries_shuffled = list(queries[:])
                random.seed(2)
                random.shuffle(queries_shuffled)
                warmup_start = time.monotonic()
                printProgressBar(0, prefix = 'Warmup:', suffix = 'Complete', length = 50)
                while True:
                    for _ in drive(queries_shuffled, search_client, command):
                        pass
                    progress = min(1, (time.monotonic() - warmup_start) / WARMUP_TIME)
                    printProgressBar(progress, prefix = 'Warmup:', suffix = 'Complete', length = 50)
                    if progress == 1:
                        break
                printProgressBar(0, prefix = 'Run:   ', suffix = 'Complete', length = 50)
                for i in range(NUM_ITER):
                    for (query, count, duration) in drive(queries_shuffled, search_client, command):
                        if count is None:
                            query_idx[query.query]["count"] = -1
                            query_idx[query.query]["duration"] = []
                        else:
                            query_idx[query.query]["count"] = count
                            query_idx[query.query]["duration"].append(duration)
                    printProgressBar(float(i + 1) / NUM_ITER, prefix = 'Run:   ', suffix = 'Complete', length = 50)
                for query in engine_results:
                    query["duration"].sort()
                results_commands[engine] = engine_results
            except EngineFailed as e:
                print("\nENGINE FAILED: %s" % e)
                search_client.kill()
                failed[engine] = "%s: %s" % (command, e)
                for earlier in results.values():
                    earlier.pop(engine, None)
                continue
            search_client.close()
        print(results_commands.keys())
        results[command] = results_commands
    for engine in failed:
        details.pop(engine, None)
        index_sizes.pop(engine, None)
    # What produced these numbers: corpus, query set, and when the run started.
    # Never fatal — see run_provenance.py.
    run = run_provenance.collect(
        query_path, COMMANDS, started_utc,
        [e for e in engines if e not in failed], failed)
    # RESULTS_PATH lets a run write somewhere other than results.json, which is
    # a tracked file holding the nightly's own output. A run that writes through
    # it and renames afterwards leaves that file deleted in the working tree.
    results_path = os.environ.get("RESULTS_PATH", "results.json")
    with open(results_path, "w") as f:
        json.dump({ "run": run, "details": details, "index_sizes": index_sizes, "results": results }, f, default=lambda obj: obj.__dict__)
    # Results are written; exit non-zero if any engine failed.
    if failed:
        for engine, reason in failed.items():
            print("FAILED %s: %s" % (engine, reason))
        # A hosted engine depends on a service outside this run, so losing it
        # costs its column (recorded under run.failed_engines), not the run:
        # the local engines' results are still published.
        if any(engine not in HOSTED_ENGINES for engine in failed):
            sys.exit(1)
        print("only hosted engines failed; the local results stand")
