# ADR-003: BotCore as the state layer, peer to UniswapEngine

**Status: accepted.** Recorded during the BotCore/UniswapEngine separation-of-concerns grilling, June 2026. Implemented in full by Plan 100 (Slices 1–5): V2/V3/V4 state consolidated into `BotCore`; the `V2BlockEngine`/`V3BlockEngine`/`V4BlockEngine` are dissolved (single live pool-state owner; engine-then-core lock order); the legacy `RustPoolCache`/`ArbPoolCacheAdapter` mirror is deleted (Slice 4 Option D); `PyToken` is completed (Slice 5). Supersedes the implicit arrangement where each block engine owned a private pool-state `HashMap` and `BotCore` sat unused.

## Context

`BotCore` (`rust/src/bot_core/mod.rs`) was designed as the single Rust owner of all runtime pool/token state, with thin `PyPool`/`PyToken` handles over `Arc<Mutex<BotCore>>`. It is the only Rust struct with reorg-rollback journals (`v2/v3_restore_before_block`, scalar + per-tick priors) and the only one that ever returned `PyPool`/`PyToken` handles.

`UniswapEngine` (`rust/src/optimizers/uniswap_engine/`) was prototyped as a mixed V2/V3/V4 arbitrage engine — a tracer bullet. In production it grew to own pool state directly: each of `V2BlockEngine`, `V3BlockEngine`, `V4BlockEngine` holds a private `HashMap` of pool state (reserves/tick_data/sqrt_price), and the PyO3 wrapper exposes ~40 methods, most of which are state registration/mutation/snapshot/verification rather than solving. `BotCore` is instantiated nowhere (production uses `UniswapArbEngine`; the library solve path uses `RustPoolCache` via `ArbPoolCacheAdapter`, a parallel Rust backend). Its V3 `calculate_tokens_out` is a stub returning `U256::ZERO`.

The result is two parallel Rust state implementations of the same V2/V3 pools, plus a third live copy inside `RustPoolCache` for the legacy solver path.

## Decision

Adopt **Option 1 — `BotCore` as a peer module.** `BotCore` becomes the single owner of pool and token state (V2, V3, and a new V4 variant). `UniswapEngine` keeps path registry, solver dispatch, result batching, the pump, and diagnostics — and reads/mutates state *through* `BotCore` via a shared `Arc<Mutex<BotCore>>`, not through private per-engine stores.

The two lock in a fixed order when nested: **engine-then-core**. The pump (single Tokio task) holds the engine lock for its per-block coordination; resolve-time briefly acquires the core lock inside the engine lock to re-derive solve-ready hop states. No code path acquires them in the opposite order.

`PyPool` and `PyToken` become the construction entry point and per-pool read handle — independent of the engine — mirroring ADR-001's "thin handles over Rust-owned state."

## Considered options

- **Option 2 — State as a field of `UniswapEngine`.** One lock (`Mutex<UniswapEngine>`) covers state + solve coordination. Rejected: makes BotCore unusable without the engine, so the legacy `ArbPoolCacheAdapter` + `RustPoolCache` path can only retire by deletion of the Python side, leaving a parallel Rust store. Entangles state with pump coordination.
- **Option 3 — `BotCore` as a field of the engine, same lock.** Same single-lock benefit as Option 2 with a cleaner internal seam. Still rejected for the same reason as Option 2: the legacy path cannot migrate onto it.
- **Option 1 — peer modules.** Costs lock-ordering discipline and an extra critical section at resolve time. Chosen because (a) it is the only topology that retires the legacy `RustPoolCache` second Rust backend at all (see "Legacy solver path retirement" below — the retirement is by deletion, not migration), (b) it makes a future non-arb consumer of Rust state (sync calc, Curve port) possible without standing up the whole pump, and (c) the deadlock surface is empty in practice: the pump is single-writer on the engine lock during the hot loop; the only Python contender is `latest_results`, which reads engine-local `self.results` and does not touch the core lock.

## Core-lock placement: per-call (Option A)

Within the engine-then-core lock order, the core (BotCore) lock is acquired **per pump step, inline**, with no engine-side mutation buffer:

