# degenbot — Domain Glossary

Ubiquitous language for the degenbot codebase. Terms here are the canonical
names used in architecture reviews (the `/improve-codebase-architecture`
skill), ADRs, and the three-layer transition. Keep this current as
deepening decisions crystallize.

## Architecture (from `/codebase-design` vocabulary)

- **Module** — anything with an interface and an implementation (function, crate, package, tier-spanning slice). Not "component"/"service."
- **Interface** — everything a caller must know to use a module: types, invariants, ordering, error modes, config, performance. Not "API"/"signature."
- **Depth** — leverage at the interface: behaviour per unit of interface. **Deep** = much behaviour behind a small interface; **shallow** = interface nearly as complex as the implementation.
- **Seam** — where a module's interface lives; a place to alter behaviour without editing in that place. Not "boundary."
- **Adapter** — a concrete thing satisfying an interface at a seam (role, not substance).
- **Leverage** — what callers get from depth (capability per unit of interface). **Locality** — what maintainers get (change/bugs/knowledge concentrate in one place).

## Three-layer system (ADR-005)

- **Rust core** — `rust/crates/degenbot-{core,-cl-math,-curve-math,-balancer-math,-abi,-decoders,-uniswap,-rpc,-bot,-pools,-db,-simulation,-submission,-price,-solvers,-executor,-fork,-pathfinding,-pool-updater,-aave-updater,-solidly-math,-evm-math,-v2-math}`. Zero `pyo3` (enforced by `just check-no-pyo3-in-cores`). `degenbot-solvers` owns the full pure solve layer (V2/CL Möbius + Balancer/Curve/Solidly dispatch + QuantAMM basket + the hop-state intake contract — ADR-015); `degenbot-bot` owns `resolve_path` (the core-bound projection) and the I/O orchestration only.
- **PyO3 wrapper** — `rust/crates/degenbot-python/src/<domain>/**`. `#[pyclass]`/`#[pyfunction]` only — arg extract → GIL release → core call → result wrap. No business logic.
- **Python companion** — `src/degenbot/**`. User-facing API, docstrings, I/O orchestration, immutable config dual-tracking, `Fraction`-based display.

### Pool structural families

The seven `PoolEntry` variants fall into **three structural families**, grouped by state-field shape and delta shape — not by DEX. The family names are load-bearing vocabulary in architecture reviews and the `BotState` deepening.

