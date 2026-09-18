#!/usr/bin/env bash
# Drive the settlement-arbitrage bot with all output teed to a log file.
#
# A self-contained launcher so the bot can be started/stopped deterministically
# without rediscovering the launch mechanics each time.
#
# One launcher, two drivers (RSP-16 / ergo V6SUQO):
#
#   ./run_bot.sh [--python|--rust] [--strategy settlement|backrun] [start|stop|status|foreground|print-cmd] [-- args...]
#
#   --python (default)    the Python driver over the PyO3-bound Rust core —
#                         legacy command/env, byte-identical
#   --rust                the pure-Rust parity driver
#                         (rust/examples/settlement_bot), built on demand
#
#   --strategy NAME       strategy arm selector (settlement|backrun). Omitted
#                         means settlement: the launcher then behaves exactly as
#                         before and exports nothing. `settlement` exports the
#                         typed selector key DEGENBOT_STRATEGY_NAME (inert until
#                         the arm readers migrate onto it, ADR-055). `backrun`
#                         has no runner in this launcher — backrun runs via the
#                         sidecar — so it refuses with a pointer instead of
#                         pretending. Ignored by stop/status.
#
#   ./run_bot.sh            # foreground (output -> console + log)
#   ./run_bot.sh start      # detached (setsid), pid -> logs/bot_run.pid
#   ./run_bot.sh stop       # kill the running bot (by pidfile + pkill; both drivers)
#   ./run_bot.sh status     # is it running? (both drivers)
#   ./run_bot.sh print-cmd  # resolved driver/command/exports; no build, no launch
#
# `--` ends launcher parsing: every following token is appended verbatim to the
# driver argv (e.g. `./run_bot.sh --rust start -- --live --permutation V2-V3-V4`).
# The launcher NEVER implies --live.
set -u
cd /workspaces/degenbot
LOGDIR=/workspaces/degenbot/logs
LOG="$LOGDIR/bot_run.log"
PIDFILE="$LOGDIR/bot_run.pid"
mkdir -p "$LOGDIR"

# --------------------------------------------------------------------------
# POST-LW-T9 CUTOVER (epic XR62VX, commit f3750093d): the fleet executor is
# the ONLY stance now — the DEGENBOT_FLEET stance key itself is RETIRED and
# fails the config load loudly if exported; registration/sim/solve host on
# the ADR-042 fleet unconditionally. (This launcher used to export
# DEGENBOT_FLEET=fleet — do NOT restore it.)
#
# Conservative (HARD/LOUD) defaults now live in the CODE, not here (Z4KQXF).
# Every invocation — run_bot.sh, a hand-run, a CI/harness — gets loud failure
# by default; there is no liberal default posture anymore. Flags that follow
# are all default-ON in code via `bot_env_flag_default_on` (opt OUT with =0):
#   (RETIRED: DEGENBOT_ASSERT_SOLVER_STATE — the ADR-021 per-solve solver-state
#     tripwire is GONE with MROOY7 task 2UVG3E and the key no longer exists in
#     the config schema (docs/rust-config-keys.md). Standing verification is
#     on-demand: sim failures arm the sim-divergence probe unconditionally, and
#     DEGENBOT_VERIFY_SPOTCHECK_PERMYRIAD adds random ops spot-checks. The
#     verify-dbg / V2-calc / reverted-swap diagnostics are now always-on DEBUG
#     events on the sim/state OTel domains, gated only by the sink filter.)
#   DEGENBOT_DUMP_CALL_TRACE     (full revm call trace on sim failure)
#   DEGENBOT_SIM_EXIT_ON_FAIL  (stop on first sim failure) - see below: this
#     script DEFAULTS it to 0; failing sims are identified via OTel traces.
#   DEGENBOT_WS_COMPLETENESS    (per-block eth_getLogs vs WS delivery cross-
#     check; NEW default-ON since B4GX7C, so a live WS log drop aborts loudly)
# High-noise per-event traces (WS delivery, drain, apply-route, swap-apply,
# register-seed) are now always-on DEBUG events on the ingest/pump/state/path
# domains. They do not reach the console unless the Rust log level enables
# `degenbot=debug` / `degenbot=trace`; see docs/logging.md.
# Forensic full-field dumps (sim call traces, seed/verifier tick maps) are
# TRACE-level events on the sim/state/verify domains; enable with
# `degenbot=trace` in RUST_LOG for an investigation (high volume).
#
# Sim-failure policy: this script DEFAULTS DEGENBOT_SIM_EXIT_ON_FAIL=0 (keep
# running through thin-margin/no-profit reverts - the routine arb-filter
# outcome). Failing simulations are identified from OTel traces/metrics going
# forward, not by killing the bot. Override to 1 to restore the old fail-fast.
# --------------------------------------------------------------------------

