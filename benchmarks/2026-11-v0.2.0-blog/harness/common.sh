# Shared settings of the ztreamer v0.2.0 / lightwalletd / zingod benchmark. Source it; do not run it.
# Every path can be overridden from the environment.

ROOT=${ROOT:-$HOME/bench-2026-11}
REPO=${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
HARNESS=$REPO/benchmarks/2026-11-v0.2.0-blog/harness
RUNS=${RUNS:-$ROOT/blog/runs}
SRC=${SRC:-$HOME/dev/zcash}

ZAKURA_CACHE=${ZAKURA_CACHE:-$ROOT/zakura-cache}
ZAKURAD=${ZAKURAD:-$SRC/zakura-v1.8.9/target/release/zakurad}
ZTREAMERD=${ZTREAMERD:-$SRC/ztreamer-v0.2.0/target/maxperf/ztreamerd}
SERVING_SUITE=${SERVING_SUITE:-$SRC/ztreamer-v0.2.0/target/release/examples/serving-suite}
LIGHTWALLETD=${LIGHTWALLETD:-$SRC/lightwalletd-v0.6.2/lightwalletd}
ZINGOD=${ZINGOD:-$SRC/zingod-v1.1.0/target/release/zingod}
ZINGOD_SNAPSHOT=${ZINGOD_SNAPSHOT:-https://snapshots.zingolabs.dev/zingod-snapshot-1.0.0/manifest.json}
CBB=${CBB:-$SRC/cbb/target/release/cbb}
LWB=${LWB:-$SRC/lwd-wallet-bench}
GHZ=${GHZ:-$ROOT/bin/ghz}
PROTO_DIR=$SRC/ztreamer-v0.2.0/crates/protocol/proto/lightwalletd

# Serving interval: Sapling activation to the tip of the archive. The node follows the live chain
# above it; every server serves the same fixed interval.
START=${START:-419200}
END=${END:-3564614}

GRPC=127.0.0.1:9067
NODE_RPC=127.0.0.1:8232
ZT_METRICS=127.0.0.1:9999
ZINGOD_ADMIN=127.0.0.1:9998

FIXTURES=$HARNESS/fixtures.json
PY=${PY:-$ROOT/venv/bin/python}

export NO_COLOR=1

log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }

HC=${HC:-herdr-core}

# Runs COMMAND in a visible herdr-core terminal NAME (bench-node, bench-server, bench-sampler,
# bench-client), a child of the terminal of the run script. Its output goes to the terminal and to
# LOG; PIDFILE gets the PID of COMMAND, and PIDFILE with .rc for .pid its exit code when it ends.
# NAME PIDFILE LOG COMMAND...
hc_bg() {
    local name=$1 pidfile=$2 logfile=$3; shift 3
    rm -f "$pidfile" "${pidfile%.pid}.rc"
    "$HC" kill "$name" > /dev/null 2>&1 || true
    "$HC" run --bg --no-notify --name "$name" --cwd "$(dirname "$pidfile")" -- bash -c '
        pidfile=$1 logfile=$2; shift 2
        "$@" > >(tee -a "$logfile") 2>&1 &
        pid=$!; echo "$pid" > "$pidfile"; wait "$pid"; echo $? > "${pidfile%.pid}.rc"' \
        _ "$pidfile" "$logfile" "$@" > /dev/null
    local i
    for ((i = 0; i < 100; i++)); do [[ -s $pidfile ]] && return 0; sleep 0.1; done
    log "$name did not start"; return 1
}

# Waits until the command of PIDFILE ends and returns its exit code.
hc_wait() {
    local rc=${1%.pid}.rc
    until [[ -f $rc ]]; do sleep 1; done
    return "$(cat "$rc")"
}

# Runs a benchmark client in the terminal bench-client and waits for it: PIDFILE LOG COMMAND...
run_client() {
    local pidfile=$1 logfile=$2; shift 2
    : > "$logfile"
    hc_bg bench-client "$pidfile" "$logfile" "$@" && hc_wait "$pidfile"
}

# Starts the sampler in the background. Arguments go to sample.py.
start_sampler() {
    local run=$1; shift
    rm -f "$run/sampler.stop"
    hc_bg bench-sampler "$run/sampler.pid" "$run/sampler.log" \
        "$PY" -I "$HARNESS/sample.py" --out "$run/resources.csv" --stop-file "$run/sampler.stop" "$@"
}

stop_sampler() {
    local run=$1
    [[ -f $run/sampler.pid ]] || return 0
    touch "$run/sampler.stop"
    hc_wait "$run/sampler.pid" || true
}

# Records a phase boundary: phases.csv has name, start and end in Unix seconds.
# RUNS/current.txt names the run and phase for the bench-progress watcher.
phase_begin() {
    PHASE_NAME=$2; PHASE_START=$(date +%s.%N); log "phase $2"
    echo "$1 $2 ${PHASE_START%.*}" > "$RUNS/current.txt"
}
phase_end() {
    local run=$1
    [[ -f $run/phases.csv ]] || echo "phase,start_unix_s,end_unix_s" > "$run/phases.csv"
    echo "$PHASE_NAME,$PHASE_START,$(date +%s.%N)" >> "$run/phases.csv"
}

# Waits until a TCP port accepts connections.
wait_port() {
    local hostport=$1 timeout=${2:-600} i
    for ((i = 0; i < timeout; i++)); do
        (exec 3<>"/dev/tcp/${hostport%:*}/${hostport#*:}") 2>/dev/null && return 0
        sleep 1
    done
    return 1
}

# JSON-RPC call to the zakura node.
node_rpc() {
    curl -s --max-time 30 -H 'content-type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}" "http://$NODE_RPC" || true
}

# Writes RUN/NAME.toml from the template NAME.toml.in.
# Writes RUN/NAME.toml from the template NAME.toml.in. @DATA@ is DATA_DIR (default RUN/data).
render_config() {
    sed "s|@ZAKURA_CACHE@|$ZAKURA_CACHE|; s|@ROOT@|$ROOT|; s|@DATA@|${3:-$1/data}|" "$HARNESS/$2.toml.in" > "$1/$2.toml"
}

# The tip height of the zakura node, or empty.
node_tip() { node_rpc getblockcount | python3 -c 'import json,sys
try: print(json.load(sys.stdin)["result"])
except Exception: print("")'; }

# Waits until the node is at the network tip: its tip equals its estimated height and the
# headers, and has not moved for 30 s, or a new block arrived in that time.
wait_node_synced() {
    local i info
    for ((i = 0; i < 7200; i++)); do
        info=$(node_rpc getblockchaininfo)
        if python3 -c 'import json,sys
r=json.loads(sys.argv[1])["result"]
sys.exit(0 if r["blocks"]>=r["headers"]-1 and r["blocks"]>=r["estimatedheight"]-2 and r["blocks"]>=int(sys.argv[2]) else 1)' "$info" "$END" 2>/dev/null; then
            log "node synced at $(node_tip)"; return 0
        fi
        sleep 5
    done
    log "node did not sync: $info"; return 1
}

# Starts zakurad on the archive state with the offline benchmark configuration.
start_node() {
    local run=$1
    render_config "$run" zakura-node
    hc_bg bench-node "$run/zakurad.pid" "$run/zakurad.log" "$ZAKURAD" -c "$run/zakura-node.toml" start
    wait_port "$NODE_RPC" 1200 || { log "zakurad RPC did not start"; return 1; }
    wait_node_synced && echo "node_tip_start=$(node_tip)" >> "$run/env.txt"
}

stop_pid() {
    local pidfile=$1 pid i
    pid=$(cat "$pidfile" 2>/dev/null) || return 0
    kill -INT "$pid" 2>/dev/null || return 0
    for ((i = 0; i < 120; i++)); do kill -0 "$pid" 2>/dev/null || return 0; sleep 1; done
    kill -KILL "$pid" 2>/dev/null || true
}

# Records the machine and the builds.
record_env() {
    local run=$1
    {
        echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo "harness_commit=$(git -C "$REPO" rev-parse HEAD)"
        echo "ztreamerd_version=$("$ZTREAMERD" --version)"
        echo "zakurad_version=$("$ZAKURAD" --version 2>/dev/null | head -1)"
        echo "ztreamerd_commit=$(git -C "$(dirname "$ZTREAMERD")/../.." rev-parse HEAD)"
        echo "lightwalletd_commit=$(git -C "$(dirname "$LIGHTWALLETD")" rev-parse HEAD)"
        echo "lightwalletd_tag=$(git -C "$(dirname "$LIGHTWALLETD")" describe --tags)"
        echo "zingod_version=$("$ZINGOD" --version)"
        echo "zingod_commit=$(git -C "$(dirname "$ZINGOD")/../.." rev-parse HEAD)"
        echo "zakurad_commit=$(git -C "$(dirname "$ZAKURAD")/../.." rev-parse HEAD)"
        echo "start=$START"
        echo "end=$END"
        echo "kernel=$(uname -srm)"
        echo "cpu=$(lscpu | sed -n 's/^Model name: *//p')"
        echo "governor=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null)"
        echo "loadavg_before=$(cat /proc/loadavg)"
    } > "$run/env.txt"
}

# Runs a server in the background: RUN NAME COMMAND... RUN/NAME.pid holds its PID. The sampler
# gives its CPU time, peak RSS and disk bytes.
launch() {
    local run=$1 name=$2; shift 2
    hc_bg bench-server "$run/$name.pid" "$run/$name.log" "$@"
}

# The durable height that ztreamerd reports on its metrics endpoint, or empty.
zt_height() {
    { curl -s --max-time 2 "http://$ZT_METRICS/metrics" || true; } \
        | sed -n 's/^ztreamer_index_durable_block_height\({[^}]*}\)\? \([0-9.e+]*\)$/\2/p' \
        | python3 -c 'import sys; v=sys.stdin.read().split(); print(int(float(v[0])) if v else "")'
}

# The newest height in the lightwalletd log: RUN.
lwd_height() {
    { grep -oE 'Adding block to cache [0-9]+|Waiting for block: [0-9]+' "$1/lwd-server.log" 2>/dev/null || true; } \
        | tail -1 | awk '/Waiting/ {print $NF - 1; next} {print $NF}'
}

# Waits until the server is within SLACK blocks of the tip of the synced node:
# RUN PIDFILE SLACK HEIGHT_COMMAND... ztreamerd keeps the non-finalized blocks out of its durable
# height, so its SLACK is the non-finalized window; lightwalletd has SLACK 0.
# Writes RUN/synced.txt with the height and the time.
wait_server_synced() {
    local run=$1 pidfile=$2 slack=$3; shift 3
    local h tip
    while :; do
        kill -0 "$(cat "$pidfile")" 2>/dev/null || { log "server exited"; return 1; }
        h=$("$@")
        tip=$(node_tip)
        if [[ -n $h && -n $tip && $h -ge $((tip - slack)) && $h -ge $END ]] \
            && wait_node_synced > /dev/null 2>&1; then
            break
        fi
        sleep 1
    done
    echo "height=$h unix_s=$(date +%s.%N)" > "$run/synced.txt"
    wait_port "$GRPC" 600
}

idle() {
    phase_begin "$1" "idle:$2"; sleep "$3"; phase_end "$1"
}

# Waits until ztreamerd has logged no replicator fill pass for 60 s: after the first sync it fills
# the range metadata of every sealed range, which a later sync does not repeat. RUN.
wait_fill_quiet() {
    local run=$1 last now
    while :; do
        last=$(grep -a 'replicator.fill_pass' "$run/server.log" | tail -1 | cut -c1-26 | sed 's/\x1b\[[0-9;]*m//g')
        [[ -z $last ]] && last=$(date -u -d @"$PHASE_START" +%Y-%m-%dT%H:%M:%S)
        now=$(date +%s)
        (( now - $(date -d "${last}Z" +%s 2>/dev/null || echo "$now") >= 60 )) && return 0
        sleep 5
    done
}

# Waits until the note commitment trees of the ztreamerd index reach its durable tip: from then on
# GetTreeState reads the index and not the node. zt-trees.py reads the height from the index.
# RUN DATA_DIR
wait_trees_ready() {
    local run=$1 data=$2 trees durable
    while :; do
        kill -0 "$(cat "$run/server.pid")" 2>/dev/null || { log "server exited"; return 1; }
        trees=$("$PY" -I "$HARNESS/zt-trees.py" "$data")
        durable=$(zt_height)
        [[ -n $trees && -n $durable && $trees -ge $durable ]] && break
        sleep 2
    done
    echo "trees=$trees durable=$durable unix_s=$(date +%s.%N)" > "$run/trees-ready.txt"
}

# The served height of zingod once it serves and every index is within the finalised depth of it,
# else the lowest finalized height of its indexes, or empty.
zingod_height() {
    local status metrics
    status=$(curl -s --max-time 2 "http://$GRPC/status" || true)
    metrics=$(curl -s --max-time 2 "http://$ZINGOD_ADMIN/metrics" || true)
    python3 -c 'import json,re,sys
heights=[int(float(v)) for v in re.findall(r"^zingod_index_finalized_height\{[^}]*\}\s+(\S+)$", sys.argv[2], re.M)]
try: st=json.loads(sys.argv[1])
except Exception: st={}
served=(st.get("chain") or {}).get("served")
if served and st.get("state") in ("serving","degraded") and heights and min(heights)>=served-1001: print(served)
elif heights: print(min(heights))
else: print("")' "$status" "$metrics"
}

# The process name of the server of LABEL, as the sampler and the report name it.
server_name() {
    case $1 in
    ztreamer-*) echo ztreamerd ;;
    lightwalletd) echo lightwalletd ;;
    zingod-*) echo zingod ;;
    esac
}

