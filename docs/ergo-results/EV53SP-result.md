# EV53SP — P2b: retire the injected snapshot path

Agent: `p2b-snapshot-retire` (dispatched worker). Tree left dirty; nothing committed.

## What changed (files + intent)

### Production (src/)

- `src/degenbot/arbitrage/engine_registry.py`
  - `start()` no longer accepts `v3_snapshot=` / `v4_snapshot=` kwargs; the injected
    `S = min(newest_block)` arm and the `self.engine.snapshot_seed_block = ...` setter
    call are gone. `start()` now reads `S` back from the shared `BotState`
    (`snapshot_block = self.engine.snapshot_seed_block`, line 179) — the DB path
    records it at `Bot` construction via `load_snapshot_from_db`; a cold start
    reads `None`. It still stashes `self._verify_snapshot_block` for the per-pool
    step-1 verify seed (unchanged consumer: `register_v3_pool` / `register_v4_pool`).
  - Removed the now-unused `UniswapV3LiquiditySnapshot` / `UniswapV4LiquiditySnapshot`
    TYPE_CHECKING imports.
  - Purged the stale `load_*_from_py` narrative: module docstring, `start()`
    docstring/comment, the `register_v3_pool` / `register_v4_pool` "fed via
    load_v*_snapshot_from_py" docstrings, and the `__init__` `_verify_snapshot_block`
    comment ("min(newest_block) across supplied snapshots").
- `src/degenbot/runner/bot_runner.py`
  - `InjectedActors.snapshots` field deleted, along with `self._injected_snapshots`
    wiring and the `_start_snapshots()` resolver (whole method removed).
  - `start()` no longer computes/passes snapshot kwargs; `engine_registry.start(...)`
    is called with `cfg.node_http`, `cfg.node_ws`, `verify_state_view=` only.
  - `self._injected_bot = None` (trim/release path, ~line 962) **kept**.
  - Docstrings de-snapshotted (module + class "stream snapshots" sequencing line,
    injectable-seam lists).

### Tests

- `tests/fakes/engine.py`: `FakeEngine.__init__` gains a `snapshot_seed_block: int | None`
  param (seeds `self._snapshot_seed_block` without recording a setter call);
  `FakeEngineRegistry.start()` drops the required `v3_snapshot` / `v4_snapshot`
  kwargs and no longer records them.
- `tests/helpers/boot_actors.py`: `snapshots=` removed from the `InjectedActors` build.
- `tests/arbitrage/test_engine_registry_start.py`: the two snapshot-kwargs tests
  migrated to seed the engine directly — `test_start_reads_the_snapshot_seed_block_from_the_engine`
  (asserts the seed is READ, `fake.seed_args == []`, then subscribe/verify) and
  `test_start_stashes_snapshot_block_for_two_step_verify`; the `_FakeSnapshot` double
  deleted; stale "non-DB setter" comments corrected.
- `tests/arbitrage/test_engine_registry_two_step_verify.py`: `_registry_started_with_snapshots`
  → `_registry_started_with_seed` (`FakeEngine(snapshot_seed_block=18_000_050)`);
  `_FakeSnapshot` deleted.
- `tests/arbitrage/test_arbitrage_session.py`: removed the `snapshots=` DI kwargs
  (30 default `(None,None,None,None)` lines + 3 real-object sites); dropped the
  `v3_snapshot`/`v4_snapshot` `start_calls` assertions; the local `_Registry.start`
  stub signature de-snapshotted.
- `tests/arbitrage/test_cockpit_phase.py`, `test_cockpit_session.py`,
  `test_cockpit_session_owner.py`: `snapshots=(object(), object(), None, None)`
  removed.
- `tests/arbitrage/test_run_ritual.py`: `_StubHost.v3_snapshot` / `.v4_snapshot`
  dead attributes removed.
- `tests/arbitrage/test_solvers/test_arbitrage_engine.py`: deleted the unused
  `_make_v3_snapshot` / `_make_v4_snapshot` helpers (referenced the retired
  `load_*_from_py`).

## Acceptance criteria evidence

