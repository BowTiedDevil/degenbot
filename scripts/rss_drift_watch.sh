#!/usr/bin/env bash
# FRKBGP close-out: RSS drift-watch baseline sampler (pair-review request).
# Samples the settlement bot's live RSS hourly for 24h into a CSV to give the
# drift tripwire a slope baseline: load-plateau vs continuous creep, per
# logs/solve-cycle-profile.md (tripwire: monotone creep / ~24GiB brush).
# Resolves the bot per sample as the LARGEST-RSS match of the eth_settlement
# process family (uv wrapper + python child; the wrapper alone reads ~30MB).
OUT=/workspaces/degenbot/logs/rss-drift-watch.csv
[ -f "$OUT" ] || echo "timestamp,epoch_s,pid,etime,rss_kb" > "$OUT"
sample_pid() {
  local best=0 best_rss=0 p rss
  for p in $(pgrep -f eth_settlement); do
    rss=$(ps -o rss= -p "$p" 2>/dev/null | tr -d ' ')
    if [ -n "$rss" ] && [ "$rss" -gt "$best_rss" ]; then best=$p; best_rss=$rss; fi
  done
  echo "$best"
}
for i in $(seq 1 24); do
  PID=$(sample_pid)
  if [ -n "$PID" ] && [ "$PID" != 0 ] && kill -0 "$PID" 2>/dev/null; then
    ETIME=$(ps -o etime= -p "$PID" | tr -d ' ')
    RSSKB=$(ps -o rss= -p "$PID" | tr -d ' ')
    echo "$(date -Iseconds),$(date +%s),$PID,$ETIME,$RSSKB" >> "$OUT"
  else
    echo "$(date -Iseconds),$(date +%s),none,down,0" >> "$OUT"
  fi
  sleep 3600
done
