# degenbot — Domain Glossary

Ubiquitous language for the degenbot codebase. Terms here are the canonical
names used in architecture reviews (the `/improve-codebase-architecture`
skill), ADRs, and the three-layer transition. Keep this current as
deepening decisions crystallize.

## Settlement arbitrage (the bot's strategy)

**"Backrun" is a legacy label in the codebase, not a description of the
mechanism** (canonical decision record:
[ADR-026](docs/adr/ADR-026-backrun-to-settlement-arbitrage-terminology.md)). Do not read the crate name `degenbot-arbitrage`, the
example `eth_backrun_*.py`, or the ADR-019/025 references as classic
victim-transaction backrunning. The bot's strategy is **block-settlement
arbitrage**, and the two must never be conflated:

- **Backrun (classic MEV — NOT this bot):** position a transaction/bundle
  immediately *after a specific, identified victim transaction*
  (mempool-ordered), profiting from that victim's price impact. A *named
  victim tx* and ordering relative to it are essential; you watch the
  mempool for a specific flow.
- **Settlement arbitrage (this bot):** after a block settles and its trades
  shift pool states, arbitrage the resulting cross-pool price discrepancies
  with a transaction at the head of the next block. There is **no labeled
  victim** — the opportunity is the post-settlement *state discrepancy*
  itself, detected by the solver from settled pool-state changes (no mempool
  victim watching, no tx-to-tx ordering).

