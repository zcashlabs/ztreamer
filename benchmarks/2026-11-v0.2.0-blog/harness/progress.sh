#!/usr/bin/env bash
# One status screen for the bench-progress watcher: watch -n 5 progress.sh
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
read -r run phase since < "$RUNS/current.txt" 2>/dev/null || { echo "no run yet"; exit 0; }
label=$(basename "$run")
now=$(date +%s)
printf 'run      %s\nphase    %s (%ds)\n' "$label" "$phase" "$((now - since))"
printf 'node     %s\n' "$(node_tip)"
case $label in
ztreamer-*) printf 'server   durable %s  trees %s\n' "$(zt_height)" "$("$PY" -I "$HARNESS/zt-trees.py" "$RUNS/${label%-restart}/data" 2>/dev/null)" ;;
lightwalletd*) printf 'server   %s\n' "$(lwd_height "$run")" ;;
zingod-*) printf 'server   %s\n' "$(zingod_height)"; curl -s --max-time 2 "http://$GRPC/status" | head -c 300; echo ;;
esac
echo; tail -n 3 "$RUNS/run-all.log" 2>/dev/null
echo; uptime; df -h --output=avail,pcent "$ROOT" | tail -1