# --------------------------------------------------------------------------
# INFO-visible runs (default; log-volume cut OPBD7L).
#
# The default posture prints INFO status lines ([sim], [bundle]-summaries,
# pump/block lifecycle) and WARN/ERROR only — no debug/trace diagnostics.
# The Rust core's granular diagnostics ([solver-dbg], [solver-st],
# [v2-calc-trace], [path] registered, [bundle] inline payload settle) stay in
# the code but are gated behind the `debug` tracing level AND the Python-side
# DEBUG logger (see docs/logging.md). To run an instrumented run, set BOTH:
#
#   * RUST_LOG         is the Rust `tracing` EnvFilter gate; raise degenbot_*
#                      crates to `debug` (docs/logging.md has the recipes).
#   * DEGENBOT_DEBUG=1 is the Python `logging` gate for forwarded records.
#
# Duplication control (ADR-043 §6 / docs/logging.md): exactly ONE console
# writer per process, derived from binding-present. In the Python driver the
# Rust→Python bridge owns the console and the Rust stderr `fmt` layer is
# routed to a sink, so every record reaches bot_run.log exactly once (the
# retired DEGENBOT_LOG_FMT two-tunnel switch is gone and warns at boot).
# The console filter comes from the typed telemetry config (Python-driver
# default `info`, plus the alloy noise throttles) — no RUST_LOG needed here;
# set RUST_LOG yourself for a one-off (`RUST_LOG=warn ./run_bot.sh` wins
# verbatim on every sink and turns the config log knobs off).
# All values respect a pre-set environment.
# --------------------------------------------------------------------------
export DEGENBOT_DEBUG="${DEGENBOT_DEBUG:-0}"
export DEGENBOT_OTEL="${DEGENBOT_OTEL:-1}"
# SIMPIPE2 T4 soak arm: the ENGINE-side inline sim (worker seam). Default 0
# (legacy option-A FFI pipeline); the soak flips 0/1 across equal windows.
export DEGENBOT_SOLVE_INLINE_SIM="${DEGENBOT_SOLVE_INLINE_SIM:-1}"
export DEGENBOT_SIM_EXIT_ON_FAIL="${DEGENBOT_SIM_EXIT_ON_FAIL:-0}"
# Publish-debounce window (ms), last dirty log -> settle decision. A/B'd on
# 2026-09-04 (telemetry-latency-playbook S7): bursts complete in 1.3-27.5 ms
# while the 50 ms code default settled full-length on ~every block — a fixed
# settle tax. 15 ms cuts ~33 ms/block with no extra solve cycles observed.
# Code default stays 50 ms; invalid/zero env values fall back to 50 ms.
export DEGENBOT_PUMP_DEBOUNCE_MS="${DEGENBOT_PUMP_DEBOUNCE_MS:-15}"
# Registration crawl hosting (PRG-5 hard cutover, epic IRUMXD): the crawl is
# FLEET-HOSTED ONLY, and since LW-T9 (epic XR62VX) the stance key is retired:
# DEGENBOT_FLEET / fleet.stance in a config FAIL the config load loudly if
# set — the fleet is unconditional. Alongside it, DEGENBOT_REG_QUEUE_BOUND /
# DEGENBOT_REG_WORKERS (crawl shell) and DEGENBOT_SOLVE_SIM_INFLIGHT
# (solve.solve_sim_inflight, sim-slots shadow cap) all fail the load loudly
# too; survivals are fleet.pool_state_updater_slots / fleet.sim_slot_cap and
# solve.inline_sim_workers.
# Typed-config parity (KAHU5W): every DEGENBOT_* env above still works
# (12-factor parity) but each key also has a typed TOML path — these exports
# map to telemetry.otel, solve.solve_inline_sim, simulation.sim_exit_on_fail,
# trace.ws_trace, and pump.pump_debounce_ms in config.toml; the full key
# table lives in docs/rust-config-keys.md. Observability note (MROOY7): the
# retired pump/queue surface (spans degenbot.pump.block / pump.log_wait /
# pump.apply_stream, series degenbot_drain_queue_depth) is succeeded by the
# stage telemetry — spans degenbot.epoch.run + degenbot.stage.{streaming,quiesced,
# publish,finalize,rewind}, series degenbot_stage_publish_cycle_seconds /
# degenbot_stage_rewind_total / _duration_seconds; the metrics endpoint is
# DEGENBOT_METRICS_ADDR (default 127.0.0.1:9464).
# Solver-state verification policy: ON-DEMAND ONLY (the ADR-021 publish
# tripwire and its DEGENBOT_ASSERT_SOLVER_STATE knob are RETIRED — MROOY7
# task 2UVG3E: the overnight scan measured 29k tripwire WARNs and tens-of-
# seconds verify spans with zero caught desyncs in 6.5h, and the knob is no
# longer in the config schema, so exporting it here would be a dead knob).
# Standing verification: sim failures arm a divergence probe on the failing
# path's next sim (the sim-divergence probe merges the engine-vs-RPC
# divergence logs), DEGENBOT_VERIFY_SPOTCHECK_PERMYRIAD adds random spot-
# checks for operators, and DEGENBOT_WS_COMPLETENESS (default ON) aborts
# loudly on a dropped WS log before state can drift. Desync containment runs
# through the resolve-seam quarantine gate (watch the
# degenbot_engine_quarantined_pools gauge / DegenbotDesyncQuarantine alert).

