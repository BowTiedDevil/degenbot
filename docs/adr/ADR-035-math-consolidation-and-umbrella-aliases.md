# ADR-035: Consolidate the AMM math family into `degenbot-math` + umbrella alias convention

**Status:** Accepted
**Date:** 2026-08-20
**Task:** ergo ZUOZDR

## Context

- The workspace has 26 publishable crates; five of them are AMM invariant-math ports: `degenbot-v2-math`, `degenbot-concentrated-liquidity-math`, `degenbot-curve-math`, `degenbot-balancer-math`, `degenbot-solidly-math`.
- crates.io has **no name reservation** — every published name is a permanent footprint: its own README, keywords, dependents, and version lineage.
- The research handoff (`docs/handoffs/crates-io-publishing-prep.md`) warns against over-splitting (citing ruff/uv internals as a caution) and its "don't shrink" guidance assumed a *post-publish* world. Pre-first-publish, consolidation is free.
- Crate identity test for publishing: (1) does it have value types / API a consumer uses directly? (2) independent external-reuse demand? (3) version independence? (4) a natural home for docs + keywords?

Facts about the five math crates: all carry the ADR-009 lockstep version (one version literal in the tree — zero independence); the same consumer set (pools, bot, solvers, umbrella; db + rpc for CL); no consumer wants a single family in isolation; no external-reuse demand signal; each is byte-exact ports of canonical Solidity math (canonical sources cited in each module doc).

## Decision

1. **Merge the five math crates into one new crate `degenbot-math`** with modules `v2`, `cl`, `curve`, `balancer`, `solidly`. Paths read as `degenbot_math::cl::`, `degenbot_math::curve::CurveDyCalculator`, etc. (each family's current root re-exports become its module root). Test suites move intact: the CL tier-3 REVM oracle (`tier3_compute_swap_step_vs_revm` + proptest regressions), the three `oracle_crosscheck` suites + snapshots (renamed with family suffixes to avoid name collisions), and the threeCriterion benches. Workspace members 28→24; publishable 26→22. The five old names are never published — no deprecation path needed.
2. **Umbrella alias convention** — whole-crate re-exports strip the `degenbot_` prefix (precedent: `alloy` does `pub use alloy_sol_types as sol_types;`, and alloy's own `pub use alloy_primitives as primitives;` shows even a *name clash inside the umbrella* is handled by giving the sub-crate the shorter name): `degenbot::pools`, `degenbot::rpc`, `degenbot::db`, `degenbot::fork`, `degenbot::simulation`, `degenbot::arbitrage`, `degenbot::submission`, `degenbot::execution`, `degenbot::decoders`, `degenbot::price`, `degenbot::aave`, `degenbot::pool_updater`, `degenbot::pathfinding`, `degenbot::order_index`, `degenbot::abi`, `degenbot::math`, `degenbot::solvers` (the `degenbot-solvers` crate takes the bare name), and `degenbot::cmd_executor` — an **intentional non-strip**: bare `executor` would be a one-letter visual collision with `degenbot::execution`, so the command-executor keeps its disambiguating `cmd_`.
3. **Partial (module-level) hoists stay**, plus the whole-crate alias alongside: `degenbot::core` (alias) with `address_utils`/`errors`/`hex_utils`/`runtime`/`eip_1559` still hoisted to the root; `degenbot::bot` (alias) with `bot_core` still hoisted — the `solvers` hoist from `degenbot_bot` is **dropped** so `degenbot::solvers` names the solvers *crate*; the engine lives at `degenbot::bot::solvers::arb_engine`. `degenbot::uniswap` (alias) with `dex_identity`/`v2_encoding` still hoisted. Root convenience hoists of items (BotState, PoolEntry, builders, lifecycles, `preset_for_variant`, `DexIdentity`, `ReservesAbi`, …) are unchanged.
4. **`degenbot-solvers` keeps its identity** — it is path-level optimization math that *consumes* the math crates; merging it into `degenbot-math` would invert a dependency. **`degenbot-fork` keeps its identity**; the runtime-vs-dev split question is a separate follow-up (see MYYV2X scope note).

## Alternatives considered

- **Status quo (26 names):** the identity test fails for the math family (no independent consumers, no version independence, one provenance story); the footprint is a permanent cost.
- **Merge the math into `degenbot-pools`:** pools is pool/token *state*; the math crates have independent consumers (solvers, db, rpc, simulation) that would then pull all of pools. Blurs two identities instead of clarifying one.
- **Defer until post-publish:** impossible in practice — after first publish, splitting a crate back out is deprecation + redirects + version drift, the expensive direction.

## Impact / migration

- 8 consumer manifests re-point (pools, bot, solvers, db, rpc, umbrella, python, simulation-dev) and ~59 source files get a scripted, longest-name-first import rewrite (`degenbot_<x>_math` → `degenbot_math::<mod>`).
- Umbrella in-crate paths (`examples/standalone_consumer.rs`, its tests) move to the aliased paths.
- Python layer: `degenbot-python` (the `degenbot_rs` wheel crate) imports the math crates at `degenbot_math::<mod>::` paths — the wheel's public API is unchanged.
- `just test-tier3 step` family retargets `pkg=degenbot-math`.
- MYYV2X (CI dry-run gauntlet) and S7R5K4 (publish loop) operate on the recomputed 22-crate order; S7R5K4's name-reprobe list is 24 names (22 + `degenbot_rs` + the example crate).
- Escape hatch: extracting a family back into its own crate later is an ordinary refactor + `cargo new` — allowed when real versioning demand appears.

## Verification (gates)

Existing oracle/unit suites run unchanged in the new crate home (the crosscheck + REVM oracles are the safety net); `just check-no-pyo3-in-cores` unchanged; G1 `just publish-dry-run` green on the final 22-crate layout; `cargo build --workspace` + full workspace test run green. One refactor commit, tagged with the ergo task.