# Starts the sampler for the server of LABEL (and the standalone node unless it is embedded).
# RUN LABEL DATA_DIR
start_label_sampler() {
    local run=$1 label=$2 data=$3 args=(--proc "$(server_name "$label")=$run/server.pid")
    [[ $label != ztreamer-direct ]] && args+=(--proc "zakurad=$run/zakurad.pid")
    case $label in
    ztreamer-*) args+=(--ztreamer-metrics "http://$ZT_METRICS/metrics" --zt-index "$data") ;;
    lightwalletd) args+=(--lwd-log "$run/lwd-server.log") ;;
    zingod-*) args+=(--zingod-metrics "http://$ZINGOD_ADMIN/metrics") ;;
    esac
    start_sampler "$run" "${args[@]}" --du "index=$data"
}

# Starts the node that LABEL needs and brings it to the tip. ztreamer-direct embeds its node, so
# the standalone node only catches up and stops, and the sync time of ztreamerd never includes the
# catch-up of its node. RUN LABEL
start_label_node() {
    local run=$1 label=$2
    if [[ $label == ztreamer-direct ]]; then
        phase_begin "$run" node-presync
        start_node "$run"
        stop_pid "$run/zakurad.pid"
    else
        phase_begin "$run" node-start
        start_node "$run"
    fi
    phase_end "$run"
}

