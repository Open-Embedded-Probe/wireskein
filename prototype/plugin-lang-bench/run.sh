#!/bin/sh
# Build every engine with its own target dir, run both captures, collect results.
#   ./run.sh [engine ...]   (default: all)
set -u
cd "$(dirname "$0")"
DATA=../../corpus/work/zerocopy
ENGINES=${*:-native quickjs lua54 luajit rhai starlark python wasm}
OUT=results.jsonl
: > $OUT
for e in $ENGINES; do
  start=$(date +%s)
  if ! CARGO_TARGET_DIR=target-$e cargo build --release -p eng-$e >build-$e.log 2>&1; then
    echo "{\"engine\":\"$e\",\"build\":\"failed\"}" >> $OUT; continue
  fi
  bin=target-$e/release/eng-$e
  size=$(stat -c %s $bin)
  for id in i2cdb-sht30-1b9dbf wch-l103-flash-pattern4k; do
    line=$(timeout 600 $bin $DATA $id scripts 2>stderr-$e.log | tail -1)
    [ -n "$line" ] || line="{\"engine\":\"$e\",\"capture\":\"$id\",\"run\":\"failed\"}"
    echo "$line" | sed "s/}\$/,\"binary_bytes\":$size}/" >> $OUT
  done
  echo "$e done in $(( $(date +%s) - start )) s"
done