The defining properties are: (1) the opportunity source is a **settled
pool-state discrepancy** (not a victim's flow), and (2) execution is a single
transaction at the **next-block head** (not ordered against a specific tx).
Use **"settlement arbitrage"** (equivalently "block-settlement / next-block /
state-driven arbitrage") when describing the opportunity. Keep **"backrun"
only as the legacy name** of the crate, example, and execution-strategy
adapter (`degenbot-arbitrage`) — never to describe the mechanism.

## Architecture (from `/codebase-design` vocabulary)

- **Module** — anything with an interface and an implementation (function, crate, package, tier-spanning slice). Not "component"/"service."
- **Interface** — everything a caller must know to use a module: types, invariants, ordering, error modes, config, performance. Not "API"/"signature."
- **Depth** — leverage at the interface: behaviour per unit of interface. **Deep** = much behaviour behind a small interface; **shallow** = interface nearly as complex as the implementation.
- **Seam** — where a module's interface lives; a place to alter behaviour without editing in that place. Not "boundary."
- **Adapter** — a concrete thing satisfying an interface at a seam (role, not substance).
- **Leverage** — what callers get from depth (capability per unit of interface). **Locality** — what maintainers get (change/bugs/knowledge concentrate in one place).

## Three-layer system (ADR-005)

- **Rust core** — `rust/crates/degenbot-{core,-concentrated-liquidity-math,-curve-math,-balancer-math,-abi,-decoders,-uniswap,-rpc,-bot,-pools,-db,-simulation,-submission,-price,-solvers,-executor,-fork,-pathfinding,-pool-updater,-aave-updater,-solidly-math,-evm-math,-v2-math}`. Zero `pyo3` (enforced by `just check-no-pyo3-in-cores`). `degenbot-solvers` owns the full pure solve layer (V2/CL Möbius + Balancer/Curve/Solidly dispatch + QuantAMM basket + the hop-state intake contract — ADR-015); `degenbot-bot` owns `resolve_path` (the core-bound projection) and the I/O orchestration only.
- **PyO3 wrapper** — `rust/crates/degenbot-python/src/<domain>/**`. `#[pyclass]`/`#[pyfunction]` only — arg extract → GIL release → core call → result wrap. No business logic.
- **Python companion** — `src/degenbot/**`. User-facing API, docstrings, I/O orchestration, immutable config dual-tracking, `Fraction`-based display.

### Pool registration lifecycle (D4 / IKGQ6F)

Canonical terms for the CL (V3/V4) pool registration verify-lifecycle — the
per-pool `Quarantined → drain+verify → Live` sequence owned by the Rust core.

- **Registration lifecycle** — the per-pool `Quarantined`/`Live` state a
  registered CL pool is in; for a Sparse pool it is always `Live`, for a
  Tracked pool it is `Quarantined` until its verification passes.
- **Quarantined** — a registered CL pool whose live `Swap`/`Mint`/`Burn`
  events are deferred to the pump buffer (during drain+pin+verify) instead of
  applied directly, so the pin's `update_block` cannot outrun
  `last_complete_block`. Only ever applied to `Tracked` pools.
- **Live** — the steady-state direct-apply contract; the **only solvable
  state**. A pool is not solvable while `Quarantined`.
- **Tracked** (`PoolTickCoverage`) — the pool's snapshot provided complete tick
  data; solver results are trustworthy. Registers `Quarantined`; must pass the
  two-step verify + tripwire before `Live`.
- **Sparse** (`PoolTickCoverage`) — no complete tick data exists; solver
  results may be inaccurate. Registers `Live` immediately, is never
  quarantined, and is **never verified** (no RPC) — DFQYM5.
- **Snapshot seed** — the registration-time (pinned) `tick_data` for a Tracked
  pool; verified exactly once against on-chain@snapshot-block (step-1), then
  consumed so memory is bounded. Comparing engine-current instead of the seed
  would false-mismatch every active pool under a rolling start (CBCH6H).
- **Last complete block** (delivery cutoff; code name
  `pump_complete_cutoff` on `BotState`) — the highest block the pump has fully
  delivered (tombstoned by the first `removed:false` log of N+1). The gate the
  registration drain uses: a pin's `update_block` cannot outrun it (the inline
  `last_complete_block` above is this fact). Owned by `BotState` as a monotone
  value; the pump driver advances it when executing the tombstone verdict —
  no shared handle crosses the `PumpFSM` capsule, and a resume never resets
  it (ADR-028 correction, 2026-08; supersedes the 3M5PO5 shared-atom bridge).
- **Verify-lifecycle** (the choreography) — the per-pool
  `set_quarantined → verify seed (RPC) → drain+pin → verify post-drain (RPC) →
  set_live` sequence plus its block-resolution + config-gating policy. Owned by
  the Rust core (IKGQ6F); the Python driver only supplies `(coverage, idents,
  verify config)`.
- **State tripwire** (this task's D-A) — the verification `MismatchError`
  raised as the terminal gate so `Live` is unreachable on unverified state;
  never auto-repair. NOT ADR-021's solve-time scalar tripwire
  (`solver_state_verifier`), which is out of registration scope.
- **Orphan sweep** — `release_all_v3_v4_quarantined` as cleanup for pools
  built but whose path never registered; never a productivity dependency.

**Resolved need-doc: no-config policy (IKGQ6F D-C, 2026-08) = tracked is always
verified.** There is NO "verify disabled" mode for tracked pools: with D-B the
verify provider is always present (the bot's one `AlloyProvider`), so "no verify
config" reduces to a missing V4 `state_view` contract ADDRESS (the `eth_call`
target for V4 verification; V3 per-pool verify reads `pool.ticks()` directly).
Core `registration_lifecycle` always requires verify for tracked (sparse skips) and
raises a typed error if a V4 tracked pool needs `state_view` and it's absent —
enforced in core (standalone Rust consumers get the same guarantee), with Python
`start()` surfacing it early as a loud failure. No vacuous-pass, no silent
permanent quarantine. Sparse is unaffected.

**Resolved need-doc: verify provider (IKGQ6F D-B, 2026-08) = one provider per
bot.** All operations on a chain share the bot's single `AlloyProvider` (cheap
`Arc::clone`); the core lifecycle receives a clone **passed-in** from the outer
owner — never stored on `BotState` (ADR-001 I/O-free pools keep the provider
off core state). No second RPC side, no separate verify-provider trait; the
separate `verify_rpc_url`/`verify_provider` plumbing is retired. The
`StateView` contract address stays as a chain-scoped value.

**Resolved need-doc: lifecycle home (IKGQ6F, 2026-08) = sibling
`bot_core/registration_lifecycle.rs`.** Registration/verify is a state-hygiene
concern (ADR-003), separate from construction: `pool_builder` only takes
`&ConstructionIo` and never touches core state, while the lifecycle mutates
core AND does RPC — so it needs the engine/core handle + a passed-in
`&AlloyProvider`. Lives alongside `liquidity_verifier.rs`/`snapshot_verify.rs`.

### Pool structural families

The seven `PoolEntry` variants fall into **three structural families**, grouped by state-field shape and delta shape — not by DEX. The family names are load-bearing vocabulary in architecture reviews and the `BotState` deepening.

- **Reserve-pair** — `reserve0/1: U112`, `update_block`, full-state delta (`V2BlockDelta`). Members: V2, AerodromeV2 (Aerodrome's `AerodromeV2PoolState.journal` is literally `ReorgJournal<V2BlockDelta>` — the variant shares the V2 delta). Apply: `apply_*_sync` (overwrites reserves).
- **Balance-vector** — `balances: Vec<U256>`, `update_block`, full-state delta (`BalancesBlockDelta` — the three nominally-distinct `CurveBlockDelta`/`BalancerWeightedBlockDelta`/`BalancerStableBlockDelta` structs are byte-identical and unify here). Members: Curve, Balancer-weighted, Balancer-stable. Apply: `apply_*_balance_update` (overwrites balances).
- **Concentrated-liquidity (CL)** — slot0 scalars (`sqrt_price_x96`/`liquidity`/`tick`) + `tick_data: HashMap<i32, TickInfo>`, partial-prior delta (`V3BlockDelta` with `scalar_priors`/`tick_priors`). Members: V3, V4 (structurally near-identical; differ in identity shape and V4's `pool_key` nesting). Apply: `apply_swap` (changes slot0) / `apply_liquidity_update` (tick-only).

**Orchestration module layout (god-file split, epic `IOVNQQ`).** The `impl BotState` method set is no longer one ~8.5k-line monolith in `bot_core/mod.rs`; each structural family's orchestration methods are colocated in a sibling `*_orchestration.rs` module of `bot_core` — `cl_orchestration.rs` (CL: V3 + V4 + the CL-common dual liquidity buffer, snapshot seeds, coverage/quarantine/lifecycle accessors), `reserve_pair_orchestration.rs` (V2 + AerodromeV2 registration/sync/snapshot/identity), and `balance_vector_orchestration.rs` (Curve + Balancer weighted/stable registration/calc/identity, with the sole-user `curve_*` helpers and clamp consts that rode in with the family). `bot_core/mod.rs` remains the assembly + re-export hub and keeps the genuinely cross-family surfaces resident (registry/reorg dispatch, the solver-facing calc/`simulate_*`/`encode_swap` CLI, and the `BotCurveBasePoolPort` delegate). These are inherent-impl splits only — the `BotState` struct and its call sites are unchanged (ADR-003/005 layout note; the split plan doc was removed in the stale-docs cleanup `71ec78b2`). The resident `mod tests` is intentionally left as one unit (its T3 decision doc was removed in the same cleanup); test-module decomposition is a follow-up.

**Trait discipline.** State-struct traits are adopted **only for the CL family** — `ConcentratedLiquidityPool` (read, rename of the legacy `V3FamilyPool`) + `ConcentratedLiquidityPoolMut` (write). V3 and V4 are two adapters behind the same per-pool interface, so by the two-adapter rule the seam is real. For reserve-pair and balance-vector, the duplication sits one layer down — on `ReorgJournal` / `BlockDelta` — and dedups there (unify the balance-vector deltas to `BalancesBlockDelta`; extend `BlockDelta` with `type RestoreState` + `landed()` to collapse five hand-duplicated `restore_*_before_block` impls into one generic `impl<D: BlockDelta> ReorgJournal<D>`). The V3 family keeps its own restore impl — `V3RestoreResult` + the scalar/tick-priors branches are a genuinely different algorithm, not a full-state delta. See ADR-014 for the formal record of the trait-vs-journal-layer split.

**Reorg-pool-state trait (ADR-016, refines ADR-014 D3).** The reorg dispatchers that survived D3's slicing on `BotState` (the per-family `*_restore_before_block` / `*_discard_before_block` / `*_journal_len` methods, 7 families × 3 ops) collapse behind a state-struct trait `ReorgPoolState` (`restore_before_block` / `discard_before_block` / `journal_len`, all returning family-agnostic `Result<(), JournalError>`). The lever D3 didn't have: returning `()` dissolves the cross-family no-op trap that defeated `PoolFamilyReg` — every family satisfies one identical signature, no associated type. The per-family field-write absorbs into each struct's own impl (same category as D1's `apply_swap`). The within-family residue (the three byte-identical balance-vector dispatchers) is the no-op-free seam; the CL family's `V3RestoreResult` absorption is the open harder case (spike `Z76ETG` decides it). Adoption tracked under ergo epic `OCXSHQ`.

### Resolve→solve boundary (ADR-015)

**Current shape — pure solve layer split across two crates by an accidental line.** `degenbot-solvers` holds the V2/CL Möbius solvers (`mobius_int`, `mobius_int_exact`, `mobius_v3_int`, `affected_keys`) — “value-only solver math … no chain/registry/async/tokio … consumable by both the standalone Rust path and the PyO3 driver shell.” But the rest of the pure solve family stayed behind in `degenbot-bot/src/solvers/arb_engine/` (the I/O orchestrator: tokio, `core: Arc<RwLock<BotState>>`, path registry, V3/V4 buffers): `solve_path` (the dispatcher) + `solve_*_path_int` (Balancer weighted/stable, Curve, Solidly), the `simulate_*_hop` swap leaves, `balancer_weighted_basket.rs` (QuantAMM), and the hop-state value types. A standalone `cargo add degenbot` consumer got V2/CL but not Balancer/Curve/Solidly/basket.

**Decision (ADR-015, 2026-07-19):** complete the seam. The pure solve layer — `solve_path` + all `solve_*_path_int` arms + `simulate_*_hop` leaves + the QuantAMM basket solver + the hop-state value types (`ResolvedHop`, `ResolvedMixedPath`, `SolvePathResult`, `*HopState`, `HopType`, `MixedPoolRef`, `PoolHop`) — moves to `degenbot-solvers`. `degenbot-bot` keeps `resolve_path` (the only core-bound step). The orchestrator’s solve-side import collapses to `degenbot_solvers::solve_path(&resolved)`. The dep graph stays a DAG (the new deps are leaf math crates `pools` already depends on); no-pyo3 invariant preserved.

**Hop (ubiquitous language).** A hop is **the solver’s snapshot-and-classifier adapter** — not a pool concept, not a math-leaf concept. It does two jobs, both for the solver:
- *Snapshot role (= selectivity).* It captures pool state at resolve time so the solve can run lock-free under rayon `par_iter` on a `Clone`+`Send` value. For CL specifically it is a *selective projection* — `build_int_v3_sequence` walks `tick_data` once in the swap direction, caps at ≤15 tick ranges, pre-accumulates `liquidity_net`, replaces map lookups with `TickMath` constants — NOT a clone of the pool (which is thousands of `TickInfo`). The balance-vector family pre-computes the invariant `D` (one Newton run, not ~25× during the golden-section) and BPT-skips. Live-read over the pool re-pays the projection ~25× or relocates it as cache-on-state with invalidation spread across every `apply_*`.
- *Classifier role.* The `ResolvedHop` enum variants let `solve_path` pattern-match on path composition to pick the algorithm (closed-form Möbius for all-V2/all-CL; golden-section for paths involving non-Möbius leaves). A `dyn PathHopSnapshot` trait variant would re-invent this as a capability query (a thinner hop behind a trait).

**Solver intake contract.** The hop-state types are `degenbot-solvers`'s intake protocol — `degenbot-bot`'s `resolve_path` projects `BotState` into them under `core.read()`, then the guard drops, then `degenbot_solvers::solve_path` runs lock-free.

**Per-family projection module (`bot_core/resolve/`, 2026-08, DECIDED).** `ArbitrageEngine::resolve_path`'s internals deepen into a pure `bot_core/resolve/` module — one file per family, free `project_<family>(&BotState, &MixedPoolRef) -> Result<(ResolvedHop, u64), MissingHopReason>` (u64 = state nonce), and the engine method shrinks to a thin dispatcher that accumulates the cross-family `max_update_block` + `state_nonces`, marking the path invalid on any per-family `Err`. The projection needs no engine `&self` (it reads only `&BotState`) and is internal to `degenbot-bot` (never reached by PyO3), so the split is a free restructure. ADR-015's placement (the projection stays in degenbot-bot) is unchanged. Per-family unit tests (missing state/identity, missing token pair, `<2` tokens, unknown variant, out-of-range) live in the new modules (Red→Green), ported from `arb_engine/tests.rs` (e.g. the existing Solidly Aerodrome/Camelot unit tests at `resolve_path_*solidly*`). The dispatcher surfaces the reason via `tracing::debug!` (path_id/hop/reason) at the invalidation point — the existing `tracing` machinery gives a runtime-configurable level, so it is invisible in normal runs but answers "why was this path rejected" on demand. Test split: the per-family projection unit tests are KEPT (ported permanently); the ad-hoc path fixtures (`path142603_*` and similar live-run solver-divergence fixtures) are treated as a WEAK parity cross-check during implementation, then DELETED once the full revm harness covers them (they were one-off debugging harnesses for failing live paths).

**CL-projection guardrail — V3/V4 are related but NOT swappable (2026-08).** `resolve/cl.rs` holds `project_v3` and `project_v4` as two **self-contained** entry functions; they share only the file + the thin `ResolvedHop::V3/V4` wrap + nonce push. There is deliberately **no** shared constructor that reinterprets sequence/sign/fee semantics, because the two `build_int_v*_sequence` families (which stay in `degenbot-pools`, untouched) differ in three load-bearing ways: (1) **fee convention** — V3 `gamma = 1_000_000 − lp_fee` vs V4 combined `swapFee = calculate_swap_fee(protocol_fee_dir, lp_fee)` when `protocolFee > 0`; (2) **current-tick drain framing** — V3 pushes a dedicated leading hop `[current, sqrt(currentTick)]` at stored liquidity, V4 folds the drain into each range's `base_liquidity`; (3) **net sign direction** — V4 applies per-prior-range `if zero_for_one { l -= net } else { l += net }`. Any attempt to "swap" V3/V4 behind one code path clobbers these; they are two adapters behind the shared `ConcentratedLiquidityPool` *interface* (ADR-014 family) with genuinely distinct swap/step internals. The lock-drop discipline (ADR-005 slice 15b-1: "the guard drops before `solve_path` runs") is load-bearing and survives the relocation unchanged.

**Closed — the hop-shape deepening (2026-07-19).** Resolved as a negative: the hop stays as `enum ResolvedHop` + match-based classifier. (1) Digest-cost motivation retired empirically — Balancer `D` / Curve `xp` are 0.04–4% of the per-path budget (spike 77LOQT, `rust/crates/degenbot-solvers/benches/digest.rs`); cross-path digest memoization rejected (CL caches justifiably, the light families don't amortize, stale-digest reorg risk > ~1% wall-time ceiling). (2) Composition-classifier motivation settled negative — `dyn PathHopSnapshot` removes the enum but not the work: the 9-way `solve_path` classifier survives as capability-query chains over trait objects with the same 9 branches (the per-composition search algorithms are path-level strategies, not per-hop plug-ins; a hop-level trait can't replace them). (3) Extensibility motivation negative — adding a DEX family under the enum is three local edits, under the trait it's more touch points (capability surface grows alongside the struct). Net: `dyn PathHopSnapshot` is wash-to-loss on depth and clear loss on runtime (heap-alloc `Box<dyn>` per hop at resolve vs inline enum payload, vtable indirection on the 25-iter simulate loop). The frozen per-solve snapshot survives on constraint #5 alone (lock-free solve: guard drops before `solve_path`). See ADR-015 CLOSURE section.

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
objects (slice A), and the 27 choreographed encode→call→decode wrappers
(`fetch_v2_reserves` / `fetch_v3_slot0_*` / `fetch_balancer_*` family) have
moved core-side (the builder-choreography port, F2R2OC / 3FVZF4) into
`bot_core/pool_builder/`; every `PyBotIo` `fetch_*`/`probe_*` now just
`block_on`s the core choreography over `&ConstructionIo`. The Python pool
builders receive `io: PyBotIo` (the `#[pyclass]`), not `&ConstructionIo`
(the core handle is reached via `PyBot.build_*_pool`). `PyBotIo` does NOT
retire fully here — see ADR-023/D0: it is trimmed to a strict translator and
the residual surface is documented `stays-python`; full retirement is owned
by follow-up epic `VK3YDM` (Rust ERC-20 + Curve port). Held-tx sharing between
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

### Synchronization primitive for `construction_io` — CLOSED (2026-07-19)

**Decision: leave it as `parking_lot::RwLock<Option<Arc<ConstructionIo>>>`; no change.**

Investigated whether to swap the RwLock on `Bot.construction_io` for `ArcSwapOption` (the slot is publish-once-at-init, read on RPC/DB delegation paths — a shape ArcSwap is purpose-built for). The evaluation cascaded to a sharper question: if the slot is truly write-once, *no* sync primitive is needed at all — a plain field set in `Bot::new` suffices (the thread-spawn creates the happens-before edge). That's blocked only by the construction seam: `PyBot::new(chain_id)` happens before the provider is known, so `set_construction_io(&self)` runs post-construction through `&self`, which forces interior mutability.

Three options surfaced: (A) merge the seam — make IO a constructor arg of `Bot::new`, drop the primitive entirely (cleanest, but a real refactor of the Python `__init__` ordering + `extract_native_alloy` choreography); (B) `OnceLock<Arc<…>>` (std, no new dep, init-once semantics, exits the D2 lock-ordering discipline — but loses the idempotent-replace path); (C) `ArcSwapOption<…>` (supports runtime re-publication, over-machinery if replace is dead).

**Disposition: stays-as-is.** Effort-to-value is poor in every direction. The slot is uncontended (one publish at `__init__`, reads on I/O-dominated paths where a ~10 ns read-guard is invisible against network/SQLite). No measured contention, no profile pointing at it, no lock-ordering near-miss (the write happens before any reader is active). Retiring the primitive is cosmetic work on a cold path. The forcing function that would make it worth doing — actual runtime mutation (hot-reloading a provider, swapping a DB, a multi-engine bot re-attaching IO) — does not exist today. Recorded here so the candidate isn't re-litigated without that forcing function.

**Broader ArcSwap audit (closed alongside).** Surveyed the other `parking_lot::{Mutex,RwLock}` sites in `degenbot-bot` for ArcSwap fit. The result: three of four candidate sites are **incrementally-mutated state** (`Arc<RwLock<BotState>>` — `apply_swap` mutates one pool's reserves per log; `Arc<Mutex<ArbitrageEngine>>` — `solve_dirty` mutates dirty sets + builds paths + solves; `Mutex<HashMap<…subscribers…>>` in `LogDispatcher` — `subscribe` appends), which is the wrong model for ArcSwap (a publish-snapshot primitive that swaps a whole `Arc<T>` atomically — would require COW-cloning the entire state per mutation). Only `construction_io` fit the shape, and it isn't worth touching. `arc-swap 1.9.2` remains transitive-only in `Cargo.lock` (no `degenbot-*` crate pulls it directly); formalizing it as a direct dep is deferred until a genuinely-fitting, contended publish-snapshot site lands.

## Block-pump dispatch seam (B — unified event seams, 2026-08)

The pump's hand-offs to the sink, the solver-state verifier, and Python's block
clock are owned by ONE module — one **dispatch owner** — but delivered over three
**application-specific pipes**, each with the delivery semantics its task needs.
"One seam" means one coordinated home, never one queue forced to fit every task.

- **Dispatch owner** — the module that owns all three pipes and coordinates
  liveness/ordering in one place. The seam worth having; NOT a single bus.
- **Drain pipe** — an ordered FIFO (`mpsc`) taking `Drain`/`Finalize`/`Publish`
  to a background drainer task → sink. Solve/dispatch/finalize must run in
  enqueue order (FIFO + engine/sink locks are what make the deferred path equal
  to the old inline one).
- **Verifier pipe** — a latest-wins `watch` to the solver-state verifier task.
  Only the most recent published block is ever verified (ADR-021); non-blocking
  so a slow verify can never stall the pump.
- **Block-clock pipe** — a DIRECT `notify_block` dispatch to the sink's
  engine notification channels, deliberately NOT a `DrainWork` item (B2), so
  a `newHeads` tick is delivered ASAP and never rides the drain FIFO behind
  solver work. Every accepted header is delivered 1:1 (no coalescing). The
  sink's `notify_block` no longer takes the `drain_lock` (the `engines` vec is
  frozen after start), so the clock does not contend with the drain fan-out.
  Callers hold no ordering guarantee on solver results.
- **Stall backstop** — the drain-pipe liveness check (B3), soak-hardened: the
  pump aborts when the queue holds a backlog (`depth >= BACKLOG_FLOOR=2`) AND
  the drainer has completed no work for `STALL_WINDOW` (~30s). The wall-clock
  window (not event-counting) is what correctly distinguishes a *frozen*
  drainer from one mid-way through a single exceptionally long solve — a live
  mainnet dry-run proved that pure strike-counting (on depth or on completion)
  false-positives under heavy multi-path solve load. A drainer that progresses
  but falls behind is observed via `pending()` (a lag metric), never aborted;
  a dead (closed-channel) drainer still aborts immediately.

## Block-pump PumpDecision seam (A — pure producer/FSM, 2026-08)

Epic A (ergo `FUE5SP`, tasks A1–A5) — the ADR-008 deepening: the block pump's
per-event *policy* lives in a **pure decision producer**, executed by a **thin
dispatcher**. Same family as ADR-008 (BlockClock) and ADR-027 (dispatch seam):
"deep module — pure producer + thin I/O driver". See ADR-028.

- **pump FSM** (`PumpFSM`) — the pure, I/O-free decision machine for the block
  pump: owns the cursor, per-block metadata snapshots, quiesce arm, recovery
  anchor, ws-delivered tracker, and the `BlockClock`. No provider, no timer, no
  `Instant`, no lock. Time enters as `now_ms` data.
- **PumpDecision** — the enum that names every effect the FSM can produce:
  `Drain`, `Publish`, `Finalize`, `Notify`, `SetLastSolved`, `Backfill`,
  `Recover`, `LogSilence`, `VerifyCompleteness`, `Stop`. The driver maps each
  onto its executor (the dispatch owner, the sink, the provider, the reorg
  coordinator, or the process).
- **thin dispatcher / executor** — `run_with_stream`
  (essentially unchanged shape, now a thin driver): feeds
  `(WsEvent + clock state + watchdog tick / now_ms)` into the FSM and executes
  the returned `PumpDecision`s. Owns all I/O: the ADR-027 `DispatchOwner`, RPC
  (`eth_getLogs`), the drainer task, the reorg coordinator, the WS-drop abort.
- **tick/clock input** — the driver reads the wall clock (a monotonic `now_ms`)
  and feeds it as data, so the FSM's watchdog rules (`on_tick`,
  `record_header`/`record_log`) are deterministic and horology-free.

The FSM owns the rules: quiesce-before-publish + solver-release gate
(`on_settle`), recovery anchor + single-writer discard (`record_backfill` /
`should_drop_recovered_forward`), watchdogs as ticks (`on_tick`), the
ws-completeness verdict (`completeness_decision`), and the drain anchor
(`drain_decision`). Cursor advancement under `drain_lock` + lock order
(`drain_lock → engine → BotState`) stay in the coordinator/engine executor,
not the FSM.

**A6 disposition (superseded, 2026-08).** Task A6's literal "collapse
`DrainSink` to one `drain(block, metadata)` that owns the quiesce gate" is
**not built**: the quiesce gate now lives in the FSM's `on_settle` (A2), so
re-owning it in the drain entry would un-build the pure-producer design. The
pump's per-block surface is already single: FSM decision → `DispatchOwner` →
`DrainWork` (ADR-027). The wide `DrainSink`/`Engine` surfaces are executor
fan-out detail behind that seam. Recorded in ADR-028's "Not decided here";
not re-litigated without a forcing function.

### Simulation engine vs. searcher strategy (DECIDED, 2026-07-20 — architecture review)

**The seam.** degenbot is a library consumed by many searchers with different on-chain strategies (backrun, sandwich, JIT-L, liquidation, …). The Rust core therefore owns only the **in-process representation of pool/token state** + the **solver methods** (the value-only swap math the operator constrains) + a **thin, general simulation executor**. The **transaction encoding** for a searcher's bot, the **profit-detection strategy** (e.g. the settlement-arbitrage example's 3-pre-balance → `execute()` → 3-post-balance WETH9/ERC6909/Multicall3 bundle, `decode_balance`, gross/net + priority-fee sizing), and the **operator policy** (thin-margin filtering, path suppression, sort order) are **out of scope for the core** — they are the searcher's code, assembled at runtime from the tools the core exposes. The settlement-arbitrage bot (`examples/eth_settlement_arbitrage_v2_v3_v4_rust.py` + its Rust strategy leaves) is ONE example strategy, not the simulation surface.