# Starts the server of LABEL on DATA_DIR in the background: RUN/server.pid, RUN/server.log.
# RUN LABEL DATA_DIR
launch_server() {
    local run=$1 label=$2 data=$3
    case $label in
    ztreamer-direct)
        render_config "$run" zakura-embedded
        launch "$run" server "$ZTREAMERD" --zakura-config "$run/zakura-embedded.toml" \
            --index-dir "$data" --grpc-listen "$GRPC" --metrics-listen "$ZT_METRICS"
        ;;
    ztreamer-snapshot)
        # Remote mode: on Mainnet the default source is https://snapshot.ztreamer.sh/mainnet.
        launch "$run" server "$ZTREAMERD" --remote-rpc "http://$NODE_RPC" \
            --index-dir "$data" --grpc-listen "$GRPC" --metrics-listen "$ZT_METRICS"
        ;;
    lightwalletd)
        mkdir -p "$data"
        launch "$run" server "$LIGHTWALLETD" --no-tls-very-insecure --grpc-bind-addr "$GRPC" \
            --http-bind-addr 127.0.0.1:9068 --data-dir "$data" --log-file "$run/lwd-server.log" \
            --rpchost "${NODE_RPC%:*}" --rpcport "${NODE_RPC#*:}" --rpcuser bench --rpcpassword bench
        ;;
    zingod-direct | zingod-snapshot)
        render_config "$run" zingod "$data"
        [[ $label == zingod-snapshot ]] && printf '\n[snapshot]\nmanifest = "%s"\n' "$ZINGOD_SNAPSHOT" >> "$run/zingod.toml"
        launch "$run" server "$ZINGOD" -c "$run/zingod.toml" start
        ;;
    *) log "unknown label $label"; return 1 ;;
    esac
}

