#!/usr/bin/env bash
# The main run of one server: start from an empty index, sync to the tip of the node, then serve.
#   run-main.sh ztreamer-direct|ztreamer-snapshot|lightwalletd|zingod-direct|zingod-snapshot
# - ztreamer-direct: ztreamerd --zakura-config, the embedded zakura on the benchmark state.
# - ztreamer-snapshot: ztreamerd --remote-rpc; it imports the default snapshot
#   (https://snapshot.ztreamer.sh/mainnet), then reads the tail from the standalone zakurad.
# - lightwalletd: its cache from the standalone zakurad.
# - zingod-direct: every index from the standalone zakurad.
# - zingod-snapshot: every index from the zingod snapshot ($ZINGOD_SNAPSHOT), then the zakurad.
# Writes $RUNS/LABEL.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
label=$1
run=${RUN:-$RUNS/$label}
data=$run/data
[[ -e $data ]] && { log "$data exists; a main run starts from an empty index"; exit 1; }
case $label in
zingod-snapshot) need_space 200 ;;
zingod-direct) need_space 120 ;;
lightwalletd) need_space 60 ;;
*) need_space 40 ;;
esac
mkdir -p "$run"
record_env "$run"
trap 'stop_pid "$run/server.pid"; stop_pid "$run/zakurad.pid"; stop_sampler "$run"' EXIT

start_label_sampler "$run" "$label" "$data"
start_label_node "$run" "$label"
phase_begin "$run" sync
launch_server "$run" "$label" "$data"
wait_label_synced "$run" "$label"
phase_end "$run"
log "synced: $(cat "$run/synced.txt")"

finish_run "$run" "$label" "$data"
echo "node_tip_after=$(node_tip)" >> "$run/env.txt"
