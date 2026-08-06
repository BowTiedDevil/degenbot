# BotRunner Extraction — architecture record for epic 5TSYKN

**Status:** spike (`IJDU4F`) findings. Feeds the implementation tasks of epic **5TSYKN** —
promote the backrun runtime driver out of `examples/` into a first-class companion module.

## 1. Friction confirmed

The driver is a script, not a module, and the tests reach *into* the script for it:

- Probes at spike time:
  - `examples/eth_backrun_v2_v3_v4_rust.py` — **2906 lines** (grew from the 1955 recorded
    when the epic was written; `BackrunSession` + `build_paths` + `consume_result_batches`
    + `_tee_block_stream` + rendering are the bulk).
  - `examples/eth_backrun_helpers.py` — 506 lines (`BackrunConfig` + 4 pure helpers).
  - `examples/` **is a package** (`examples/__init__.py` exists), so `from examples.…`
    currently resolves — which is why the seams have stayed green this long.
- **No `src/degenbot/**` file imports from `examples.`** (verified: `rg "from examples\." src/`
  is empty). Every `build_paths` / `BackrunSession` reference inside `src/`
  (`bot/_bot.py`, `arbitrage/engine_registry.py`, `arbitrage/verification_retry.py`) is
  **docstring-only prose**, not an import. So the consumer graph is entirely test-side.
- The offending import that motivates the epic — `tests/arbitrage/test_backrun_session.py`
  does `from examples.eth_backrun_v2_v3_v4_rust import BackrunSession` — is real and live.

## 2. Placement decision (ADR-005 discipline — the two-consumer check)

**The driver stays in the Python companion.** It is Python-ecosystem orchestration:

- it owns the asyncio event loop, the `main()` policy, SIGINT install/restore, `dotenv`,
  and the CLI (`_build_arg_parser`) — every one of which `docs/migration-guides/
  three-layer-transition.md` §2.4 / `rust-owned-bot.md` class as `stays-python`;
- the Rust core already exposes the *engine* the driver controls (`Bot` + `EngineRegistry`
  + the `dispatch_profitable` / `dispatch_and_submit` seam, the `engine.block_stream()`
  pump). A standalone Rust consumer gets those; `BotRunner` is the **cockpit** over them.

**The deepening is module-ization, not a Rust port.** `BotRunner` must not move into a core
crate, and the epic must not over-reach into the pump/engine/dispatch core (non-goal). The
work is: (a) extract the driver into a tested module, (b) split its single `run()` into four
named seams so each is individually testable. Nothing engine-owned changes.

