#!/usr/bin/env bash
# Finishes a run whose script stopped after the sync, with the server and sampler still running:
# RUN LABEL DATA_DIR SYNC_END_UNIX_S. Records the end of the sync phase, then runs finish_run.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
run=$1 label=$2 data=$3 sync_end=$4
trap 'stop_pid "$run/ztreamerd.pid"; stop_pid "$run/lightwalletd.pid"; stop_pid "$run/zakurad.pid"; stop_sampler "$run"' EXIT
sync_start=$(awk -F, '$1=="sync" {print $2}' "$run/phases.csv")
[[ -n $sync_start ]] || sync_start=$(cat "$run/sync-start.txt")
echo "sync,$sync_start,$sync_end" >> "$run/phases.csv"
echo "height=$(zt_height) unix_s=$sync_end resumed=1" > "$run/synced.txt"
PHASE_START=$sync_end
finish_run "$run" "$label" "$data"
