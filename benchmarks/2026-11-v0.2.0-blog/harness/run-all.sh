#!/usr/bin/env bash
# Every run of the benchmark, one after the other. A step whose directory has DONE is skipped, so
# a stopped run-all resumes at the first unfinished step. The first failure stops run-all.
set -Eeuo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
mkdir -p "$RUNS"
steps=(
    "main ztreamer-direct" "restart ztreamer-direct" "wallet ztreamer-direct"
    "main ztreamer-snapshot" "restart ztreamer-snapshot"
    "main lightwalletd" "restart lightwalletd" "wallet lightwalletd"
    "main zingod-direct" "restart zingod-direct" "wallet zingod-direct"
    "main zingod-snapshot" "restart zingod-snapshot"
)
for step in "${steps[@]}"; do
    read -r kind label <<< "$step"
    dir=$RUNS/$label; [[ $kind != main ]] && dir=$RUNS/$label-$kind
    [[ -f $dir/DONE ]] && { log "skip $kind $label"; continue; }
    log "start $kind $label"
    bash "$HARNESS/run-$kind.sh" "$label"
    touch "$dir/DONE"
    log "done $kind $label"
done
log "all runs done"