Proposed new package boundary: **`src/degenbot/runner/`** — a new top-level companion
namespace (the epic's proposal), housing `BotRunner` + the moved driver plumbing. This is
distinct from `src/degenbot/arbitrage/` (engine-adjacent: `engine_registry`,
`recurring_verify`, `verification_retry`, `policy` all stay there). Rationale: the driver
is an *orchestrator over* arbitrage (and future engine families), not an arbitrage intern,
so it gets its own module rather than being buried as `arbitrage/runner.py`.

## 3. Proposed `BotRunner` interface

`BackrunSession` (line 587) already IS a `BotRunner` in behavior — it collapses the startup
reordering behind a facade with injectable actors. The extraction renames it to `BotRunner`
and widens it from a two-method facade (`start()` / `run()`) into **four named seams** that
mirror the existing internal boundaries, preserving the injectable-actor pattern:

```python
# src/degenbot/runner/bot_runner.py
class BotRunner:
    def __init__(
        self,
        cfg: BackrunConfig,
        *,
        bot: Bot | None = None,                      # injectable fake seam
        engine_registry: EngineRegistry | None = None,
        async_w3: AsyncAlloyProvider | None = None,
        snapshots: tuple[Any, Any, Any, Any] | None = None,
        path_builder: Callable[..., Awaitable[None]] | None = None,
        consumer: Callable[..., Awaitable[None]] | None = None,
        install_sigint: bool = True,
        background_registration: bool | None = None,
    ) -> None: ...

    async def start(self) -> "BotRunner":
        """Phase A (unchanged): build actors → fetch block → load snapshots →
        engine_registry.start() → stops at Backfilled, pre-resume."""

    async def build_paths(self, *, background: bool | None = None) -> None:
        """Discover + register paths via the pipeline; owns the Sub-A/B
        construction-context and the state-trim. Injected path_builder replaces
        the real build_paths (test seam)."""

    async def consume(self) -> None:
        """Attach the result consumer (block-stream tee + recurring-verify),
        resume the pump, and become the permanent main loop."""

    async def dispatch(self, results, *, current_block, ...) -> None:
        """The encode→simulate→submit leaf (currently _dispatch_profitable);
        kept as a method so the sim/submit seam is unit-testable without the
        block loop."""
```

**Method-vs-module split (what stays "thin example"):**

| Responsibility | Stays in module | Stays in `examples/` (policy) |
|---|---|---|
| Actor lifecycle + phase ordering | `BotRunner.start/run` | — |
| Path discovery/registration | `build_paths` + `PathRegistrationPipeline` + `ConstructionContext` + `run_registration_pipeline` | — |
| Result/block-loop plumbing | `consume_result_batches`, `_tee_block_stream`, `_reprime`, `_apply_block_if_ready`, `_apply_result_if_ready` | — |
| Sim/submit | `BotRunner.dispatch` (ex-`_dispatch_profitable`), `_render_sim_summary`, `_render_profit_logs`, `_render_sim_failures`, `_render_fot_tokens`, `_dump_failure_fixture` | — |
| Config + pure helpers | `BackrunConfig`, `classify_revert`, `format_failure_breakdown`, `filter_thin_margin_results`, `format_sim_diag_line` (moved from `eth_backrun_helpers.py`) | — |
| Pure pool direction | `resolve_directions` (moved into `arbitrage/` — it is pool math, consumer-agnostic) | — |
| Module constants | factory/pool-manager/WETH/executor constants, `REG_*`, `PATH_PERMUTATION_FILTER`, `MIN_*` | — |
| CLI + entrypoint | — | `_build_arg_parser`, `main()`, `if __name__ == "__main__"`, SIGINT wrapper, `dotenv` read |

`_build_arg_parser` stays example-side (CLI policy), but it is already tested by
`test_eth_backrun_main_args.py` — the test must be rerouted to import from the example's
thin entrypoint (which re-exports it or the test moves with the parser). See §6.

## 4. Module layout

```
src/degenbot/runner/
  __init__.py            # re-export BotRunner (public surface)
  bot_runner.py          # BotRunner (ex-BackrunSession) — start/build_paths/consume/dispatch
  build_paths.py         # build_paths + PathRegistrationPipeline + ConstructionContext
                         #   + run_registration_pipeline + resolve_directions(+ repo constants)
  consume.py             # consume_result_batches + _tee_block_stream + _reprime
                         #   + _apply_block_if_ready + _apply_result_if_ready
  dispatch.py            # _dispatch_profitable (→BotRunner.dispatch) + the _render_* helpers
  config.py              # BackrunConfig + from_env + classify_revert/format_*/filter_* helpers
```

The already-package-owned collaborators stay put: `arbitrage/recurring_verify.py`
(`run_recurring_verify_until_done`), `arbitrage/engine_registry.py`, `arbitrage/verification_retry.py`.

## 5. Consumer enumeration

**Verified: no `src/` consumer imports from `examples.`** — the only defect is test-side.
Full table of files reaching into the examples (all in `tests/`):

| Test file | Symbol(s) reached | Switch target |
|---|---|---|
| `arbitrage/test_backrun_session.py` | `BackrunSession`, `BackrunConfig`, `ConstructionContext`, `PathRegistrationPipeline`, `run_registration_pipeline`, `REG_QUEUE_BOUND`, `REG_WORKERS`, factory/WETH constants | `degenbot.runner` (BotRunner, config) + `runner.build_paths` |
| `arbitrage/test_consumer_block_stream.py` | `Dispatcher`, `consume_result_batches`, `_dispatch_profitable`, `_tee_block_stream` | `degenbot.runner.*` |
| `arbitrage/test_registration_pipeline.py` | `run_registration_pipeline`, `PathRegistrationPipeline` | `degenbot.runner.build_paths` |
| `arbitrage/test_render_sim_failures.py` | `_render_sim_failures` | `degenbot.runner.dispatch` |
| `arbitrage/test_synthetic_v2_round_trip.py` | `resolve_directions`, `EngineRegistry` | `degenbot.arbitrage` (resolve_directions) + existing `EngineRegistry` |
| `arbitrage/test_eth_backrun_main_args.py` | `_build_arg_parser` | thin example entrypoint (re-export) |
| `arbitrage/test_eth_backrun_helpers.py`, `test_backrun_config.py`, `test_verification_retry_config.py`, `test_revert_taxonomy.py` | `BackrunConfig` + helpers | `degenbot.runner.config` |

The `import examples.… as runner; runner.EngineRegistry` references in
`test_engine_registry_register_path.py`, `test_engine_registry_two_step_verify.py`,
`test_path_policy.py` re-import `EngineRegistry` *through* the example for **module-constant
mutation** (e.g. toggling `PATH_PERMUTATION_FILTER`), not for `BackrunSession`. These switch to
importing `PathRegistrationPipeline`/`EngineRegistry` directly from the package — confirming
the seam does not need to stay bundled with the driver.

No consumer needs behavior the proposed seam does not offer: the injectable-actor +
module-function seams cover every test reach.

## 6. Test-wiring diff sketch

`test_backrun_session.py`'s fake injection (`bot`/`engine_registry`/`async_w3` + module-function
`path_builder`/`consumer`) is the OLKZ3L `engine=` seam scaled up; it **must survive the move
unchanged in semantics**. The only diff is the import target:

