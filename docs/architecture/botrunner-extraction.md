# BotRunner Extraction — architecture record for epic 5TSYKN

**Status:** **IMPLEMENTED** (epic **5TSYKN**, 9/9 tasks done). This record started as the
spike (`IJDU4F`) findings; the implementation landed the driver in `src/degenbot/runner/`
and thinned the example to `argv → BotRunner`. See [§4 Final layout](#4-module-layout-final)
for what shipped and [§10 Deviations](#10-deviations-from-the-proposal) for where the
outcome differs from the original proposal. The design rationale below is preserved as the
spike record.

Epic 5TSYKN promotes the backrun runtime driver out of `examples/` into a first-class
companion module.

## 1. Friction confirmed

At spike time the driver was a script, not a module, and the tests reached *into* the
script for it:

- Probes at spike time:
  - `examples/eth_backrun_v2_v3_v4_rust.py` — **2906 lines** (grew from the 1955 recorded
    when the epic was written; `BackrunSession` + `build_paths` + `consume_result_batches`
    + `_tee_block_stream` + rendering are the bulk).
  - `examples/eth_backrun_helpers.py` — 506 lines (`BackrunConfig` + 4 pure helpers).
  - `examples/` **is a package** (`examples/__init__.py` exists), so `from examples.…`
    currently resolves — which is why the seams have stayed green this long.
- **No `src/degenbot/**` file imports from `examples.`** (verified: `rg "from examples\." src/`
  is empty). Every `build_paths` reference inside `src/` (`bot/_bot.py`,
  `arbitrage/engine_registry.py`, `arbitrage/policy.py`) is **docstring-only prose**,
  not an import. So the consumer graph at spike time was entirely test-side.
- The offending import that motivated the epic — `tests/arbitrage/test_backrun_session.py`
  did `from examples.eth_backrun_v2_v3_v4_rust import BackrunSession` — was real and live at
  spike time. Both ends are gone: the test is `tests/arbitrage/test_arbitrage_session.py`
  (`from degenbot.runner import BotRunner`), and the entrypoint is
  `examples/eth_settlement_arbitrage_v2_v3_v4_rust.py`, a 134-line `argv → BotRunner` shell.

## 2. Placement decision (ADR-005 discipline — the two-consumer check)

**The driver stays in the Python companion.** It is Python-ecosystem orchestration:

- it owns the asyncio event loop, the `main()` policy, SIGINT install/restore,
  and the CLI (`_build_arg_parser`) — every one of which `docs/migration-guides/
  three-layer-transition.md` (removed in the stale-docs cleanup `71ec78b2`) §2.4 /
  `rust-owned-bot.md` class as `stays-python`;
- the Rust core already exposes the *engine* the driver controls (`Bot` + `EngineRegistry`
  + the `dispatch_profitable` / `dispatch_and_submit` seam, the `engine.block_stream()`
  pump). A standalone Rust consumer gets those; `BotRunner` is the **cockpit** over them.

**The deepening is module-ization, not a Rust port.** `BotRunner` must not move into a core
crate, and the epic must not over-reach into the pump/engine/dispatch core (non-goal). The
work is: (a) extract the driver into a tested module, (b) split its single `run()` into four
named seams so each is individually testable. Nothing engine-owned changes.

Proposed new package boundary: **`src/degenbot/runner/`** — a new top-level companion
namespace (the epic's proposal), housing `BotRunner` + the moved driver plumbing. This is
distinct from `src/degenbot/arbitrage/` (engine-adjacent: `engine_registry` and
`policy` stay there). Rationale: the driver
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
        scheduler: Callable[[Coroutine[Any, Any, None]], asyncio.Task[Any]] = asyncio.create_task,
    ) -> None: ...

    async def start(self) -> "BotRunner":
        """Phase A (unchanged): build actors → fetch block → load snapshots →
        engine_registry.start() → stops at subscribed, pre-resume."""

    async def build_paths(self, *, background: bool | None = None) -> None:
        """Discover + register paths via the pipeline; owns the Sub-A/B
        construction-context and the state-trim. Injected path_builder replaces
        the real build_paths (test seam)."""

    async def consume(self) -> None:
        """Attach the result consumer, resume the pump, and become the
        permanent main loop."""

    async def dispatch(self, results, *, current_block, ...) -> None:
        """The encode→simulate→submit leaf (currently _dispatch_profitable);
        kept as a method so the sim/submit seam is unit-testable without the
        block loop."""
```

**Method-vs-module split (what stays "thin example"):** the proposal's map — package versus
example policy — held: the extraction moved every responsibility the proposal listed into the
package, and §4 names where each one lives now (the crawl and the encode→simulate→submit leaf
have since moved to the Rust core — §10.6). The four seams above survived as the session's
internal seams rather than as `BotRunner` methods: `run()` drives `runner/_run_ritual.py`'s
state machine over `runner/session.py`'s `Session`.

`_build_arg_parser` was to stay example-side (CLI policy) per this spike, but the final
gate (zero `from examples.eth_backrun` imports in `tests/`/`src/`) forced it into the
package as `runner/cli.py::build_arbitrage_arg_parser` (see §10). It is tested by
`test_eth_arbitrage_main_args.py`, which imports from the package.

## 4. Module layout (final)

Responsibility map, as shipped:

| Responsibility | Shipped home | Stays in `examples/` (policy) |
|---|---|---|
| Actor lifecycle + phase ordering | `BotRunner.start/run` (`runner/bot_runner.py`) over `runner/session.py`'s `Session`, ordered by `runner/_run_ritual.py`'s `RunRitual` | — |
| Path discovery/registration | `build_paths` + `ConstructionContext` + `BuildPathsOptions` (`runner/build_paths.py`); the crawl itself is the engine's (`degenbot_bot::arb_engine::crawl`) | — |
| Result/block-loop plumbing | `BlockLoop` + `consume_result_batches` + `_reprime` + `_apply_block_if_ready` + `_apply_result_if_ready` (`runner/_consume.py`) | — |
| Sim/submit | The Rust core (`degenbot-batch-executor` + the engine's `dispatch_profitable_results`); Python keeps the construction boundary (`runner/_sim_submit.py`), the submit-record render + smoke FSM (`runner/_dispatch.py`), and the rendering (`runner/_render.py`) | — |
| Config + pure helpers | `ArbitrageConfig` + `build` (`runner/config.py`); `format_failure_breakdown` / `format_sim_diag_line` (`runner/_render.py`). The revert taxonomy and the thin-margin floor are Rust: `degenbot::decoders::revert::classify_revert`, `degenbot_arbitrage::filter_thin_margin_results` | — |
| Pure pool direction | `resolve_directions`, re-exported by `degenbot.pathfinding` from `degenbot._ffi` (the Rust `degenbot_pathfinding::directions` seam) | — |
| Module constants | Absorbed by the typed config: `runner/config.py`'s fields + `runner/identity.py`'s deployment/operator material | — |
| CLI + entrypoint | `build_arbitrage_arg_parser` (`runner/cli.py`) | `main()`, `if __name__ == "__main__"`, the SIGINT wrapper |

```
src/degenbot/runner/
  __init__.py            # the package face: BotRunner, Session (+SessionPhase/PhaseError),
                         #   InjectedActors/Actors, ActivationGateRefused, ArbitrageConfig,
                         #   build_paths / ConstructionContext / BuildPathsOptions
  bot_runner.py          # BotRunner — the operator verbs: start / run / enqueue_path /
                         #   trigger_discovery / shutdown
  session.py             # Session — the one owner of a pump session's coordination state,
                         #   with the lifecycle FSM and the InjectedActors/Actors records
  build_paths.py         # build_paths + ConstructionContext + BuildPathsOptions + crawl_config
                         #   (the crawl is the engine's: degenbot_bot::arb_engine::crawl)
  _run_ritual.py         # RunRitual — one owner of a running session's startup ordering
  _consume.py            # BlockLoop + consume_result_batches + _reprime
                         #   + _apply_block_if_ready + _apply_result_if_ready
  _dispatch.py           # RawEngineResult + render_submit_records + track_submission_smoke
                         #   + the relay-provider resolution
  _render.py             # MergedOutcome/DisplayPolicy + the _render_* helpers
                         #   + format_failure_breakdown + format_sim_diag_line
  _sim_submit.py         # BatchExecutor construction (build_executor_for/build_batch_executor)
  _actors.py             # the collaborator builders + ActivationGateRefused
  _session_watch.py      # SessionWatch + SessionEndVerdict (the session's end-state)
  _relay_posture.py      # RelayPosture — where signed bytes are broadcast
  cli.py                 # build_arbitrage_arg_parser (argparse CLI surface)
  config.py              # ArbitrageConfig (+ build) — the typed config loader
  identity.py            # deployment/operator addresses, published keys, checksum helper
  published_operator_private_keys.txt  # the repo-published operator keys identity.py reads
  diag.py                # DiagConfig + the tracemalloc / RSS / faulthandler probes

# ADR-013 stable home for the GIL-probe stuck-watchdog (moved out of the runner leaf):
src/degenbot/diagnostics/
  __init__.py            # re-exports mark_progress / start_gil_probe from degenbot._ffi
```

The already-package-owned collaborators stay put: `arbitrage/engine_registry.py` and
`arbitrage/policy.py`.

## 5. Consumer enumeration

**Verified at spike time: no `src/` consumer imported from `examples.`** — the only defect
was test-side. This is the spike-time enumeration; task 7 rerouted every row to the package,
and no `tests/` file imports from `examples.` any more. The files were then renamed to the
locked arbitrage names (`git show 0a041a1c9`), so the shipped readers are
`test_arbitrage_session.py`, `test_arbitrage_config.py`, `test_eth_arbitrage_helpers.py`,
`test_eth_arbitrage_main_args.py`, `test_consumer_block_stream.py`,
`test_render_sim_failures.py` and `test_synthetic_v2_round_trip.py`:

| Test file | Symbol(s) reached | Switch target |
|---|---|---|
| `arbitrage/test_backrun_session.py` | `BackrunSession`, `BackrunConfig`, `ConstructionContext`, `PathRegistrationPipeline`, `resolve_directions`, factory/WETH constants (PRG-5: the bounded crawl shell — queue + offload executor + `REG_WORKERS`/`REG_QUEUE_BOUND` — retired in favor of the fleet intake) | `degenbot.runner` (BotRunner, config) + `runner.build_paths` |
| `arbitrage/test_consumer_block_stream.py` | `Dispatcher`, `consume_result_batches`, `_dispatch_profitable`, `_tee_block_stream` | `degenbot.runner.*` |
| `arbitrage/test_registration_pipeline.py` | `run_registration_pipeline`, `PathRegistrationPipeline` | `degenbot.runner.build_paths` |
| `arbitrage/test_render_sim_failures.py` | `_render_sim_failures` | `degenbot.runner._render` |
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
(the registration `scheduler` seam — production `asyncio.create_task`, with the deterministic
test double for replayable hand-offs — and the other injectable actors) are asserted by the
existing orchestration tests with **no behavior change**.

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

## 9. Implementation tasks (epic 5TSYKN — all done)

1. `IJDU4F` (this spike) — **done** (this doc).
2. `RVSYWB` — **done**: `BackrunConfig` + helpers → `runner/config.py`; re-pointed the 4 config/helper test files.
3. `DKUOBL` — **done**: `BotRunner` (ex-`BackrunSession`) → `runner/bot_runner.py`; split `run()` into
   `start/build_paths/consume/dispatch`; injectable seams preserved.
4. `JKYVST` — **done**: `build_paths` + `PathRegistrationPipeline` + `ConstructionContext` +
   `run_registration_pipeline` + `resolve_directions` → `runner/build_paths.py`.
5. `CXWQDI` — **done**: `consume_result_batches` + tee/reprime/apply plumbing → `runner/consume.py`.
6. `DZTFSJ` — **done**: `_dispatch_profitable` (→ `BotRunner.dispatch`) + render helpers → `runner/dispatch.py`.
7. `OBAYPI` — **done**: rerouted all `tests/arbitrage/*` imports to the package; reconciled the dangling
   `_discovery_producer_forever` (deleted the 3 stale producer tests, kept the run()-level trim test).
8. `XYID3W` — **done**: thinned `examples/eth_backrun_v2_v3_v4_rust.py` to `argv → BotRunner`;
   deleted `examples/eth_backrun_helpers.py`; moved the arg-parser to `runner/cli.py`.
9. `ASJKB3` — **done**: validation gate — `just test-python` (2502 passed / 0 failed) +
   `just lint-rust` + `just test-rust` green; `rg "from examples\.eth_backrun" tests/ src/` returns nothing.

## 10. Deviations from the proposal

1. **`resolve_directions` landed in `runner/build_paths.py`, not `arbitrage/`.** The proposal
   slated it for `arbitrage/` (pool math), but it shipped colocated with its only caller
   (`build_paths`) in the runner package. It remains discoverable as `resolve_directions`.
2. **The arg-parser moved into the package as `runner/cli.py::build_backrun_arg_parser`.**
   The spike said CLI policy stays example-side, but ASJKB3's gate required **zero**
   `from examples.eth_backrun` imports in `tests/`/`src/`, and
   `test_eth_backrun_main_args.py` could not keep reaching into the example. The example
   keeps a thin `_build_arg_parser` alias for `main()`.
3. **A third module was added: `runner/driver_constants.py`.** The driver's ~15 shared
   module constants (factories, pool-manager/WETH addresses, `REG_*`, `MIN_*`, fee
   presets, `PATH_PERMUTATION_FILTER`) were consolidated into one dedicated module so
   `bot_runner`/`build_paths`/`consume`/`dispatch` import them from a single home.
4. **A new ADR-013 home was added: `degenbot/diagnostics/`.** `consume.py` reached
   `degenbot._ffi` directly for the GIL-probe stuck-watchdog (`mark_progress` /
   `start_gil_probe`), which tripped `test_ffi_boundary.py` (ADR-013: only `__init__.py`
   may reach `_ffi`). A stable `degenbot.diagnostics` home now re-exports them.
5. **Two pre-existing, unrelated failures were closed to reach the green gate.** They were
   artifacts of the in-flight `CDJEPJ-1/2` (ERC-20 metadata batching) + `TF7RZB` (builder
   identity) work — `erc20_builder`'s leaf `_ffi` import (fixed via a `degenbot.database`
   TYPE_CHECKING re-export) and `test_bot`'s stale `_io`/`build_v4_pool` fakes (extended
   to the `fetch_erc20_metadata_batch` seam + 11-field return surface).
6. **The shipped layout has been renamed and narrowed since the epic closed; §4's map and
   tree describe today's `src/degenbot/runner/`, not the tree task 6 left.** `consume.py` →
   `_consume.py`, `dispatch.py` → `_dispatch.py`, and the render helpers left dispatch for
   `_render.py`; `driver_constants.py` retired into the typed config (`runner/config.py`)
   plus `runner/identity.py`'s deployment material; `BackrunConfig.from_env` became
   `ArbitrageConfig.build`, and `build_backrun_arg_parser` became
   `build_arbitrage_arg_parser`. `resolve_directions` belongs to `degenbot.pathfinding`
   (§10.1's `runner/build_paths.py` home was superseded, and §10.3's `driver_constants.py`
   no longer exists). The `BackrunSession` alias the transition kept is gone from
   `runner/__init__.py`, and `BotRunner` narrowed to five operator verbs over `Session` —
   the four seams proposed in §3 are that session's internals, not facade methods.
   Three responsibilities left the package for the Rust core since: the crawl
   (`degenbot_bot::arb_engine::crawl`, which retired the `PathRegistrationPipeline` /
   `run_registration_pipeline` pair), the encode→simulate→submit leaf
   (`degenbot-batch-executor` + the engine's `dispatch_profitable_results`), and two pure
   helpers (`classify_revert` → `degenbot::decoders::revert::classify_revert`;
   `filter_thin_margin_results` → `degenbot_arbitrage::filter_thin_margin_results`).
   `_tee_block_stream` went with `arbitrage/recurring_verify.py` (`git show 00424a440`) —
   the residue §4 carried until now.

## Running parity gate (RSP-8, ergo 23DLCY)

The Python driver and the pure-Rust driver (`rust/examples/settlement_bot`,
the `cargo add degenbot` consumer twin) are continuously compared by the
running parity gate: the offline fixture boot gate (shared oracle
`tests/standalone_parity/fixtures/settlement_bot_boot.json`, driven on both
axes) plus the recorded dual-driver decision diff. See
[rust-settlement-bot-parity.md § Running parity gate](rust-settlement-bot-parity.md#running-parity-gate-rsp-8-ergo-23dlcy)
and run `just test-settlement-parity`.
