#!/usr/bin/env bash
# Supplementary run: restart a server on the index of its main run, and measure the time to
# serving, then memory (anonymous and file-backed RSS) at idle and under load, and a second wallet
# download.
#   run-restart.sh LABEL
# The main run directory is $RUNS/LABEL; this run writes $RUNS/LABEL-restart.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
label=$1
main=$RUNS/$label
data=$main/data
run=${RUN:-$RUNS/$label-restart}
[[ -d $data ]] || { log "no index in $main"; exit 1; }
mkdir -p "$run"
record_env "$run"
trap 'stop_pid "$run/server.pid"; stop_pid "$run/zakurad.pid"; stop_sampler "$run"' EXIT

start_label_sampler "$run" "$label" "$data"
start_label_node "$run" "$label"
phase_begin "$run" restart
launch_server "$run" "$label" "$data"
wait_label_synced "$run" "$label"
phase_end "$run"

idle "$run" warm 60
out=$run/serving
mkdir -p "$out"
phase_begin "$run" suite:concurrency-10000
run_client "$out/concurrency.pid" "$out/concurrency.log" \
    "$SERVING_SUITE" --endpoint "http://$GRPC" --label "$label-restart" --output "$out/concurrency" \
    --start "$START" --end "$END" --seed 20260829 --suites concurrency --clients 1,16,64,256 \
    --concurrency-blocks 10000 --seconds 20 || log "concurrency exited $?"
phase_end "$run"
phase_begin "$run" suite:sync
run_client "$out/sync.pid" "$out/sync.log" \
    "$SERVING_SUITE" --endpoint "http://$GRPC" --label "$label-restart" --output "$out/sync" \
    --start "$START" --end "$END" --seed 20260829 --suites sync --sync-chunk 10000 \
    || log "sync exited $?"
phase_end "$run"
idle "$run" after 30
log "done: $run"
# The snapshot labels have no wallet run: the restart is the last run on their index.
[[ $label == *-snapshot ]] && { stop_pid "$run/server.pid"; stop_pid "$run/zakurad.pid"; offload_index "$label"; }
exit 0
