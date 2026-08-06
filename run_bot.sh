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

# --------------------------------------------------------------------------
# Conservative (HARD/LOUD) defaults now live in the CODE, not here (Z4KQXF).
# Every invocation — run_bot.sh, a hand-run, a CI/harness — gets loud failure
# by default; there is no liberal default posture anymore. Flags that follow
# are all default-ON in code via `bot_env_flag_default_on` (opt OUT with =0):
#   DEGENBOT_ASSERT_SOLVER_STATE (ADR-021 publish tripwire, fatal on desync)
#   DEGENBOT_VERIFY_DBG          (structural verify diagnostics / divergence set)
#   DEGENBOT_DUMP_CALL_TRACE     (full revm call trace on sim failure)
#   DEGENBOT_V2_CALC_TRACE       (V2 reserves slot8 before each sim)
#   DEGENBOT_DEBUG_V4_SOLVE      (V4 solver intermediates)
#   DEGENBOT_SIM_LOG_REVERTED_SWAPS (per-hop actual-vs-predicted on revert)
#   DEGENBOT_SIM_EXIT_ON_FAIL=1  (stop on first sim failure) + IGNORE_BUCKETS
#     default "" (empty allowlist = trap on EVERY bucket, sys.exit(3))
# Per-target/high-noise (still OFF): DEGENBOT_DRAIN_DBG, DEGENBOT_TRACE_REGISTER_SEED
#
# To run a long-lived soak that trades through thin-margin/no-profit reverts
# (the routine arb-filter outcome), override the fail-fast:
#   DEGENBOT_SIM_EXIT_ON_FAIL=0 ./run_bot.sh
# --------------------------------------------------------------------------

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
