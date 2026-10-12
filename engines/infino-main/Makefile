clean:
	rm -fr idx
	rm -fr target

compile: target/release/build_index target/release/do_query

# FORCE, so the recipe runs every time and cargo decides whether anything has
# to be rebuilt. Without it make sees two files that exist, has no prerequisite
# that could be newer, and declares them up to date. The engine's real sources
# are an out-of-tree path dependency that make cannot see at all, so a checkout
# moved to another commit still serves the binary built from the previous one,
# and the run reports timings for an engine nobody chose.
target/release/build_index target/release/do_query &: FORCE
	@echo "\n\n\n--- Building infino's binary ---" >&2
	@RUSTFLAGS='-C target-cpu=native' cargo build --release --bin build_index --bin do_query

FORCE:
.PHONY: FORCE

index: idx

serve: target/release/do_query
	@target/release/do_query idx


idx: target/release/build_index
	@echo "\n\n\n---- Indexing infino ----"
	@mkdir -p idx
	@target/release/build_index idx < ${CORPUS}
