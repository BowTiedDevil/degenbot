# Transition Guide: Sweeping Lingering Pure-Python Modules to the Three-Layer Architecture

> **Purpose.** ADR-005's stateful core (the `Bot`/`PyBot`/`PyLiquidityPool`/
> `PyErc20Token` family + the per-family companions) landed as slices 3–15 of
> the Polars three-layer migration (ergo `XQ5UX6`). That closed the
> **stateful** topology. The **stateless** half of the convention — pure-Rust
> math/decode/encode leaves wrapped by thin `#[pyfunction]`s and ultimately
> consumed by the companions — is only partially landed. Many Python modules
> still hold pure-math, decoding, and encoding logic that **already has a Rust
> counterpart** but is not yet routed through it, or that has no Rust port yet.
>
> This guide gives agents the guideposts to (1) evaluate a given module against
> the architecture, (2) move the remaining responsibility from Python to Rust,
> and (3) transition the tests. It is the rubric for the sibling ergo epic
> ("Three-layer architecture: sweep lingering pure-Python modules to Rust").

## 1. The three layers (recap)

Per `rust/AGENTS.md` "Three-Layer Pattern", specialized for shared state by
ADR-005, and realized across the Cargo workspace at `rust/crates/`:

| Layer | Where it lives | `pyo3`? | Holds |
|-------|----------------|---------|-------|
| **Rust core** | `rust/crates/degenbot-{core,-cl-math,-curve-math,-balancer-math,-abi,-decoders,-uniswap,-rpc,-bot}` | **none** (enforced by `just check-no-pyo3-in-cores`) | data + state-machine logic + pure math + protocols (DexIdentity, encoders, decoders) |
| **PyO3 wrapper** | `rust/crates/degenbot-python/src/<domain>/**` | all | `#[pyclass]`/`#[pyfunction]` only — arg extraction → GIL release → core call → result wrap. **No business logic.** |
| **Python companion** | `src/degenbot/**` | n/a | user-facing API, docstrings, I/O orchestration (SQLAlchemy, web3.py, publisher/subscriber, price oracle), immutable config dual-tracking, `Fraction`-based display |

The **standalone-Rust-core constraint** (ADR-005) is first-class: anything a
standalone Rust consumer (`examples/standalone_consumer.rs`, `cargo add
degenbot`) would need must live in a core crate from day one — never "move it
later," which strands it across the future crate boundary. `DexIdentity` is
the precedent: Rust-side at introduction, Python its first consumer.

Canonical references:
- `rust/AGENTS.md` — generic three-layer rule, the Nine Rules, GIL discipline, module-naming convention.
- `rust/CONTEXT.md` — glossary; {Polars-Inspired Three-Layer Architecture}, {PyBot}, {PyLiquidityPool}, {PyErc20Token}, {degenbot-decoders}, {degenbot-uniswap}.
- ADR-001 (I/O-free pools), ADR-003 (Bot as state owner), ADR-005 (FFI topology + crate-split target), ADR-006 (per-chain orchestrator), ADR-007 (unregister seam).
- `docs/architecture/rust-owned-bot.md` — component map + pump/engine lifecycle.

## 2. Evaluating a module: the triage rubric

Before touching a module, classify it. Run this rubric against every Python
file flagged by the sweep. The output of evaluation is **one of four
dispositions**, and the disposition decides the work:

### 2.1 The "where does this belong?" decision table

Adapted from `rust/AGENTS.md` "Porting Decision Framework":

| Criterion | Keep in Python (companion/`src/`) | Port to Rust (a core crate) |
|-----------|-----------------------------------|------------------------------|
| Hot path? | Called infrequently / at construction | Per-block, per-tx, in tight loops |
| GIL bottleneck? | Already releases GIL via I/O `await`/`.detach()` | CPU-bound, holds GIL during compute |
| Type shape? | Heavy Python-object manipulation (ORM, `WeakSet`, `Fraction`) | Pure numeric / byte / `Address` / `U256` |
| I/O shape? | ORM queries, WS subscriptions, price-oracle refresh lifecycle | Pure transform: logs→structs, int→int, bytes→calldata |
| Python-ecosystem coupling? | SQLAlchemy, web3.py, `publisher`/`WeakSet`, `Fraction` display | Standalone logic reusable without Python |
| Test shape? | Needs mocks/Anvil/RPC fixtures | Clear input→output, property-testable |

### 2.2 The four dispositions