- **Per WS log (`apply_log`):** pump holds engine lock, briefly nests the core lock, mutates BotCore directly (no copy, no buffering), releases both. Inserts affected pool IDs into the engine's dirty sets exactly as today. BotCore is current the instant `apply_log` returns — the literal eager-processing invariant.
- **Per coalesced solve (`solve_dirty`):** pump holds engine lock, takes the core lock **once** for the re-derive of all N affected paths (single consistent snapshot, no re-acquire thrash), releases, then `solve_path` runs pure `&self` over the per-path resolved cache exactly as today.

This is Option A over Option B (engine-side transient mutation buffer drained only by `solve_dirty`). A was chosen because:

1. The performance argument for B was illusory at mainnet scale — the uncontended `parking_lot::Mutex` (~25ns) × ~15 relevant logs/block ≈ 375ns/block against a 12s block time.
2. `solve_dirty` under A takes the core lock once for the whole re-derive and sees a consistent snapshot, exactly as B would — the re-derive benefit is not B-exclusive.
3. B reintroduces a transient pool-state copy on the engine, however short-lived, which muddies ADR-003's clean "the engine owns no pool state" claim and adds drain invariants. A preserves it literally.

## Lock-ordering rule

**Engine-then-core is the only direction that ever nests, and the two locks are taken by disjoint caller sets.**

- Python-facing `PyBotCore` methods (`register_v2_pool`, `update_v2_pool`, `get_pool`, `calculate_tokens_out`, etc.) take the **core lock alone** — they never call into the engine.
- The pump and engine methods take **engine-then-core** when they need both.
- **No code path holds the core lock and then calls into the engine.** Such a path would invert the order and deadlock against an engine-then-core caller. A future Python method that wants both must take the engine lock first.

The deadlock surface is empty in practice: the pump is single-writer on the engine lock during the hot loop; the only Python contender, `latest_results`, reads engine-local `self.results` and does not touch the core lock.

## Reorg detection and restore (Option α)

**Detection lives on the pump** (it owns WS event ordering already). The pump reads the canonical reorg signal — the `removed: bool` field Alloy exposes on every `eth_subscribe` log — rather than inferring forks from block-number comparison. `removed: true` means the log was orphaned; `removed: false` means it's on the canonical fork (either fresh or re-emitted after a reorg). Detection by `removed`-flag is authoritative: it has no false positives from out-of-order delivery and catches the case where a removed log's block number is still ≥ `last_solved_block` (which a block-number heuristic would silently drop).

**Restore coordination lives on the engine** (it spans the solver's derived state and `BotCore`'s pools). On detecting a reorg, the pump calls `engine.handle_reorg(target_block)` under the engine lock, which:

1. Acquires the core lock (engine-then-core ordering) and calls `core.restore_all_pools_before_block(target)` — per-pool `ReorgJournal::restore_before_block(target)`. Pools untouched by the reorg have no delta at/after `target`, so this is a no-op per pool; touched pools get scalar + per-tick priors restored. Idempotent.
2. Invalidates `path_resolved` for all paths (marks every path dirty) — derived state was built from pre-restore pool state.
3. The next `solve_dirty` re-derives and re-solves naturally; `compute_diff_and_send` emits `expired`/`updated`/`removed` diffs against the `delivered` set. Python sees forked paths vanish from batches without an "un-receive" — `delivered` stays the truth of what Python has seen.

**Journal depth is user-configurable, default 1 mainnet epoch (32 blocks).** A reorg deeper than the journal cannot be restored from deltas — that path is fail-stop the pump with a diagnostic (a &gt;32-block reorg on mainnet is a Severity-1 event where auto-recovery can mask bigger problems). The user-configurable depth lets operators on other chains (or with different risk tolerance) raise or lower the bound.

**Mid-block reorg window is accepted as inherent to eager processing.** If `apply_log` applied logs from block N before the next WS message reveals N was reorged, the journal's delta-pushing `apply_*` (ADR-003) makes those applies recoverable — `restore_before_block` undoes them — but the window exists. Deferring `apply_log` until block finality would close the window but destroys the zero-latency property that is the whole point of the eager architecture. Accepted as a known bound; recovery via the journal is first-class rather than "restart from snapshot."

