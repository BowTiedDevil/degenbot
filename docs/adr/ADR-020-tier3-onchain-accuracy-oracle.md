# ADR-020: Tier-3 — On-Chain Accuracy Oracle (revm + Canonical Reference Bytecode)

**Status: accepted (architecture).** This ADR records Tier 3 as a first-class
tier in the ADR-005 dual-path coverage framework. It is **additive on
acceptance** — it defines the tier and its discipline, and the existing
Tier-0/1/2 mechanically-enforced gates (standalone promotion, static
reachability, behavioral dual-driver parity) are unchanged. Full mechanical
enforcement of a Tier-3 oracle for *every* CL-math capability the solver
reaches is **deferred** (see Deferred) until the pattern is proven across the
pool families. The seed corpus (V3 deploy+seed `just test-tier3-swap`, V3/V4
`computeSwapStep` `just test-tier3-step`) ships with the acceptance as
`#[ignore]d` examples
reachable only through explicit `just test-tier3-*` recipes — they build
canonical-reference bytecode, so they are not in the default
`cargo test`/`just test-rust` path.

> **Post-acceptance update:** the `just test-tier3-*` family recipes were
> consolidated into a single `just test-tier3 [family]` verb (no family = all
> families), and the family tests now ALSO run in the default `just test-rust`
> path — they load the committed tier-3 artifact bytecode toolchain-free.

## Context

The ADR-005 dual-path framework makes a testable claim — Rust and Python are
two consumers of one Rust core, both first-class — and enforces it
mechanically across three tiers:

- **Tier 0 (standalone promotion):** `examples/standalone_consumer.rs` proves
  a `cargo add degenbot` consumer reaches core capabilities with no Python in
  the build graph.
- **Tier 1 (reachability, static):** `reachability.rs` diffs what the PyO3
  binding reaches against what the umbrella re-exports.
- **Tier 2 (behavioral dual-driver parity):** a fixture driven through *both*
  `BotState` and `PyBot` must produce identical results from a shared JSON
  fixture.

Tier 2 is the load-bearing bug-catching tier for **cross-FFI-seam** divergence
(arg extraction, rounding, direction flags) — but its oracle for CL math is a
**Rust twin**: `v3_simulate_swap`/`v4_simulate_swap` re-derive the same
algorithm the engine uses. When the engine and its Tier-2 twin share a bug,
they agree with each other and both diverge from the canonical on-chain
contract. The just-fixed V4 `CurrencyNotSettled` failure was exactly this class:
the solver's `build_int_*_sequence` and the V4 `int_simulate_v3_swap` twin
both computed the same word-boundary-drop behavior; both agreed; both
diverged from the real `PoolManager.swap` bytecode. Only an oracle whose
reference is the **real canonical-deployed contract bytecode** breaks that
shared-bug class. That is Tier 3.

## Decision

### D1 — Tier 3 is the on-chain accuracy oracle: Rust math + solver === real canonical bytecode.

A Tier-3 oracle deploys the canonical, foundry-compiled reference contract
(V3 `UniswapV3Pool` for V3/V4 CL pools; V4 `PoolManager` for V4 singleton
swaps) as **real bytecode** in an in-process revm `CacheDB`, seeds its
storage slot-for-slot from a Rust `V3PoolState`/`V4PoolState` using the
`v3_storage_slots`/`v4_storage_slots` encoders, drives the swap, and asserts
the Rust `v3_simulate_swap`/`v4_simulate_swap` (and the solver's
`int_simulate_*` path) equals the on-chain `Swap`
event **byte-for-byte** (amount0, amount1, sqrtPriceX96, liquidity, tick).

The oracle's reference is the contract itself, not a re-derivation. This is
the structural difference from Tier 2: a shared implementation bug across the
engine and its twin is invisible to Tier 2 but REDs Tier 3.

### D2 — The harness-from-canonical-reference-contract discipline.

Canonical reference bytecode comes from the protocol's own source
(`lib/v3-core`, `lib/v4-core`), compiled with **foundry/solc at the protocol's
canonical build settings** (V3: solc 0.7.6, optimizer runs=800 — the settings
that produce a ≤24576-byte runtime deployable under EIP-170 as on mainnet).
The harness is a **thin Solidity deployer + callback**, not the production
executor: V3's `V3SwapOracleHarness` implements `IUniswapV3PoolDeployer`
(setparamatically populates `parameters()` before the `new UniswapV3Pool()`)
and `IUniswapV3SwapCallback` (mints the input token to the pool); V4's unlocker
harness implements `IUnlockCallback` (cycles `unlock`/`swap`/`settle`/`take`).
The harness never carries math — it only orchestrates the canonical contract.
This keeps the oracle focused on swap-math correctness.