# Two-runtime contract (7LV6VN T5): solve bins, rayon resolve, sim runtime,
# and the sim-driver cap all derive from the detected cgroup budget inside
# the Rust core (cpu_budget::leftover_worker_budget), leaving the I/O
# headroom to the ambient runtime by construction. An operator export of
# DEGENBOT_SOLVE_CPUS / DEGENBOT_INLINE_SIM_WORKERS / DEGENBOT_FLEET_SIM_SLOT_CAP
# still wins when set explicitly - none are pre-set here.
# (DEGENBOT_SOLVE_SIM_INFLIGHT is retired — fails the load loudly, LW-T9.)

# --------------------------------------------------------------------------
# Driver selection (RSP-16 / ergo V6SUQO): one launcher, two drivers.
#
#   --python  runs examples/eth_settlement_arbitrage_v2_v3_v4_rust.py over the
#             PyO3-bound Rust core — the legacy command/env, byte-identical,
#             and the default when no driver flag is given.
#   --rust    runs the standalone `cargo add degenbot` consumer
#             (rust/examples/settlement_bot, package
#             `degenbot-settlement-bot-example`), built on demand.
#
# RUST_PROFILE selects the cargo profile for --rust: `release` (default — the
# same profile the Python driver's installed .so is built with; a COLD release
# build takes minutes) or `dev` (the debug profile, for fast iteration).
# --------------------------------------------------------------------------
RUST_PROFILE="${RUST_PROFILE:-release}"
case "$RUST_PROFILE" in
    release) RUST_PROFILE_DIR=release; RUST_BUILD_FLAGS=(--release) ;;
    dev)     RUST_PROFILE_DIR=debug;   RUST_BUILD_FLAGS=() ;;
    *)
        echo "error: unknown RUST_PROFILE '$RUST_PROFILE' (expected release|dev)" >&2
        exit 2
        ;;