**`ExecutionStrategy` seam (ADR-025).** The
user-owned execution layer over the thin engine: `PayloadComposer` (Encode) +
Probe declared reads + Assess gate + Fee default, in the pyo3-free
`degenbot-execution` crate. Python and Rust consumers meet the SAME seam — see
the [execution-strategy guide](docs/execution-strategy.md).

**Load-bearing consequence.** Code wedging any one strategy (the 7-call pre/post balance bundle, `compute_priority_fee`, `dispatch_profitable_results`'s categorization + suppression + thin-margin policy) into a crate that claims to be the simulation core is the wedging AGENTS.md forbids. The duplicated `simulate_one` (RPC `eth_simulateV1`) ↔ `simulate_path_on_evm` (revm `transact_one`) 7-call bundle is duplicated *settlement-strategy* code, not duplicated *engine* code — its dedup target is the strategy layer, not the simulation core. The simulation engine itself stays deliberately thin: "execute these calls against the EVM, return per-call outcomes (status, gas, output, revert, optionally touched state)."

**Adapter decision — in-process revm only (DECIDED, 2026-07-20).** RPC `eth_simulateV1` simulation, its `stateOverrides` JSON builder (`build_simulation_state_overrides` → alloy `StateOverride`), **and** `eth_createAccessList` **all retire**. The sole simulation executor is the in-process revm path (`BlockSimHandle` over the `CacheDB<WarmCodeCache<BotStateDb<WrapDatabaseAsync<AlloyDB>>>` stack); the sole override mechanism is `CacheDB::insert_account_storage` / `insert_account_info` (`apply_simulation_overrides`, the explicit-balance-wins merge); the sole access-list creation is an `Inspector`-based collector on the first `transact_one` run (warmed slots collected in-realtime as a byproduct of execute() — retires the post-re-`transact` `emit_access_list_from_state` path as the primary AL source). This realizes the long-term goal (high-performance in-process sims, minimize external RPC I/O): the RPC surface stays only for cold-miss state fetches (`AlloyDB` underneath) + non-sim primitives (`eth_feeHistory` for `compute_priority_fee`'s market percentiles), never for whole-transaction execution or access-list creation. The two-adapter rule does **not** justify an RPC-sim seam — there is one adapter (revm); anything RPC-shaped that survives is a *primitive the revm path calls underneath*, not a peer simulation executor. Strategy params (`SimulationOverrideParams`: owner, executor addresses, runtime bytecode, warmup slots, funding amounts) cross the strategy→engine seam; the engine renders them to `CacheDB` inserts only. AL output crosses the engine→strategy seam: the engine produces the warmed-slot set via the Inspector; the strategy decides whether/how to attach it to the submitted tx.

**Strategy-relocate sequencing — DEFERRED into HZL664 (DECIDED, 2026-07-23; ADR-019 D5 task `JB22F5`).** Where the settlement-strategy code (`SimResult`, `SimulateContext`, `SimulatePath`, `FailBuckets`, `compute_priority_fee`, `fits_int128`, the 7-call bundle, `decode_balance`, `dispatch_profitable_results` + its thin-margin / suppression / categorization policy, `filter_thin_margin_results`) lives once ADR-019 is done. Three options surfaced: (A) a new workspace member `degenbot-arbitrage` crate the PyO3 binding depends on transiently + that a pure-Rust consumer could reach; (B) defer the relocate until the step-6 PyO3 decompose (`HZL664`) — the binding reaches the strategy directly today (`degenbot-python/src/simulation/dispatch.rs` calls `dispatch_profitable_results`, `SimResult`, `SimulateContext`), so no piece can move to `examples/` until the binding stops reaching it; (D) strand the strategy inside the PyO3 crate (violates the standalone-Rust-core framing). **Decision: (B) — defer.** The relocate inseparably couples to the PyO3 decompose: there is no standalone leaf in step 5 that satisfies its AC rg (`compute_priority_fee | dispatch_profitable_results | SimResult` returns nothing in `degenbot-simulation/src/`) without first severing the binding's reach, which is step 6's scope. The signature collapse of `dispatch_profitable_results` (`Option<BotState>` → required) likewise lands in step 6 — the `Option` arm survives only because the PyO3 caller's `engine: Option<...>` keeps it alive. Step 5 therefore folds its AC into `HZL664`: after the decompose rewrites the PyO3 surface onto engine primitives, the strategy becomes unreachable from the binding and moves to a real `examples/` bin (the "no new crate / example bin" shape ADR-019 D5 prefers) — satisfying the step-5 AC rg at that point. `JB22F5` is marked done with this deferral note (no code change); the substantive relocate ships as part of `HZL664`.

**PyO3-decompose gated by the same relocate (DECIDED, 2026-07-23; ADR-019 D7 task `HZL664`).** Surveying `HZL664`'s listed primitive wrappers against the current engine shape under decision (a) (ship additive wrappers alongside the existing monolith; no retire this step) surfaced that **every** primitive is either already exposed or blocked by the deferred strategy relocate: `fetch_priority_fee_percentiles` is already wrapped (`fetch_fee_history_py` in `degenbot-python/src/submission/submit.rs`; step 1's leaf moved to `degenbot-rpc::fees`); `PyBlockSimHandle::build` is blocked because `BlockSimHandle::build` takes `ctx: &SimulateContext<'_>` which mixin engine primitives (`provider`, `base_fee_next`, `current_block`, `block_timestamp`, `block_priority_fees`) with strategy config (`executor_owner`/`executor_address`/`weth_address`/`pool_manager_address`/`multicall3_address`/`inject_code`/`runtime_bytecode`/`warmup`); `apply_simulation_overrides` standalone is blocked because the `CacheDB` is built inside `BlockSimHandle::build` + the override adaptor reads `SimulateContext::override_params()`; the AL-Inspector output is blocked because the AL today is embedded in `SimResult.access_list` (a strategy type). The root cause is one + the same: `SimulateContext` engine-strategy mix is not split, + that split *is* the strategy relocate decision (B) deferred to `233TVH`. So the "additive half" of `HZL664` is near-empty: the fee primitive already exists; the other three are all blocked on the `SimulateContext` coupling. **Decision (a1) — defer `HZL664` into `233TVH`.** This is decision (B)'s logic applied one level deeper (the relocate gates the decompose the same way it gated JB22F5): no fake primitive wrappers re-wrapping the monolithic strategy shape under a new name. `HZL664`'s additive surface + its retire (the deletion of `dispatch_profitable_py`/`PyDispatchCandidate`/`PyDispatchOutcome`/`PySimulateContext`) both fold into `233TVH`, which becomes the combined step-6+7: split `SimulateContext` into engine-primitive args + `SimulationOverrideParams` (the engine-primitive type already at `sim::evm/state_override.rs`) → expose `PyBlockSimHandle::build(provider, base_fee_next, current_block, block_timestamp, override_params, bot_state, warm_cache)` as a genuine engine primitive → rewrite the Python driver's `_dispatch_profitable` onto the decomposed primitives → finally retire the monolith + pyclasses + move the strategy to `examples/` (satisfying JB22F5's AC rg at that point).

