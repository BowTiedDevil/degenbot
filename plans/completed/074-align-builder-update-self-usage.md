# Plan 074: Align Builder `update` I/O — Eliminate `self` Usage in `update()`

## Overview

Make every builder's `update()` method receive all I/O through its `io` parameter, eliminating all `self` access within `update()`. This removes the sync/async asymmetry where sync builders call `self._fetch_reserves()` / `self._fetch_vault_tokens()` while async builders inline the same I/O, and clears all remaining PLR6301 errors in the builders without `noqa` suppressions. The `PoolBuilder` and `AsyncPoolBuilder` protocols are changed to `@staticmethod` on `update`, creating a type-level backpressure against future `self` usage in `update()`.

## Problem

### Deletion test

If you deleted every `update()` method from every builder class, `Bot.update()` would break — it dispatches to the builder registry. But the protocol would still compile, and `build()` would be unaffected. The `update` methods exist only to re-fetch state from chain and push it to the pool via `external_update()`. They perform I/O, not computation — yet some of them access `self` to reach I/O helpers, while others perform identical I/O directly through `io`.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Sync V2/Camelot/Aerodrome `update` calls `self._fetch_reserves` | `v2_pool_builder.py`, `camelot_builder.py`, `aerodrome_v2_builder.py` | A `@staticmethod` called on `self` — ruff sees `self` as unused, but the call works. Inconsistent with async builders that inline the same I/O via `io.call()` |
| Sync Balancer `update` calls `self._fetch_vault_tokens` | `balancer_builder.py` | Same `@staticmethod`-on-`self` pattern. Update takes `io` as a parameter but reaches through `self` for the I/O helper anyway |
| Sync V3/V4/Curve `update` already I/O-only via `io` | `v3_pool_builder.py`, `v4_pool_builder.py`, `curve_pool_builder.py` | Already correct — they take `io` and call `io.call()` directly. Marked `# noqa: PLR6301` because `self` is unused |
| Async V2/V3/V4 `update` never uses `self` | `async_v2_pool_builder.py`, `async_v3_pool_builder.py`, `async_v4_pool_builder.py` | Already correct — inline I/O directly via `io`. Some have `# noqa: PLR6301`, some are unsuppressed |
| `noqa: PLR6301` suppressions across builders | 4 `update()`-related suppressions + 2 unsuppressed PLR6301 errors | Each suppression is a code smell that a reviewer must investigate and dismiss. The linter signal is real but the suggested fix (staticmethod/free function) currently breaks protocol conformance |
| Sync/async `update` are structural mirrors but I/O diverges | V2 sync: `self._fetch_reserves(address, io, block)` vs async: inline `io.call()` + `eth_abi.decode()` | Same logical operation, two implementation styles. A newcomer must read both to understand they do the same thing |

## Solution

### Step 1: Update V2-family `update()` to call `_fetch_reserves` via class, not `self`

`_fetch_reserves` is already a `@staticmethod` on `V2BuilderBase`. It takes `(pool_address, io, block_identifier)` — zero `self` state. The fix is a call-site change: replace `self._fetch_reserves(...)` with `V2BuilderBase._fetch_reserves(...)`.

```python
# Before (in V2PoolBuilder.update, CamelotBuilder.update, AerodromeV2Builder.update):
reserves0, reserves1 = self._fetch_reserves(pool.address, io, block_identifier=block_number_)

# After:
reserves0, reserves1 = V2BuilderBase._fetch_reserves(pool.address, io, block_identifier=block_number_)
```

This follows the existing base-class pattern: `_fetch_v2_common_data`, `_register_pool`, `_log_pool` are all called on `self` today, but `_fetch_reserves` is the only one used from `update()`. The others are only used from `build()`, which legitimately uses `self`. No rename — `_fetch_reserves` keeps its underscore prefix as an internal implementation detail.

### Step 2: Move all Balancer I/O static methods to `BalancerBuilderBase`

Move `_fetch_vault_tokens`, `_fetch_pool_id`, `_fetch_swap_fee`, `_fetch_weights`, `_fetch_amp`, `_fetch_rate_providers`, `_fetch_rates`, and `_detect_pool_type` from `BalancerBuilder` to `BalancerBuilderBase`. This mirrors `V2BuilderBase`, `V3BuilderBase`, and `V4BuilderBase` where all shared helpers live on the base class for sync/async reuse. No rename — all helpers keep their underscore prefixes as internal implementation details.

