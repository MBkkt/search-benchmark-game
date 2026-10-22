### Build requirements

- git -- not very old to clone submodules
- cmake -- version 3.28.3 or higher
- clang -- version 21.1.8 or higher
- lld -- version 21.1.8 or higher
- glibc -- version 2.35 or higher (lower can be also ok, but we didn't check)

Everything else build from source!
And you don't need to do anything for it.

### About indexing speed

Although iresearch supports parallel indexing (as do Lucene and Tantivy), we intentionally use single-threaded indexing for benchmarks. This makes indexing slower, but keeps results more consistent and comparable across engines.

### How to collect pgo?

It's mostly useful for not manually optimized code paths.
It was with intersection_union which was improved by 15% before we optimized it manually.
Fot other queries it gives around 1-2%, so mostly don't cost it.

```bash
cd ./engines/iresearch-26.10.3
rm -rf ./pgo ./cs_pgo
make clean
make compile_gen_pgo
cd ./../..
SERVE_TYPE=serve_gen_pgo ENGINES=iresearch-26.10.3 make bench
cd ./engines/iresearch-26.10.3/pgo
llvm-profdata-21 merge -output=./code.profdata ./code-*.profraw
cd ./..
# make clean
# make compile_gen_cs_pgo
# cd ./../..
# SERVE_TYPE=serve_gen_cs_pgo ENGINES=iresearch-26.10.3 make bench
# cd ./engines/iresearch-26.10.3/cs_pgo
# llvm-profdata-21 merge -output=./code.profdata ./../pgo/code.profdata ./code-*.profraw
# mv ./code.profdata ./../pgo/code.profdata
# cd ./..
make clean
make compile_use_pgo
cd ./../..
ENGINES=iresearch-26.10.3 make bench
```