esac
RUST_PKG=degenbot-settlement-bot-example
RUST_BIN="rust/target/$RUST_PROFILE_DIR/$RUST_PKG"

# The Python driver's byte-identical legacy invocation (uv rebuilds the Rust
# extension if any rust source / Cargo.toml is newer than the installed build).
PY_BOT_CMD=(uv run python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py)

# Process-name patterns `stop`/`status` match in addition to the pidfile — one
# per driver (see stop/status).
DRIVER_NAME_PATTERNS=(eth_settlement_arbitrage_v2_v3_v4 degenbot-settlement-bot-example)

DRIVER=python
DRIVER_SET=0
ACTION=""
STRATEGY=""
STRATEGY_SET=0
PASSTHROUGH=()

usage() {
    echo "usage: $0 [--python|--rust] [--strategy settlement|backrun] {start|stop|status|foreground|print-cmd} [-- extra bot args]" >&2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --python|--rust)
            if [ "$DRIVER_SET" = 1 ]; then
                echo "error: driver flags are mutually exclusive (got '$1' after '--$DRIVER')" >&2
                usage
                exit 2
            fi
            DRIVER="${1#--}"
            DRIVER_SET=1
            ;;
        --strategy)
            if [ "$STRATEGY_SET" = 1 ]; then
                echo "error: --strategy given more than once" >&2
                usage
                exit 2
            fi
            if [ $# -lt 2 ]; then
                echo "error: --strategy requires an argument (settlement|backrun)" >&2
                usage
                exit 2
            fi
            case "$2" in
                settlement|backrun) STRATEGY="$2" ;;
                *)
                    echo "error: unknown --strategy '$2' (expected settlement|backrun)" >&2
                    usage
                    exit 2
                    ;;
            esac
            STRATEGY_SET=1
            # Consume the value; the loop's trailing shift consumes the flag.
            shift
            ;;
        --)
            # Everything after `--` is the driver's own argv, verbatim. The
            # launcher NEVER implies --live; pass it here explicitly.
            shift
            PASSTHROUGH=("$@")
            break
            ;;
        start|stop|status|foreground|print-cmd)
            ACTION="$1"
            ;;
        -*)
            echo "error: unknown driver flag '$1'" >&2
            usage
            exit 2
            ;;
        *)
            usage
            exit 1
            ;;
    esac
    shift
done

# Strategy selector (ADR-055): additive and inert by default. An explicit
# `settlement` exports the typed selector key (a valid enum value the config
# loader accepts; no arm reader consumes it yet). `backrun` has no in-launcher
# runner, so it refuses with a pointer rather than pretending; stop/status
# ignore the selector entirely so a running bot stays stoppable.
case "${ACTION:-foreground}" in
    start|foreground|print-cmd)
        case "$STRATEGY" in
            "") : ;;
            settlement) export DEGENBOT_STRATEGY_NAME=settlement ;;
            backrun)
                echo "error: --strategy backrun has no runner in this launcher; backrun runs via the sidecar (rust/crates/degenbot-submission/src/bin/backrun_sidecar.rs) — see docs/sidecar_runbook.md" >&2
                exit 2
                ;;
        esac
        ;;
esac

if [ "$DRIVER" = rust ]; then
    BOT_CMD=("$RUST_BIN" "${PASSTHROUGH[@]}")
else
    BOT_CMD=("${PY_BOT_CMD[@]}" "${PASSTHROUGH[@]}")
fi