```python
# Before:
class BalancerBuilder(BalancerBuilderBase):
    @staticmethod
    def _fetch_vault_tokens(io, pool_id, block) -> tuple[list[str], list[int]]: ...
    @staticmethod
    def _fetch_pool_id(io, address, block) -> bytes: ...
    # ... etc

# After:
class BalancerBuilderBase:
    # ...existing decode_pool_id, decode_vault_tokens, etc...
    @staticmethod
    def _fetch_vault_tokens(io, pool_id, block) -> tuple[list[str], list[int]]: ...
    @staticmethod
    def _fetch_pool_id(io, address, block) -> bytes: ...
    # ... etc

class BalancerBuilder(BalancerBuilderBase):
    # inherits all helpers
```

Update `BalancerBuilder.build()` and `update()` to call `BalancerBuilderBase._fetch_vault_tokens(...)` etc. instead of `self._fetch_vault_tokens(...)`. The future `AsyncBalancerBuilder` will call the same base-class helpers.

### Step 3: Make all concrete `update()` methods `@staticmethod`

Now that no builder's `update()` uses `self` (all I/O flows through `io` and base-class helpers called via `Cls._fetch_reserves(...)`), add `@staticmethod` to every `update()` method and remove all `# noqa: PLR6301` suppressions.

```python
# Before:
def update(  # noqa: PLR6301
    self,
    pool: AbstractLiquidityPool,
    *,
    io: PoolIO | None = None,
    block_number: int | None = None,
) -> bool: ...

# After:
@staticmethod
def update(
    pool: AbstractLiquidityPool,
    *,
    io: PoolIO | None = None,
    block_number: int | None = None,
) -> bool: ...
```

This applies to all builders — sync and async (including `@staticmethod async def update(...)` on async builders).

### Step 4: Change both protocols to `@staticmethod` on `update`

Now that all concrete `update()` methods are `@staticmethod`, change the protocols to match:

```python
# Before:
class PoolBuilder(Protocol):
    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        io: PoolIO | None = None,
        block_number: int | None = None,
    ) -> bool: ...

class AsyncPoolBuilder(Protocol):
    async def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        io: AsyncPoolIO | None = None,
        block_number: int | None = None,
    ) -> bool: ...

# After:
class PoolBuilder(Protocol):
    @staticmethod
    def update(
        pool: AbstractLiquidityPool,
        *,
        io: PoolIO | None = None,
        block_number: int | None = None,
    ) -> bool: ...

class AsyncPoolBuilder(Protocol):
    @staticmethod
    async def update(
        pool: AbstractLiquidityPool,
        *,
        io: AsyncPoolIO | None = None,
        block_number: int | None = None,
    ) -> bool: ...
```

**This is the key design decision**: `@staticmethod` on the protocol is not just cosmetic — it creates **type-level backpressure** against `self` usage in `update()`. If a future developer writes `def update(self, pool, ...)` on a concrete builder, mypy `--strict` rejects it: *"Protocol member expected class or static method"*. The I/O-separation invariant this plan establishes is enforced by the type system, not just by convention.

**Impact on `Bot.update()`**: `Bot.update()` currently calls `builder.update(pool, ...)`. Python allows calling static methods on instances, so no change is needed at the call site.

### Design decisions

- **`@staticmethod` on protocol vs leaving protocol as instance method**: `@staticmethod` on the protocol creates a type-enforced invariant — mypy rejects any concrete `update()` that uses `self`. An instance-method protocol would allow a future developer to silently reintroduce `self._fetch_*` calls, defeating the plan's purpose.
- **Base-class `@staticmethod` for all helpers (not module-level functions)**: Keeps all shared helpers on the class hierarchy alongside existing decode helpers (`V2BuilderBase.decode_immutable_data`, `V3BuilderBase.decode_slot0`, etc.). A module-level function would create an inconsistency — callers would need to remember "base class for decode helpers, bare function for fetch helpers." One pattern, one mental model.
- **Keep underscore prefixes on moved helpers**: All builder-base helpers use underscore prefixes (`_fetch_v2_common_data`, `_register_pool`, `_log_pool`). The moved Balancer helpers follow the same convention. They're internal implementation details shared across the builder family, not public API.
- **`@staticmethod` not `@classmethod`**: A `@classmethod` receives the class as the first arg, useful when a single `update` implementation needs to dispatch differently per subclass. No builder needs that — each builder has its own `update` with fixed type-checking logic. `@staticmethod` is the minimal interface.
- **Sync and async protocols are structurally identical**: Both use `@staticmethod` on `update`. The async protocol uses `@staticmethod async def update(...)`. This was validated with mypy `--strict` and ruff — both accept the combination.

