# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Layout: Multi-context

This repo has a `CONTEXT-MAP.md` at the root pointing to per-module `CONTEXT.md` files under `src/degenbot/<module>/`. Each module context defines its own domain terms, relationships, and ambiguity rulings. The root map holds cross-module relationships and cross-module ambiguity rulings.

```
/
├── CONTEXT-MAP.md                     ← module index + cross-cutting content
├── docs/
│   ├── adr/                           ← system-wide decisions (e.g., ADR-001 I/O-free pools)
│   └── agents/
├── src/
│   └── degenbot/
│       ├── types/CONTEXT.md
│       ├── erc20/CONTEXT.md
│       ├── registry/CONTEXT.md
│       ├── arbitrage/CONTEXT.md
│       ├── aave/CONTEXT.md
│       ├── curve/CONTEXT.md           ← I/O-free pool architecture
│       ├── uniswap/CONTEXT.md         ← V2/V3/V4 pools
│       └── connection/CONTEXT.md
└── UBIQUITOUS_LANGUAGE.md             ← legacy (replaced by CONTEXT-MAP.md)
```

## Before exploring, read these

1. **`CONTEXT-MAP.md`** at the repo root — read it first for the module index and cross-cutting content.
2. **Per-module `CONTEXT.md`** — read each one relevant to the area you're about to work in. The map's bullet summaries help you identify which module(s) to read.
3. **`docs/adr/`** — read ADRs that touch the area you're about to work in. If per-context ADR directories appear under `src/<module>/docs/adr/`, check those too.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The producer skill (`/grill-with-docs`) creates them lazily when terms or decisions actually get resolved.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in the relevant `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/grill-with-docs`).

## I/O-Free Architecture Terms

When working with Curve pools or other I/O-free features, use these terms as defined in `src/degenbot/curve/CONTEXT.md`:

- **Data Provider** — a protocol (`CurveDataProvider`) injected at pool construction for on-demand data; replaces the former individual fetcher callbacks
- **Provider Method** — a single method on a DataProvider (e.g., `CurveDataProvider.D()`) called lazily when data is needed
- **I/O Decoupling** — the architectural separation of pool logic from on-chain I/O
- **CurveDataProviderImpl** — the production implementation of `CurveDataProvider`; a structured class with real methods and shared I/O helpers, created by the builder with a `ProviderAdapter`

**Incorrect**: "the pool fetches rates from the provider"
**Correct**: "the pool calls its injected CurveDataProvider methods"

## Enum Naming: PoolFamily vs PoolInvariant

Two enums cover related but distinct concepts:

- **`PoolFamily`** (in `types/pool_type.py`) — identifies a pool's mathematical invariant family for type resolution and DB kind derivation. Values: `CONSTANT_PRODUCT`, `CONCENTRATED_LIQUIDITY`, `STABLESWAP`, `WEIGHTED`.
- **`PoolInvariant`** (in `types/hop_types.py`) — identifies the solver dispatch path for arbitrage optimization. Values: `CONSTANT_PRODUCT`, `BOUNDED_PRODUCT`, `SOLIDLY_STABLE`, `CURVE_STABLESWAP`, `BALANCER_WEIGHTED`, `BALANCER_MULTI_TOKEN`.

A `PoolFamily` maps 1:1 to `PoolInvariant` for V2/V3, but N:1 for Curve/Stable and Balancer/Weighted.

**Incorrect**: "`PoolInvariant.CONCENTRATED_LIQUIDITY`" (in solver context — use `BOUNDED_PRODUCT`)
**Correct**: "`PoolFamily.CONCENTRATED_LIQUIDITY`" for pool identity, "`PoolInvariant.BOUNDED_PRODUCT`" for solver dispatch

## Pool Protocol Terms

- **`CacheablePool`** — a protocol for pools that register with the Rust solver cache, requiring `reserves_for_cache()` and `fee_for_cache()` methods
- **`SwapEncoder`** — a standalone module for encoding swap calldata from `SwapAmounts`

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0007 (event-sourced orders) — but worth reopening because…_
