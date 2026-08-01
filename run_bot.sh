#!/usr/bin/env bash
# Drive the backrun bot with all output teed to a log file.
#
# A self-contained launcher so the bot can be started/stopped deterministically
# without rediscovering the launch mechanics each time.
#
#   ./run_bot.sh            # foreground (output -> console + log)
#   ./run_bot.sh start      # detached (setsid), pid -> logs/bot_run.pid
#   ./run_bot.sh stop       # kill the running bot (by pidfile + pkill)
#   ./run_bot.sh status     # is it running?
set -u
cd /workspaces/degenbot
LOGDIR=/workspaces/degenbot/logs
LOG="$LOGDIR/bot_run.log"
PIDFILE="$LOGDIR/bot_run.pid"
mkdir -p "$LOGDIR"

# Capture reverted-swap actual-vs-predicted output (per docs/architecture/
# sim_v4_swap_step_rounding.md) to pin the V3-V4-V3 IIA over-prediction
# magnitude. Default off; set to `1` here so a failing block emits the
# `[sim-revert-swap]` per-hop comparison that `log_reverted_swaps_vs_hop_outputs`
# produces in the backrun-strategy simulator.
export DEGENBOT_SIM_LOG_REVERTED_SWAPS=1
export DEGENBOT_DEBUG_V4_SOLVE=1

# Option-A solver-state accuracy gate (AV42C7): after every on_drain solve,
# diff each solved path's per-hop pool state against the chain at the solve
# block and PANIC immediately on any mismatch, BEFORE the hop outputs can
# reach the encoder/simulator. Catches a solver running on a desynced
# snapshot (nonce proves change, not accuracy). See solver_state_verifier.rs.
export DEGENBOT_ASSERT_SOLVER_STATE=1

# Exit on the first sim failure so the failing block's context is intact
# in the log (no continued-progress noise overwriting it).
# DISABLED for the +1 / V4 ~0.01% residual characterization (post-E7ALWT):
# the bot must keep running so we can gather frequency + a reproducible
# fixture across blocks, rather than dying on first occurrence. Re-enable
# once the FSM (3M5PO5) lands and the residuals are diagnosed.
export DEGENBOT_SIM_EXIT_ON_FAIL=0

# The actual bot invocation (uv rebuilds the Rust extension if any rust
# source / Cargo.toml is newer than the installed build).
BOT_CMD=(uv run python examples/eth_backrun_v2_v3_v4_rust.py)

start() {
    if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        echo "[runner] bot already running (pid $(cat "$PIDFILE"))" >&2
        return 1
    fi
    : > "$LOG"
    : > "$PIDFILE"
    # setsid: new session + no controlling terminal, so the launching shell
    # can exit without the pump dying (SIGHUP) and the tool shell's return
    # isn't entangled with the bot's life. exec is NOT used so `$!` is the
    # (setsid'd) uv pid we record.
    setsid "${BOT_CMD[@]}" >>"$LOG" 2>&1 < /dev/null &
    echo $! > "$PIDFILE"
    echo "[runner] started bot pid $(cat "$PIDFILE") $(date -Is)"
}

stop() {
    if [ -f "$PIDFILE" ]; then
        kill -TERM "$(cat "$PIDFILE")" 2>/dev/null
        sleep 1
        kill -9 "$(cat "$PIDFILE")" 2>/dev/null
        rm -f "$PIDFILE"
    fi
    # The uv wrapper may exit while its python child lingers; kill by name too.
    pkill -9 -f eth_backrun_v2_v3_v4 2>/dev/null
    echo "[runner] stopped $(date -Is)"
}

status() {
    if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        echo "[runner] running pid $(cat "$PIDFILE")"
        ps -o pid,etime,cmd -p "$(cat "$PIDFILE")" 2>/dev/null | tail -1
    else
        echo "[runner] not running"
    fi
}

case "${1:-foreground}" in
    start) start ;;
    stop) stop ;;
    status) status ;;
    foreground)
        echo "[runner] starting bot $(date -Is)"
        "${BOT_CMD[@]}" 2>&1 | tee -a "$LOG"
        ;;
    *)
        echo "usage: $0 {start|stop|status|foreground}" >&2
        exit 1
        ;;
esac
