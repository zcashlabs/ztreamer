#!/usr/bin/env bash
# Supplementary run: restart a server on the index of its main run, and measure the time to
# serving, then memory (anonymous and file-backed RSS) at idle and under load.
#   run-restart.sh direct|remote|lwd
# The main run directory is $ROOT/runs/MODE; this run writes $ROOT/runs/MODE-restart.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
mode=$1
main=$ROOT/runs/$mode
run=${RUN:-$ROOT/runs/$mode-restart}
mkdir -p "$run"
record_env "$run"
trap 'stop_pid "$run/server.pid"; stop_pid "$run/zakurad.pid"; stop_sampler "$run"' EXIT

procs=(--proc "server=$run/server.pid")
[[ $mode != direct ]] && procs+=(--proc "zakurad=$run/zakurad.pid")
if [[ $mode == lwd ]]; then
    start_sampler "$run" "${procs[@]}" --lwd-log "$run/lwd-server.log"
else
    start_sampler "$run" "${procs[@]}" --ztreamer-metrics "http://$ZT_METRICS/metrics"
fi

if [[ $mode == direct ]]; then
    # Bring the node to the tip first, as in the main run.
    phase_begin "$run" node-presync
    start_node "$run"
    stop_pid "$run/zakurad.pid"
    phase_end "$run"
    render_config "$run" zakura-embedded
else
    phase_begin "$run" node-start
    start_node "$run"
    phase_end "$run"
fi

phase_begin "$run" restart
case $mode in
direct)
    launch "$run" server "$ZTREAMERD" --zakura-config "$run/zakura-embedded.toml" \
        --index-dir "$main/index" --grpc-listen "$GRPC" --metrics-listen "$ZT_METRICS"
    ;;
remote)
    launch "$run" server "$ZTREAMERD" --remote-rpc "http://$NODE_RPC" \
        --index-dir "$main/index" --grpc-listen "$GRPC" --metrics-listen "$ZT_METRICS"
    ;;
lwd)
    launch "$run" server "$LIGHTWALLETD" --no-tls-very-insecure --grpc-bind-addr "$GRPC" \
        --http-bind-addr 127.0.0.1:9068 --data-dir "$main/lwd-data" --log-file "$run/lwd-server.log" \
        --rpchost "${NODE_RPC%:*}" --rpcport "${NODE_RPC#*:}" --rpcuser bench --rpcpassword bench
    ;;
esac
if [[ $mode == lwd ]]; then
    wait_server_synced "$run" "$run/server.pid" 0 lwd_height "$run"
else
    wait_server_synced "$run" "$run/server.pid" 100 zt_height
fi
phase_end "$run"

idle "$run" warm 60
out=$run/serving
mkdir -p "$out"
for blocks in 10000; do
    phase_begin "$run" "suite:concurrency-$blocks"
    "$SERVING_SUITE" --endpoint "http://$GRPC" --label "$mode-restart" --output "$out/concurrency" \
        --start "$START" --end "$END" --seed 20260829 --suites concurrency --clients 1,16,64,256 \
        --concurrency-blocks "$blocks" --seconds 20 > "$out/concurrency.log" 2>&1 || log "concurrency exited $?"
    phase_end "$run"
done
phase_begin "$run" suite:sync
"$SERVING_SUITE" --endpoint "http://$GRPC" --label "$mode-restart" --output "$out/sync" \
    --start "$START" --end "$END" --seed 20260829 --suites sync --sync-chunk 10000 \
    > "$out/sync.log" 2>&1 || log "sync exited $?"
phase_end "$run"
idle "$run" after 30
log "done: $run"
