#!/usr/bin/env bash
# Runs every serving workload against one server: RUN_DIR LABEL.
# The server must be running on $GRPC with its index complete to $END.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
run=$1 label=$2
out=$run/serving
mkdir -p "$out"

suite() {
    local name=$1; shift
    phase_begin "$run" "suite:$name"
    "$SERVING_SUITE" --endpoint "http://$GRPC" --label "$label" --output "$out/$name" \
        --start "$START" --end "$END" --fixtures "$FIXTURES" --seed 20260829 \
        --timeout-seconds 600 --warmup 1 "$@" > "$out/$name.log" 2>&1 || log "suite $name exited $?"
    phase_end "$run"
}

# Native Rust/Tonic client: per-RPC latency, range scaling, closed-loop concurrency, wallet download.
suite latency --suites latency --requests 500
suite ranges --suites ranges --range-sizes 10,100,1000,10000,100000 \
    --range-requests 1000 --range-request-counts 100000=100
suite concurrency-10k --suites concurrency --clients 1,4,16,64,128,256 \
    --concurrency-blocks 10000 --seconds 20
suite concurrency-100 --suites concurrency --clients 1,4,16,64,128,256 \
    --concurrency-blocks 100 --seconds 20
suite sync --suites sync --sync-chunk 10000

# ghz: throughput and latency per RPC at fixed concurrency, one connection per worker. Requests
# with heights come from seeded arrays that ghz takes round-robin, the same for every server.
mkdir -p "$out/ghz/data"
python3 -I "$HARNESS/ghz-data.py" "$out/ghz/data" "$START" "$END"
order=(GetLatestBlock GetLightdInfo GetBlock GetBlockNullifiers GetTreeState GetLatestTreeState
    GetTransaction GetTaddressBalance GetSubtreeRoots GetBlockRange GetBlockRangeNullifiers)
for rpc in "${order[@]}"; do
    for c in ${GHZ_CLIENTS:-1 16 64 256}; do
        phase_begin "$run" "ghz:$rpc:$c"
        "$GHZ" --insecure --proto "$PROTO_DIR/service.proto" --import-paths "$PROTO_DIR" \
            --call "cash.z.wallet.sdk.rpc.CompactTxStreamer.$rpc" -D "$out/ghz/data/$rpc.json" \
            -c "$c" --connections "$c" -z "${GHZ_SECONDS:-15}s" --timeout 120s --skipFirst "$c" \
            --format json -o "$out/ghz/$rpc-c$c.json" "$GRPC" > "$out/ghz/$rpc-c$c.log" 2>&1 \
            || log "ghz $rpc c=$c exited $?"
        phase_end "$run"
    done
done
log "serving done: $out"