1. **`done`** — the module's responsibility already lives in Rust and the
   Python side already delegates through the `Py*` wrapper (e.g. V2/V3/V4
   pool state and calc). **Action:** none. Record the disposition in the
   task body + close it. A fast, valid close; the sweep is a sweep, not a
   port-everything mandate.

2. **`partial`** — a Rust counterpart exists but the Python path is still the
   authority for some callsites (the common lingering case: a `degenbot-*-math`
   leaf exists but the companion's calc still calls the Python port).
   **Action:** finish the routing — make the companion call the Rust leaf via
   the `#[pyfunction]`/`#[pyclass]` seam, cross-check parity, delete the
   dead Python port.

3. **`port-now`** — no Rust counterpart yet, but the rubric says it belongs in
   Rust (pure math/decode/encode with no I/O or Python-ecosystem coupling).
   **Action:** land a new core leaf (or extend an existing one) under the
   three-layer discipline (§3), then route.

4. **`stays-python`** — the rubric says keep in Python (I/O, ORM, orchestration,
   price oracle, `Fraction` display). **Action:** none, but record *why* (which
   criterion), so the next sweep doesn't re-litigate.

### 2.3 The smell checklist

A module is likely `partial`/`port-now` if any of these hold:
- It is a 1:1 port of a Solidity/Vyper library and a `degenbot-*-math` leaf
  for that library already exists (`calculations/*`, `uniswap/v3_libraries/`,
  `uniswap/v4_libraries/`, `balancer/libraries/`, `aave/libraries/`,
  `curve/calculators/`).
- It hand-slices EVM log bytes and `degenbot-decoders` already decodes the
  same event (`uniswap/log_decoders.py`).
- It ABI-encodes/decodes and `degenbot-abi` already covers the signature
  (`abi_adapter.py`, `contract/decoding.py`).
- It holds mutable pool/token state that the companion now reads through a
  `Py*` handle, leaving it a dead mirror (`types/state_cache.py`,
  `uniswap/v{2,3,4}_pool_state.py`, `curve/stableswap_pool_state.py`,
  `aerodrome/v2_pool_state.py`, `uniswap/concentrated/*`).
- `rg` for the module's symbols shows zero live consumers outside the module
  and its own tests (dead code post-companion).

If `rg` shows the symbol is referenced only by parity/equivalence tests
against a Rust implementation, it is `partial` with the Python port retained
as the oracle — see §4.3 for the oracle-retirement protocol.

## 3. Moving responsibility from Python to Rust

The cutover discipline (from ADR-005 slice 11 "Curve family port", the
canonical three-sub-slice shape). Each port follows three ordered sub-steps;
**each leaves tests green** before the next starts (red-green per sub-step):

### 3.1 Sub-step A — pure-Rust core leaf (stateful: state port)

- New/extended code goes in the matching core crate (`rust/crates/degenbot-*-math`
  for pure math; `degenbot-decoders` for log decoders; `degenbot-uniswap` for
  DEX identity/encoding; `degenbot-abi` for encode/decode; `degenbot-bot` for
  state-coupled dispatch).
- **Zero `pyo3`** in the core file. Verify with `just check-no-pyo3-in-cores`
  (the gate walks `cargo tree` for pyo3 under default features for every core
  + the umbrella). If a `From<…> for PyErr` is needed, put it behind a
  non-default `pyo3` feature in the core (orphan-rule precedent in
  `degenbot-core::errors`).
- Idiomatic Rust, `Result<T, E>` returns, `#[cfg(test)]` unit tests —
  independently runnable without Python.
- Name the pure fn `*_rust()` or a plain `pub fn name()` in a `_py`-less file;
  never name a `#[pyfunction]` in a core crate.
- For a **math** port: a direct 1:1 port of the deployed contract arithmetic,
  cross-checked byte-for-byte against the Python oracle (see §4.2). Mirror the
  Solidity variable names where it aids direct-port correspondence
  (lint-allowed in `degenbot-balancer-math`/`-curve-math`).

### 3.2 Sub-step B — PyO3 wrapper (the `degenbot-python` binding layer)

- Lives in `rust/crates/degenbot-python/src/<domain>/**` (per-domain subdirs
  mirroring the cores: `abi/`, `cl_math/`, `rpc/`, `uniswap/`, `bot/`).
- `#[pyfunction]` for stateless ops (no `Py` prefix); `#[pyclass]` for
  stateful handles (keep the `Py` prefix unconditionally per ADR-005 — no
  `name=` override).
- **Thin translator only**: extract args → `py.detach()` for I/O/long CPU →
  call the core → wrap result. Extract owned data before `detach()`; the
  detached closure must not reference any `Bound<'_, PyAny>` (UB otherwise).
- Map errors at the boundary: Rust `Result` → Python exception via
  `From<…> for PyErr`. Cache Python refs with `PyOnceLock`
  (`conversion::cache`). Register every symbol in `c_api.rs`.
- Read/write guard discipline (ADR-005): stateful `#[pyclass]` wrappers
  classify each method read (`.read()`) or write (`.write()`).

### 3.3 Sub-step C — route the companion through Rust + delete the Python port

- The Python companion (`src/degenbot/**`) calls the Rust seam instead of the
  Python implementation. **Immutable config stays dual-tracked** (Rust carries
  it for the future math port; Python keeps it for its own calc/display) — this
  is the V3/V4/Curve/Balancer companion discipline.
- Update the *builder* to register via `PyBot::register_*` and hand the
  `PyLiquidityPool`/`PyErc20Token` handle to the companion ctor (mirror the
  `CurvePoolBuilder._register_pool` twin pattern + `make_*_pool` factories).
- Delete the now-dead Python implementation (no back-compat layer — per root
  `AGENTS.md`). Keep it only if it is the **parity oracle** for an
  as-yet-unrouted Rust calc (§4.3).

### 3.4 Cutover invariants (do not strand state)

- **One state owner.** Never leave a Python mirror of state that Rust now
  owns — that re-creates the retired `RustPoolCache`/`ArbPoolCacheAdapter`
  split (ADR-003 "delete, not migrate").
- **No dead Rust surface without a Python consumer**, and **no dead Python
  companion without a Rust backing** (except landed-but-inert standalone data
  like `DexIdentity`, whose first consumer is Rust unit tests).
- Each sub-step verified by: `cargo check`/`fmt --check`/`clippy --deny
  warnings`/`test` over the workspace; `just check-no-pyo3-in-cores`;
  `ruff check`/`ruff format --check`/`ty check` on `src/`; the affected
  companion's tests; import probe if the extension rebuilt.

## 4. Transitioning the tests

### 4.1 The test factory pattern (construction cutovers)

Every direct `Pool(...)`/`Token(...)` construction site in tests must route
through a `tests/helpers/<family>_factory.py::make_*` helper that builds via
`PyBot::register_*` → `get_pool`/`get_token` → companion — mirroring
`Bot.build_pool()` (ADR-005). Existing factories:
`v2_pool_factory.py`, `v3_pool_factory.py`, `v4_pool_factory.py`,
`curve_pool_factory.py`, `balancer_pool_factory.py`, `erc20_factory.py`,
`bot_factory.py`. Each call creates its own short-lived `PyBot` (the returned
handle holds an `Arc` clone, outliving the `PyBot`) so tests stay isolated.
A construction cutover = `sed`-swap the symbol + a `make_*` idempotent guard
(`get_*`-first because `Bot::register_*` asserts on duplicate).

### 4.2 The red-green parity cross-check

For a math/decode/encode port, **do not delete the Python port before the Rust
leaf is cross-checked against it**. The pattern (slices 5, 6, 11c):

1. **Red:** write the Rust leaf returning placeholder/`ZERO` values; the
   parity test (Rust result vs Python oracle over a fixture corpus = the
   *existing* Python port's outputs) fails.
2. **Green:** implement the Rust port as a direct 1:1 Solidity/Vyper
   transcription; the parity test passes byte-for-byte.
3. **Route + retire:** re-point the companion/the calc site at the Rust seam
   (§3.3); the Python port now has zero live consumers → delete it (and its
   parity tests, which become tautologies). Keep the Rust `#[cfg(test)]`
   corpus as the regression set.

Property-based (`proptest`) roundtrip encode→decode is the standard invariant
for ABI work; integer-exact agreement with the on-chain contract is the
standard for swap math (the `test_py_bot.py` parity tests are the canonical
reference and validate the *fee/retained-fraction convention*, which has bitten
ports before — see the slice-5 gamma-numer bug).

### 4.3 The oracle-retirement protocol

If a Rust calc exists but is not yet wired into the companion's calc path, the
Python port is the **parity oracle** and stays until the routing lands. When
routing lands, the oracle's parity tests are deleted *with* it. Do not leave an
"equivalence" test harness comparing two live implementations indefinitely —
the slice-12 `PiecewiseMobiusSolver` and the retired `_legacy/` package are the
precedent for deleting equivalence tests alongside the retired implementation.

### 4.4 Fork / Anvil tests

Fork-gated tests (marked with the anvil fixtures) validate calc parity against
on-chain `getAmountOut`/`get_dy`/slot0 — the ground truth a unit corpus can't
reach. A port must keep these green; if a fork test's premise was tied to a
deleted Python class, rewrite it under the companion+`dex.variant` model (see
`docs/migration-guides/dex-subclass-collapse.md` "Fork tests pending a
follow-up rewrite"). Four fork files were module-skipped there — each port task
should re-enable or close them.

### 4.5 Delegation-detection tests

When a companion **delegates** calc to Rust (slice 5), assert "Rust was hit
with the right args" rather than only "result matches" (parity tests already
cover the math). The `_DelegateSpy` pattern wraps a `PyLiquidityPool` to record
`calculate_tokens_out/in` calls via `__getattr__` pass-through. Use this for
any routing cutover where the math already has a parity test — it proves the
delegation seam, not just the numbers.

## 5. Validation gates (run after every sub-step)

```
just test-rust            # cargo test --workspace (Rust unit + integration)
just test-rust-python     # pytest tests/rust (PyO3-wrapped Python tests)
just test-python          # full pytest suite (compile-test-contracts first)
just lint-rust            # clippy --fix --all-targets --deny warnings
just check-no-pyo3-in-cores  # cores + umbrella pyo3-free under default features
just lint-python          # ruff + ty
just format               # cargo fmt + ruff format (apply); --check variants for CI
just lint-markdown        # if docs touched
just lint-context-maps    # if CONTEXT.md touched
just lint-commits         # commit-range commitlint
```

The extension is auto-rebuilt on import by maturin — **do not** manually rebuild
it, recreate the venv, or reinstall the package after Rust changes; any
`uv run`/`cargo test` triggers a rebuild if needed.

## 6. What already landed (don't redo)

ADR-005 slices 3–15 (ergo `XQ5UX6`, all done) closed the **stateful** topology:
- Slice 3 — `Erc20Token` companion over `PyErc20Token`.
- Slices 4–5 — V2 companion (state + calc delegation) over `PyLiquidityPool`.
- Slice 6 — `DexIdentity` + DEX presets in `degenbot-uniswap` (Rust core).
- Slice 7 — V2 DEX subclass collapse (Sushi/Pancake/Swapbased/Camelot hollow
  subclasses deleted; `LiquidityPool` + `dex.variant`).
- Slices 8–9 — V3/V4 companions over `PyLiquidityPool`.
- Slice 10 — `UniswapEngine` lock unification onto shared `Arc<RwLock<Bot>>`.
- Slices 11–12 — Curve + Balancer family ports (state + companion + pure-math
  leaves `degenbot-curve-math`/`degenbot-balancer-math`).
- Slice 13 — crate split (`degenbot-core`/`-python`/umbrella `degenbot` +
  `examples/standalone_consumer.rs`).
- Slice 14 — `PyBotIo` stateful I/O struct (sync RPC choreography ported;
  the Python `SyncPoolIO` stays as parity gate).
- Slice 15 — pickle multiprocessing retired + Rust-side parallel solve fan-out.

BLocked: ADR-003 `Bot`=state + engine=solving split; ADR-006 one `Bot` per
chain; ADR-007 unregister seam.

The remaining work is the **stateless leaves not yet wired** (math/decode/
encode) and **dead Python mirrors** of now-Rust-owned state — that is the
sweep this guide serves.

## 7. Anti-patterns to avoid

- **Keeping a Python mirror of Rust-owned state** "for the tests." Rewrite the
  tests against the `Py*` handle instead (ADR-003 "delete, not migrate").
- **Adding `pyo3` to a core crate file** outside a feature gate. It's a code
  smell and fails `just check-no-pyo3-in-cores`.
- **Business logic in the binding layer.** The wrapper extracts, calls, wraps.
  If logic crept in, move it to the core.
- **Dropping the `Py` prefix** or adding a `#[pyclass(name = "…")]` override.
  ADR-005 keeps the prefix unconditionally.
- **Leaving an equivalence/parity test harness** comparing two live
  implementations after the routing cutover — it becomes a tautology and hides
  regressions; retire it with the oracle (§4.3).
- **Landing standalone data on the Python side first** "to move later." The
  standalone constraint means it lands in a core crate from day one
  (`DexIdentity` precedent).
- **Manual extension rebuild / venv recreate.** Maturin rebuilds on import.