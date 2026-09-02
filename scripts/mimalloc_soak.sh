#!/usr/bin/env bash
# mimalloc purge-delay soak (epic AZZDBI T1). One invocation = one labeled soak arm.
#
#   scripts/mimalloc_soak.sh <label> <shutdown_ms> [MIMALLOC_*=VALUE ...]
#
# Runs the dry-run bot detached via ./run_bot.sh with DEGENBOT_HOTPATH=1 +
# HOTPATH_SHUTDOWN_MS (cooperative timed exit -> hp.json) and the in-process
# /proc sampler (DEGENBOT_PROCMEM_SECS/DEGENBOT_PROCMEM_CSV). MIMALLOC_* args
# are exported BEFORE launch (mimalloc reads them at first use). mimalloc's
# exit stats land in bot_run.log; they are carved out into artifacts after the
# run. Artifacts -> logs/mimalloc/<label>/:
#   procmem.csv hp.json mimalloc_stats.log bot_log_tail.log run_env.txt
# Refuses to clobber an existing label.
set -euo pipefail
cd /workspaces/degenbot

LABEL="${1:?label required}"
SHUTDOWN_MS="${2:?shutdown_ms required, e.g. 2400000}"
shift 2

OUT="logs/mimalloc/$LABEL"
if [ -e "$OUT" ]; then
  echo "[soak] refusing to clobber $OUT - delete it first or pick a new label" >&2
  exit 1
fi
mkdir -p "$OUT"

for kv in "$@"; do
  case "$kv" in
    MIMALLOC_*=*) export "${kv%%=*}"="${kv#*=}" ;;
    DEGENBOT_MIMALLOC_*=*) export "${kv%%=*}"="${kv#*=}" ;;
    *) echo "[soak] ignoring non-mimalloc arg: $kv" >&2 ;;
  esac
done

export MIMALLOC_SHOW_STATS=1
export DEGENBOT_HOTPATH=1
export HOTPATH_SHUTDOWN_MS="$SHUTDOWN_MS"
export HOTPATH_OUTPUT_PATH="$PWD/$OUT/hp.json"
export HOTPATH_OUTPUT_FORMAT=json
export HOTPATH_REPORT=functions-timing,threads
export DEGENBOT_PROCMEM_SECS="${DEGENBOT_PROCMEM_SECS:-1}"
export DEGENBOT_PROCMEM_CSV="$PWD/$OUT/procmem.csv"

{ env | grep -E '^(MIMALLOC|DEGENBOT_PROCMEM|DEGENBOT_HOTPATH|HOTPATH|DEGENBOT_SIM_EXIT)' | sort; } > "$OUT/run_env.txt"

echo "[soak] label=$LABEL shutdown_ms=$SHUTDOWN_MS"
cat "$OUT/run_env.txt" | sed 's/^/[soak] env: /'
./run_bot.sh start
sleep 8
PYPID=$(pgrep -P "$(head -n1 logs/bot_run.pid)" | head -n 1 || true)
echo "[soak] python pid=${PYPID:-?} - startup (WS, DB snapshots, backfill) precedes the pump"

elapsed=0
while [ "$elapsed" -lt "$SHUTDOWN_MS" ]; do
  sleep 60
  elapsed=$((elapsed + 60000))
  rss=$(ps -o rss= -p "${PYPID:-0}" 2>/dev/null | tr -d ' ' || echo '?')
  echo "[soak +$((elapsed / 1000))s] rss_kb=$rss"
done

# Cooperative timed exit unwinds the pump; allow >1 min grace so the guard
# drops and mimalloc prints its exit stats - only force-kill if still alive
grace=0
while pgrep -f eth_settlement_arbitrage_v2_v3_v4 >/dev/null 2>&1 && [ "$grace" -lt 120 ]; do
  sleep 5
  grace=$((grace + 5))
done
if pgrep -f eth_settlement_arbitrage_v2_v3_v4 >/dev/null 2>&1; then
  echo "[soak] bot still alive 120s after the timed exit - forcing stop (mimalloc stats will be lost)"
  ./run_bot.sh stop >/dev/null 2>&1 || true
fi

# carve artifacts out of bot_run.log (start-mode truncates it at launch)
tail -n 120 logs/bot_run.log > "$OUT/mimalloc_stats.log"
tail -n 4000 logs/bot_run.log > "$OUT/bot_log_tail.log"
# per-block epochs: full-run dispatch extract (latency/pump health compactly)
grep -a 'fan-out ENTER' logs/bot_run.log > "$OUT/dispatch_events.log" || true
echo "[soak] artifacts:"
ls -l "$OUT"
echo "[soak] procmem rows: $(( $(wc -l < "$OUT/procmem.csv") - 1 )) | hp.json: $(test -s "$OUT/hp.json" && echo present || echo MISSING)"