### D3 — The V3 `setupPool()` pattern: full-TX-gas CREATE, not constructor 63/64.

Deploying a ~22 KB runtime (V3 `UniswapV3Pool`) inside a constructor's `new`
reverts in revm: the EIP-150 63/64 gas-forwarding rule starves the
`G_CODEDEPOSIT` charge (≈ runtime_bytes × 200 ≈ 4.4 M gas) when the
constructor has already spent on mock-token deploys. The fix is structural and
canonical for all Tier-3 harnesses: the constructor deploys the **mock tokens
+ sets `parameters()`**, and a **`setupPool()` external** performs the real
pool CREATE. The `setupPool()` CALL forwards the **full transaction gas**
(63/64 of ~16.7 M, well above 4.4 M). The oracle's pinned test
(`tier3_v3_pool_swap_vs_revm.rs`) drives deploy → `setupPool` → seed → swap.
This is a load-bearing revm-specific discipline, recorded here so future
harnesses do not rediscover it as a bare-revert OOG.

### D4 — Fully-consistent fresh-pool seeding via whole-slot writes (the LOK class is avoided by seeding, not by engine extension).

The pool's storage is seeded slot-for-slot directly from the Rust state using
the `v3_storage_slots`/`v4_storage_slots` encoders: `slot0` (sqrtPrice + tick
+ unlocked), `liquidity`, every `ticks(tick)` entry (gross|net packed word),
and every occupied `tickBitmap(word)`. `feeGrowthGlobal` and observation
fields are **zeroed** (the fresh-pool invariant: `encode_v3_slot0_fresh` sets
`unlocked = true` and zeros the observation cardinality/index). This is a
**fully-consistent fresh pool**: no half-seeded slot can trip the pool's
reentrancy lock (`LOK` revert) or an observation-growth branch. The LOK class
is avoided by whole-slot-set seeding (`insert_account_storage` writes the
full 32-byte word, not a field mask), NOT by extending the engine state with
storage-mutation primitives. The oracle stays a pure *reader* of engine state.

### D5 — When a Tier-3 oracle is REQUIRED.

A Tier-3 oracle is required for **any CL-math pool-state computation or
multi-step solver crossing calc** the engine performs: V3/V4
`v3_simulate_swap`/`v4_simulate_swap`, the solver's `build_int_*_sequence` +
`int_simulate_v3_swap`, and any `compute_swap_step`-orchestrated walk that
crosses tick word boundaries. A pure V2 (constant-product) calc has a
closed-form oracle (Tier 2 suffices); CL math does not — the multi-step walk
with its liquidity-net application and word-boundary tick search is where the
shared-bug class lives. New CL-math capabilities that cross the FFI boundary
and reach the solver should grow a Tier-3 slice.

### D6 — The shared-fixture discipline: pinned mainnet regression + proptest fuzz.

Two fixture shapes, in that order:

1. **Pinned regression**: a captured mainnet path (or a hand-authored minimal
   state) with a recorded expected `Swap` event. Byte-exact, fast, the
   stability anchor.
2. **Proptest fuzz**: over (pool state incl. SPARSE tick topologies spanning
   uninitialized word boundaries, amount, direction, zero_for_one) against the
   same on-chain oracle. This is the tier's payoff — sparse-edge topologies no
   human need think to author are generated and RED against the canonical walk
   before the bug ships. The V4 `CurrencyNotSettled` word-boundary-drop class
   is the canonical example a fuzz case would have caught.

## Consequences

- Tier 3 is now **part of the default suite**. The harness bytecode is committed
  under `tier3-oracle/artifacts/`, so the oracle tests run in the default
  `cargo test`/`just test-rust` path with NO toolchain at runtime. Stale-bytecode
  drift is guarded two ways: `tier3_harness_artifacts.rs` (toolchain-free) hashes
  the tracked harness sources against `artifacts/manifest.json`, and
  `just verify-tier3-artifacts` (the authoritative compile-vs-use check, in the
  CI `tier3-oracle` job) recompiles every harness with the real solc/forge
  toolchain and asserts the committed bytecode equals a fresh build. `just
  test-tier3-{step,swap,v2,v4,curve,balancer}` rebuild + republish the
  artifacts and re-run the families; `just rebuild-tier3-artifacts` republishes
  without running. The pre-push hook runs the oracle tests (committed bytecode);
  the toolchain compile check runs only in CI.