**Capstone resolution — Rust-canonical, NOT Python re-derivation (DECIDED, 2026-07-23; ADR-019 D4/D7 task `233TVH`).** The final task's own Context asked: is the canonical example the Rust one (Python a thin driver over it) or the Python one (Rust strategy a reference impl the Python bot re-derives)? Three considerations forced the call: (1) AGENTS.md's "Rust is the engine; Python is a driver shell, **not a co-implementation**" directly forbids Python re-deriving the 7-call bundle + `decode_balance` + `compute_priority_fee` + the categorization — which is what 233TVH's "Python constructs its own 7-call vector" AC would require; (2) the whole ADR-019 epic was the engine-vs-strategy split — un-wedging the strategy from the engine crate, not re-wedging it into Python; (3) ADR-019 D1 retired the RPC sim path *to make the in-process revm path the sole executor* — having Python re-implement the 7-call bundle over `PyBlockSimHandle` primitives would resurrect a second strategy implementation alongside the Rust one, the exact duplication ADR-019 D1 resolved. **Decision (R): Rust-canonical.** The strategy stays in Rust (a `degenbot-arbitrage` crate — the only internally-consistent shape: AGENTS.md forbids wedging the strategy back into the `degenbot-simulation` engine crate, + a Rust `examples/` bin can't be reached by the PyO3 binding, so a crate is the only reachable Rust home). The Python bot stays a thin driver: it calls a thin PyO3 wrapper over `degenbot-arbitrage::dispatch_profitable_results` (the existing `dispatch_profitable_py`, re-sourced), reads the outcome, chains to `dispatch_and_submit_py` — it does NOT construct the 7-call vector, decode balances, size priority fees, or run fan-out policy. **Two ACs dissolve under (R):** "`eth_backrun_v2_v3_v4_rust.py` no longer calls `dispatch_profitable_py`" (it keeps calling it — now understood as a thin wrapper over the Rust strategy, not a monolith to retire) + "composes primitives end-to-end" (that composition stays in Rust). The substantive 233TVH work under (R) is the Rust-side un-wedging: create `degenbot-arbitrage`, split `SimulateContext` (engine primitives stay in `degenbot-simulation`; strategy config moves to the strategy crate, deriving `SimulationOverrideParams` for the engine's `BlockSimHandle::build`), move the strategy code (`SimResult`/`SimulateContext`/`SimulatePath`/`FailBuckets`/`compute_priority_fee`/`fits_int128`/the 7-call bundle `simulate_path_on_evm`/`decode_balance`/`dispatch_profitable_results`/`filter_thin_margin_results`/the categorization + suppression policy/constants) to the strategy crate, collapse `dispatch_profitable_results`'s `Option<BotState>` → required (the PyO3 caller always supplies the engine now). `PyBlockSimHandle::build` is exposed as a new primitive for standalone engine-direct consumers (the settlement-arbitrage strategy uses it internally). This satisfies JB22F5's AC rg at last (`compute_priority_fee | dispatch_profitable_results | SimResult` returns nothing in `degenbot-simulation/src/`).

**SHIPPED (2026-07-23, commit `050e99fd`).** `degenbot-arbitrage` crate created; `SimulateContext` split (engine primitives stay in `degenbot-simulation`; strategy config moved); `BlockSimHandle::build` now takes block-env primitives + a projected `&SimulationOverrideParams` (the engine never names `SimulateContext`); `BlockSimHandle::evm_mut` exposes the borrowed `&mut evm` the strategy drives; `simulate_path` (the strategy-coupled method) removed; strategy code (`SimResult`/`SimulateContext`/`SimulatePath`/`FailBuckets`/`compute_priority_fee`/`fits_int128`/the 7-call `simulate_path_on_evm`/`simulate_in_process_with_db`/`decode_balance`/the calldata builders/`dispatch_profitable_results`/`DispatchCandidate`/`DispatchOutcome`/`filter_thin_margin_results`/constants) relocated to the strategy crate; the PyO3 seam re-sourced to `degenbot-arbitrage` for strategy types + `degenbot-simulation` for the engine handle (the Python driver stays a thin cockpit — no 7-call re-derivation); stranded engine deps (`degenbot-submission`/`degenbot-abi`/`futures`/the `BlockPriorityFees` re-export/tokio `sync`) removed. Gates green: `just test-rust` (engine 17 + strategy 21 + reachability + standalone), `just lint-rust` (clippy clean), `just check-no-pyo3-in-cores`, `just test-python` (360 wrapped `tests/rust` tests passing within the full suite). ADR-019 epic fully done (7/7 ergo tasks).

### ERC6909 vault profit capture (SMOZG3 / ADR-034)

- **ERC6909 capture** — the operator's `erc6909_profit` toggle
  (`DEGENBOT_ERC6909_PROFIT=1`): captures Uniswap-V4 profit as an ERC6909
  claim on the PoolManager (a fresh `V4_MINT_COMPACT` — no pre-held position
  required) instead of custody WETH. The `execute()` config packs
  `check_mode=2` (the unconditional on-chain floor
  `PM.erc6909WETH(after) >= before`), and the declarative harness asserts the
  vault delta to the 0.1% oracle pattern (`assert_erc6909_capture`).
- **Stream effect is pure-V4 only** — per `family_axis_support`, the capture
  axis branches the stream only for `v4_v4`/`v4_v4_v4`; other families keep
  custody capture with only the mode-2 floor armed.
- **batch×capture decline (interim)** — `use_v4_batch` + `erc6909_profit`
  on a WETH terminal is unexecutable on the *current* executor artifact
  (the batch tail-settle takes the WETH delta into custody; the follow-up
  mint reverts `D0`) — the funnel declines the combination
  (`erc6909_batch_capture_declines`). The executor is **pre-deployment**
  (operated via state-override code injection, `INJECT_EXECUTOR_CODE=1`
  default) with in-repo Vyper source, so composing the two at the source is
  **TGUZCT** — the decline is a fail-closed interim until it ships.

### Execution strategy seam (ADR-025)

**The seam.** The execution side of degenbot is the **developer's own `cmd_executor` adapter**, not a general execution layer. A new pyo3-free `degenbot-execution` crate owns the **`ExecutionStrategy`** trait + its value types (the solve-result view, the gate protocol, `ExecutionResult`) — no default strategy. `degenbot-arbitrage` implements it as the **default adapter** (stays Rust-canonical per ADR-019 R). A foreign user's crate `impl ExecutionStrategy`, or supplies a Python callable lifted into it via **`PyPayloadComposer`/`PyExecutionStrategy`** (the Polars-`map_elements` model) — both meet the same seam. This is the execution-side twin of ADR-015's `degenbot-solvers` relocation; the two-adapter rule (settlement arbitrage + a user's own contract) justifies the seam.