## Pool's authority over its own math

A pool instance owns both its state and its single-pool swap math; the solve engine reads state **by reference** for path-level optimization (Mobius) or threads pool-to-pool by calling each pool's swap calc — but never mutates pool state arbitrarily during a solve. Pool state changes only through recognized event-application methods (`LiquidityMap::apply_swap`, `apply_liquidity_update`) that push a delta to the Reorg Journal.

This mirrors the Python `UniswapLpCycle` pattern: it iterates `self.swap_pools`, calls `pool.calculate_tokens_out_from_tokens_in` per hop, threads `output → next input`, and never mutates a pool. The Rust consequence is the engine↔core read/write asymmetry: `UniswapEngine` reads `BotCore` state via reference-returning accessors (e.g. `core.v3_map.get_pool(&key)`); only `LiquidityMap.apply_*` methods mutate, and every mutation journals a delta. Without this rule, a future engine implementer could mutate pool internals mid-solve under the engine lock and corrupt the journal or the per-pool derivation cache.

**Consequence for calc placement (Q8):**

- `BotCore::calculate_tokens_out` / `calculate_tokens_in` and `PyPool`'s thin-handle versions **stay on `BotCore`** — they are the per-pool swap math over state, the Rust-core analogue of `LiquidityPool.calculate_tokens_out_from_tokens_in`. The legacy `RustPoolCache` mirror (retired under Option D, above) was the only *other* place single-pool calc previously lived; its deletion makes `BotCore` the single home.
- The V3 stub (`calculate_tokens_out` returning `U256::ZERO`, "Slice 7 not implemented") **needs implementation** — V3/V4 single-pool CL swap math is required for the future-state single-pool-query consumer pattern ("If I swap 100 USDC into this V3 pool, how much WETH do I get out?").
- `BotCore::encode_swap` **stays on `BotCore`** — per-pool swap calldata encoding (V2 today; V3/V4 pending); reads immutables the pool already owns. The production-path command-stream encoding (`encode_cmd_stream` in `eth_backrun_helpers.py`) stays Python-side — a different layer composing per-pool calldata into an executor payload, not single-pool encoding.

## Legacy solver path retirement: delete, not migrate

The legacy library solve path is `ArbitragePath` (Python) → `ArbSolver` (Python, holds a Rust `RustPoolCache`) → `RustPoolCache` (Rust PyO3 surface that mirrors Python pool reserves and clones per-path hop state). `ArbPoolCacheAdapter` (Python) subscribes to Python pool state updates and pushes reserves+fee into the Rust `RustPoolCache`. This is a **second Rust backend** alongside the production `BotCore`/`UniswapArbEngine` path.

**Decision: delete the mirror.** Specifically:

- Rust: `RustPoolCache`, `RustIntHopState`, `RustArbResult` PyO3 classes deleted from `mobius_py.rs`.
- Python: `ArbPoolCacheAdapter` deleted. `ArbSolver`'s registered-path surface (`register_pool`, `update_pool`, `remove_pool`, `register_path`, `update_path`, `update_all_paths`, `remove_path`, `solve_registered`, `solve_registered_ints`, `solve_cached`, `solve_cached_batch`, `get_pool_cache`) deleted. `ArbSolver.solve(SolveInput)` retained on the pure-Python f64 path (the Rust fast paths were already removed long ago per `rust/CONTEXT.md`).
- Python: `ArbitragePath` unchanged — it builds `SolveInput` from pool state at solve time and calls `solver.solve(...)`, which never depended on the mirror.
- Rust: `PyBotCore`/`PyPool`/`PyToken` gain nothing from this retirement — they serve the production path, not the legacy one.

### Considered options

