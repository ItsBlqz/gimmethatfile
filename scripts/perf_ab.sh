#!/usr/bin/env bash
# Compare the engine's speed in the working tree against another commit.
#
#     scripts/perf_ab.sh [base-ref] [runs] [-- extra args for perf_compare.py]
#
# Defaults: base-ref = origin/main, runs = 4. The base commit's engine is built
# with *this* tree's benchmark (examples/perf.rs + examples/common/), so both
# sides run exactly the same scenarios. Runs alternate between base and head
# so a slower or faster moment on the machine affects both alike.
#
# Results go to target/perf/ (base.jsonl, head.jsonl, summary.md). Exits with
# status 1 if this tree is slower (see scripts/perf_compare.py).
set -euo pipefail

cd "$(dirname "$0")/.."
BASE_REF=${1:-origin/main}
RUNS=${2:-4}
shift $(( $# > 2 ? 2 : $# ))
[[ "${1:-}" == "--" ]] && shift
OUT=target/perf
BASE_DIR=target/perf-base
# First Python that actually runs (on Windows, `python3` may be a Store stub).
PY=""
for c in python3 python; do
    if "$c" -c "" >/dev/null 2>&1; then PY=$c; break; fi
done
EXE=""; [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]] && EXE=.exe

mkdir -p "$OUT"
rm -f "$OUT/base.jsonl" "$OUT/head.jsonl" "$OUT/summary.md"

echo "Building head (working tree)..."
cargo build -q --release -p gtf-engine --example perf
cp "target/release/examples/perf$EXE" "$OUT/perf-head$EXE"

echo "Building base ($BASE_REF) with this tree's benchmark..."
BASE_SHA=$(git rev-parse "$BASE_REF")
git worktree remove --force "$BASE_DIR" 2>/dev/null || true
git worktree add -q --detach "$BASE_DIR" "$BASE_SHA"
trap 'git worktree remove --force "$BASE_DIR" 2>/dev/null || true' EXIT
mkdir -p "$BASE_DIR/crates/engine/examples"
cp crates/engine/examples/perf.rs "$BASE_DIR/crates/engine/examples/perf.rs"
rm -rf "$BASE_DIR/crates/engine/examples/common"
cp -r crates/engine/examples/common "$BASE_DIR/crates/engine/examples/common"
if ! (cd "$BASE_DIR" && CARGO_TARGET_DIR="$PWD/../perf-base-target" cargo build -q --release -p gtf-engine --example perf); then
    echo "::warning::The benchmark doesn't build against $BASE_REF (engine API changed?). Skipping the comparison."
    exit 0
fi
cp "target/perf-base-target/release/examples/perf$EXE" "$OUT/perf-base$EXE"

for i in $(seq 1 "$RUNS"); do
    echo "Round $i/$RUNS"
    if (( i % 2 )); then order="base head"; else order="head base"; fi
    for side in $order; do
        "$OUT/perf-$side$EXE" >> "$OUT/$side.jsonl"
    done
done

"$PY" scripts/perf_compare.py "$OUT/base.jsonl" "$OUT/head.jsonl" --summary "$OUT/summary.md" "$@"