## Files Involved

**Primary:**
- `src/degenbot/builders/protocol.py` — change `update` to `@staticmethod` on both protocols
- `src/degenbot/builders/v2_pool_builder.py` — update `update()` to call `V2BuilderBase._fetch_reserves`, make `@staticmethod`
- `src/degenbot/builders/camelot_builder.py` — same as V2
- `src/degenbot/builders/aerodrome_v2_builder.py` — same as V2
- `src/degenbot/builders/balancer_builder.py` — remove moved I/O helpers, update `build()` and `update()` to call `BalancerBuilderBase._fetch_*`, make `update` `@staticmethod`
- `src/degenbot/builders/balancer_builder_base.py` — receive moved I/O helpers
- `src/degenbot/builders/v3_pool_builder.py` — make `update` `@staticmethod`, remove `noqa`
- `src/degenbot/builders/v4_pool_builder.py` — make `update` `@staticmethod`, remove `noqa`
- `src/degenbot/builders/curve_pool_builder.py` — make `update` `@staticmethod`, remove `noqa`
- `src/degenbot/builders/async_v2_pool_builder.py` — make `update` `@staticmethod`, remove `noqa`
- `src/degenbot/builders/async_v3_pool_builder.py` — make `update` `@staticmethod`
- `src/degenbot/builders/async_v4_pool_builder.py` — make `update` `@staticmethod`

**Secondary:**
- `src/degenbot/bot.py` — no change needed (calling `builder.update(pool, ...)` on an instance works for `@staticmethod`)
- `src/degenbot/builders/CONTEXT.md` — update protocol description to reflect `@staticmethod` `update`

**No change needed:**
- `src/degenbot/async_bot.py` — same call pattern as `Bot`

## Implementation Order

### Slice 1: Eliminate `self` usage in V2-family and Balancer `update()` methods

1. In `V2PoolBuilder.update`, `CamelotBuilder.update`, `AerodromeV2Builder.update`: replace `self._fetch_reserves(...)` with `V2BuilderBase._fetch_reserves(...)`
2. Move all I/O `@staticmethod` helpers from `BalancerBuilder` to `BalancerBuilderBase`: `_fetch_vault_tokens`, `_fetch_pool_id`, `_fetch_swap_fee`, `_fetch_weights`, `_fetch_amp`, `_fetch_rate_providers`, `_fetch_rates`, `_detect_pool_type`
3. In `BalancerBuilder.build()` and `update()`: replace `self._fetch_*(...)` with `BalancerBuilderBase._fetch_*(...)`
4. Run: `just test-python` — expect all green

### Slice 2: Make all `update` methods `@staticmethod`

1. Add `@staticmethod` to `update` on every builder class (sync and async)
2. Remove all `# noqa: PLR6301` from `update` methods
3. Run: `just test-python` — expect all green

### Slice 3: Change both protocols to `@staticmethod` on `update`

1. Add `@staticmethod` to `update` on `PoolBuilder` protocol
2. Add `@staticmethod` to `update` on `AsyncPoolBuilder` protocol (producing `@staticmethod async def update(...)`)
3. Run `just lint` + `just test-all` — expect all green, no PLR6301 errors

### Slice 4: Add tests and update documentation

1. Add structural conformance tests (see Testing section)
2. Add behavioral integration tests (see Testing section)
3. Update `src/degenbot/builders/CONTEXT.md` — note that `update` is `@staticmethod` on both protocols
4. Run `just lint` + `just test-all` — full validation

## Testing

### Per-slice test runs

