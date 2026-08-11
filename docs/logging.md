# Controlling Tracing / Logging

How to control what the bot writes to stderr / `logs/bot_run.log`, the level at
which each message appears, and how to dial a noisy diagnostic up or down.

There are two independent knobs that both must allow a record before you see it:

1. **`RUST_LOG`** — the Rust `tracing` level filter (a `EnvFilter`). Gates the
   **Rust core's** `tracing::info!` / `tracing::debug!` / `tracing::warn!` /
   `tracing::error!` / `tracing::trace!` events *before* they reach either
   sink.
2. **`DEGENBOT_DEBUG`** — the **Python `logging`** level (INFO by default,
   DEBUG when set). Gates Python-side records and Rust records that *survive*
   step 1 once they are forwarded into Python `logging`.

So a Rust `debug!` line requires **both** `RUST_LOG` to include that target at
`debug` (or above) **and** `DEGENBOT_DEBUG=1` (for the Python-forwarded copy).
The plain `[debug-...]` lines you see on stderr with no timestamp prefix are the
`fmt`-layer copy, which is controlled by `RUST_LOG` alone.

---

## The two tunnels (how a Rust record reaches you)

When the `degenbot_rs` Python module initializes
(`init_logging_subscriber` in `crates/degenbot-python/src/python_log_layer.rs`),
it installs one `tracing_subscriber::Registry` with two layers sharing a
single `EnvFilter`:

- **`fmt` layer → stderr** — ANSI-colored, `[timestamp] LEVEL target: msg`.
- **`PythonLogLayer` → Python `logging`** — batches records (lock-free queue,
  256/batch or 50 ms flush) and forwards them to the Python logger named after
  the Rust target with `::` → `.` (e.g. `degenbot_bot.bot_core.block_pump`).
  This is why **every Rust line appears twice** in a teed log: once as the
  `fmt` stderr line (with timestamp/level) and once as the plain
  `[prefix] ...` Python copy.

The `EnvFilter` is:

- `RUST_LOG` if that env var is set;
- otherwise a default of `info` globally, with a handful of third-party
  `alloy_*` / `tungstenite` targets throttled to `warn` (they emit routine
  lifecycle INFO noise that is not degenbot-originated).

Python `logging` base config lives in `src/degenbot/logging.py`. It wires the
crate-root loggers (`degenbot_bot`, `degenbot_core`, `degenbot_rs`,
`degenbot_rpc`, `degenbot_decoders`, `degenbot_uniswap`,
`degenbot_simulation`, `degenbot_backrun_strategy`) to a stdout
`QueueHandler`/`QueueListener` pair. Rust records are forwarded to these
loggers, so the Python side is the **second gate**.

---

## Quick reference

| I want to … | Set this |
|---|---|
| Raise the Rust core to `debug` everywhere | `RUST_LOG=debug` |
| Raise the Rust core to `debug` **and** let Python loggers pass `debug` | `RUST_LOG=debug DEGENBOT_DEBUG=1` |
| Keep `info` default but see the fine-grained solver/sim diagnostics | `RUST_LOG=info,degenbot_bot=debug,degenbot_backrun_strategy=debug` |
| Silence third-party `alloy`/`tungstenite` entirely | `RUST_LOG=alloy=off,tungstenite=off` (or rely on the built-in `=warn` default) |
| Get just `warn`/`error` (quietest useful run) | `RUST_LOG=warn` |
| Disable ALL Rust core logs from the console | `RUST_LOG=off` |