- **PayloadComposer** — the Encode part of an `ExecutionStrategy`: `solve result → payload bytes` for ONE execution contract. Rust users implement it; Python users supply a callable. The canonical `cmd_executor` encoder (`CmdExecutorComposer` wrapping `encode_cmd_stream`) is the default adapter.
- **Probe / Assess (parts of an `ExecutionStrategy`)** — Probe is declared data (which pre/post read-calls to snapshot: label/addr/selector); the engine runs them. Assess is how deltas → profit + pass/fail (built-in shapes like sum-of-deltas / return-value, or a user's tiny interpreter). **Priority-fee/gas pricing (Fee) is the defaulted pricing half of Assess, not a fifth seam** — `net = gross − gas×(base_fee_next + priority_fee)` is defined in terms of the pricing policy, so pricing can't be independently ordered; a built-in market-percentile (`compute_priority_fee`) is the default, overridable.
- **Solve-result view** — the seam's input contract: `SolvePathResult` (amounts: `optimal_input`/`hop_outputs`/`consumed_inputs`) + `PathInfo` (hop descriptors) projected to a typed Python `SolveResult` view. Today the per-hop amounts do NOT cross to Python on the clean path (`SimResult` carries pre-built `execute_calldata`, not the amounts) — exposing them is the one genuinely new surface.
- **Default-stays-Rust-canonical wall.** The canonical `dispatch_profitable_results` / `dispatch_profitable_py` **never reads a Python transform** — it uses the Rust default adapter only and returns `execute_calldata` exactly as today. The seam *adds* a foreign-contract path for a user's own dispatch loop; it never lets Python re-derive the canonical 7-call bundle (ADR-019 R + AGENTS.md "driver shell, not a co-implementation"). A foreign user's success/failure gate is **their own searcher code** over the thin engine, not a `SimGate` hook wedged into the engine.

**The original Candidate-1 deepen (the 27-way `three_hop_*` fan-out + the dead `V4V4ArbitragePayload`/`V4V3ArbitragePayload`/`CmdExecutorComposer` payload builders) becomes internals of the default adapter.** Delete the dead encoders (facet B) and collapse the 27+8 combinatorial fan-out behind `CmdExecutorComposer::compose` (facet A), Red→Green against the golden-master vectors (`composers_parity.rs`/`composers_3hop_parity.rs`/`native_eth_3hop_bridge.rs`) — now pinning the default adapter's output.

### Onchain pool-state probe — `degenbot-rpc::abi` (DECIDED, (A) planned 2026-07-20)

**Decision: `degenbot-rpc::src/abi.rs` is the single deep home for onchain
pool-state probing.** An architecture review surfaced that one deep module
already exists — `encode_*` / `decode_*` / `fetch_*` for every probe shape
(V2 `getReserves`, V3/V4 `slot0`/`liquidity`, V3/V4 `tickBitmap`/`tickLiquidity`,
`balanceOf`/`allowance`/`totalSupply`) — but three consumers circumvented it and
reinvented the primitives from scratch. The reference adapter proving the seam is
real is `PyBotIo` (`degenbot-python/src/bot/py_bot_io.rs`), which delegates every
fetch through the home.

**The stragglers (the hygiene work — "slice A"):**

- `solvers/arb_engine/diagnostic.rs` — `fn_selector`/`encode_call`/`build_v2/3/4_calls`/`decode_v2/3/4_results`/`uint_value`/`int_value_to_i32` (alloy `DynSolValue` directly, bypassing the sol! macro path the home uses).
- `bot_core/liquidity_verifier.rs` — `encode_calldata`/`decode_uint256/128`/`decode_int128`/`decode_v3/v4_*_result`.
- `pool-updater/src/verify.rs` — `ticks_calldata`/`tick_bitmap_calldata`/`int_selector_calldata`/`decode_ticks_return`/`decode_tick_bitmap_return` (V3 half only).
- `aave/src/updater/verify.rs` — `decode_uint256_return`.

Each routes through the home; the reinventions delete. **Error shape:** a
per-consumer `From<ProviderError>` adapter at the call site maps the home's
`ProviderError::DecodingError` to the consumer's error enum (`LiquidityVerifyError::Mismatch`,
`RunError::Provider`, etc.); the home's interface is **not** extended with a
richer `DecodeOrRevert` (the revert-vs-mismatch distinction lives in
`require_success` inspecting `MulticallResult.success` *before* decode, so it
survives delegation unchanged).

