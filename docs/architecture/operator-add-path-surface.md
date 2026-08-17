# Operator Add-Path-At-Any-Time Surface (NWTUM3)

An operator can **add a specific path**, or **trigger a bounded on-demand
discovery sweep**, on a *live, running* bot — without restarting it and without
stalling its update/solve/dispatch. This page documents the three reusable
seams that make that possible, from the innermost (the registration pipeline)
to the wire-facing operator channel.

It complements the verify-lifecycle shared decisions in the NWTUM3 ergo task
(Python driver; the Rust-owned `bot_core/registration_lifecycle.rs` is the
middle/hosted form; **D-B** one shared provider, **D-C** no-config fail-fast,
**D-A** tripwire = verification mismatch).

## 1. The long-lived registration pipeline

`examples/eth_settlement_arbitrage_v2_v3_v4_rust.py::PathRegistrationPipeline` is the
**single** place that turns a path's hop steps into registered, verified,
released pools. Both the built-in discovery source AND an operator add route
through the *same* `_consume` body (build → register + verify → per-path
release → dedup → `register_path`), so an operator-added path is treated
identically to a discovered one.

The pipeline is **long-lived** and owned by the session (`BotRunner`),
not created and dropped inside `build_paths`. That is the key structural
change that makes mid-run adds possible:

- `run()` builds a `ConstructionContext` once and holds it, and creates the
  `PathRegistrationPipeline` on the session (`self._pipeline`).
- `build_paths` reuses the caller-supplied pipeline (falls back to building its
  own only when none is supplied).
- Because the pipeline retains its own `ConstructionContext`, it keeps
  constructing through the Rust `PoolBuilder` even after the main-loop trim
  (`self.bot = None`) drops the Python bot — **adding a path mid-run does not
  resurrect Python construction state**, satisfying the trimmed-state AC.

### Programmatic API

`BotRunner` exposes two coroutines that route into the live pipeline. Both
**never await the pump**, so a mid-run command cannot stall solve/dispatch:

```python
await session.enqueue_path(path_steps, directions=None)   # add ONE path
n = await session.trigger_discovery(bound=None)           # bounded one-shot sweep
```

`enqueue_path` takes the same `path_steps` list the pipeline's discovery uses
(step objects with a `.type` pool-table base class, `.address`, and `.hash` for
V4) plus optional per-hop `directions`. `trigger_discovery` runs a *bounded*
sweep distinct from the unbounded background producer. Both raise
`RuntimeError` loudly if no live pipeline exists (e.g. an injected/fake run).

## 2. The operator command channel (wire)

`src/degenbot/operator/operator_channel.py` hosts a **Unix-domain-socket,
JSON-lines** protocol so an operator can steer the bot from a *separate
process*. The bot runs an `OperatorServer` asyncio task; a client writes one
JSON command line and reads one JSON response line.

Requests / responses (one JSON object per line):

```
{"op": "add_path",
 "steps": [{"family": "V2|V3|V4", "address": "0x..",
            "hash": "0x..(v4 pool_id, v4 only)"}, ...],
 "directions": [true, false]}           # optional; auto-resolved if absent
{"op": "discover", "bound": 5}

{"ok": true,  "detail": "..."}
{"ok": false, "error": "..."}
```

Key properties:

- **No Python class crosses the wire.** `step_from_wire` maps a `family`
  string (V2/V3/V4) to the matching pool-table base class, the single source of
  the classification `_consume` needs.
- **Wire hygiene.** `wrap_handler` turns any exception into `{"ok": false,
  "error": ...}` so a malformed or failing command never crashes the host, and
  the server guards JSON decode / op dispatch the same way.
- **Self-sufficient `close()`.** It cancels the in-flight `serve_forever` loop
  before `wait_closed()`, so shutdown never blocks on a pending accept loop,
  and unlinks the socket file.

The example bot hosts the server when launched with `--operator-socket <path>`,
routing commands into `session.enqueue_path` / `session.trigger_discovery`.

## 3. The CLI client

`degenbot path` in `src/degenbot/cli/path.py` is a thin client over
`send_command` (no local pool/db work):

```
degenbot path add --socket /tmp/bot.sock \
    --hop v3:0x<weth/usdt> --hop v4:0x<manager>:0x<pool_id>:[--direction zfo]
degenbot path discover --socket /tmp/bot.sock [--bound 5]
```

Hop spelling is `FAMILY:ADDRESS` (V2/V3) or `V4:ADDRESS:HASH` (HASH = the 0x+64
pool id). `--direction zfo`/`ozf` applies a per-hop True/False list; omitting it
lets the bot auto-resolve. A rejecting or unreachable host surfaces as a
`click.ClickException`, so a failed command exits non-zero rather than silently.

## Why no in-process CLI hack

The `degenbot` CLI cannot add a path "into" a running bot by importing the
session — the bot lives in a different process. The socket channel is the honest
boundary: the CLI is a driver, the bot's process owns the state, and the wire
is a tiny versioned protocol a client of any vintage can talk to.

## Test coverage

- `tests/operator/test_operator_channel.py` — wire round-trip (`add_path`,
  `discover`), family mapping, error → `ok:false` host-survival, malformed
  request, `wrap_handler` normalization.
- `tests/cli/test_path_cli.py` — hop parsing, `--direction` mapping, live
  round-trip + error exit against a threaded `OperatorServer`.
- `tests/arbitrage/test_backrun_session.py` — programmatic surface raises
  without a live pipeline; a mid-run add during an advancing consumer does not
  stall or abort dispatch.
