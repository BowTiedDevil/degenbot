# Plan 060: Unify Sync/Async Builder Orchestration via Shared Pure-Logic Base Classes

## Overview

Extract shared decode, construct, and register logic from V3 and V4 sync/async builder pairs into base classes (`V3BuilderBase`, `V4BuilderBase`), mirroring the existing `V2BuilderBase` pattern. The sync and async builders retain separate `build()` / `update()` methods (per the established decision in the builders' CONTEXT.md) but delegate all pure-logic steps to the shared base, eliminating ~800 lines of duplicated orchestration code.

## Problem

### Deletion test

If you deleted `AsyncV3PoolBuilder.build()` and `AsyncV4PoolBuilder.build()` and replaced them with calls to shared pure helpers on a base class, the same logic would exist once. Complexity concentrates rather than duplicating. The async builders become thin `async def` wrappers that `await` I/O calls and pass results to shared decode/construct functions.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| V3 builder pair duplicates ~350 lines of decode/construct logic | `v3_pool_builder.py` (404 lines) + `async_v3_pool_builder.py` (387 lines) | The `build()` method follows the same sequence (DB lookup → RPC fetch → decode → construct pool → register) but the decode and construct steps are copy-pasted. A new pool constructor parameter requires editing both files. |
| V4 builder pair duplicates ~350 lines of decode/construct logic | `v4_pool_builder.py` (401 lines) + `async_v4_pool_builder.py` (371 lines) | Same problem as V3. The V4 builders additionally share tick-data fetching logic that differs only in `io.call()` vs `await io.call()`. |
| V2 builder already demonstrates the solution | `v2_builder_base.py` (266 lines) | `V2BuilderBase` owns `_fetch_v2_common_data()`, `decode_immutable_data()`, `extract_db_values()`, `resolve_deployer_and_init_hash()`. Both `V2PoolBuilder` and `AsyncV2PoolBuilder` call these helpers. V3/V4 lack this shared base, creating an inconsistency in the builder architecture. |
| Tick data fetcher is defined in sync builder, not shared | `v3_pool_builder.py` lines 55–73 | `_make_tick_data_fetcher()` is a static method on the sync builder. The async builder can't call it because the returned fetcher uses sync I/O. The pure-logic parts (tick word/bit position calculation, bitmap assembly) should be on the base class. |

## Solution

### Step 1: Create `V3BuilderBase` with shared pure-logic helpers

Extract from both `V3PoolBuilder` and `AsyncV3PoolBuilder`:

- **`_decode_immutable_v3_data(common_bytes, fee_bytes)`** — decode factory, fee, tick spacing from contract call results
- **`_construct_v3_pool(decoded_data, tokens, tick_data, state_block, ...)`** — build the `UniswapV3Pool` constructor kwargs dict from decoded data
- **`_extract_v3_db_values(db_row)`** — map a database row to constructor kwargs
- **`_assemble_tick_data(tick_bitmap_words, tick_data_entries)`** — assemble raw RPC results into the `dict[int, BitmapAtWord]` and `dict[int, LiquidityAtTick]` structures the pool constructor expects
- **`_register_v3_pool(pool, pools_registry, db, chain_id)`** — register in pool registry and persist to DB

These are pure functions — no I/O, no `await`, no `self._connections`. They take pre-fetched data and return structured results.

```python
class V3BuilderBase:
    """Shared pure-logic helpers for V3 pool builders.

    Both V3PoolBuilder (sync) and AsyncV3PoolBuilder (async) delegate
    decode/construct/register steps to these helpers. Only the I/O
    steps differ between sync and async.
    """

    @staticmethod
    def decode_immutable_v3_data(raw_call_result: HexBytes) -> V3ImmutableData:
        ...

    @staticmethod
    def construct_v3_pool_kwargs(
        *,
        address: ChecksumAddress,
        chain_id: ChainId,
        immutable: V3ImmutableData,
        tokens: tuple[Erc20Token, ...],
        tick_data: dict[int, LiquidityAtTick],
        tick_bitmap: dict[int, BitmapAtWord],
        state_block: int,
        state_cache_depth: int,
        silent: bool,
    ) -> dict[str, Any]:
        ...

    @staticmethod
    def register_v3_pool(pool: UniswapV3Pool, *, pools, db, chain_id) -> None:
        ...
```

### Step 2: Create `V4BuilderBase` with shared pure-logic helpers

Same pattern as V3, extracting:

- **`_decode_v4_slot0(slot0_result)`** — decode sqrt price, tick, protocol fee from `slot0` call result
- **`_construct_v4_pool_kwargs(...)`** — build `UniswapV4Pool` constructor kwargs dict
- **`_register_v4_pool(pool, pools, managed_pools, db, chain_id)`** — register in pool registry and managed pool registry, persist to DB
- **Tick position calculation** (shared between sync/async tick data fetchers)

### Step 3: Refactor sync builders to delegate to base class

`V3PoolBuilder` inherits `V3BuilderBase`. Its `build()` method performs sync I/O (`io.call(...)`) and passes results to `self.decode_immutable_v3_data(...)`, `self.construct_v3_pool_kwargs(...)`, etc.

`V4PoolBuilder` inherits `V4BuilderBase`. Same pattern.

### Step 4: Refactor async builders to delegate to base class

`AsyncV3PoolBuilder` inherits `V3BuilderBase`. Its `build()` method performs async I/O (`await io.call(...)`) and passes results to the same `self.decode_immutable_v3_data(...)`, `self.construct_v3_pool_kwargs(...)`, etc.

`AsyncV4PoolBuilder` inherits `V4BuilderBase`. Same pattern.

### Step 5: Refactor tick data fetcher to separate pure logic from I/O

`_make_tick_data_fetcher()` currently mixes pure-logic (tick position calculation, bitmap assembly) with I/O (fetching tick/word data). Split it:

- Pure logic: `compute_tick_bitmap_positions(tick_lower, tick_upper, tick_spacing)` → returns the set of word positions and tick indices needed
- I/O wrapper: sync and async fetcher closures that call `io.call()` / `await io.call()` for the computed positions, then pass results to the pure assembly function

### Design decisions

- **Separate classes, shared helpers (not a single class with sync+async)**: Consistent with the builders' CONTEXT.md decision: "Making `build()` async on all builders would force async on sync users." The base class provides only pure helpers; `build()` / `update()` remain on the concrete sync/async classes.
- **Pure functions as `@staticmethod`**: The helpers don't need `self`. Making them `@staticmethod` signals that they're pure and testable in isolation.
- **Frozen dataclasses for intermediate results**: `V3ImmutableData`, `V3MutableData`, etc. as frozen dataclasses carrying decoded values between I/O and construction. This makes the data flow explicit and testable.
- **Tick data fetcher split**: The pure-logic part of tick data fetching (which words/positions to query) is shared. The I/O part (actually making the calls) remains sync/async-specific. This is the same split the Curve module uses: `_resolve_calculation_inputs_via_io()` does I/O, `DyCalculator.calculate()` does pure math.
- **No changes to V2 builder family**: V2 already uses `V2BuilderBase`. This plan brings V3/V4 to the same pattern.

## Files Involved

**Primary:**
- `src/degenbot/builders/v3_builder_base.py` — new file: shared V3 pure-logic helpers
- `src/degenbot/builders/v4_builder_base.py` — new file: shared V4 pure-logic helpers
- `src/degenbot/builders/v3_pool_builder.py` — inherit `V3BuilderBase`, replace inline decode/construct with calls to base class helpers
- `src/degenbot/builders/async_v3_pool_builder.py` — inherit `V3BuilderBase`, replace inline decode/construct with calls to base class helpers
- `src/degenbot/builders/v4_pool_builder.py` — inherit `V4BuilderBase`, replace inline decode/construct with calls to base class helpers
- `src/degenbot/builders/async_v4_pool_builder.py` — inherit `V4BuilderBase`, replace inline decode/construct with calls to base class helpers

**Secondary:**
- `src/degenbot/builders/__init__.py` — export new base classes if needed
- `src/degenbot/builders/CONTEXT.md` — add `V3BuilderBase` / `V4BuilderBase` terminology
- `src/degenbot/uniswap/v3_functions.py` — pure-logic tick calculations may be moved or referenced from base class

**No change needed:**
- `src/degenbot/builders/v2_builder_base.py` — already follows this pattern
- `src/degenbot/builders/v2_pool_builder.py` — already uses `V2BuilderBase`
- `src/degenbot/builders/async_v2_pool_builder.py` — already uses `V2BuilderBase`
- `src/degenbot/builders/erc20_builder.py` — no sync/async pair (async has its own `AsyncErc20Builder`)

## Implementation Order

### Slice 1: Create `V3BuilderBase` with `decode_immutable_v3_data` and `construct_v3_pool_kwargs`

1. Create `src/degenbot/builders/v3_builder_base.py`
2. Extract `_decode_immutable_v3_data` as a `@staticmethod` (identify the decode logic in both `V3PoolBuilder.build()` and `AsyncV3PoolBuilder.build()`)
3. Extract `construct_v3_pool_kwargs` as a `@staticmethod`
4. Define `V3ImmutableData` frozen dataclass if intermediate structure is warranted
5. Run: `just test-python` — expect no change yet (base class exists but isn't used)

### Slice 2: Refactor `V3PoolBuilder` to use `V3BuilderBase`

1. Make `V3PoolBuilder` inherit `V3BuilderBase`
2. Replace inline decode logic in `build()` with `self.decode_immutable_v3_data(...)`
3. Replace inline pool construction with `self.construct_v3_pool_kwargs(...)`
4. Run: `just test-python` — expect all tests green (behavior unchanged)

### Slice 3: Refactor `AsyncV3PoolBuilder` to use `V3BuilderBase`

1. Make `AsyncV3PoolBuilder` inherit `V3BuilderBase`
2. Replace inline decode logic in `build()` with `self.decode_immutable_v3_data(...)`
3. Replace inline pool construction with `self.construct_v3_pool_kwargs(...)`
4. Run: `just test-python` — expect all tests green (behavior unchanged)

### Slice 4: Create `V4BuilderBase` and refactor both V4 builders

1. Create `src/degenbot/builders/v4_builder_base.py` with shared V4 helpers
2. Refactor `V4PoolBuilder` and `AsyncV4PoolBuilder` to inherit `V4BuilderBase`
3. Run: `just test-python` — expect all tests green

### Slice 5: Refactor tick data fetcher

1. Extract pure-logic tick position calculations from `_make_tick_data_fetcher()` into `V3BuilderBase` / `V4BuilderBase` as static methods
2. Keep I/O wrappers in the concrete sync/async builders
3. Run: `just test-python` — expect all tests green

### Slice 6: Validate and clean up

1. Run `just lint` + `just test-all`
2. Verify line counts: `V3PoolBuilder` + `AsyncV3PoolBuilder` should be significantly smaller (target: ~200 lines each, down from 400)
3. Update `src/degenbot/builders/CONTEXT.md` with `V3BuilderBase` / `V4BuilderBase` terminology
4. Verify no duplicated decode/construct logic remains by grepping for constructor-kwarg-building patterns

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Slices 1–3 are the V3 refactoring; Slices 4–5 are V4 and tick data. No behavior change — the same pools are constructed with the same parameters.

### New unit tests

```python
# tests/builders/test_v3_builder_base.py


def test_decode_immutable_v3_data():
    """V3BuilderBase.decode_immutable_v3_data decodes factory, fee, tick spacing from raw call result."""
    raw = ...  # known ABI-encoded result
    immutable = V3BuilderBase.decode_immutable_v3_data(raw)
    assert immutable.factory == expected_factory
    assert immutable.fee == expected_fee
    assert immutable.tick_spacing == expected_tick_spacing


def test_construct_v3_pool_kwargs():
    """V3BuilderBase.construct_v3_pool_kwargs produces valid constructor kwargs."""
    kwargs = V3BuilderBase.construct_v3_pool_kwargs(
        address=...,
        chain_id=...,
        immutable=...,
        tokens=...,
        tick_data=...,
        tick_bitmap=...,
        state_block=...,
    )
    pool = UniswapV3Pool(**kwargs)
    assert pool.address == expected_address
```

### Integration tests

Existing V3/V4 pool construction integration tests cover the `build()` path end-to-end. The base class extraction is behavior-preserving — these tests should pass unchanged.

## Benefits

- **Locality**: Bug fixes to decode logic apply once, not twice. Adding a new field to the pool constructor signature touches one file (the base class), not two (sync + async).
- **Leverage**: ~800 lines of duplicated orchestration code collapses into shared helpers. The concrete sync/async builders become thin I/O wrappers.
- **Depth**: The builder base class is a deep module — callers get full decode/construct/register behavior from a few static method calls. The shallow `build()` methods become even shallower, but that's correct for I/O orchestration (the I/O steps are necessarily exposed at the seam).
- **Consistency**: V3/V4 builders follow the same pattern already established by `V2BuilderBase`. A contributor who has worked on V2 builders can apply the same mental model to V3/V4.

## Risks

- **Behavioral divergence during refactoring**: If the sync and async builders have subtly different decode logic (e.g., different error handling, different DB query logic), extracting shared helpers could accidentally merge divergent behavior. Mitigation: compare the two `build()` bodies line-by-line before extracting. Where they differ, parameterize the helper or keep the logic in the concrete builder.
- **Constructor signature changes**: If the `UniswapV3Pool` or `UniswapV4Pool` constructor signature changes during this plan, both the base class and the concrete builders need updating. Mitigation: this plan doesn't change any pool constructor — it only reorganizes builder code.
- **Tick data fetcher complexity**: The `_make_tick_data_fetcher()` function mixes closure capture with I/O. Splitting pure logic from I/O requires careful boundary analysis. Mitigation: extract the pure calculations first, test them in isolation, then wire the I/O wrapper.

## Relationship to Other Plans

- **Plan 043** (Extract V2 Variant Builders): Completed. Established `V2BuilderBase` pattern. This plan extends that pattern to V3/V4.
- **Plan 052** (Migrate V3/V4/Curve/ERC20 Builders to Full PoolIO): Completed. All builders use `PoolIO` for I/O. This plan is orthogonal — it reorganizes the internal structure of builders that already use PoolIO.
- **Plan 059** (Delete Deprecated `build_*` Pass-Throughs from Bot): Complementary. A cleaner `Bot` surface makes the builder refactoring easier to reason about. Execute 059 first to avoid touching deprecated methods that are about to be deleted.
- **Plan 058** (Collapse Subscription Stubs): Orthogonal. Different module.

## Status

[ ] Slice 1: Create `V3BuilderBase` with decode and construct helpers
[ ] Slice 2: Refactor `V3PoolBuilder` to use `V3BuilderBase`
[ ] Slice 3: Refactor `AsyncV3PoolBuilder` to use `V3BuilderBase`
[ ] Slice 4: Create `V4BuilderBase` and refactor both V4 builders
[ ] Slice 5: Refactor tick data fetcher (split pure logic from I/O)
[ ] Slice 6: Validate and clean up