# A thin bash supervisor becomes the detached session leader so the driver's
# REAL pid is what lands in the pidfile (stop is a direct TERM to the driver)
# and its exit status is recorded in the log — `setsid <driver>` alone would
# leave no parent to report rc. $1 = log path, $2 = pidfile, rest = driver argv.
DETACH_WRAPPER='
log="$1"; pidfile="$2"; shift 2
"$@" >>"$log" 2>&1 </dev/null &
child=$!
printf "%s\n" "$child" > "$pidfile"
wait "$child"
rc=$?
printf "[runner] bot exited rc=%s %s\n" "$rc" "$(date -Is)" >>"$log"
'

# Print the EFFECTIVE chain-1 RPC URIs — exactly what the bot's cascade
# (degenbot.config.resolve_rpc_uris: CLI > OS env > config.toml) will resolve —
# into console + log BEFORE the first provider call. A later-applied shell
# export can shadow the devcontainer containerEnv (the resolver reads
# os.environ), and this surfaces such a stomp immediately instead of as a
# connection-refused chain-ID failure (2026-09-10 incident).
resolve_rpc_line() {
    uv run python -c 'from degenbot.config import resolve_rpc_uris as _r; _h, _w = _r(1); print(f"[runner] resolved rpc http={_h} ws={_w}")' 2>/dev/null || true
}

# Log the resolved cascade line (console + log), remembering the ws URI.
RESOLVED_WS=""
log_resolved_rpcs() {
    local line
    line="$(resolve_rpc_line)"
    [ -n "$line" ] || return 0
    printf '%s\n' "$line" | tee -a "$LOG" >&2
    RESOLVED_WS="${line##*ws=}"
}

# The pure-Rust driver's live arm is gated on SMOKE_RPC_URL (the analogue of
# the Python driver's own cascade read); bind the ws URI the SAME Python cascade
# resolves so both drivers arm the same endpoint. An empty/absent ws leaves
# SMOKE_RPC_URL unset — the example then stops after the offline parity-ledger
# print (its CI-safe posture). Never implies --live.
arm_smoke_rpc() {
    [ "$DRIVER" = rust ] || return 0
    if [ -z "$RESOLVED_WS" ]; then
        echo "[runner] warning: no resolved ws URI — SMOKE_RPC_URL unset, rust driver stays offline" | tee -a "$LOG" >&2
        return 0
    fi
    export SMOKE_RPC_URL="$RESOLVED_WS"
}

# Build-on-demand for --rust: a deliberately cheap mtime probe (binary missing,
# or any rust source/manifest newer). Cargo owns the real dependency graph and
# is incremental, so over-triggering costs only a fast no-op cargo run.
rust_binary_stale() {
    if [ -x "$RUST_BIN" ] \
        && [ -z "$(find rust/Cargo.toml rust/Cargo.lock rust/crates rust/examples -type f -newer "$RUST_BIN" -print -quit 2>/dev/null)" ]; then
        return 1
    fi
    return 0
}

ensure_rust_binary() {
    [ "$DRIVER" = rust ] || return 0
    rust_binary_stale || return 0
    echo "[runner] building $RUST_PKG ($RUST_PROFILE profile — a cold release build takes minutes)" | tee -a "$LOG" >&2
    if ! ( cd rust && cargo build -p "$RUST_PKG" "${RUST_BUILD_FLAGS[@]}" ) >>"$LOG" 2>&1; then
        echo "[runner] cargo build failed — see $LOG" >&2
        return 1
    fi
}

