import subprocess
import os
from os import path
import time
import json
import random
from collections import defaultdict

COMMANDS = os.environ['COMMANDS'].split(' ')

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
            stdin=subprocess.PIPE)

    def query(self, query, command):
        query_line = "%s\t%s\n" % (command, query)
        self.process.stdin.write(query_line.encode("utf-8"))
        self.process.stdin.flush()
        recv = self.process.stdout.readline().strip()
        if recv == b"UNSUPPORTED":
            return None
        cnt = int(recv)
        return cnt

    def close(self):
        self.process.stdin.close()
        self.process.stdout.close()

def drive(queries, client, command):
    for query in queries:
        start = time.monotonic()
        count = client.query(query.query, command)
        stop = time.monotonic()
        duration = int((stop - start) * 1e6)
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
    declared = os.environ.get("INFINO_BENCH_INDEX_BYTES")
    index_sizes = {}
    for engine in engines:
      measured = index_size_bytes(path.join(dirname, engine, "idx"))
      if measured is None and declared:
        try:
          measured = int(declared)
        except ValueError:
          measured = None
      index_sizes[engine] = measured

    results = {}
    for command in COMMANDS:
        results_commands = {}
        for engine in engines:
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
                        query_idx[query.query] = {count: -1, duration: []}
                    else:
                        query_idx[query.query]["count"] = count
                        query_idx[query.query]["duration"].append(duration)
                printProgressBar(float(i + 1) / NUM_ITER, prefix = 'Run:   ', suffix = 'Complete', length = 50)
            for query in engine_results:
                query["duration"].sort()
            results_commands[engine] = engine_results
            search_client.close()
        print(results_commands.keys())
        results[command] = results_commands
    # What produced these numbers: corpus, query set, and when the run started.
    # Never fatal — see run_provenance.py.
    run = run_provenance.collect(query_path, COMMANDS, started_utc, engines)
    # RESULTS_PATH lets a run write somewhere other than results.json, which is
    # a tracked file holding the nightly's own output. A run that writes through
    # it and renames afterwards leaves that file deleted in the working tree.
    results_path = os.environ.get("RESULTS_PATH", "results.json")
    with open(results_path, "w") as f:
        json.dump({ "run": run, "details": details, "index_sizes": index_sizes, "results": results }, f, default=lambda obj: obj.__dict__)
