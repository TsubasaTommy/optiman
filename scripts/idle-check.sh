#!/usr/bin/env bash
# Measures the agent's CPU time while idle (after it has converted one file).
# Usage: scripts/idle-check.sh [binary] [seconds]
set -euo pipefail

BIN=${1:-target/release/optimize-image-folder}
SECS=${2:-60}
# Allowed CPU time while idle, in centiseconds (ps resolution is 10 ms).
MAX_CS=${MAX_IDLE_CS:-0}

WORK=$(mktemp -d)
mkdir -p "$WORK/home" "$WORK/photos"
cat > "$WORK/home/config.toml" <<CFG
folders = ["$WORK/photos"]
original = "delete"
settle_seconds = 1
effort = 3
CFG

OIF_HOME="$WORK/home" "$BIN" --config "$WORK/home/config.toml" run --log-stderr 2> "$WORK/agent.log" &
PID=$!
trap 'kill $PID 2>/dev/null || true; rm -rf "$WORK"' EXIT

sleep 3
cp tests/fixtures/photo_orient6.jpg "$WORK/photos/a.jpg"
for _ in $(seq 1 60); do
  [ -f "$WORK/photos/a.jxl" ] && [ ! -f "$WORK/photos/a.jpg" ] && break
  sleep 1
done
if [ ! -f "$WORK/photos/a.jxl" ]; then
  echo "FAIL: the agent did not convert the test file"; cat "$WORK/agent.log"; exit 1
fi
sleep 3

# ps prints cputime as [[dd-]hh:]mm:ss.cc
to_cs() { awk -F: '{ s = 0; for (i = 1; i <= NF; i++) s = s * 60 + $i; printf "%d", s * 100 + 0.5 }'; }
cpu() { ps -o time= -p "$PID" | tr -d ' ' | to_cs; }
threads() {
  if [ "$(uname)" = "Darwin" ]; then ps -M -p "$PID" | tail -n +2 | wc -l | tr -d ' '; else ls "/proc/$PID/task" | wc -l; fi
}

BEFORE=$(cpu)
sleep "$SECS"
AFTER=$(cpu)
RSS_KB=$(ps -o rss= -p "$PID" | tr -d ' ')
DELTA=$((AFTER - BEFORE))

echo "idle window       : ${SECS}s"
echo "CPU time consumed : $((DELTA * 10)) ms"
echo "threads           : $(threads)"
echo "resident memory   : $((RSS_KB / 1024)) MB"
if [ "$(uname)" = "Darwin" ]; then
  # Idle wake-ups over a 5-second sample (should be 0).
  top -l 2 -s 5 -pid "$PID" -stats pid,cpu,idlew,threads 2>/dev/null | tail -1 | awk '{ print "top (5s sample)   : cpu " $2 "%, idle wakeups " $3 }'
fi
sed -n '1,20p' "$WORK/agent.log"

if [ "$DELTA" -gt "$MAX_CS" ]; then
  echo "FAIL: agent used CPU while idle"; exit 1
fi
echo "OK: no CPU used while idle"
