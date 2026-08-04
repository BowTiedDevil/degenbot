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

# On a sim failure, dump the FULL nested revm call trace (every frame:
# depth/target/selector/outcome) so a depth-8 empty-calldata PoolManager Halt
# can be attributed against the executor Vyper source (2LTKVO / W2UWZO).
export DEGENBOT_DUMP_CALL_TRACE=1
# Log each V2 hop's reserves slot8 as read from the shared per-block CacheDB
# right before that path's sim — the ground truth for what the executor's
# Vyper `_v2_get_amount_out` (V2_SWAP_CALC) SLOADs (path-11354 1-wei probe).
export DEGENBOT_V2_CALC_TRACE=1

# Option-A solver-state accuracy gate (AV42C7): run at the PUBLISH point —
# when a quiesce-gated (block-final/coalesced) result is about to be sent to
# Python for simulation — diff each path's per-hop pool state against the
# chain at its own anchor `update_block` and STOP THE PUMP CLEANLY on a genuine
# desync (sets shutdown; was a wedging panic pre-AV42C7-fix). It
# does NOT run after every transient on_drain solve: mid-block stale results
# that the eager design discards (re-solved at block completion) never trip
# it, while a desync on a result that SURVIVES to publication does. See
# solver_state_verifier.rs. The monotonic-update_block fix (backfill drain no
# longer rewinds a head-fresh pool's metadata) removes the 160-218-block
# stale false alarms. Env-gated:
export DEGENBOT_ASSERT_SOLVER_STATE=1

# Per-pool swap-arrival / liquidity-route diagnostics. Set to a V3 pool
# address (0x…) or a V4 pool_id hex to emit a `[trace] swap-apply` line for
# every Swap dispatched for that pool — with its on-chain sqrtPriceX96,
# liquidity, tick, and block. This proves whether a within-tick swap that
# should have advanced the solver's sqrtPrice actually ARRIVED and was
# applied (vs. never delivered). Default empty (off).
#   DEGENBOT_DRAIN_DBG=0xaCDb27b266142223e1e676841C1E809255Fc6d07 ./run_bot.sh
# To trace ALL liquidity routing instead: DEGENBOT_TRACE_LIQUIDITY_GLOBAL=1
export DEGENBOT_DRAIN_DBG="${DEGENBOT_DRAIN_DBG:-}"

# [diag] registration-seed probe: log every V3 pool's seed (update_block +
# sqrtPriceX96 + tick) at register_v3_pool time, so a solver-state mismatch
# can be traced to its seed — head-fresh means a post-registration rewind;
# historical means the seed source is stale. Default off.
export DEGENBOT_TRACE_REGISTER_SEED="${DEGENBOT_TRACE_REGISTER_SEED:-}"

# Structural verify diagnostics (`[verify-dbg] V3/V4 pin`, pump block
# complete, set_live tail): emit the pinned (update_block, tick_data.len(),
# pump_count_at_or_below, last_complete_block) at registration-drain time so a
# step-2 post-drain verify mismatch can be correlated to the drain that
# produced the pin (H1: seed update_block=head outruns the DB-seeded tick_data
# block with pump_count_at_or_below==0). Pure logging; zero behavior change.
export DEGENBOT_VERIFY_DBG=1

# Fail-fast default (DEGENBOT_SIM_EXIT_ON_FAIL=1): stop LOUDLY on the first
# sim failure so a real failure is caught immediately with a [sim-fixture]
# dump instead of being silently traded-through. With =1 the bot stops on the
# FIRST thin-margin / no-profit revert — a routine arb-filtering outcome (all
# executor frames `ok`, `revert=0x`, `reverted=0`) — so it does not sustain
# long operation; for a sustained trade-through soak run it with
# `DEGENBOT_SIM_EXIT_ON_FAIL=0 ./run_bot.sh`.
#
# Known crash classes in the ignore-bucket set (default `CurrencyNotSettled`)
# are logged with per-block `[sim-revert-swap]` samples and continued past
# rather than trapped, so a single run still gathers per-block data for the
# tracked class. The solver-state tripwire (DEGENBOT_ASSERT_SOLVER_STATE=1)
# stays armed independently and stops the pump on a genuine desync.
export DEGENBOT_SIM_EXIT_ON_FAIL="${DEGENBOT_SIM_EXIT_ON_FAIL:-1}"

# Trap on EVERY bucket: empty the ignore-bucket allowlist so the FIRST sim
# failure of any class (including the known CurrencyNotSettled/IIA solver-
# math class) dumps a [sim-fixture] and sys.exit(3). No bucket is traded
# through. Set to a comma-sep bucket list to re-enable ignore-behavior.
export DEGENBOT_SIM_EXIT_IGNORE_BUCKETS=""

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