# Waits until the server of LABEL is at the tip of the synced node. ztreamerd keeps the
# non-finalized blocks out of its durable height (slack 100); lightwalletd must reach the tip;
# zingod serves its non-finalized blocks and must be within one block. RUN LABEL
wait_label_synced() {
    local run=$1 label=$2
    case $label in
    ztreamer-*) wait_server_synced "$run" "$run/server.pid" 100 zt_height ;;
    lightwalletd) wait_server_synced "$run" "$run/server.pid" 0 lwd_height "$run" ;;
    zingod-*) wait_server_synced "$run" "$run/server.pid" 1 zingod_height ;;
    esac
}

# Everything after the sync: wait for background work, idle, serve, idle, record sizes.
# ztreamerd builds its note commitment trees and fills its range metadata after the block sync;
# the trees phase ends when GetTreeState reads the index. RUN LABEL DATA_DIR
finish_run() {
    local run=$1 label=$2 data=$3
    if [[ $label == ztreamer-* ]]; then
        phase_begin "$run" trees
        wait_trees_ready "$run" "$data"
        phase_end "$run"
        phase_begin "$run" post-sync
        wait_fill_quiet "$run"
        phase_end "$run"
    fi
    idle "$run" warm 60
    bash "$HARNESS/serve.sh" "$run" "$label"
    idle "$run" after 30
    du -sb "$data" > "$run/index-size.txt"
    log "done: $run"
}

