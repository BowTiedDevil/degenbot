# Balancer V2 — Contract Reference

Operational details for Balancer V2 weighted/stable pools: which canonical
contracts the Rust engine implements (the Python companions are thin shells
over it), how each contract generation is detected on-chain, and the
deployment lineage used as parity ground truth.

Sources scraped for this reference (2026-09):

- balancer-v2-monorepo @ `f8b6f44f21afaf3c802536ed478277f945e7f256`
  (2026-05-28, "Bump hardhat minor version (#2652)") — also the vendored
  reference for the Tier-3 oracle.
- balancer-labs/balancer-deployments @ HEAD (`addresses/base.json`,
  `addresses/mainnet.json`).

## Canonical math pin (Tier-3 oracle)

`tier3-oracle/src-balancer/BalancerSwapOracleHarness.sol` compiles the
canonical library sources fetched at the pinned commit into
`tier3-oracle/lib/balancer-src/`:

| Vendored source | Canonical package | Version at pin |
|-----------------|-------------------|----------------|
| `FixedPoint.sol`, `LogExpMath.sol`, `Math.sol` | `pkg/solidity-utils` | 4.0.0 |
| `WeightedMath.sol` | `pkg/pool-weighted` | 2.0.1 |
| `StableMath.sol` | `pkg/pool-stable` | 0.1.0 |
| (vault interfaces) | `pkg/vault` | 3.0.1 |

The harness reproduces the fee/scaling/direction sequence of the Rust engine
(`degenbot-pools::simulate_balancer_weighted_swap` /
`simulate_balancer_stable_swap`), which is the sole owner of Balancer swap
math; the Python companions are thin shells over it. Byte-exact parity is
asserted in
`rust/crates/degenbot-pools/tests/tier3_balancer_swap_vs_revm.rs`
(`just test-tier3 balancer`); artifact integrity is enforced by
`tier3_harness_artifacts.rs` + `verify-tier3-artifacts.sh`.

## Contract-generation matrix

| Contract | Deployment lineage → factory | Swap math | Rust engine | Python companion |
|----------|------------------------------|-----------|-------------|------------------|
| `WeightedPool2Tokens` (2021) | mainnet `20210418-weighted-pool` → `WeightedPool2TokensFactory` `0xA5bf2ddF098bb0Ef6d120C98217dD6B141c74EE0`, `WeightedPoolFactory` `0x8E9aa87E45e92bad84D5F8DD1bff34Fb92637dE9` (DEPRECATED) | pow **V1** (FixedPoint without the `TWO`/`FOUR` fast paths) | YES — `PowVersion.V1` | shell over Rust core |
| `WeightedPool` (current, post-2022) | Base `20230320-weighted-pool-v4` (ACTIVE) → `WeightedPoolFactory` `0x4C32a8a8fDa4E24139B51b456B42290f51d6A1c4`; mainnet `20230206-weighted-pool-v3` / `20230320-weighted-pool-v4` | pow **V2** (LogExpMath fast paths for `y == ONE/TWO/FOUR`) | YES — `PowVersion.V2` | shell over Rust core |
| `StablePool` (plain, 2021-06) | mainnet `20210624-stable-pool` | pre-`roundUp` invariant revision | NOT exercised by any parity matrix — see gaps | shell over Rust core (unexercised) |
| `MetaStablePool` | mainnet `20210727-meta-stable-pool` → `MetaStablePoolFactory` `0x67d27634E44793fE63c467035E31ea8635117cd4` (DEPRECATED) | `StableMath._calculateInvariant(amp, balances, roundUp=true)` (`P_D` round up) | YES — `InvariantVersion.V2` | shell over Rust core |
| `ComposableStablePool` (incl. MetaStable-era V2/V3 and V5/V6 factories) | mainnet `20220906`–`20240223`; Base `20230711-composable-stable-pool-v5` / `20240223-composable-stable-pool-v6` (Base factories listed DEPRECATED — pools live in the V2 vault registry regardless) | `INVARIANT_V1` (round-down `D_P`) + BPT-in-balances | YES — `InvariantVersion.V1` + `bpt_idx` skip | shell over Rust core |
| **Balancer V3** (the 2024-12 vault + 2026 factories) | Base/mainnet `20241204-v3-vault` + `v3-weighted-pool*` / `v3-stable-pool*` tasks | hook-based, new vault | NOT in scope (V2-vault pools only) | — |

Shared across chains: the V2 Vault itself (`0xBA12222222228d8Ba445958a75a0704d566BF2C8`).

## Version detection at construction

- **Weighted pow version** — `detect_pow_version()` (`balancer/pools.py`) scans
  the deployed bytecode for the `TWO` constant (`0x1bc16d674ec80000`) used by
  the `y == TWO` fast path, absent from 2021-era contracts → `PowVersion.V1`.
- **Stable invariant version** — `resolve_invariant_version()`
  (`builders/balancer_builder_base.py`) maps the Vault pool specialization:
  `General = 0` → `ComposableStablePool` → `INVARIANT_V1`;
  `MinimalSwapInfo = 1` → `MetaStablePool` → `INVARIANT_V2`.
- **BPT-in-balances** — `detect_bpt_index()` probes the token list; `None`
  marks MetaStable (no BPT token).

## Known deployed pools used as parity ground truth

Golden-oracle parity (mainnet, block 24,407,242 —
`tests/golden/data/tests/balancer/`):

| Pool | Notes |
|------|-------|
| `0x5c6Ee304…DB8F56` (80 BAL / 20 WETH) | 2021-era weighted (pow V1) |
| `0x3e5FA951…CAA32` (USDT / WETH) | weighted pair |
| `0xff083f57…1FBe88` (GTC / WETH) | weighted pair |
| `0x0b09deA16…2a35` | two-token weighted |
| `0x9D0D36cC…c30ed` | **multi-token** weighted (N > 2) |
| `0x32296969…130230` | stable (boosted/composable lineage) |

## Gaps / follow-ups

1. **Plain `StablePool` (2021) unexercised**, and `TwoTokenPool`-specialized
   stables have no inference branch (only specializations 0 and 1 map).
2. **BalancerQueries on Base — resolved.** The two addresses are per-chain
   deployments of the same `20220721-balancer-queries` artifact:
   `0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5` has code on Ethereum mainnet
   and none on Base, while `0x300Ab2038EAc391f26D9F895dc61F8F66a548833` has
   code on Base (its `vault()` is the canonical V2 Vault) and none on
   mainnet. The driver constant (`BALANCERQUERIES_CONTRACT_ADDRESS` in
   `balancer/deployments.py`) is intentionally mainnet-scoped for the
   mainnet parity tests; the deployments registry's `0x300Ab` is the Base
   counterpart. No Base-scoped constant is added because no consumer queries
   BalancerQueries on Base.
