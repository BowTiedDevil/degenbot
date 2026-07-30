#!/usr/bin/env bash
# Drive the backrun bot with all output teed to a log file.
# stdout is sent to /dev/null (per instruction) but duplicated to the log.
# stderr is also captured into the log.
set -u
cd /workspaces/degenbot
LOG=/workspaces/degenbot/logs/bot_run.log
: > "$LOG"
echo "[runner] starting bot $(date -Is)"
# Capture reverted-swap actual-vs-predicted output (per docs/architecture/
# sim_v4_swap_step_rounding.md) to pin the V3-V4-V3 IIA over-prediction
# magnitude. Default off; set to `1` here so a failing block emits the
# `[sim-revert-swap]` per-hop comparison that `log_reverted_swaps_vs_hop_outputs`
# produces in the backrun-strategy simulator.
export DEGENBOT_SIM_LOG_REVERTED_SWAPS=1
export DEGENBOT_DEBUG_V4_SOLVE=1
# Exit on the first sim failure so the failing block's context is intact
# in the log (no continued-progress noise overwriting it).
export DEGENBOT_SIM_EXIT_ON_FAIL=1
# Process substitution: stdout -> tee (writes log, discards its own stdout);
# stderr merged into that same stream so tracebacks land in the log too.
exec uv run python examples/eth_backrun_v2_v3_v4_rust.py > >(tee -a "$LOG" > /dev/null) 2>&1