start() {
    if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        echo "[runner] bot already running (pid $(cat "$PIDFILE"))" >&2
        return 1
    fi
    : > "$LOG"
    : > "$PIDFILE"
    ensure_rust_binary || return 1
    log_resolved_rpcs
    arm_smoke_rpc
    # setsid: new session + no controlling terminal, so the launching shell can
    # exit without the pump dying (SIGHUP) and the tool shell's return isn't
    # entangled with the bot's life. The supervisor records the driver's own pid
    # and its exit status (see DETACH_WRAPPER). Output is captured by direct fd
    # redirection (never a tee pipeline), so the log is authoritative and
    # survives the launching shell going away.
    local wrapper_pid _i
    setsid bash -c "$DETACH_WRAPPER" runner "$LOG" "$PIDFILE" "${BOT_CMD[@]}" < /dev/null &
    wrapper_pid=$!
    # The supervisor writes the driver pid as soon as it forks it; fall back to
    # the supervisor pid if that never happened (driver failed to fork).
    for _i in 1 2 3 4 5 6 7 8 9 10; do
        [ -s "$PIDFILE" ] && break
        sleep 0.1
    done
    [ -s "$PIDFILE" ] || echo "$wrapper_pid" > "$PIDFILE"
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
    # The supervisor (or the uv wrapper) may exit while its child lingers; kill
    # by name too, covering BOTH drivers.
    pkill -9 -f eth_settlement_arbitrage_v2_v3_v4 2>/dev/null
    pkill -9 -f degenbot-settlement-bot-example 2>/dev/null
    echo "[runner] stopped $(date -Is)"
}

status() {
    if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        echo "[runner] running pid $(cat "$PIDFILE")"
        ps -o pid,etime,cmd -p "$(cat "$PIDFILE")" 2>/dev/null | tail -1
        return 0
    fi
    # A stale/empty pidfile doesn't mean no driver is live — match BOTH driver
    # process names (the Python example script and the Rust binary).
    local name pids
    pids=""
    for name in "${DRIVER_NAME_PATTERNS[@]}"; do
        pids="$pids $(pgrep -f "$name" 2>/dev/null | tr '\n' ' ')"
    done
    pids="${pids// /}"
    if [ -n "$pids" ]; then
        echo "[runner] running pid $pids"
        return 0
    fi
    echo "[runner] not running"
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
    ensure_rust_binary || return 1
    echo "[runner] starting bot $(date -Is)" | tee -a "$LOG" >&2
    log_resolved_rpcs
    arm_smoke_rpc
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

# CI-verifiable surface: print the resolved driver, the full command array
# (passthrough included), the effective profile, and every export — then exit 0
# without building or launching.
print_cmd() {
    local line ws
    echo "[runner] driver=$DRIVER"
    echo "[runner] strategy=${STRATEGY:-settlement(default; no export)}"
    if [ "$STRATEGY" = settlement ]; then
        echo "[runner] export DEGENBOT_STRATEGY_NAME=settlement"
    fi
    if [ "$DRIVER" = rust ]; then
        echo "[runner] rust-profile=$RUST_PROFILE"
        echo "[runner] rust-binary=$RUST_BIN"
    fi
    echo "[runner] bot-cmd: $(printf '%q ' "${BOT_CMD[@]}")"
    echo "[runner] export DEGENBOT_DEBUG=$DEGENBOT_DEBUG"
    echo "[runner] export DEGENBOT_OTEL=$DEGENBOT_OTEL"
    echo "[runner] export DEGENBOT_SOLVE_INLINE_SIM=$DEGENBOT_SOLVE_INLINE_SIM"
    echo "[runner] export DEGENBOT_SIM_EXIT_ON_FAIL=$DEGENBOT_SIM_EXIT_ON_FAIL"
    echo "[runner] export DEGENBOT_PUMP_DEBOUNCE_MS=$DEGENBOT_PUMP_DEBOUNCE_MS"
    if [ "$DRIVER" = rust ]; then
        line="$(resolve_rpc_line)"
        ws="${line##*ws=}"
        if [ -n "$ws" ]; then
            echo "[runner] export SMOKE_RPC_URL=$ws"
        else
            echo "[runner] export SMOKE_RPC_URL=<unset: no resolved ws URI — rust driver stays offline>"
        fi
    fi
}

case "${ACTION:-foreground}" in
    start) start ;;
    stop) stop ;;
    status) status ;;
    foreground) foreground ;;
    print-cmd) print_cmd ;;
    *)
        usage
        exit 2
        ;;
esac