- **M — migrate the mirror onto `BotCore`.** `ArbPoolCacheAdapter` would retarget from `RustPoolCache` to `PyBotCore`; the registered-path solve APIs would land on `BotCore`. **Rejected**: this re-pollutes `BotCore` with solver-shaped concerns (path registry, registered-path solve) that ADR-003 keeps on `UniswapEngine` — `BotCore` owns *state*, the engine owns *solving*. Migration isn't a clean retirement, it's moving the pollution to a new home. Worse, it would leave two Rust backends reading the same conceptual pool state through two different Python adapters.
- **K — keep as-is.** Leaves two parallel Rust backends indefinitely; the future architecture (one Rust core, one pump-driven engine, thin Python handles) never arrives. Rejected.
- **D — delete the mirror.** Chosen. After deletion, Rust state lives in exactly one place (`BotCore` + its `LiquidityMap`s); Python reads through one handle family (`PyPool`/`PyToken`/`PyBotCore`); Python solves through one engine (`UniswapArbEngine`) or through the pure-Python `ArbSolver.solve(SolveInput)` for library consumers without a mirror.

### Why D serves the long-term goal (Rust core + thin Python interface)

M and K both preserve the *second* Rust backend indefinitely. D removes it today. When Curve ports to Rust, the question "does new Curve state live in `BotCore`'s `LiquidityMap` or in `RustPoolCache`?" has only one answer under D; under M/K it's ambiguous and the easier path is to keep using the mirror — perpetuating the split. D makes the architecture converge under its own gravity: one Rust core, one engine, thin handles, and a pure-Python fallback left in place for not-yet-ported families until their Rust-native equivalents under `UniswapArbEngine` arrive (at which point `ArbSolver.solve(SolveInput)` itself deletes).

### Cost acknowledged

`ArbitragePath` consumers lose the *unused* Rust-accelerated registered-path solve path. The drop is from a speed nobody gets (no library caller exercises `solve_registered`/`solve_cached` — verified: `ArbitragePath` calls only `solver.solve(SolveInput)`, the pure-Python f64 path) to a speed they were always going to get once the mirror retired. Production (`examples/eth_backrun_v2_v3_v4_rust.py`) uses `UniswapArbEngine` directly and never touches `RustPoolCache`/`ArbPoolCacheAdapter`/`ArbSolver`.

### Deletion-order caveat for the plan

`ArbPoolCacheAdapter`'s `isinstance(pool, CacheablePool)` check and the `CacheablePool` protocol (`reserves_for_cache`/`fee_for_cache`) are used *only* by the adapter (verified). The V2/V3/Aerodrome pool classes' `reserves_for_cache`/`fee_for_cache` methods may be exposed for other reasons — the plan's deletion-test step must verify before removing the protocol itself.

## Token state: Rust owns metadata, Python owns the price oracle

Token state splits along the same line as pool state under ADR-003: **Rust owns the state it computes on; Python owns what it orchestrates.**

- **Rust-owned (`BotCore.tokens: HashMap<Address, TokenEntry>`):** address, decimals, symbol, name — the immutable metadata that Rust-core computations need without a Python round-trip. Two such computations drive this: (1) decimal normalization for cross-token profit comparison and multi-hop output reconciliation, (2) token-equivalence rules in path validation (e.g. WETH == EtherPlaceholder on chains with native ETH). `PyToken` is the thin handle reading this state — the analog of `PyPool` for tokens, not a stub.
- **Python-owned (`Erc20Token` wrapping a `PyToken`):** the price oracle (`ChainlinkPriceContract`), an I/O construct with its own subscription/refresh lifecycle that cannot move to Rust. `Erc20Token` becomes an orchestration layer over a `PyToken` handle, adding price-oracle + display concerns — the same split as `PyPool` (Rust: state+math) vs. `PyBotCore`/`Bot` (Python: I/O orchestration).

`build_erc20token` stays Python-side as the construction entry point — it fetches metadata from RPC, constructs the `PyToken` in `BotCore`, and wraps it with the price oracle.

### Considered options

- **T2 — delete `BotCore.tokens`/`PyToken` entirely.** Considered and rejected during grilling. Rust-core computation wants decimal normalization and token-equivalence rules; without Rust-owned token metadata those computations either round-trip Python on every comparison or simply can't exist in Rust. Pulls against the future architecture (the whole point of Rust core is avoiding per-computation Python round-trips).
- **T1 — keep as-is structurally, leave `PyToken` as `address`-only stub.** Rejected: `address` is already on `PoolEntry` (token0/token1), so a stub returning only `address` adds nothing. The interface has to be completed (`decimals`/`symbol`/`name` getters) or the struct is dead weight.
- **T3 — complete `PyToken` as a real read handle (chosen).** Rust owns token metadata as state; `PyToken` reads it; Python's `Erc20Token` wraps the handle with the price-oracle I/O layer. Same line as the pool-side split.

