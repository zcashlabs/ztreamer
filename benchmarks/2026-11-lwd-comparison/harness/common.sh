# Shared settings of the 2026-11 ztreamer / lightwalletd benchmark. Source it; do not run it.
# Every path can be overridden from the environment.

ROOT=${ROOT:-$HOME/bench-2026-11}
REPO=${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
HARNESS=$REPO/benchmarks/2026-11-lwd-comparison/harness

ZAKURA_CACHE=${ZAKURA_CACHE:-$ROOT/zakura-cache}
ZAKURAD=${ZAKURAD:-$ROOT/zakura-v1.8.4/target/release/zakurad}
ZTREAMERD=${ZTREAMERD:-$REPO/target/maxperf/ztreamerd}
SERVING_SUITE=${SERVING_SUITE:-$REPO/target/release/examples/serving-suite}
LIGHTWALLETD=${LIGHTWALLETD:-$ROOT/bin/lightwalletd}
GHZ=${GHZ:-$ROOT/bin/ghz}
PROTO_DIR=$REPO/crates/protocol/proto/lightwalletd

# Serving interval: Sapling activation to the tip of the archive. The node follows the live chain
# above it; every server serves the same fixed interval.
START=${START:-419200}
END=${END:-3564614}

GRPC=127.0.0.1:9067
NODE_RPC=127.0.0.1:8232
ZT_METRICS=127.0.0.1:9999

FIXTURES=$HARNESS/fixtures.json

export NO_COLOR=1

log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }

# Starts the sampler in the background. Arguments go to sample.py.
start_sampler() {
    local run=$1; shift
    rm -f "$run/sampler.stop"
    python3 -I "$HARNESS/sample.py" --out "$run/resources.csv" --stop-file "$run/sampler.stop" "$@" &
    echo $! > "$run/sampler.pid"
}

stop_sampler() {
    local run=$1
    touch "$run/sampler.stop"
    wait "$(cat "$run/sampler.pid")" 2>/dev/null || true
}

# Records a phase boundary: phases.csv has name, start and end in Unix seconds.
phase_begin() { PHASE_NAME=$2; PHASE_START=$(date +%s.%N); log "phase $2"; }
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
render_config() {
    sed "s|@ZAKURA_CACHE@|$ZAKURA_CACHE|; s|@ROOT@|$ROOT|" "$HARNESS/$2.toml.in" > "$1/$2.toml"
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
    "$ZAKURAD" -c "$run/zakura-node.toml" start > "$run/zakurad.log" 2>&1 &
    echo $! > "$run/zakurad.pid"
    wait_port "$NODE_RPC" 1200 || { log "zakurad RPC did not start"; return 1; }
    wait_node_synced
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
        echo "ztreamer_commit=$(git -C "$REPO" rev-parse HEAD)"
        echo "ztreamer_dirty=$(git -C "$REPO" status --porcelain -- crates bin Cargo.lock | wc -l)"
        echo "ztreamerd_version=$("$ZTREAMERD" --version)"
        echo "zakurad_version=$("$ZAKURAD" --version 2>/dev/null | head -1)"
        echo "lightwalletd_commit=$(git -C "$REPO/../lightwalletd" rev-parse HEAD)"
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
    "$@" > "$run/$name.log" 2>&1 &
    echo $! > "$run/$name.pid"
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
        last=$(grep -a 'replicator.fill_pass' "$run/ztreamerd.log" | tail -1 | cut -c1-26 | sed 's/\x1b\[[0-9;]*m//g')
        [[ -z $last ]] && last=$(date -u -d @"$PHASE_START" +%Y-%m-%dT%H:%M:%S)
        now=$(date +%s)
        (( now - $(date -d "${last}Z" +%s 2>/dev/null || echo "$now") >= 60 )) && return 0
        sleep 5
    done
}

# Everything after the sync: wait for background work, idle, serve, idle, record sizes.
# RUN LABEL DATA_DIR
finish_run() {
    local run=$1 label=$2 data=$3
    if [[ $label == ztreamer-* ]]; then
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
