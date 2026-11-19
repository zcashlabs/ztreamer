#!/usr/bin/env bash
# Wallet sync benchmarks of one server on the index of its main run:
#   run-wallet.sh ztreamer-direct|zingod-direct|lightwalletd
# 1. lwd-wallet-bench: its recommended `suite` (app-open, catch-up, first-sync, enhance,
#    transparent, mixed, scale, tip-follow) with the default profiles and repetitions. No soak:
#    on loopback with one wallet it repeats the app-open, catch-up and enhance steps.
# 2. cbb (https://github.com/dismad/cbb): the sweep of its bench-*.sh scripts, 50 to 200 wallets,
#    a cold and a warm pass, 50 ranges of 100 blocks from height 2,000,000, with GetTreeState
#    every 5 ranges and with BlockRange only.
# Writes $RUNS/LABEL-wallet.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
label=$1
main=$RUNS/$label
data=$main/data
run=${RUN:-$RUNS/$label-wallet}
[[ -d $data ]] || { log "no index in $main"; exit 1; }
mkdir -p "$run/lwb" "$run/cbb"
record_env "$run"
trap 'stop_pid "$run/server.pid"; stop_pid "$run/zakurad.pid"; stop_sampler "$run"' EXIT

start_label_sampler "$run" "$label" "$data"
start_label_node "$run" "$label"
phase_begin "$run" restart
launch_server "$run" "$label" "$data"
wait_label_synced "$run" "$label"
phase_end "$run"
idle "$run" warm 60

name=${label//-/_}
phase_begin "$run" lwb:suite
run_client "$run/lwb/suite.pid" "$run/lwb/suite.log" \
    env PYTHONPATH="$LWB/src" "$LWB/.venv/bin/python" -m lwdbench.cli suite --services "$name" \
    --endpoint "$name=plain://$GRPC" --own "$name" --label "$label" --out-dir "$run/lwb" --no-geo --soak-min 0 \
    --fixtures "$LWB/fixtures/mainnet.json" || log "lwd-wallet-bench exited $?"
phase_end "$run"

for mode in tree blockrange; do
    extra=()
    [[ $mode == blockrange ]] && extra=(--tree-every 0)
    for conc in 50 100 150 200; do
        for pass in cold warm; do
            phase_begin "$run" "cbb:$mode:$conc:$pass"
            run_client "$run/cbb/$mode-$conc-$pass.pid" "$run/cbb/$mode-$conc-$pass.log" \
                "$CBB" --endpoint "$label=http://$GRPC" --concurrency "$conc" --steps 50 --chunk 100 \
                --start-height 2000000 "${extra[@]}" --json-out "$run/cbb/$mode-$conc-$pass.json" \
                || log "cbb $mode $conc $pass exited $?"
            phase_end "$run"
        done
    done
done
idle "$run" after 30
log "done: $run"
stop_pid "$run/server.pid"; stop_pid "$run/zakurad.pid"
offload_index "$label"
