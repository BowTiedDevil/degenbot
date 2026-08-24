#!/usr/bin/env bash
# Drive the settlement-arbitrage bot with all output teed to a log file.
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
#   DEGENBOT_SIM_LOG_REVERTED_SWAPS (per-hop actual-vs-predicted on revert)
#   DEGENBOT_SIM_EXIT_ON_FAIL=1  (stop on first sim failure) + IGNORE_BUCKETS
#     default "" (empty allowlist = trap on EVERY bucket, sys.exit(3))
#   DEGENBOT_WS_COMPLETENESS    (per-block eth_getLogs vs WS delivery cross-
#     check; NEW default-ON since B4GX7C, so a live WS log drop aborts loudly)
# Per-target/high-noise (still OFF): DEGENBOT_DRAIN_DBG, DEGENBOT_TRACE_REGISTER_SEED
#
# To run a long-lived soak that trades through thin-margin/no-profit reverts
# (the routine arb-filter outcome), override the fail-fast:
#   DEGENBOT_SIM_EXIT_ON_FAIL=0 ./run_bot.sh
# --------------------------------------------------------------------------

# --------------------------------------------------------------------------
# Debug-visible instrumented runs (default ON here).
#
# The Rust core's granular diagnostics ([solver-dbg],
# [solver-st], [v2-calc-trace], ...) are gated behind the `debug` tracing
# level AND the Python-side DEBUG logger (see docs/logging.md). Set BOTH below
# so those lines reach logs/bot_run.log in these instrumented runs:
#
#   * RUST_LOG        is the Rust `tracing` EnvFilter gate. Raising the
#                     degenbot_* crates to `debug` makes the fine-grained
#                     diagnostics visible while `info` keeps routine status
#                     lines; the alloy/tungstenite targets are held at `warn`
#                     to mirror the code's built-in default and stop their
#                     lifecycle INFO noise from flooding the log.
#   * DEGENBOT_DEBUG=1 is the Python `logging` gate. Without it the crate-root
#                     Python loggers stay at INFO and drop the debug records
#                     before they reach the log/bot_run.log tee.
#
# Both respect a pre-set value: set RUST_LOG / DEGENBOT_DEBUG yourself to opt
# out (e.g. RUST_LOG=warn ./run_bot.sh) or tighten the scope.
# --------------------------------------------------------------------------
DEFAULT_RUST_LOG="info,degenbot_bot=debug,degenbot_arbitrage=debug,degenbot_simulation=debug,degenbot_solvers=debug,alloy_pubsub=warn,alloy_transport=warn,alloy_transport_ws=warn,alloy_transport_ipc=warn,alloy_transport_http=warn,alloy_provider=warn,alloy_rpc=warn,alloy_network=warn,alloy_contract=warn,tungstenite=warn"
export RUST_LOG="${RUST_LOG:-$DEFAULT_RUST_LOG}"
export DEGENBOT_DEBUG="${DEGENBOT_DEBUG:-1}"
export DEGENBOT_OTEL="${DEGENBOT_OTEL:-1}"

# The actual bot invocation (uv rebuilds the Rust extension if any rust
# source / Cargo.toml is newer than the installed build).
BOT_CMD=(uv run python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py)

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
    # (setsid'd) uv pid we record. Output is captured by direct fd redirection
    # (never a tee pipeline), so the log is authoritative and survives the
    # launching shell going away.
    setsid "${BOT_CMD[@]}" >>"$LOG" 2>&1 < /dev/null &
    echo $! > "$PIDFILE"
    # Runner diagnostics go to the log too, so the whole launch is in one place.
    echo "[runner] started bot pid $(cat "$PIDFILE") $(date -Is)" | tee -a "$LOG" >&2
}

stop() {
    if [ -f "$PIDFILE" ]; then
        kill -TERM "$(cat "$PIDFILE")" 2>/dev/null
        sleep 1
        kill -9 "$(cat "$PIDFILE")" 2>/dev/null
        rm -f "$PIDFILE"
    fi
    # The uv wrapper may exit while its python child lingers; kill by name too.
    pkill -9 -f eth_settlement_arbitrage_v2_v3_v4 2>/dev/null
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

foreground() {
    if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        echo "[runner] bot already running (pid $(cat "$PIDFILE")) — stop it first" >&2
        return 1
    fi
    # Fresh truncation, same as `start`, so the log always reflects this run
    # (the append-only `tee -a` behaviour is gone for determinism).
    : > "$LOG"
    : > "$PIDFILE"
    echo "[runner] starting bot $(date -Is)" | tee -a "$LOG" >&2
    # Bot output goes to the log by direct fd redirection — authoritative and
    # immune to a closing console (no `tee` pipeline to SIGPIPE and drop the
    # tail). The console is only a live mirror fed by `tail -f`.
    "${BOT_CMD[@]}" >>"$LOG" 2>&1 < /dev/null &
    BOTPID=$!
    echo "$BOTPID" > "$PIDFILE"
    tail -f -n +1 "$LOG" &
    TAILPID=$!
    # Forward Ctrl-C / TERM to the bot so it stops cleanly (the pump then gets
    # its exit path rather than being killed out from under the lock).
    trap 'kill -TERM "$BOTPID" 2>/dev/null' INT TERM
    wait "$BOTPID"
    BOTRC=$?
    kill "$TAILPID" 2>/dev/null
    wait "$TAILPID" 2>/dev/null
    rm -f "$PIDFILE"
    trap - INT TERM
    echo "[runner] bot exited rc=$BOTRC $(date -Is)" | tee -a "$LOG" >&2
    return "$BOTRC"
}

case "${1:-foreground}" in
    start) start ;;
    stop) stop ;;
    status) status ;;
    foreground) foreground ;;
    *)
        echo "usage: $0 {start|stop|status|foreground}" >&2
        exit 1
        ;;
esac