**Test discipline (load-bearing).** The home's existing `mod tests` carries the
independent-oracle discipline ("reference vectors computed independently with
`eth_abi` + `eth_utils.keccak` in a throwaway Python probe — a DIFFERENT ABI
encoder than alloy's `sol!`"), but **has no tests for `decode_tick_data` /
`decode_tick_bitmap` / `decode_v4_tick_*`** — the gap the stragglers' tests
(`pool-updater/verify.rs::decode_ticks_return_matches_ref_encoder` etc., built
with `cast keccak` + hand-rolled `DynSolValue`) currently fill. Slice A migrates
those independent-oracle tests to the home **first** (Red+Green against the
existing home decode, proving the home correct before any straggler touches it),
then reroutes the stragglers in four independent per-consumer commits. This is a
Tier-2-style strengthen: the home's test surface grows, then consumers reroute
behind it.

**Out of scope for slice A — `extsload` (V4 storage-slot reads).**
`pool-updater/verify.rs`'s V4 path probes `PoolManager` storage slots via
`extsload(bytes32[])` (selector `0xdbd035ff`), NOT an ABI method call. The home
has no extsload surface. This is a genuinely different probe mechanism (direct
storage reads vs ABI calls) and is **not** force-unified into the home —
deferred to the batch-probe extraction (slice B), where its shape decides
whether it joins a `ProbeRequest` enum or stays a peer.

**Slice B (batch-multicall orchestration) — DEFERRED indefinitely (2026-07-20).**
What each straggler *also* reinvents is the cross-hop / all-ticks multicall3
batch build + heterogeneous-result decode (a layer the home's single-call
`fetch_*` does not cover). An architecture review (B-grilling) surfaced that the
three multicall3-batch shapes (diagnostic's cross-hop heterogeneous, verifier's
two-phase discover-then-verify, pool-updater's mixed-type index-split) plus the
V4 `extsload` single-`eth_call` path differ on too many axes (dispatch mechanism,
phase count, output type) to unify behind one `ProbeRequest` enum without
re-introducing the ADR-014 trap (a unified trait that no-ops on shapes it doesn't
fit). Extracting only the narrow plumbing (`ProbeBatch` over a single-call enum,
extsload excluded) was weighed against the ADR-014 lesson.

**Disposition: deferred indefinitely.** After slice A, the dangerous
duplication (byte-identical encode/decode copies with divergent bug surfaces) is
fully eliminated — that's the class that caused silent misclassifications. The
residual is **structural scaffolding duplication, not logic duplication**: three
consumers each write ~30 lines of the same `build Vec<(Addr,Bytes)>` →
`multicall3_batch` → `zip + decode-by-index` loop with their own index
bookkeeping (`HopFetch { start, n }` in diagnostic, the `tick_count` split in
pool-updater V3, the two-batch split in verifier) + their own `require_success`
adapter (~90 lines total across 3 consumers). Not a bug-hiding class today — a
noise + off-by-one-in-one-consumer risk. `multicall3_batch` dispatch itself was
never duplicated (it lives in `degenbot-rpc::multicall3`; all consumers already
call it).

**Revisit only on a forcing function:** (a) a 4th multicall3-batch consumer
lands (the scaffold copies a 4th time, dedup pays), or (b) an off-by-one bug in
one consumer's index split that the others don't have (the bug-hiding risk
becomes real). Neither exists today. Re-litigating without that forcing function
re-raises the ADR-014 trap.

**ADR alignment:** no conflict — ADR-003 names the onchain probe as
cross-consumer infrastructure ("diagnostics, verification … all consume it");
this decision *realizes* that for the single-call layer. No new crate deps
(all four straggler crates already depend on `degenbot-rpc`), no pyo3-in-cores
violation, no behaviour change.

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
- **`crypto`** → `degenbot.crypto`. Top-level `crypto.py` (81 lines: `function_selector`, `keccak256`, `event_topic`) becomes `degenbot.crypto`. Note: `function_selector` currently delegates to `_ffi.contract`; under the mirror it re-exports from `degenbot.contract`. `keccak256` and `event_topic` delegate to Rust FFI pyfunctions (parity pinned in `tests/test_crypto_parity.py`, ergo 5JKNQH).
- **`fork`** → `degenbot.fork`. Top-level `anvil_fork.py` (514 lines) becomes `degenbot.fork`, mirroring `_ffi.fork`.
- **`db`** → `degenbot.db` (genuine cross-cutting infrastructure — DB row types + ops, consumed by `database/`, `cli/`, `exceptions/`, `updater/`). Existing `database/_ffi.py` is the partial barrier; consolidate to `degenbot.db` as the single mirror home.
- **`deployments`** → `degenbot.uniswap.deployments` (see above — Uniswap-protocol-family identity, not cross-cutting).
- **`price`** → **not a single home.** `_ffi.price` exposes two distinct pyclasses consumed by two different domains: `PyChainlinkPriceFeed` → `degenbot.chainlink` (already re-exported from `chainlink/__init__.py`), `PyAavePriceOracle` → `degenbot.aave` (already re-exported from `aave/__init__.py`). The Rust crate `degenbot-price` is implementation; its pyclasses belong to their consuming domains, not a shared `degenbot.price`.

### Dead / test-only submodules (un-homed, lazy)

- **`executor`** — 0 callers anywhere (production or test). Truly dead surface. Stays un-homed; a `degenbot.executor` home appears only if a Python consumer lands. (The `contracts/` Vyper executor + `degenbot-executor` Rust crate exist, but no Python leaf reaches `_ffi.executor`.)
- **`subscriber`** — 0 production callers; test-only (`tests/fakes/subscribers.py`, `tests/test_pubsub_seam_parity.py` reach `_ffi.subscriber` for `PySubscription` / `register_subscriber`). Un-homed for production; when a production consumer appears it gets `degenbot.subscriber`. The test fakes import directly from `_ffi.subscriber` — under the strict Pydantic barrier, test code is leaf code and must import from the home once it exists; until then the test imports are the signal that a home is needed.

### Clean single-home submodules (reroute only)

`balancer_math` → `degenbot.balancer.math` · `cl_math` → `degenbot.uniswap.math` · `curve_math` → `degenbot.curve.math` · `solidly_math` → `degenbot.aerodrome.math` · `solady` → `degenbot.utils.solady` (existing subpackage `utils/solady/libzip.py` is the only consumer; mirrors 1:1) · `dex_identity` → `degenbot.uniswap.dex_identity` · `provider` → `degenbot.provider` (already a package) · `simulation` → `degenbot.dispatch` · `submission` → `degenbot.dispatch` (both under `dispatch/`) · `cancel` → `degenbot.updater` · `pool` → CLI-only consumer (`cli/pool.py`); `degenbot.pool` mirror home created when a non-CLI consumer appears, or `cli/pool.py` is the home itself if the CLI stays the sole consumer (verify during cutover).

## Executor command layer (degenbot-executor)

The layer that turns a solver result into the `bytes` passed to the on-chain
`cmd_executor.execute(bytes, config)`. Two first-class axes the grammar must
express — where the stream's entry capital comes from, and where its terminal
profit goes — are the load-bearing vocabulary for the axes refactor (epic
`463V2C`).

**Command stream** — the `bytes` payload `execute()` runs; the atomic unit the
command grammar emits. A stream is a sequence of compact opcodes against an
address table.
_Avoid_: "payload" (reserved for the solve-result → strategy seam, ADR-025).

**Encode request** — the per-path intake value the composer funnel consumes: the
path plus the solver's amounts (optimal input, per-hop outputs, per-hop consumed
inputs) plus the operator's declared axes, as one unit. One per path, built once
at the producing site. It is the contract the CL overfeed-clamp invariant
attaches to: `consumed_inputs[i]` is the executable input to hop i, and for an
over-fed CL hop it is the clamped value the on-chain exact-in loop terminates on
(UO3JM4).
_Avoid_: "command stream" (the `bytes` the request is encoded into). "payload"
(the ADR-025 solve-result → strategy seam bytes). "EncodeOptions" (the axis
bundle is a part of the request, not the request).

**Encode context** — the session-scoped bundle of deployment addresses (executor,
PoolManager, WETH) shared by every encode request in a session. One per session,
never per-path.
_Avoid_: folding it into the encode request (session scope re-stated per path).

**Command grammar** — the rules + per-shape-class description (protocol
sequence × funding source × profit capture × builder bribe) that derive a valid
command stream, including the ordering the stream must satisfy. Distinct from
the "command stream" it emits, and from the "composer" (the concrete encoder
that executes the grammar).
_Avoid_: "composer" for the model; "encoder" (the byte-layout details live in
the encoder methods the matrix calls).

**Funding source** — the declared origin of a command stream's **entry (seed)
capital**: chosen **at runtime per path by the strategy/operator** (an economic
knob — self-fund is cheaper gas for small opportunities, flash is needed to
access outside capital for large ones), not a fixed config. Exactly one per
stream. Values: **self-funded** (asset held by the executor), **pool
flash-loan** (a **flash source pool** — see below, in-path or off-path), a
**PoolManager free take** (a positive delta owed to the executor), an
external-lender flash (**Aave**), or **ERC-6909 burn-to-settle** (burn a held
claim to fund settlement). Inter-hop inputs and their sizing are an
implementation detail, not a funding decision.
_Avoid_: "capital source", "flash source".

**Profit capture** — the declared destination of a command stream's **terminal
profit** (the excess over the entry capital the stream refunds); one value per
stream. Values: **custody** (retained by the executor), **owner** (sent to the
immutable `OWNER_ADDR`), **native** (ETH), **ERC-6909 mint**, and (with the
Balancer integration) **Balancer Vault**. Modeled as a declared value even
where the current executor cannot yet express it.
_Avoid_: "profit taking", "settlement".

**Builder bribe** — a separately-declared payment (recipient + amount) a
command stream pays a block builder, **orthogonal to profit capture**: it is a
distinct output axis, not part of where the profit excess goes. Carried via the
`execute` `config` parameter / dedicated commands.
_Avoid_: "tip", "fee".

**Ledger** — the accounting target an operation reads from or writes to: the
executor's ERC-20 balance, the PoolManager delta, an ERC-6909 held balance, a
direct pool-to-pool handoff, or (with Balancer/Aave) an external Vault/lender.
The ordering invariant the grammar enforces is **credit-before-debit within a
ledger**.
_Avoid_: "realm", "book", "track".

**Hop coupling** — how one hop's output passes to the next: directly
pool-to-pool, via the executor balance, or via a ledger delta. Distinct from
"funding source" (the seed) — this is the inter-hop handoff, including the
**repayment pivot** by which a borrowed ledger is settled.
_Avoid_: "handover".

**Flash source pool** — the pool whose own swap-callback lends the stream's
entry capital (the "no-prefund" Uniswap-family flash borrow). Distinct from an
external lender (Aave) and from a non-flash funding source. A flash source pool
may be **in-path** (also a hop — the unified borrow-and-swap callback, repaid
by the path itself, last) or **off-path** (an independent borrowing point whose
capital is not part of the trade; e.g. a V2 pool that delivers the profit token
to the executor so we retain the excess).
_Avoid_: conflating with "funding source" (the axis) or "pool flash-loan" alone.

**Repayment pivot** — the derived hop or mechanism that settles a borrowed
ledger; chosen by token roles + hop coupling, never hand-picked. Part of the
derived enclosure, not a user axis.
_Avoid_: "repay hop" (implies a hop; a pivot may be a settle, not a swap).

**Derivation outcome** — the tri-state result of turning a shape-class into a
command stream: `Encoded` / `Decline` / `Reject`. `None` used to collapse
the last two into one value; they are meaningfully different:
- **Decline** — the derivation layer declines to encode a path's family (no
  producer/row for the shape, or a producer guard such as arity/`fits`
  returns `None`). A routine, expected outcome for an unsupported or
  unencodable path; the strategy skips it. Maps to `None` at the public
  `encode_cmd_stream` seam.
