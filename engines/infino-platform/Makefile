# infino-platform: benchmarks infino on the hosted platform, over its REST API.
# Builds the engines/infino-branch code against the published infino 0.11.2
# crate, with hosted support (see Cargo.toml).
#
#   INFINO_HOST         https://<gateway>/v1/<database> (or https://<gateway>/<database>)
#   INFINO_API_KEY      the account's API key
#   INFINO_BENCH_TABLE  the table inside that database; a scale run defaults to
#                       <corpus>_<scale> (webcrawl_1b), anything else to sbg
#
# These can go in the repo's .env (not committed) as plain KEY=value lines,
# with no quotes and no `export`.
-include ../../.env
export INFINO_API_KEY
ifdef INFINO_BENCH_TABLE
export INFINO_BENCH_TABLE
endif

# Removes the local build only; the hosted table is kept.
clean:
	rm -fr target

compile: target/release/build_index target/release/do_query

target/release/build_index target/release/do_query &:
	@echo "\n\n\n--- Building infino-platform's binary ---" >&2
	@RUSTFLAGS='-C target-cpu=native' cargo build --release --bin build_index --bin do_query

# Loads $(CORPUS) into the hosted table. Reuses the table if it already holds
# the corpus; fails if it holds anything else.
index: compile check-host
	@INFINO_BENCH_INDEX_URI="$(INFINO_HOST)" target/release/build_index < ${CORPUS}

serve: check-host
	@INFINO_BENCH_INDEX_URI="$(INFINO_HOST)" target/release/do_query

check-host:
	@test -n "$(INFINO_HOST)" || { echo "INFINO_HOST is required (https://<gateway>/v1/<database>)" >&2; exit 1; }
	@test -n "$(INFINO_API_KEY)" || { echo "INFINO_API_KEY is required" >&2; exit 1; }

.PHONY: compile index serve clean check-host
