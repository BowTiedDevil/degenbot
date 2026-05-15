# Plan 035: Builder Protocol — Replace Union Type with Shared Interface

## Overview

Replace the `V2PoolBuilder | V3PoolBuilder | V4PoolBuilder | CurvePoolBuilder` union type in Bot with a `PoolBuilder` protocol. Bot dispatches by dict lookup alone — no isinstance chains, no union type annotations. The typed `build_xxx_pool()` convenience methods are kept as narrowing delegates (not removed) due to heavy test usage.

## Files Involved

**Primary:**
- `src/degenbot/bot.py` — replace union types with `PoolBuilder`; eliminate `_dispatch_build()` isinstance chain; keep typed `build_xxx_pool()` methods as delegates
- `src/degenbot/builders/protocol.py` (new) — define `PoolBuilder` protocol

**No change needed:**
- `src/degenbot/builders/v2_pool_builder.py` — already satisfies protocol structurally
- `src/degenbot/builders/v3_pool_builder.py` — already satisfies protocol structurally
- `src/degenbot/builders/v4_pool_builder.py` — already satisfies protocol structurally
- `src/degenbot/builders/curve_pool_builder.py` — already satisfies protocol structurally

## Problem

Bot's builder registry uses a union type annotation: `dict[type, V2PoolBuilder | V3PoolBuilder | V4PoolBuilder | CurvePoolBuilder]`, repeated 4 times across `bot.py`. The seam is shallow: the interface is nearly as complex as the implementation because each builder variant leaks through.

The concrete symptom is `_dispatch_build()`:

```python
def _dispatch_build(self, *, builder: V2PoolBuilder | ... | CurvePoolBuilder, ...):
    if isinstance(builder, V3PoolBuilder):
        return builder.build(address, ..., tick_bitmap=..., tick_data=..., ...)
    if isinstance(builder, CurvePoolBuilder):
        return builder.build(address, ..., ...)  # no tick_kwargs
    # V2 builder default
    return builder.build(address, ..., deployer_address=..., init_hash=..., ...)
```

This isinstance chain exists because each builder's `build()` accepts different kwargs. But the chain is unnecessary — Python already handles unknown kwargs gracefully. If we just forward all kwargs via `**kwargs`, each builder will accept what it recognizes and raise `TypeError` for genuinely unknown kwargs, which is the correct behavior.

Adding a new pool family currently means: (a) new builder class, (b) extend the union type in 4 places, (c) add an isinstance branch, (d) add a pass-through method. This plan reduces it to: (a) new builder class, (b) register it.

### Why keep `build_xxx_pool()` methods?

The original plan proposed removing them. The audit found ~60+ test call sites. Migrating all of them is a large, mechanical, error-prone change that doesn't improve the architecture — it just changes the spelling. Instead, the typed methods stay as thin delegates that call `build_pool()` internally and narrow the return type. They're convenience methods with zero logic:

```python
def build_v2_pool(self, pool_address: str, **kwargs) -> UniswapV2Pool:
    pool = self.build_pool(pool_address, **kwargs)
    assert isinstance(pool, UniswapV2Pool)
    return pool
```

### V4's different calling convention

V4 `build()` takes `pool_id` + `pool_manager_address` as required kwargs instead of a positional `address`. `build_pool()` already handles this with a `pool_id` fast path that short-circuits directly to `build_v4_pool()`. The `_dispatch_build` isinstance chain only fires for V2/V3/Curve. The protocol change doesn't affect the V4 fast path.

## Solution

### Step 1: Define the `PoolBuilder` protocol

```python
# src/degenbot/builders/protocol.py

from typing import Any, Protocol

from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool


class PoolBuilder(Protocol):
    """Protocol for pool construction and state updates.

    Each builder owns:
    - The I/O choreography (DB lookup → RPC fetch → decode → construct)
    - Pool registration in the Pool Registry
    - State updates via pool.external_update()

    Builders do NOT own:
    - Pool type resolution (Bot's job)
    - Connection management (received via ConnectionManager)
    - Database lifecycle (received via DatabaseSessionManager)
    """

    def build(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        state_block: int | None = None,
        silent: bool = False,
        **kwargs: Any,
    ) -> AbstractLiquidityPool: ...

    def update(
        self,
        pool: Any,
        *,
        block_number: int | None = None,
    ) -> bool: ...
```