- **Reserve-pair** — `reserve0/1: U112`, `update_block`, full-state delta (`V2BlockDelta`). Members: V2, AerodromeV2 (Aerodrome's `AerodromeV2PoolState.journal` is literally `ReorgJournal<V2BlockDelta>` — the variant shares the V2 delta). Apply: `apply_*_sync` (overwrites reserves).
- **Balance-vector** — `balances: Vec<U256>`, `update_block`, full-state delta (`BalancesBlockDelta` — the three nominally-distinct `CurveBlockDelta`/`BalancerWeightedBlockDelta`/`BalancerStableBlockDelta` structs are byte-identical and unify here). Members: Curve, Balancer-weighted, Balancer-stable. Apply: `apply_*_balance_update` (overwrites balances).
- **Concentrated-liquidity (CL)** — slot0 scalars (`sqrt_price_x96`/`liquidity`/`tick`) + `tick_data: HashMap<i32, TickInfo>`, partial-prior delta (`V3BlockDelta` with `scalar_priors`/`tick_priors`). Members: V3, V4 (structurally near-identical; differ in identity shape and V4's `pool_key` nesting). Apply: `apply_swap` (changes slot0) / `apply_liquidity_update` (tick-only).

**Trait discipline.** State-struct traits are adopted **only for the CL family** — `ConcentratedLiquidityPool` (read, rename of the legacy `V3FamilyPool`) + `ConcentratedLiquidityPoolMut` (write). V3 and V4 are two adapters behind the same per-pool interface, so by the two-adapter rule the seam is real. For reserve-pair and balance-vector, the duplication sits one layer down — on `ReorgJournal` / `BlockDelta` — and dedups there (unify the balance-vector deltas to `BalancesBlockDelta`; extend `BlockDelta` with `type RestoreState` + `landed()` to collapse five hand-duplicated `restore_*_before_block` impls into one generic `impl<D: BlockDelta> ReorgJournal<D>`). The V3 family keeps its own restore impl — `V3RestoreResult` + the scalar/tick-priors branches are a genuinely different algorithm, not a full-state delta. See ADR-014 for the formal record of the trait-vs-journal-layer split.

### Resolve→solve boundary (ADR-015)

**Current shape — pure solve layer split across two crates by an accidental line.** `degenbot-solvers` holds the V2/CL Möbius solvers (`mobius_int`, `mobius_int_exact`, `mobius_v3_int`, `affected_keys`) — “value-only solver math … no chain/registry/async/tokio … consumable by both the standalone Rust path and the PyO3 driver shell.” But the rest of the pure solve family stayed behind in `degenbot-bot/src/solvers/arb_engine/` (the I/O orchestrator: tokio, `core: Arc<RwLock<BotState>>`, path registry, V3/V4 buffers): `solve_path` (the dispatcher) + `solve_*_path_int` (Balancer weighted/stable, Curve, Solidly), the `simulate_*_hop` swap leaves, `balancer_weighted_basket.rs` (QuantAMM), and the hop-state value types. A standalone `cargo add degenbot` consumer got V2/CL but not Balancer/Curve/Solidly/basket.

**Decision (ADR-015, 2026-07-19):** complete the seam. The pure solve layer — `solve_path` + all `solve_*_path_int` arms + `simulate_*_hop` leaves + the QuantAMM basket solver + the hop-state value types (`ResolvedHop`, `ResolvedMixedPath`, `SolvePathResult`, `*HopState`, `HopType`, `MixedPoolRef`, `PoolHop`) — moves to `degenbot-solvers`. `degenbot-bot` keeps `resolve_path` (the only core-bound step). The orchestrator’s solve-side import collapses to `degenbot_solvers::solve_path(&resolved)`. The dep graph stays a DAG (the new deps are leaf math crates `pools` already depends on); no-pyo3 invariant preserved.

**Hop (ubiquitous language).** A hop is **the solver’s snapshot-and-classifier adapter** — not a pool concept, not a math-leaf concept. It does two jobs, both for the solver:
- *Snapshot role (= selectivity).* It captures pool state at resolve time so the solve can run lock-free under rayon `par_iter` on a `Clone`+`Send` value. For CL specifically it is a *selective projection* — `build_int_v3_sequence` walks `tick_data` once in the swap direction, caps at ≤15 tick ranges, pre-accumulates `liquidity_net`, replaces map lookups with `TickMath` constants — NOT a clone of the pool (which is thousands of `TickInfo`). The balance-vector family pre-computes the invariant `D` (one Newton run, not ~25× during the golden-section) and BPT-skips. Live-read over the pool re-pays the projection ~25× or relocates it as cache-on-state with invalidation spread across every `apply_*`.
- *Classifier role.* The `ResolvedHop` enum variants let `solve_path` pattern-match on path composition to pick the algorithm (closed-form Möbius for all-V2/all-CL; golden-section for paths involving non-Möbius leaves). A `dyn PathHopSnapshot` trait variant would re-invent this as a capability query (a thinner hop behind a trait).

**Solver intake contract.** The hop-state types are `degenbot-solvers`’s intake protocol — `degenbot-bot`’s `resolve_path` projects `BotState` into them under `core.read()`, then the guard drops, then `degenbot_solvers::solve_path` runs lock-free. The lock-drop discipline (ADR-005 slice 15b-1: “the guard drops before `solve_path` runs”) is load-bearing and survives the relocation unchanged.

**Deferred — the hop-shape deepening.** Open: does `ResolvedHop` dissolve behind a `trait PathHopSnapshot { simulate; mobius_shape }`? Deferred to a separate session; resumes with the constraints in ADR-015. **The digest-cost argument is retired** (2026-07-19 spike, ergo 77LOQT, `rust/crates/degenbot-solvers/benches/digest.rs`): Balancer stable `D` = 1.0–1.6 µs, Curve `xp` = 63–114 ns — both <4% of the per-path budget against 82/144 µs Phase B solves. The "extend CL's digest memoization to Balancer/Curve" candidate is empirically rejected (CL caches justifiably — its O(N log N) tick walk is 10×+ heavier; the light families don't amortize and a stale-digest reorg bug is worse than ~1% wall time). The frozen-per-solve snapshot survives on constraint #5 alone (lock-free solve: guard drops before `solve_path`) — a *where-the-digest-lives* argument, not a cost argument.

### Construction-I/O executor

**Current shape — `PyBotIo`** (Rust `#[pyclass]`, `degenbot.bot.PyBotIo`)
is the sole construction-I/O executor: every builder's `build()`/`update()`
and the type-resolution + tick-fetcher paths receive `io: PyBotIo` and call
`io.fetch_X()` / `io.probe_X()` directly (ADR-005 slice 14). The Python
`PoolIO`/`SyncPoolIO`/`AsyncPoolIO` protocols and the encode→call→decode
parity-gate fallbacks are deleted; `AsyncBot` and the async builders are
retired (sync `Bot` + `PyBotIo` is the only construction path). `PyBotIo`
also implements the 7-method generic RPC surface (`call`/`call_raw`/
`get_block*`/`get_code`/`get_balance`) used by the Curve detection modules.
`AsyncAlloyProvider` survives for the pump/subscribe/verify loop only —
never for construction.

**Shipped (slice A) — `ConstructionIo` core trait (architecture review,
2025-07-18).** Construction-I/O is deepened behind a core trait owned by
`Bot` (ADR-003: `Bot` is the single state owner; construction-I/O is part of
its lifecycle, so the handle belongs on `Bot`, not a side-channel
`#[pyclass]`). Two-trait split — one seam per concern, matching the
`TickMapDb` / `TickBootstrapRpc` precedent:

- **`DbConstruction`** — async trait, 12 methods covering the construction-
  time DB reads/writes (`fetch_erc20_token`, `fetch_pool_row`,
  `fetch_pool_kind`, `fetch_token_by_id`, `fetch_exchange`,
  `fetch_liquidity_positions`, `fetch_initialization_maps`,
  `fetch_pool_manager`, `fetch_v4_pool_by_pool_hash`,
  `fetch_managed_liquidity_positions`, `fetch_managed_initialization_maps`,
  `update_erc20_token_metadata`). Returns `degenbot_db::rows::*` core rows
directly
  (no `Py*` mirror at the trait). Propagates `DbError` **loudly** (Decision
  8 (A) — the trait never swallows; the choreography decides whether to
  degrade). Native adapter `DegenbotDbConstruction` holds a **persistent**
  `DegenbotDb` (held connection, not per-call `DegenbotDb::open` — matches
  XEANMB; deletes the 12×-open boilerplate).
- **`RpcConstruction`** — async trait, 7 generic RPC methods
  (`get_block_number`, `get_block`, `get_block_timestamp`, `get_code`,
  `get_balance`, `call`, `call_raw`). Native adapter `AlloyRpcConstruction`
  wraps `degenbot-rpc`'s `AlloyProvider`. **Alloy-only** — the legacy non-alloy
  Python-provider fallback is dropped from the trait (the `PyBotIo` choreography
  fallback retains it temporarily; deleted with the builder-choreography port).
- **`ConstructionIo`** — composite handle (`Arc<dyn DbConstruction> + Arc<dyn
  RpcConstruction>`) held by `Bot`. The no-DB path is a **`NoDb` adapter**
  (every method returns `None`/empty), not an `Option` at the call site —
  `ConstructionIo.db` is always `Some`. `NoDb` doubles as the first
  in-memory test fake.

`PyBotIo`'s 12 DB + 7 generic RPC methods now delegate through the trait
objects (slice A); `PyBotIo` retires fully once the 27 choreography wrappers
move core-side (the builder-choreography port). Builders receive
`&ConstructionIo` (sourced from `Bot`), not a parallel I/O object. The 27
choreographed encode→call→decode wrappers (the `fetch_v2_reserves` /
`fetch_v3_slot0_*` / `fetch_balancer_*` family) stay on `PyBotIo` for this
slice, composing over the trait's `call` / DB primitives; they move core-side
in a follow-up (the builder-choreography port). Held-tx sharing between
`DbConstruction`'s connection and `tick_assembly`'s `SnapshotDb` held-tx
is a separate, later slice.

Migration note: `docs/migration-guides/construction-io-trait.md`. The formal
record of the Construction-I/O trait + adapter pattern lands as a future
ADR (the ADR-014 slot is taken by the pool-state-deepening decisions —
see `docs/adr/ADR-014-pool-state-deepening-layer.md`).

**Posture (Decision 8 (A), unified):** DB errors propagate at the trait;
the choreography decides whether to degrade. This unifies the codebase
under the posture `tick_assembly` already established as canonical —
"Do NOT restore the swallow."

**Breaking change (0.6.x):** non-alloy Python providers are no longer
supported for construction. Migration note: supply a `PyAlloyProvider`.
An ADR recording the I/O-seam-is-core / alloy-only / loud-error posture
will land with the slice.

## The `_ffi` seam (Pydantic barrier — DECIDED)

**Decision:** `degenbot._ffi` is **private** — a raw Rust extension imported by ONE barrier per domain, never by leaf code. Model: pydantic-core (`_pydantic_core` is imported only by `pydantic_core/__init__.py`; the companion `pydantic` never touches it). Replaces degenbot's prior mixed state (ban test + allowlist back-door + direct `_ffi.<sub>` leaf imports).

- **Ban rule (target):** "no file outside its domain's barrier module may contain `degenbot._ffi`." Mechanically enforceable; no allowlist, no submodule-vs-symbol distinction.
- **Home placement:** 1:1 mirror — every consumed `_ffi.<sub>` maps to a `degenbot.<domain>` home. Cross-cutting concerns are elevated to first-class domains (not a `common` junk-drawer), but homes are created lazily on first Python consumer (no empty pass-throughs for dead submodules).
- **Survey basis:** Polars (leaf-imports `_plr` freely, no ban), Pydantic (strict one-barrier, `_` truly private), cryptography (`bindings/_rust` namespaced), Ruff (thin CLI, N/A). Pydantic is the match because degenbot's ban test already signals "private" intent.

## Dispositions (per `_ffi.<sub>`)

### `deployments` — STAYS, correctly placed

- **Home:** `degenbot.uniswap::deployments` (Rust) → `degenbot.uniswap.deployments` (Python mirror).
- **Not eliminated.** The factory→identity lookup (`resolve_deployer`, `resolve_v2/v3_init_hash`, `verify_v2/v3`) is the standalone-Rust-core verification mechanism (ADR-005 / Fork A, JC6OFG): `register_v2/v3_pool` re-resolves `(deployer, init_hash)` from the embedded JSON and verifies the CREATE2 address at registration time. A standalone `Bot` verifies with no Python; if the builder carried identity in, Rust would trust rather than verify. Presets cannot replace it.
- **Not cross-cutting.** PancakeSwap/SushiSwap/Swapbased/Camelot/Aerodrome are Uniswap-V2/V3 protocol forks; their deployment identity is Uniswap-protocol-family data. One boundary handling all V2-style DEXes via `factory + variant_tag` is slice 7's deliberate collapse. `degenbot-uniswap::deployments` is the Uniswap family's identity module, not a cross-cutting registry.
- **Python work:** reroute `_ffi.deployments` leaf imports → `degenbot.uniswap.deployments`. No Rust structural change.
- **Deferred:** 11 Balancer factory rows in the JSON for "exhaustive lookup" — the one cross-family leak. Carve out to `degenbot.balancer.deployments` when Balancer gets an identity crate (today only `degenbot-balancer-math` exists).
- **Deferred:** whether `resolve_*` / `verify_*` free functions become methods on a typed `DeploymentRegistry` is a deepening question, not a mirror dependency.

### Cross-cutting submodules — 1:1 mirror, UNIFORM

**Decision (uniform):** every consumed `_ffi.<sub>` maps to a `degenbot.<sub>` home at the top level — including cross-cutting concerns, which are elevated to first-class domains (not a `common` junk-drawer). Homes are created lazily on first Python consumer; dead submodules stay un-homed. Top-level grab-bag `.py` files dissolve *into* their mirror home (the file's content moves into the package, not preserved as a floating peer).

- **`abi`** → `degenbot.abi`. The home bridges the Rust `degenbot-abi` core (encode/decode/decode_single) with EIP-55 checksumming. No `eth_abi` fallback — Rust is the only backend. Consumers: `contract/decoding.py`, plus `aerodrome`/`aave`/`builders/*` via `degenbot.abi`.
- **`contract`** → `degenbot.contract` (already a package). `crypto.py` reaches `_ffi.contract` for `get_function_selector`; reroute to `degenbot.contract.get_function_selector`.
- **`crypto`** → `degenbot.crypto`. Top-level `crypto.py` (81 lines: `function_selector`, `keccak256`, `event_topic`) becomes `degenbot.crypto`. Note: `function_selector` currently delegates to `_ffi.contract`; under the mirror it re-exports from `degenbot.contract`. `keccak256` is the one remaining non-Rust hashing surface (wraps `eth_utils.crypto.keccak`) — a follow-up exposes a Rust `keccak256` pyfunction.
- **`fork`** → `degenbot.fork`. Top-level `anvil_fork.py` (514 lines) becomes `degenbot.fork`, mirroring `_ffi.fork`.
- **`db`** → `degenbot.db` (genuine cross-cutting infrastructure — DB row types + ops, consumed by `database/`, `cli/`, `exceptions/`, `updater/`). Existing `database/_ffi.py` is the partial barrier; consolidate to `degenbot.db` as the single mirror home.
- **`deployments`** → `degenbot.uniswap.deployments` (see above — Uniswap-protocol-family identity, not cross-cutting).
- **`price`** → **not a single home.** `_ffi.price` exposes two distinct pyclasses consumed by two different domains: `PyChainlinkPriceFeed` → `degenbot.chainlink` (already re-exported from `chainlink/__init__.py`), `PyAavePriceOracle` → `degenbot.aave` (already re-exported from `aave/__init__.py`). The Rust crate `degenbot-price` is implementation; its pyclasses belong to their consuming domains, not a shared `degenbot.price`.

### Dead / test-only submodules (un-homed, lazy)

- **`executor`** — 0 callers anywhere (production or test). Truly dead surface. Stays un-homed; a `degenbot.executor` home appears only if a Python consumer lands. (The `contracts/` Vyper executor + `degenbot-executor` Rust crate exist, but no Python leaf reaches `_ffi.executor`.)
- **`subscriber`** — 0 production callers; test-only (`tests/fakes/subscribers.py`, `tests/test_pubsub_seam_parity.py` reach `_ffi.subscriber` for `PySubscription` / `register_subscriber`). Un-homed for production; when a production consumer appears it gets `degenbot.subscriber`. The test fakes import directly from `_ffi.subscriber` — under the strict Pydantic barrier, test code is leaf code and must import from the home once it exists; until then the test imports are the signal that a home is needed.

### Clean single-home submodules (reroute only)

`balancer_math` → `degenbot.balancer.math` · `cl_math` → `degenbot.uniswap.math` · `curve_math` → `degenbot.curve.math` · `solidly_math` → `degenbot.aerodrome.math` · `solady` → `degenbot.utils.solady` (existing subpackage `utils/solady/libzip.py` is the only consumer; mirrors 1:1) · `dex_identity` → `degenbot.uniswap.dex_identity` · `provider` → `degenbot.provider` (already a package) · `simulation` → `degenbot.dispatch` · `submission` → `degenbot.dispatch` (both under `dispatch/`) · `cancel` → `degenbot.updater` · `pool` → CLI-only consumer (`cli/pool.py`); `degenbot.pool` mirror home created when a non-CLI consumer appears, or `cli/pool.py` is the home itself if the CLI stays the sole consumer (verify during cutover).
