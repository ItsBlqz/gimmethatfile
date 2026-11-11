#!/usr/bin/env bash
# Real-world benchmark: download public files over the internet with 1 and 8
# connections, interleaved, and check every file's SHA-256.
#
#     scripts/realworld.sh [rounds] [sources]
#
# rounds defaults to 2; sources is a comma list of kernel,debian,hetzner
# (default: all). Prints a markdown table (also written to
# target/realworld/summary.md). Uses roughly 2 GB per round with all sources.
# These numbers depend on the network at that moment: use them to watch
# trends, never to pass or fail a change.
set -euo pipefail

cd "$(dirname "$0")/.."
ROUNDS=${1:-2}
SOURCES=${2:-kernel,debian,hetzner}
OUT=target/realworld
EXE=""; [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]] && EXE=.exe
mkdir -p "$OUT"

declare -A URL SHA
URL[kernel]=https://mirrors.edge.kernel.org/pub/linux/kernel/v6.x/linux-6.10.tar.xz
SHA[kernel]=774698422ee54c5f1e704456f37c65c06b51b4e9a8b0866f34580d86fef8e226
URL[debian]=https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-13.7.0-amd64-netinst.iso
SHA[debian]=a7ef94ac2fb9a7fec454552abd629b7cc9d5155c886165a45649f5ce6167e355
URL[hetzner]=https://sin-speed.hetzner.com/100MB.bin
SHA[hetzner]=78804633849100426b0bd8d6ca3318ce1b49900edc18ae34d61b50176e3aacad

cargo build -q --release -p gtf-engine --example get
GET="target/release/examples/get$EXE"
RESULTS="$OUT/results.tsv"
: > "$RESULTS"

one() { # source connections
    local dir="$OUT/dl-$1-$2"
    rm -rf "${dir:?}"; mkdir -p "$dir"
    local log; log=$("$GET" "${URL[$1]}" "$2" "$dir" 2>&1 | tr '\r' '\n') || true
    local mbps; mbps=$(echo "$log" | sed -nE 's/^done: .* avg ([0-9.]+) MB\/s.*/\1/p')
    local conns; conns=$(echo "$log" | awk '/Downloading/{for(i=1;i<=NF;i++) if($i=="conns"){s+=$(i-1);n++}} END{if(n) printf "%.1f", s/n; else print "-"}')
    local ok=no
    [[ -n "$mbps" && "$(sha256sum "$dir"/* | cut -c1-64)" == "${SHA[$1]}" ]] && ok=yes
    printf "%s\t%s\t%s\t%s\t%s\n" "$1" "$2" "${mbps:-failed}" "$conns" "$ok" >> "$RESULTS"
    echo "$1, $2 connection(s): ${mbps:-failed} MB/s, ~$conns open, sha256 $ok" >&2
    rm -rf "${dir:?}"
}

IFS=, read -ra SRC <<< "$SOURCES"
for r in $(seq 1 "$ROUNDS"); do
    for s in "${SRC[@]}"; do
        if (( r % 2 )); then one "$s" 1; one "$s" 8; else one "$s" 8; one "$s" 1; fi
    done
done

{
    echo "### Real-world downloads ($(date -u +%Y-%m-%d), $ROUNDS round(s))"
    echo
    echo "MB/s per run. Network conditions vary a lot: compare trends, not single numbers."
    echo
    echo "| Source | 1 connection | 8 connections | Avg. open (8) | SHA-256 |"
    echo "|---|---:|---:|---:|---|"
    for s in "${SRC[@]}"; do
        one_c=$(awk -F'\t' -v s="$s" '$1==s && $2==1 {printf "%s%s", sep, $3; sep=", "}' "$RESULTS")
        eight=$(awk -F'\t' -v s="$s" '$1==s && $2==8 {printf "%s%s", sep, $3; sep=", "}' "$RESULTS")
        open8=$(awk -F'\t' -v s="$s" '$1==s && $2==8 {printf "%s%s", sep, $4; sep=", "}' "$RESULTS")
        okall=$(awk -F'\t' -v s="$s" '$1==s && $5!="yes" {bad=1} END{print bad ? "MISMATCH" : "all ok"}' "$RESULTS")
        echo "| $s | $one_c | $eight | $open8 | $okall |"
    done
} | tee "$OUT/summary.md"
