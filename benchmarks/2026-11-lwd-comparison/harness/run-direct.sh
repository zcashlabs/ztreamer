#!/usr/bin/env bash
# ztreamerd in embedded (direct) mode on the archive state: sync from genesis, then serving.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
run=${RUN:-$ROOT/runs/direct-$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$run"
render_config "$run" zakura-embedded
record_env "$run"
trap 'stop_pid "$run/ztreamerd.pid"; stop_pid "$run/zakurad.pid"; stop_sampler "$run"' EXIT

# Bring the archive state to the live tip with a standalone node first, as the remote and
# lightwalletd runs do, so that the sync time of ztreamerd is not the catch-up of the node.
phase_begin "$run" node-presync
start_node "$run"
stop_pid "$run/zakurad.pid"
phase_end "$run"

start_sampler "$run" --proc "ztreamerd=$run/ztreamerd.pid" \
    --ztreamer-metrics "http://$ZT_METRICS/metrics" --du "index=$run/index"
phase_begin "$run" sync
launch "$run" ztreamerd "$ZTREAMERD" --zakura-config "$run/zakura-embedded.toml" \
    --index-dir "$run/index" --grpc-listen "$GRPC" --metrics-listen "$ZT_METRICS"
wait_server_synced "$run" "$run/ztreamerd.pid" 100 zt_height
phase_end "$run"
log "synced to $END"

finish_run "$run" ztreamer-direct "$run/index"