Note: V4 builder's `build()` signature doesn't match this protocol (it takes `pool_id` + `pool_manager_address` instead of `address`). This is acceptable because V4 goes through `build_pool()`'s `pool_id` fast path, not through `_dispatch_build()`. The V4 builder is registered for `update()` dispatch only. If we later want V4 to go through `_dispatch_build()`, we'd give V4PoolBuilder a separate `build(address, ...)` that delegates to its existing `build(pool_id=..., pool_manager_address=address, ...)`.

### Step 2: Replace union types and `_dispatch_build()` in Bot

Replace all 4 union type annotations with `PoolBuilder`:

```python
# Before:
self._builders: dict[type, V2PoolBuilder | V3PoolBuilder | V4PoolBuilder | CurvePoolBuilder] = {}
def register_builder(self, pool_class, builder: V2PoolBuilder | ... | CurvePoolBuilder): ...
def _dispatch_build(self, *, builder: V2PoolBuilder | ... | CurvePoolBuilder, ...): ...
def _builder_for_pool(self, pool) -> V2PoolBuilder | ... | CurvePoolBuilder: ...

# After:
self._builders: dict[type, PoolBuilder] = {}
def register_builder(self, pool_class: type[AbstractLiquidityPool], builder: PoolBuilder): ...
def _dispatch_build(self, *, builder: PoolBuilder, address, chain_id, **kwargs): ...
def _builder_for_pool(self, pool) -> PoolBuilder: ...
```

Replace `_dispatch_build()` isinstance chain with `**kwargs` forwarding:

```python
# Before: isinstance chain with 3 branches
def _dispatch_build(self, *, builder, address, chain_id, deployer_address, init_hash,
                    state_block, tick_bitmap, tick_data, silent, state_cache_depth):
    if isinstance(builder, V3PoolBuilder):
        return builder.build(address, chain_id=chain_id, deployer_address=deployer_address,
                             init_hash=init_hash, state_block=state_block,
                             tick_bitmap=tick_bitmap, tick_data=tick_data,
                             silent=silent, state_cache_depth=state_cache_depth)
    if isinstance(builder, CurvePoolBuilder):
        return builder.build(address, chain_id=chain_id, state_block=state_block,
                             silent=silent, state_cache_depth=state_cache_depth)
    return builder.build(address, chain_id=chain_id, deployer_address=deployer_address,
                         init_hash=init_hash, state_block=state_block,
                         silent=silent, state_cache_depth=state_cache_depth)

# After: one-liner
def _dispatch_build(
    self,
    *,
    builder: PoolBuilder,
    address: ChecksumAddress,
    chain_id: ChainId,
    **kwargs: Any,
) -> AbstractLiquidityPool:
    return builder.build(address, chain_id=chain_id, **kwargs)
```

The `build_pool()` method passes kwargs through:

```python
return self._dispatch_build(
    builder=builder,
    address=address,
    chain_id=chain_id,
    # Variant-specific kwargs forwarded as-is
    deployer_address=deployer_address,
    init_hash=init_hash,
    state_block=state_block,
    tick_bitmap=tick_bitmap,
    tick_data=tick_data,
    silent=silent,
    state_cache_depth=state_cache_depth,
)
```

Each builder's `build()` already declares which kwargs it accepts. If a builder receives a kwarg it doesn't recognize (e.g., `tick_bitmap` passed to `CurvePoolBuilder.build()`), Python raises `TypeError` — which is the correct failure mode, since it means `build_pool()` routed to the wrong builder.

### Step 3: Simplify typed `build_xxx_pool()` methods as delegates

Keep the typed methods but reduce them to delegates:

```python
def build_v2_pool(self, pool_address: str, *, chain_id=None, deployer_address=None,
                  init_hash=None, state_block=None, silent=False) -> UniswapV2Pool:
    pool = self.build_pool(
        pool_address, chain_id=chain_id, deployer_address=deployer_address,
        init_hash=init_hash, state_block=state_block, silent=silent,
    )
    assert isinstance(pool, UniswapV2Pool)
    return pool
```

Same pattern for `build_v3_pool`, `build_v4_pool`, `build_curve_pool`. The `assert` is both a runtime safety check and a type narrow for mypy — no `cast` needed.

No existing test call sites need to change.

## Implementation Order

1. **Step 1**: Create `builders/protocol.py` with `PoolBuilder` protocol
2. **Step 2**: Replace union types and `_dispatch_build()` in `bot.py`
3. **Step 3**: Simplify typed methods as delegates
4. Run `just test-python` and `just lint` after each step

Steps 2 and 3 can be one commit since they're tightly coupled.

## Testing

### Protocol conformance

Add a test that each builder satisfies `PoolBuilder`:

```python
def test_v2_builder_satisfies_protocol():
    # V2PoolBuilder satisfies PoolBuilder structurally
    assert isinstance(V2PoolBuilder, type)  # just to verify import
```

Since `PoolBuilder` is a `Protocol` without `@runtime_checkable`, `isinstance` checks on instances work for structural subtyping. But the real test is that `mypy` / `pyright` confirms conformance — run `just lint` after Step 1.

### Bot dispatch

All existing builder and Bot tests should pass unchanged. The protocol change is structural — no behavior change.

### TypeError safety

Verify that forwarding irrelevant kwargs to a builder raises `TypeError`:

```python
# This should raise TypeError — CurvePoolBuilder.build() doesn't accept tick_bitmap
with pytest.raises(TypeError):
    CurvePoolBuilder(...).build("0x...", tick_bitmap={...})
```

This is the existing Python behavior — the `**kwargs` forwarding doesn't subvert it.

## Benefits

- **Leverage**: Bot only needs to know the `PoolBuilder` protocol, not four distinct constructor signatures. Adding a new pool family is 2 touch points (create builder, register it) instead of 4+.
- **Locality**: `_dispatch_build()` collapses from a 20-line isinstance chain to a one-liner.
- **No isinstance in Bot dispatch**: The builder union type (4 occurrences) and the isinstance chain are both eliminated. Type annotations simplify to `PoolBuilder`.
- **Zero test migration**: Typed convenience methods stay. ~60+ test call sites unchanged.

## Risks

- **TypeError on wrong-kwargs forwarding**: If `build_pool()` somehow routes to the wrong builder, kwargs like `tick_bitmap` would hit `CurvePoolBuilder.build()` and raise `TypeError`. This is actually *better* than the current behavior — the isinstance chain silently drops kwargs that might matter. A `TypeError` fails fast and visibly.
- **V4 builder doesn't match the `build(address, ...)` protocol exactly**: V4's `build()` takes `pool_id` + `pool_manager_address` as required kwargs instead of a positional `address`. This doesn't cause a problem because V4 goes through `build_pool()`'s `pool_id` fast path. But it means V4PoolBuilder isn't a structural match for the `PoolBuilder` protocol's `build(address, ...)`. Two options: (a) accept the mismatch since V4 never goes through `_dispatch_build`, or (b) give V4PoolBuilder a wrapper `build(address, **kwargs)` that delegates. Option (a) is simpler and honest about the seam.
- **No behavior change**: Purely structural — no pool construction logic changes.

## Relationship to Other Plans

- **Plan 028** (Builder Registry & Pool Class Restructuring): Complete. Created the builder registry as `dict[type, V2PoolBuilder | ...]`. This plan replaces the union type with a protocol, completing the "no isinstance in Bot" goal.
- **Plan 001** (Extract Pool Builders from Bot): Complete. Created the builder classes. This plan gives them a shared interface.
- **ADR-001** (I/O-Free Pools): Consistent. The `PoolBuilder` protocol describes the I/O boundary — builders do I/O, pools don't.