`RUST_LOG` directives are comma-separated `target=level` pairs, applied
most-specific-first. `level` is one of `trace`, `debug`, `info`, `warn`,
`error`, `off`. A bare `RUST_LOG=info` sets the global default; add
`target=debug` overrides per crate / module. See
[`tracing-subscriber`'s EnvFilter docs](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html).

---

## Debug-named diagnostics gated at `debug`

The following noisy diagnostics are emitted at `debug` level, so they are
**invisible under the default `info` filter**. Re-enable them by raising the
relevant crate to `debug` (`RUST_LOG=degenbot_bot=debug,degenbot_backrun_strategy=debug`).
They were historically `info`/`warn` and flooded `bot_run.log` — that spam is
why they were demoted. They are intentionally **kept in the code**, just gated.

| prefix | crate target | what it does |
|---|---|---|
| `[debug-v4-solve]` | `degenbot_bot` | per-V4-hop solver intermediates (tick, liquidity, sqrt price, …) |
| `[solver-dbg]` | `degenbot_bot` | solve-entry debug trace (`rebuild_and_solve_affected`, `solve_all`) |
| `[solver-st]` | `degenbot_bot` | per-path solver pool-state dump for cross-referencing against sim |
| `[v2-calc-trace]` | `degenbot_backrun_strategy` | V2 reserves slot-8 read immediately before each path sim |

These are all "leftover debug tracing" — useful when investigating a specific
mismatch (e.g. the path-11354 / path-142603 fixes), but far too loud to print
every block by default. To debug one of them, run with the crate set to
`debug`:

```bash
RUST_LOG=info,degenbot_bot=debug,degenbot_backrun_strategy=debug DEGENBOT_DEBUG=1 \
    uv run python examples/eth_backrun_v2_v3_v4_rust.py
```

or, for just the V2 calc trace:

```bash
RUST_LOG=info,degenbot_backrun_strategy=debug DEGENBOT_DEBUG=1 \
    uv run python examples/eth_backrun_v2_v3_v4_rust.py
```

---

## Env-gated hard/loud diagnostics (independent of level)

Several diagnostics are additionally gated by **dedicated env flags** that work
*whatever* `RUST_LOG` level is active. Most are **default-ON** (conservative /
loud) and are disabled by setting the flag to a falsey value (`0`, `false`,
`off`, `no`, `""`). `DEGENBOT_DEBUG` does not affect them — they are checked in
code, not by the tracing filter.

| env var | default | gates |
|---|---|---|
| `DEGENBOT_ASSERT_SOLVER_STATE` | ON | ADR-021 publish tripwire — aborts the process on a verified solver-state desync |
| `DEGENBOT_VERIFY_DBG` | ON | structural verify diagnostics / divergence set |
| `DEGENBOT_DUMP_CALL_TRACE` | ON | full revm call trace on a sim failure |
| `DEGENBOT_V2_CALC_TRACE` | ON | V2 reserves slot-8 read before each sim (see `[v2-calc-trace]` above) |
| `DEGENBOT_DEBUG_V4_SOLVE` | ON | V4 solver intermediates (see `[debug-v4-solve]` above) |
| `DEGENBOT_SIM_LOG_REVERTED_SWAPS` | ON | per-hop actual-vs-predicted on revert |
| `DEGENBOT_SIM_EXIT_ON_FAIL` | 1 | stop on first sim failure (`=0` for a soak run) |
| `DEGENBOT_WS_COMPLETENESS` | ON | per-block `eth_getLogs` vs WS delivery cross-check (aborts loudly on a live WS drop) |
| `DEGENBOT_DRAIN_DBG` | **OFF** | per-event debug-drain log for a specific pool address (opt-in) |
| `DEGENBOT_TRACE_REGISTER_SEED` | **OFF** | registration-seed trace (opt-in) |

`run_bot.sh` documents this set in its header. To run a long-lived soak that
trades through the routine thin-margin/no-profit reverts (instead of trapping on
the first one), override the fail-fast:

```bash
DEGENBOT_SIM_EXIT_ON_FAIL=0 ./run_bot.sh
```

---

## Python-side control

- **`DEGENBOT_DEBUG=1`** — sets the degenbot + Rust-bridge Python logger levels
  to `DEBUG` (else `INFO`). This is the Python-side gate that, together with
  `RUST_LOG`, controls whether Rust `debug!` records reach the Python-forwarded
  copy.
- **`DEGENBOT_DEBUG_FUNCTION_CALLS=1`** — enables the `@log_function_call`
  decorator annotations (very noisy; opt-in).

---

## Practical example: quiet default vs. deep dive

**Default (quiet):** the shipped defaults give you INFO-level operational logs
(the `[sim]`, `[solver]`, `[dispatch]` status lines) with the debug diagnostics
muted and the `alloy`/`tungstenite` lifecycle noise throttled to `warn`:

```bash
./run_bot.sh
```

**Deep dive (everything):** full Rust + Python debug, all hard/loud gates
explicitly on:

```bash
RUST_LOG=debug DEGENBOT_DEBUG=1 \
    uv run python examples/eth_backrun_v2_v3_v4_rust.py
```

**Targeted (a single subsystem):** only the V2 calc + solver internals, keeping
the rest at info:

```bash
RUST_LOG=info,degenbot_bot=debug,degenbot_backrun_strategy=debug DEGENBOT_DEBUG=1 \
    uv run python examples/eth_backrun_v2_v3_v4_rust.py
```
