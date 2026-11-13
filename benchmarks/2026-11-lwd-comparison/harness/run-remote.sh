#!/usr/bin/env bash
# ztreamerd in remote mode: zakurad on the archive state, the default snapshot source
# (https://snapshot.ztreamer.sh/mainnet), then the tail from the node. Then serving.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
run=${RUN:-$ROOT/runs/remote-$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$run"
record_env "$run"
trap 'stop_pid "$run/ztreamerd.pid"; stop_pid "$run/zakurad.pid"; stop_sampler "$run"' EXIT

start_sampler "$run" --proc "ztreamerd=$run/ztreamerd.pid" --proc "zakurad=$run/zakurad.pid" \
    --ztreamer-metrics "http://$ZT_METRICS/metrics" --du "index=$run/index"
phase_begin "$run" node-start
start_node "$run"
phase_end "$run"
phase_begin "$run" sync
launch "$run" ztreamerd "$ZTREAMERD" --remote-rpc "http://$NODE_RPC" \
    --index-dir "$run/index" --grpc-listen "$GRPC" --metrics-listen "$ZT_METRICS"
wait_server_synced "$run" "$run/ztreamerd.pid" 100 zt_height
phase_end "$run"
log "synced to $END"

finish_run "$run" ztreamer-remote "$run/index"