# The directory on the second drive (nvme0n1p1) for indexes that no run needs any more, or empty
# if that drive is not mounted.
OFFLOAD_DEV=${OFFLOAD_DEV:-/dev/nvme0n1p1}
offload_dir() {
    local mnt
    mnt=$(findmnt -no TARGET "$OFFLOAD_DEV" 2>/dev/null | head -1)
    [[ -n $mnt && -w $mnt ]] && echo "$mnt/bench-2026-11-blog"
    return 0
}

# Stops the run unless the benchmark disk has NEED_GB free: NEED_GB.
need_space() {
    local avail
    avail=$(df --output=avail -BG "$ROOT" | tail -1 | tr -dc 0-9)
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) free=${avail}G need=$1G" >> "$RUNS/disk.log"
    (( avail >= $1 )) && return 0
    log "only ${avail} GB free on $ROOT, need $1 GB"; return 1
}

# Moves the index of LABEL to the second drive once its last run is done. The run directory keeps
# its CSVs, logs and index-size.txt; data.offloaded names the new place. Without the second drive
# the index stays. LABEL
offload_index() {
    local label=$1 data=$RUNS/$1/data dest
    [[ -d $data ]] || return 0
    dest=$(offload_dir)
    [[ -n $dest ]] || { log "second drive not mounted; $data stays"; return 0; }
    mkdir -p "$dest/$label"
    log "offload $data to $dest/$label/data"
    rsync -a --remove-source-files "$data/" "$dest/$label/data/" && rm -rf "$data" \
        && echo "$dest/$label/data" > "$RUNS/$label/data.offloaded"
    df -h --output=avail "$ROOT" | tail -1 | xargs -I{} log "free after offload: {}"
}
