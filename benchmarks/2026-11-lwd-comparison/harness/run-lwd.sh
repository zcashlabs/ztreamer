#!/usr/bin/env bash
# lightwalletd on zakurad (archive state): ingest from Sapling activation, then serving.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
run=${RUN:-$ROOT/runs/lwd-$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$run/lwd-data"
record_env "$run"
trap 'stop_pid "$run/lightwalletd.pid"; stop_pid "$run/zakurad.pid"; stop_sampler "$run"' EXIT

start_sampler "$run" --proc "lightwalletd=$run/lightwalletd.pid" --proc "zakurad=$run/zakurad.pid" \
    --lwd-log "$run/lwd-server.log" --du "index=$run/lwd-data"
phase_begin "$run" node-start
start_node "$run"
phase_end "$run"
phase_begin "$run" sync
launch "$run" lightwalletd "$LIGHTWALLETD" --no-tls-very-insecure --grpc-bind-addr "$GRPC" \
    --http-bind-addr 127.0.0.1:9068 --data-dir "$run/lwd-data" --log-file "$run/lwd-server.log" \
    --rpchost "${NODE_RPC%:*}" --rpcport "${NODE_RPC#*:}" --rpcuser bench --rpcpassword bench
wait_server_synced "$run" "$run/lightwalletd.pid" 0 lwd_height "$run"
phase_end "$run"
log "synced to $END"

finish_run "$run" lightwalletd "$run/lwd-data"
