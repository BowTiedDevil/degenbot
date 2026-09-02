#!/usr/bin/env bash
# T3 matrix driver (epic AZZDBI): run the purge-delay arms sequentially, one
# 40-min pump per arm, analyzing each with scripts/mimalloc_analyze.py and
# appending one verdict line per arm to logs/mimalloc/matrix_summary.log.
#
# Arms (fixed DEGENBOT_MIMALLOC_PURGE_DELAY_MS overrides auto-discovery):
#   delay-12s  - ~1 block interval
#   delay-24s  - ~2 blocks (what auto-discovery picked live)
#   delay-60s  - ~5 blocks
#   madv-free  - MIMALLOC_PURGE_DECOMMITS=0 (MADV_FREE, lazy reclaim) at 12s
# Comparison baseline: logs/mimalloc/baseline-default (T2, default 10ms).
# Each arm shares the T2 protocol (2400000ms pump window, dry-run).
set -uo pipefail
cd /workspaces/degenbot

SUMMARY=logs/mimalloc/matrix_summary.log
shutdown_ms=2400000

for arm_spec in \
  "delay-12s DEGENBOT_MIMALLOC_PURGE_DELAY_MS=12000" \
  "delay-24s DEGENBOT_MIMALLOC_PURGE_DELAY_MS=24000" \
  "delay-60s DEGENBOT_MIMALLOC_PURGE_DELAY_MS=60000" \
  "madv-free DEGENBOT_MIMALLOC_PURGE_DELAY_MS=12000 MIMALLOC_PURGE_DECOMMITS=0"; do
  label="${arm_spec%% *}"
  rest="${arm_spec#* }"
  read -ra extra <<< "$rest"

  if [ -e "logs/mimalloc/$label" ]; then
    echo "[matrix] $label already present - skipping" | tee -a "$SUMMARY"
    continue
  fi

  echo "[matrix] ARM START $label $(date -Is)" | tee -a "$SUMMARY"
  bash scripts/mimalloc_soak.sh "$label" "$shutdown_ms" "${extra[@]}" \
    >> "logs/mimalloc/soak-$label.console.log" 2>&1

  if uv run python scripts/mimalloc_analyze.py "logs/mimalloc/$label" \
      --log "logs/mimalloc/$label/mimalloc_stats.log" > "/tmp/mx_$label.txt" 2>&1; then
    echo "[matrix] ARM $label analyzed" | tee -a "$SUMMARY"
  else
    echo "[matrix] ARM $label ANALYZE FAILED - retry with full run log" | tee -a "$SUMMARY"
    uv run python scripts/mimalloc_analyze.py "logs/mimalloc/$label" \
      --log logs/bot_run.log >> "$SUMMARY" 2>&1
  fi
  python3 - <<PYEOF | tee -a "$SUMMARY"
import json
s = json.load(open('logs/mimalloc/$label/summary.json'))
fmt = lambda x: round(x/1e6, 2) if isinstance(x, (int, float)) else x
print('[matrix] %s: blocks=%s faults/block=%.0f+-%.0f rss@blk %.2f-%.2fGB solve.on_drain.p95=%s' % (
    label.split('/')[-1], s.get('blocks_analyzed'),
    (s.get('min_flt_per_block_mean') or 0), (s.get('min_flt_per_block_std') or 0),
    fmt(s.get('rss_at_block_min_kb')) or 0, fmt(s.get('rss_at_block_max_kb')) or 0,
    (s.get('solve', {}).get('SolveCoordinator::on_drain', {}) or {}).get('p95')))
PYEOF
done
echo "[matrix] ALL ARMS COMPLETE $(date -Is)" | tee -a "$SUMMARY"