1. `rg -n 'v3_snapshot|v4_snapshot|_start_snapshots|snapshot_seed_block'
   src/degenbot/arbitrage/ src/degenbot/runner/` →
   - `engine_registry.py:36-37` — the retirement-note comment (mentions the retired
     `load_*_from_py` names; not a live seam).
   - `engine_registry.py:179` — `snapshot_block = self.engine.snapshot_seed_block`,
     the **live DB arm read** (the intended surviving production reference).
   No `v3_snapshot` / `v4_snapshot` / `_start_snapshots` remain; the injected-seam
   names are gone from `src/`.
2. The DB arm is the only `S` source: `Bot` construction loads the snapshot
   (`bot/_bot.py:200` → `bot/mod.rs:383`) and records `S` on `BotState`; `start()`
   only reads it back. Cold start → `None`.
3. Gates: see below.
4. This sign-off; task closed with `ergo set` (JSON argument).

## Gates run (foreground)

| gate | command | exit |
|---|---|---|
| Python lint (touched src) | `uv run --no-sync ruff check src/degenbot/arbitrage/engine_registry.py src/degenbot/runner/bot_runner.py` | **0** — All checks passed |
| build receipt | `just rebuild-if-stale` | **0** — rebuilt the stale extension (installed 2328/567200432eb1b4f8 → fresh 2332/58ae1018d4a8a043) |
| registry + runner unit suites | `uv run --no-sync pytest tests/arbitrage/test_engine_registry_start.py tests/arbitrage/test_engine_registry_two_step_verify.py tests/arbitrage/test_run_ritual.py tests/arbitrage/test_cockpit_phase.py tests/arbitrage/test_cockpit_session.py tests/arbitrage/test_cockpit_session_owner.py tests/arbitrage/test_solvers/test_arbitrage_engine.py -q` | **0** — 80 passed |
| session suite | `uv run --no-sync pytest tests/arbitrage/test_arbitrage_session.py -q` | **0** — 46 passed |
| whole arbitrage dir + relay posture | `uv run --no-sync pytest tests/arbitrage/ tests/dispatch/test_relay_posture.py -q` | **0** — 531 passed, 1 deselected |
| import sanity | `uv run --no-sync python -c "from degenbot.runner.bot_runner import InjectedActors; ..."` | `InjectedActors.snapshots` absent |

Note on ruff: the repo's `ruff check` (default config) flags 9 **pre-existing** errors in
the touched *test* files (`import-outside-top-level`, `invalid-function-name`,
`unused-import`, `pytest-*` style). They exist on `HEAD` too and are outside the repo's
lint gate (`just lint-python-check` = `ruff check src/ examples/`). The touched **src**
files are clean.

## Deliberately left (out of the p2b scope)

- `src/degenbot/_ffi/__init__.pyi:485-505, 1497` — the `snapshot_seed_block` docstring
  still names `load_v3_snapshot_from_py` / `load_v4_snapshot_from_py`. This is a
  **generated** stub (`just gen-stubs --check` gates it) whose source is the Rust
  `bot/engine/mod.rs` doc — Rust is explicitly out of scope, so the stub was not
  hand-edited.
- `src/degenbot/bot/_bot.py` (owned by the concurrent P3 worker) — its stale
  `load_*_from_py` comment is tracked on P4a; untouched.
- The Python snapshot facade modules (`v3_snapshot.py` / `v4_snapshot.py`), the Rust
  `DatabaseSnapshot` pyclass seam, the public snapshot exports, the golden test's
  inline `snapshot.pending_updates` application, and the `snapshot_binary.py` /
  `RETIRED_ENGINE_MEMBERS` tombstones — all KEPT per the P2a inventory.
- No Rust file touched.

## PENDING (manager decides)

None — every gate ran foreground to completion.

## Tree / foreign activity (named, not touched)

Concurrent sessions were editing the Rust tree throughout: `degenbot-solvers`
(`mixed/*`, `profit_envelope/*`, new `solidly_t.rs`), `degenbot-aave/*`,
`degenbot-cli*`, `aave_updater.rs`, `_ffi/aave.pyi`, `docs/cli/aave.md`,
`docs/rust-cli.md`; and a Python session changed `src/degenbot/discovery.py` and
`src/degenbot/__init__.py`. All left completely alone. The `just rebuild-if-stale`
run blocked briefly on the shared cargo build lock (a foreign compile was in flight)
then compiled the foreign Rust edits and installed a fresh extension.