- **Reject** — a Plan *was* built (the producer returned a stream) but the
  ledger validator rejected it (`ValidationError`). By the D4 contract a
  successfully-built Plan never violates the ordering invariants, so a Reject
  is definitionally a latent bug: it is **always fatal** — the revm
  matrix/honesty suite hard-fails and a live run aborts. Never swallowed,
  never degraded to a skip.
_Avoid_: collapsing both under "None"/"unencodable"/"invalid"; treating a Reject
like a skip.

**Hop facts** — the per-protocol descriptor the Plan walker consumes to derive a
command stream: which ledgers a hop touches, credit/debit, direction, funding
and capture role, and repayment obligation. The *data* half of ADR-029 D4
("coupling/ledger facts as data"); a new protocol adds one hop-facts descriptor
+ one mechanics module, never per-family Plan bodies (per-shape enclosure
modules under `grammar_walker/shapes/` are the code half they feed).
_Avoid_: conflating with "ledger" (a hop-facts entry is per protocol; a ledger
is a location an operation reads/writes).

**Mechanics** (ADR-031 D4) — the shared step-primitive library the walker
shape modules compose: one builder per `PlanStep` variant (`v2_flash`,
`v3_flash`, `v2_swap`, `v4_swap`, `v4_unlock`, `v4_take_compact`,
`v4_settle*` …) plus the per-protocol facts builders (`v2_hop_facts`,
`v3_hop_facts`, `v4_hop_facts_netzero`). Flash primitives derive their
recipient routing from `facts.out_dest` by default; a shape passes an
explicit recipient triple only where the facts tag cannot express it
(e.g. a downstream pool's flash repayment). Since epic `6SWFBS` no shape
module builds a `PlanStep` literal — step construction is mechanics-only.
_Avoid_: "encoders" (the byte side) or "ledger" (the validator side).

**Enclosure** — the callback-nesting structure of a command stream —
which `FlashSwap`/`V4Unlock` wraps which, and the repayment order. Per ADR-029
D3 it is the grammar's output, never a user axis; per ADR-031 (as corrected
2026-08, epic `PZBGP7`) it is computed in six per-shape modules under
`grammar_walker/shapes/` behind a `(len, repay-sequence)` dispatcher — a
genuine `Repay`/`OutDest`-tag partition covers the single-V4-middle residual
only. The take-before-credit / terminal-V2-draw classes are caught by the
`LedgerValidator` + revm contract matrix (ADR-029 D5), not made
unrepresentable by construction.
_Avoid_: "nesting"/"wrapping" as the canonical term.