## Consequences

- The block engines lose their private pool-state `HashMap`s. They are **dissolved**: V2's engine (no non-state concerns) deletes entirely; the V3/V4 buffer-apply-verify trinity moves into `BotCore` as a first-class **`LiquidityMap`** concept (one per CL family, keyed by `Address` / `(Address, PoolId)`). Accurate pool state is a cross-cutting concern, not a solver-engine concern — diagnostics, verification, classifiers, and a future Curve port all consume it without going through the solve engine.
- `UniswapEngine`'s `apply_log` becomes a *consumer* of `BotCore`'s `LiquidityMap`, not an owner; solver dispatch, path registry, result batching, the pump, and diagnostics stay on the engine. The 9 ad-hoc `liquidity_verifier::*` call sites in `py_binding.rs` collapse onto `LiquidityMap::verify_against_onchain`.
- **Result batching stays on `UniswapEngine`** — `result_tx`, `delivered`, and `compute_diff_and_send` (incremental `ResultBatch` diffs) are solver-shaped (diffing the solver's `results` against what Python has `delivered`), not state-shaped. BotCore has no role in result batching.
- The eager-processing architecture is preserved literally: `apply_log` mutates BotCore immediately (no buffer, no lag), `solve_dirty` coalesces the re-derive+solve exactly as today.
- Reorg rollback (`restore_before_block`) reaches the live hot path for the first time — currently the pump applies events forward only and **ignores the `eth_subscribe` `removed: bool` flag** on log events (the canonical reorg signal). Wiring this is a new behavior, not a mechanical port.
- `RustPoolCache` / `ArbPoolCacheAdapter` are retired by **deletion** (see "Legacy solver path retirement" below) — their third live copy dissolves along with the Rust `RustPoolCache` PyO3 surface itself.
- Single-pool swap calc (`calculate_tokens_out`/`calculate_tokens_in`) and swap encoding (`encode_swap`) **stay on `BotCore`** — per-pool math over state, mirroring Python's `calculate_tokens_out_from_tokens_in`. The V3 stub needs implementation (V3/V4 single-pool CL swap math required for the future-state library consumer pattern).

## Generalization discipline

`LiquidityMap` is implemented as **concrete per-family types** (`LiquidityMap<V3PoolState>`, `LiquidityMap<V4PoolState>`) sharing the generic `LiquidityEventBuffer`. No trait abstraction yet — only two CL families share the shape today, and Curve's per-block-cache architecture is explicitly different (mirror-free, inline dependency resolution). A third sample is required before extracting an abstraction, per the project's ruling against abstracting against a sample of one.

## Related

- **ADR-001** (I/O-free pools) — `PyPool`/`PyToken` as thin handles over Rust state is the same shape; this ADR extends it across the FFI state-ownership seam.
- `rust/CONTEXT.md` already names the block engines "transitional … repoint their solve calls to gen-3"; this ADR completes that transition by moving their state down into `BotCore`.
- **ADR-004** (Typed TickMap boundary for CL verifier + liquidity-apply seam) — resolves the per-pool CL state seam this ADR left flat. The {Slot0 Head / Tick Bookkeeping Map} term in `rust/CONTEXT.md` recorded the split as "held as a non-structural distinction" pending a typed-boundary consumer; ADR-004's survey found that consumer (six `takes-whole-but-wants-one` sites: `verify_v3_pool`/`verify_v4_pool` + `apply_v3_liquidity_update`/`apply_v4_liquidity_update` + their batch entry points) and adopts a `TickMap` trait that carries the "don't read slot0" rule in the type system instead of a module doc comment. ADR-004 also cleans up three stale CONTEXT.md terms (`{LiquidityMap}`, `{PyBotCore}`, `{PyPoolCache}`) that outlived this ADR's "Legacy solver path retirement: delete, not migrate" deletion of `RustPoolCache`/`PyPoolCache`.