- The standalone-Rust-core invariant (ADR-005) is unaffected: Tier-3 oracle
  *tests* live in `rust/crates/<crate>/tests/` and depend on `revm` as a
  **dev-dependency** (gated), never as a core-crate dependency. The no-pyo3
  and no-network-in-cores invariants hold.
- The `v3_storage_slots`/`v4_storage_slots` encoder crates become
  **doubly-load-bearing**: they are both the production seeding layer (the
  DB-aware pool updaters) and the Tier-3 oracle's seeding path. A drift
  between an encoder and the canonical storage layout now REDs a Tier-3 test,
  not just a round-trip self-test. The V3 encoder's gross|net bit order is
  validated against the real `UniswapV3Pool` runtime (Solidity big-endian:
  first struct field = HIGH 128 bits), not just round-trip encode/decode.
- The deploy + seed pipeline proven for V3 (deploy → `setupPool` → seed →
  reads-back-byte-exact) is the **load-bearing prerequisite** for V4
  `PoolManager` oracle reuse: the V4 unlocker harness reuses the same revm
  `CacheDB` seeding discipline with the V4 singleton at its canonical address.

## Alternatives considered

- **Tier-3 oracle as a re-derivation (a second Rust twin).** Rejected: this is
  Tier 2. The shared-bug class survives a twin. Only real bytecode breaks it.
- **Deploy the production `cmd_executor.vy` as the harness.** Rejected: the
  oracle's scope is swap MATH, not the executor's transaction encoding or
  funding model. A thin Solidity deployer + callback keeps the oracle focused
  (mirrors ADR-019's strategy-vs-engine separation).
- **Seed via `StateOverride` (revm pre-execution overrides).** Rejected by
  ADR-019 D2: `CacheDB` insertion is the sole state-override mechanism. Tier 3
  reuses the same `insert_account_storage` path the simulation engine uses.
- **Mechanical enforcement from day 1 (a `reachability.rs`-style guard
  requiring a Tier-3 oracle for every CL-math capability).** Deferred (next).

## Deferred

- **Full mechanical enforcement.** The ADR-005 dual-path enforcement
  (`reachability.rs` self-cleaning guard for the umbrella allowlist) does not
  yet require a Tier-3 oracle per CL-math capability. That guard is added
  once the pattern is proven across V3 *and* V4 *and* the proptest fuzz is
  green on both — tracked as the enforcement sub-task of epic `UP5NH6`
  (task `BQ43DK`). The ADR-005 framework is explicitly *extensible* to a
  fourth tier; this ADR is that extension's definition, not its enforcement.
- **V4 `PoolManager` byte-exact swap oracle.** The V4 unlocker harness reuses
  the V3 deploy+seed pipeline's discipline (D3/D4) at the V4 singleton
  address; the V4 slice is a sub-task of `2LTKVO`.
- **Dense-tick fixtures for the V3 swap byte-exact assertion.** A single
  `[-spacing,+spacing]` position OOGs once the swap reaches the isolated
  boundary — V3 treats the boundary as an uninitialized word-0 edge, defers
  the liquidity net, and walks to `MIN_TICK` with phantom liquidity. Crossing
  behaviour needs **overlapping positions** (a dense active range) so the
  crossed tick's net application leaves liquidity > 0. This is the remaining
  slice of `2LTKVO`; the deploy+seed foundation and the reads-back-byte-exact
  anchor are landed (commit `908ab604`).

## References

- [ADR-005](ADR-005-polars-inspired-three-layer-architecture.md) — the dual-path
  coverage framework Tier 3 extends (Tier 0/1/2 definitions).
- [ADR-019](ADR-019-in-process-revm-sole-simulation-executor-strategy-engine-separation.md)
  — in-process revm as the sole simulation executor; Tier 3 reuses the same
  revm `CacheDB` seeding stack offline (D2: `CacheDB` insertion is the sole
  state-override mechanism).
- [`docs/architecture/in_process_sim_served_slots.md`](../architecture/in_process_sim_served_slots.md)
  — the V3/V4 storage-slot layout reference the encoders mirror.
- AGENTS.md "Dual-Path Coverage Tests" — the Tier 0/1/2 definitions this ADR
  extends with Tier 3; the mechanical-enforcement update is task `BQ43DK`.
- `just test-tier3-step` / `just test-tier3-swap` —
  the seed recipes (`tier3-oracle/`).
