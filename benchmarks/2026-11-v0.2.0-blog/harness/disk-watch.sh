#!/usr/bin/env bash
# The bench-disk watcher: logs the free space of the benchmark disk every 60 s. Below MIN_GB
# (default 25) it stops the server under test, so the run fails and run-all stops before the disk
# is full, and it exits, which notifies the session.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
min=${MIN_GB:-25}
while :; do
    avail=$(df --output=avail -BG "$ROOT" | tail -1 | tr -dc 0-9)
    echo "$(date -u +%H:%M:%S) free ${avail} GB · offload drive: $(offload_dir || echo none)"
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) free=${avail}G watch" >> "$RUNS/disk.log"
    if (( avail < min )); then
        read -r run _ < "$RUNS/current.txt"
        log "free space ${avail} GB < ${min} GB: stopping the server of $run"
        kill -INT "$(cat "$run/server.pid")" 2>/dev/null
        exit 1
    fi
    sleep 60
done
