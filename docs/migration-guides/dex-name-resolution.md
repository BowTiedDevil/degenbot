# DEX-name resolution — spike design note (WCSS3V)

**Status:** design spike for task WCSS3V (epic VQ4OHX). Implementation is the
dependent task QHGN2E. **Do not implement past this note without explicit
approval.**

## Problem

The structural [`Pool`] handle's [`Identity`] value object is deployment-agnostic —
it surfaces the protocol *family sub-variant* (`uniswap_v2`, `uniswap_v3`,
`uniswap_v4`) but not *which* deployment a V2/V3 pool belongs to (Uniswap vs.
SushiSwap vs. PancakeSwap, Camelot, Aerodrome, SwapBased, …). That resolution was
explicitly deferred in `pool.rs`'s module doc. The end goal (AGENTS.md) is a Rust
core that owns everything, so DEX-name resolution should be Rust-owned, not a
Python-side lookup.

## Current state (findings)

- **`Identity`** (`degenbot-pools/src/pool.rs`): three variants —
  `ReservePair { variant }`, `ConcentratedLiquidity { variant }`,
  `BalanceVector { variant }` — all `Copy` / `Eq`.
- **`degenbot-uniswap::deployments`** is keyed `(chain_id, factory)` and currently
  deserializes only the CREATE2-critical JSON fields (`deployer`, `init_hash`,
  `implementation_address`). The `RawRecord` **deliberately ignores** the
  `name` / `pool_type` / `variant` / `dex_variant` / `family` fields (serde skips
  unknown fields). It has `resolve_v2_init_hash` / `resolve_v3_init_hash` /
  `resolve_deployer` but **no DEX-name resolver**.
- **`deployments.json`** (32 rows) reliably carries a per-row `name` ("Uniswap V2",
  "SushiSwap V3", …) and a structured `pool_type` (`uniswap-v2`, `sushiswap-v3`,
  `pancakeswap-v2`, `aerodrome-v2`, `balancer-stable`, …). `dex_variant` is mostly
  `None` (17/32 rows) so `pool_type`/`name` is the dependable source.
- **Decimals/chain context:** the V2 identity already stores a deployment-specific
  `variant: DexVariant` (UniswapV2 vs SushiswapV2 vs PancakeswapV2 vs …) plus a
  chain-resolved `deployer`/`init_hash`. The V3/V4 CL identities carry `factory`
  but **no `variant` and no `chain_id`**. None of the identities store `chain_id`.
  A `(chain_id, factory)` deployment lookup therefore cannot be performed from a
  `PoolEntry` alone today — chain_id is not on the structural identity.
- `Pool::new(&PoolEntry)` is the shared structural constructor (used by all
  `pool_handle_*` tests + the PyO3 handle); it holds **only** the `&PoolEntry`.

## Data source at Rust-core scope

**Yes, the data exists** (the JSON's `name`/`pool_type`), but **not yet exposed**:
`deployments::RawRecord` strips it. A small port slice is required:
deserialize the `pool_type` field into a Rust [`DexName`] enum and add
`resolve_dex_name(chain_id, factory) -> Option<DexName>` in `deployments.rs`
(mapping `pool_type` prefix → enum, e.g. `sushiswap-v3` → `SushiSwap`,
`pancakeswap-v2` → `PancakeSwap`, `aerodrome-v2` → `Aerodrome`,
`balancer-stable` → `Balancer`).

## Candidate enum shapes

### (A) `dex: Option<DexName>` field on the Identity variants — **recommended**

```rust
pub enum Identity {
    ReservePair { variant: ReservePairVariant, dex: Option<DexName> },
    ConcentratedLiquidity { variant: ConcentratedLiquidityVariant, dex: Option<DexName> },
    BalanceVector { variant: BalanceVectorVariant, dex: Option<DexName> },
}
```

- **Pros:** directly satisfies QHGN2E's criterion *"`Pool::identity()` surfaces the
  DEX name"*; `Option<DexName>` is the natural degradation mechanism ("unknown
  deployment → generic variant") required by the acceptance criteria; one small
  `DexName` enum (no variant explosion); `Copy`/`Eq` preserved.
- **Cost:** `Pool::identity()` needs chain_id. Because `Pool` holds only
  `&PoolEntry`, chain_id must come from the identity. Recommendation: store
  `chain_id: u64` on the identities that participate in DEX-name resolution
  (V3 + V4; V2 already has a deployment-specific `DexVariant` it can map). This
  is consistent with the identities already storing chain-resolved `deployer` /
  `init_hash` — an identity *is* chain-scoped. `Pool::new(&entry)` then needs no
  signature change.

### (B) expand the sub-variant enums — rejected

`ReservePairVariant::UniswapV2` → `UniswapV2Uniswap` / `UniswapV2Sushi` / …;
`ConcentratedLiquidityVariant::UniswapV3` → `UniswapV3` / `SushiSwapV3` /
`PancakeSwapV3` / ….

- **Pros:** exhaustive, no `Option`.
- **Cons:** combinatorial variant explosion (families × DEXes), hard to extend
  when new deployments ship, and **duplicates** the deployment-specificity the V2
  identity already carries via `DexVariant`. It does not map cleanly onto the
  32-row JSON or the V4 singleton (no factory row). Rigid.

### (C) separate `resolve_dex(chain_id, factory) -> DexName` lookup; `Identity` stays deployment-agnostic — partial

Keep `Identity` structural; expose a pure `resolve_dex_name` free function.

- **Pros:** cleanest data model; trivial to test in isolation; identity stays
  lightweight.
- **Cons:** does **not** by itself make `Pool::identity()` *surface* the name —
  the caller must call the lookup separately. Fails the acceptance criterion as
  written. Could be combined with (A) as the underlying engine, but standalone (C)
  is insufficient.

## Recommendation

**Shape (A)**, backed by a port slice in `deployments.rs`:

1. Add `#[non_exhaustive] enum DexName { Uniswap, SushiSwap, PancakeSwap, Camelot,
   Aerodrome, SwapBased, Balancer }` in `degenbot-uniswap::dex_identity`
   (protocol-domain data, alongside `DexIdentity`).
2. Port the `pool_type` field into `deployments::RawRecord` and add
   `resolve_dex_name(chain_id, factory) -> Option<DexName>`.
3. Add `chain_id: u64` to the V3/V4 (and for uniformity V2/Aerodrome) pool
   identities; resolve `Pool::identity()` via the lookup. Unknown `(chain, factory)`
   → `dex: None` (generic variant, no error).
4. V2 can optionally map its existing `DexVariant` → `DexName` as a fallback so a
   non-JSON Sushi/Pancake V2 still names itself.

QHGN2E then adds the dual-driver test (a V2 and a V3 fixture with known
deployments → resolved name; one unknown deployment → `None`).

## Open question for approval

- Is Shape (A) approved?
- Approve storing `chain_id` on the V3/V4 identities (the mechanism to reach the
  `(chain, factory)` lookup from `Pool::identity()`), vs. threading chain_id
  through `Pool::new` (more churn on every call site)?

[`Pool`]: ../../rust/crates/degenbot-pools/src/pool.rs
[`Identity`]: ../../rust/crates/degenbot-pools/src/pool.rs
[`DexName`]: #candidate-enum-shapes
