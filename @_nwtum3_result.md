# NWTUM3 — Operator Add-Path-At-Any-Time Surface: result

**Status: done.** Split into three incremental phases, each committed separately.

## What landed

**S1 — reusable registration seam** (`39181bb5`): extracted the per-path
registration inline work from `build_paths` into a long-lived,
pump-concurrent `PathRegistrationPipeline`. It owns the `ConstructionContext` +
engine registry + the shared counters + the registered-path dedup set + the
bounded backpressured producer/consumer. The operator surface (`enqueue_path`,
`trigger_discovery`) routes through the **same** `_consume` body as discovery
(build → register+verify → per-path release → dedup → register_path), so an
operator-added path is treated identically to a discovered one. Fail-fast
tripwire preserved.

**S2 — programmatic surface on the session** (`3a20edc9`): `BackrunSession`
now **owns** the long-lived pipeline (created in `run()` from the retained
`ConstructionContext`, passed to `build_paths`) and exposes
`enqueue_path(steps, directions=None)` / `trigger_discovery(bound=None)` that
route into it **at any time** — including after `build_paths` returns and the
main-loop trim drops the Python `bot` (the retained `ConstructionContext` keeps
constructing through the Rust `PoolBuilder`). Both **never await the pump**,
so a mid-run add cannot stall solve/dispatch. They raise `RuntimeError` loudly
when no live pipeline exists.

**S3 — operator command channel + CLI** (`dffab96d`): a Unix-domain-socket
JSON-lines channel (`degenbot.operator.operator_channel`) so an operator can
steer the bot from a *separate* process. `degenbot path add` /
`degenbot path discover` are thin clients; the example bot hosts the server via
`--operator-socket <path>`. No Python class crosses the wire (family strings →
pool-table base classes via `step_from_wire`), exceptions → `ok:false` (never
crash the host), and `OperatorServer.close()` is self-sufficient (cancels
`serve_forever` before `wait_closed`).

**S4 — docs** (`docs/architecture/operator-add-path-surface.md`).

## Acceptance criteria

- Add a specific path at any point during a live run → registered, verified,
  released per D4, solved on the next dirty block. ✅ (same `_consume` as
  discovery; per-path release in the retained seam)
- CLI subcommand + programmatic API exist and are documented; bounded on-demand
  discovery exists (in addition to the unbounded background producer). ✅
- Adding paths mid-run does not abort/stall/interfere with the pump. ✅
  (`test_mid_run_add_path_does_not_stall_dispatch`)
- Works with the trimmed state — adding a path doesn't require the dropped
  Python `bot`/builders. ✅ (pipeline retains its own `ConstructionContext`)

## Validation

- `tests/operator/test_operator_channel.py` (6): wire round-trip, family
  mapping, error→`ok:false` host-survival, malformed request, wrap normalization.
- `tests/cli/test_path_cli.py` (8): hop parsing, `--direction` mapping, live
  round-trip + error exit against a threaded `OperatorServer`.
- `tests/arbitrage/test_backrun_session.py` (34): programmatic surface raises
  without a live pipeline; mid-run add during an advancing consumer does not
  stall or abort dispatch.
- Full `tests/arbitrage/ + operator/ + cli/`: **434 passed**; ruff clean,
  `ty` clean, markdownlint clean.

## Deferred / follow-ups

- Full solve-while-forever-discovery behavior → **U6TKNU** (already referenced
  by the task's Validation Gates).
- The Rust-side operator surface — NWTUM3 is the Python driver. The eventual
  host is the Rust runtime in `bot_core/registration_lifecycle.rs` (D-B/D-C
  shared-provider + fail-fast), per the three-layer direction; the Python seams
  here are the driver shell that will delegate to it.