```diff
-from examples.eth_backrun_v2_v3_v4_rust import BackrunSession
-from examples.eth_backrun_helpers import BackrunConfig
+from degenbot.runner import BotRunner as BackrunSession   # temporary alias, then rename
+from degenbot.runner.config import BackrunConfig
```

Production call sites rename `BackrunSession(...)` → `BotRunner(...)` in the same commit
(examples/eth_backrun_v2_v3_v4_rust.py `main()`), so the fake-injection constructor defaults
(`background_registration` auto-select, injectable actors) are asserted by the existing
orchestration tests with **no behavior change**.

## 7. ADR interaction

**No new ADR required.** `BotRunner` fits ADR-006 (Bot as per-chain orchestrator — the Rust
`Bot` owns state/RPC; the Python `BotRunner` is the per-chain *deployment cockpit*) and
ADR-003 (Bot is the single Rust state owner; `BotRunner` owns no pool state — it trims and
drops the Python `bot` once the engine owns canonical state). ADR-005's three-layer framing is
satisfied: `BotRunner` lives entirely in the Python companion layer over the Rust-owned
engine. Recorded here so the epic does not invent an ADR for what is a doc-level placement.

## 8. Caveats & findings for the implementation tasks

1. **Dangling test import (pre-existing, unrelated to the epic):**
   `Test6VZN7HOngoingDiscovery` in `test_backrun_session.py` imports
   `_discovery_producer_forever` from the example, but that symbol **does not exist** in
   `examples/eth_backrun_v2_v3_v4_rust.py` — `build_paths` says "Rediscovery was stripped —
   6VZN7H" (single-pass DFS). This stale test class must be reconciled (re-WRITE to the
   single-pass reality, or deleted) when task 5 reroutes tests. Do not carry a nonexistent
   import into the package.
2. **Line drift:** the example is 2906 lines now, not 1955 — the epic's size estimate is
   stale; extraction is larger than originally counted but the seam boundaries are unchanged.
3. **Moves must remove the old copy** (commit-checklist rule): when `build_paths`,
   `BackrunSession`, `consume_result_batches`, etc. land in `src/degenbot/runner/`, delete
   them from `examples/eth_backrun_v2_v3_v4_rust.py` (thin to `argv → BotRunner`) and delete
   `examples/eth_backrun_helpers.py` once `BackrunConfig`/helpers move.

## 9. Implementation tasks (to register under epic 5TSYKN)

1. `IJDU4F` (this spike) — **done** (this doc).
2. Move `BackrunConfig` + helpers → `runner/config.py`; re-point the 4 config/helper test files.
3. Move `BotRunner` (ex-`BackrunSession`) → `runner/bot_runner.py`; split `run()` into
   `start/build_paths/consume/dispatch`; preserve injectable seams.
4. Move `build_paths` + `PathRegistrationPipeline` + `ConstructionContext` +
   `run_registration_pipeline` (+ `resolve_directions` → `arbitrage/`) → `runner/build_paths.py`.
5. Move `consume_result_batches` + tee/reprime/apply plumbing → `runner/consume.py`.
6. Move `_dispatch_profitable` (→ `BotRunner.dispatch`) + render helpers → `runner/dispatch.py`.
7. Reroute all `tests/arbitrage/*` imports to the package; reconcile the dangling
   `_discovery_producer_forever` reference (finding 1).
8. Thin `examples/eth_backrun_v2_v3_v4_rust.py` to `argv → BotRunner`; delete
   `examples/eth_backrun_helpers.py`.
9. Validation gate (runs last, depends on 3–8): `just test-python` + `just lint-rust` +
   `just test-rust` green; `rg "from examples\.eth_backrun" tests/ src/` returns nothing.