**Walker shape family (ADR-031, epics `PZBGP7` + `6SWFBS`)** — no family
has a hand-authored Plan body. `facts_for` sets per-variant facts plus
position-scoped axes (below); `derive_plan` routes on
`(len, repay-sequence)` gates to six per-shape modules under
`grammar_walker/shapes/` — the 3-hop rule-walkers
(`rule_walk_v2v3`, `rule_walk_v4_led`, `rule_walk_v2v3_v4_mixed`,
`tag_residual`) are themselves composed of the shared **mechanics**
primitives, and the 2-hop shapes (seed→V4, V4-led, all-V2 chain,
uniswap-only) are pure walks over them. Every shape module carries a
RED→GREEN honesty probe asserting zero `PlanStep::` literals in its walk
region, and byte-identity is pinned by per-shape golden stream tables +
`glopcn_bytepin` across every family × amount set × entry point.

**Terminal form** (`HopFacts.terminal_form`, epic T5) — how the trailing
hop of a V4-mid 3-hop shape completes: `DirectHandoff` (swap completes on
its own pool, output to SELF) vs `UnlockInternal` (trailing swap is an op
inside the enclosing V4Unlock's inner). Set on the terminal hop only,
consumed only by the merged `v3v4{v2,v4}` arm.

**Repay mechanism** (`HopFacts.repay_mechanism`, epic T6c) — the *physical*
across-hops repayment transport, distinct from the `repay` category:
AutoFromExecutor / TransferInCallback / V4TakeInUnlock (unlock-delta) /
DownstreamFlashDelivery / DownstreamTakeSeeds. Currently only
`AutoFromExecutor` is set (v3v2v4's leading V3 flash) — the vocabulary
exists as data so future plans set it, not as prose.

**Seed delivery** (`HopFacts.seed_delivery`, epic T6c) — how the WETH seed
reaches the pool that needs it: `Erc20Transfer` (callback prefund) vs
`V4TakeCompact` (in-unlock delta claim). Currently set only on v2v3v4's
hop0 (`V4TakeCompact`).