Each slice runs `just test-python`. No compatibility period needed — the change is thin and mechanical.

### Structural conformance tests

Verify that each builder's `update` method is callable both as a class method and on an instance, matching the `Bot.update()` dispatch pattern:

```python
def test_v2_builder_update_is_staticmethod():
    """update() is a static method — no self injection."""
    assert isinstance(inspect.getattr_static(V2PoolBuilder, "update"), staticmethod)
    # Instance call works (matches Bot.update dispatch)
    builder = V2PoolBuilder(ctx)
    assert builder.update(pool, io=io) is True
    # Class call works
    assert V2PoolBuilder.update(pool, io=io) is True
```

One test per builder class (V2, Aerodrome, Camelot, V3, V4, Curve, Balancer, AsyncV2, AsyncV3, AsyncV4). Tests verify:
- `inspect.getattr_static(Cls, "update")` returns a `staticmethod`
- Calling on an instance works (matches `Bot.update()` dispatch)
- Calling on the class works

### Behavioral integration tests

Exercise the full `Bot.update()` → builder → `external_update()` path with fake pools and I/O:

```python
def test_v2_builder_update_dispatches_through_io():
    """update() fetches reserves via io.call and pushes to pool."""
    fake_io = FakePoolIO(...)
    builder = V2PoolBuilder(ctx)
    updated = builder.update(pool, io=fake_io, block_number=42)
    assert updated is True
    assert pool.reserves_token0 == expected_reserves0
    assert pool.reserves_token1 == expected_reserves1
```

One test per builder family (V2, Aerodrome, Camelot, V3, V4, Curve, Balancer). Tests verify:
- `update()` returns `True` when state changes
- `update()` returns `False` when state is unchanged
- Pool's `external_update()` was called with correct values
- All I/O flowed through `io`, not `self` (implicit — staticmethod enforces this)

### Existing test coverage

Existing tests continue to cover:
- Builder `update()` paths via `Bot.update()` integration tests
- Builder `build()` paths via `Bot.build_pool()` integration tests
- Balancer-specific builder tests

## Benefits

- **Locality**: I/O in `update()` flows through one seam (`io`) instead of two (`io` + `self._fetch_*`)
- **Leverage**: One protocol shape (`@staticmethod` `update`) works for all builders — sync and async, V2 and V3 and V4 and Curve and Balancer
- **Depth**: Removes the shallow-seam divergence where sync builders access `self` for I/O helpers while async builders use `io` directly — both now use `io` exclusively
- **Backpressure**: `@staticmethod` on the protocol makes mypy `--strict` reject any future `update()` that uses `self` — the I/O-separation invariant is enforced by the type system, not just convention
- **Noise reduction**: Eliminates all `# noqa: PLR6301` suppressions and unsuppressed PLR6301 errors in builders

## Risks

- **Protocol `@staticmethod` conformance check**: Python's `@runtime_checkable` protocols may not enforce `@staticmethod` correctly at runtime. Mitigation: our protocols are not passed to `isinstance()` checks for `update` — they're structural types for static checking only. The `Bot._builder_for_pool` registry lookup returns a `PoolBuilder`-typed value and calls `.update()` — this works for `@staticmethod` because Python allows calling static methods on instances. Verified with mypy `--strict` and ruff.
- **`@staticmethod async def` unfamiliarity**: The combination `@staticmethod async def update(...)` is unusual but valid Python. Verified with mypy `--strict` and ruff — both accept it. The async protocol mirrors the sync protocol's structure precisely.

## Relationship to Other Plans

- **Plan 070** (Balancer Builder): Completed. `BalancerBuilder` and `BalancerBuilderBase` were created by that plan. This plan moves I/O helpers between them and aligns `update` with the other builders.
- **Plan 069** (Remove Dy Calculation Closures): Completed. Removed closure-based fetchers from Curve pools. Orthogonal — this plan works on builder `update`, not pool calculation.
- Independent of all other active plans.

## Status

[x] Slice 1: Eliminate `self` usage in V2-family and Balancer `update()` methods
[x] Slice 2: Make all `update` methods `@staticmethod`
[x] Slice 3: Change both protocols to `@staticmethod` on `update`
[x] Slice 4: Add tests and update documentation
