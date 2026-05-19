# Plan 060: Unify Sync/Async Builder Orchestration via Shared Pure-Logic Helpers

## Overview

Extract shared decode, DB-extract, and snapshot-loading logic from V3 and V4 sync/async builder pairs into `V3BuilderBase` and `V4BuilderBase` classes containing only `@staticmethod` helpers, mirroring the existing `V2BuilderBase` pattern. The sync and async builders retain separate `build()` / `update()` methods (per the established decision in the builders' CONTEXT.md) but delegate pure-logic steps to the shared base, eliminating ~150 lines of duplicated decode/extract logic and fixing the missing async tick data fetcher.

## Problem

### Deletion test

If you deleted the duplicated decode/extract logic from each async builder and replaced it with calls to shared `@staticmethod` helpers on a base class, the same logic would exist once. The async builders become thin `async def` wrappers that `await` I/O calls and pass results to shared helpers.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| V3 builder pair duplicates decode logic | `v3_pool_builder.py` + `async_v3_pool_builder.py` | The decode steps (immutable data from RPC, slot0, DB row → kwargs) are copy-pasted. A new pool constructor parameter requires editing both files. Same decode logic also appears in `update()` — three places total. |
| V4 builder pair duplicates decode logic | `v4_pool_builder.py` + `async_v4_pool_builder.py` | Same problem as V3. `decode_v4_slot0()` and `extract_v4_db_values()` are copy-pasted. |
| DB snapshot-loading block is copy-pasted across all four builders | ~40 lines × 4 = ~160 lines total | The "load tick bitmap + tick data from DB snapshot" block is nearly identical across V3 sync, V3 async, V4 sync, V4 async — differing only in the `hasattr` key (`"pool_id"` for V3, `"managed_pool_id"` for V4) and the DB model class. This is the single worst duplication. |
| No async tick data fetcher | `async_v3_pool_builder.py`, `async_v4_pool_builder.py` | Both async builders pass `tick_data_fetcher=None` to pool constructors. Async-built pools cannot lazy-populate tick data. `tick_data_fetcher.py` only provides a sync fetcher (`make_tick_data_fetcher()`). |
| V2 builder already demonstrates the solution | `v2_builder_base.py` | `V2BuilderBase` owns `decode_immutable_data()`, `extract_db_values()`, `resolve_deployer_and_init_hash()` as `@staticmethod`s. `V2PoolBuilder` inherits and calls them. `AsyncV2PoolBuilder` calls them as `V2BuilderBase.some_method()` without inheriting. V3/V4 lack this shared base. |

### What's NOT duplicated (and why the line count is lower than it appears)

The I/O-heavy orchestration (~80% of each builder's `build()` method) cannot be shared between sync and async:

- `io.call(...)` vs `await io.call(...)` are different statements
- Token building: `erc20_builder.build(...)` vs `await erc20_builder.build(...)`
- Block number fetch: `io.get_block_number()` vs `await io.get_block_number()`
- DB queries interleave with sync setup code

The deduplicable code is the pure-logic kernel: ~40-50 lines per builder pair (decode helpers, DB extract, snapshot loading, slot0 decode). Total: ~150 lines across both pairs, plus the new async tick fetcher.

## Solution

### Step 1: Create `V3BuilderBase` with shared `@staticmethod` helpers

Extract from both `V3PoolBuilder` and `AsyncV3PoolBuilder`:

- **`decode_immutable_data(factory_result, token0_result, token1_result, fee_result, tick_spacing_result)`** — decode factory, token addresses, fee, tick spacing from raw call results. Returns frozen `V3ImmutableData` dataclass.
- **`decode_slot0(slot0_result)`** — decode sqrt_price_x96, tick from `slot0()` call result. Shared between `build()` and `update()`.
- **`extract_db_values(pool_from_db)`** — extract factory, token addresses, fee, tick spacing, deployer from a DB row. Returns frozen `V3DbValues` dataclass.
- **`load_tick_snapshot(pool_from_db, pool_table_class)`** — the ~40-line DB snapshot-loading block that loads `initialization_maps` + `liquidity_positions` into `working_tick_bitmap` / `working_tick_data`. Returns `tuple[dict[int, BitmapAtWord], dict[int, LiquidityAtTick], bool]`.
- **`resolve_deployer_and_init_hash(chain_id, factory, deployer_address, init_hash)`** — already exists on `V2BuilderBase`; V3 uses the same logic. Could be shared from `V2BuilderBase` or duplicated (6 lines, not worth abstracting further).

These are pure functions — no I/O, no `await`, no `self._connections`. They take pre-fetched data and return structured results.

```python
@dataclass(frozen=True)
class V3ImmutableData:
    """Decoded immutable V3 pool data from RPC calls."""
    factory: ChecksumAddress
    token0_address: ChecksumAddress
    token1_address: ChecksumAddress
    fee: int
    tick_spacing: int


@dataclass(frozen=True)
class V3Slot0Data:
    """Decoded V3 slot0 data, shared by build() and update()."""
    sqrt_price_x96: int
    tick: int


@dataclass(frozen=True)
class V3DbValues:
    """Immutable values extracted from a V3 DB row."""
    factory: ChecksumAddress
    token0_address: ChecksumAddress
    token1_address: ChecksumAddress
    fee: int
    tick_spacing: int
    deployer_address: str | None


class V3BuilderBase:
    """Shared pure-logic helpers for V3 pool builders.

    Both V3PoolBuilder (sync) and AsyncV3PoolBuilder (async) delegate
    decode/extract/snapshot steps to these helpers. Only the I/O
    steps differ between sync and async.
    """

    @staticmethod
    def decode_immutable_data(...) -> V3ImmutableData: ...

    @staticmethod
    def decode_slot0(slot0_result: HexBytes) -> V3Slot0Data: ...

    @staticmethod
    def extract_db_values(pool_from_db) -> V3DbValues: ...

    @staticmethod
    def load_tick_snapshot(
        pool_from_db, pool_table_class: type,
    ) -> tuple[dict[int, BitmapAtWord], dict[int, LiquidityAtTick], bool]: ...
```

### Step 2: Create `V4BuilderBase` with shared `@staticmethod` helpers

Same pattern as V3, extracting V4-specific decode logic:

- **`decode_slot0(slot0_result)`** — decode sqrt_price_x96, tick, protocol_fee, lp_fee from V4 `getSlot0(bytes32)` call result. Returns frozen `V4Slot0Data` dataclass. Shared between `build()` and `update()`.
- **`decode_protocol_fees(packed_uint24)`** — extract the two uint12 protocol fees from the packed uint24. Pure bit manipulation.
- **`extract_db_values(pool_from_db)`** — extract currency addresses, hook, tick spacing, fee, state_view from a V4 DB row (via `PoolManagerTable` join).
- **`load_tick_snapshot(pool_from_db, pool_table_class)`** — same pattern as V3, but uses `"managed_pool_id"` instead of `"pool_id"` for the `hasattr` check. Returns same `tuple[dict, dict, bool]`.

```python
@dataclass(frozen=True)
class V4Slot0Data:
    """Decoded V4 slot0 data, shared by build() and update()."""
    sqrt_price_x96: int
    tick: int
    protocol_fee_one_to_zero: int
    protocol_fee_zero_to_one: int
    lp_fee: int


class V4BuilderBase:
    """Shared pure-logic helpers for V4 pool builders."""

    @staticmethod
    def decode_slot0(slot0_result: HexBytes) -> V4Slot0Data: ...

    @staticmethod
    def extract_db_values(pool_from_db, pool_manager_in_db) -> V4DbValues: ...

    @staticmethod
    def load_tick_snapshot(
        pool_from_db, pool_table_class: type,
    ) -> tuple[dict[int, BitmapAtWord], dict[int, LiquidityAtTick], bool]: ...
```

### Step 3: Refactor sync builders to delegate to base classes

`V3PoolBuilder` inherits `V3BuilderBase`. Its `build()` and `update()` methods perform sync I/O (`io.call(...)`) and pass results to `self.decode_immutable_data(...)`, `self.decode_slot0(...)`, etc.

`V4PoolBuilder` inherits `V4BuilderBase`. Same pattern.

### Step 4: Refactor async builders to call base class helpers

`AsyncV3PoolBuilder` does **not** inherit `V3BuilderBase`. It calls the `@staticmethod` helpers as `V3BuilderBase.decode_slot0(...)`, mirroring how `AsyncV2PoolBuilder` calls `V2BuilderBase.decode_immutable_data()` without inheriting.

Rationale for no inheritance on async builders:
- `AsyncV2PoolBuilder` doesn't inherit `V2BuilderBase` — this plan follows the same pattern for consistency.
- The base classes provide only `@staticmethod`s. Inheritance adds no value beyond a shorter call syntax.
- Avoids MRO complications and coupling to the base class `__init__`.

### Step 5: Create `async_make_tick_data_fetcher()` for async pools

The existing `tick_data_fetcher.py` provides `make_tick_data_fetcher()` using sync `PoolIO`. Both async builders pass `tick_data_fetcher=None` to pool constructors — async-built pools cannot lazy-populate tick data.

Create `async_make_tick_data_fetcher()` alongside the existing sync function in `tick_data_fetcher.py`:

- Same algorithm, but accepts `AsyncPoolIO` and uses `await io.call()` internally
- Returns `Callable[[int, int], Coroutine[Any, Any, None]]` instead of `Callable[[int, int], None]`
- The pure-logic parts (bit extraction, active tick computation) are already factored into `_fetch_v3()` / `_fetch_v4()` — create async counterparts `_async_fetch_v3()` / `_async_fetch_v4()` that delegate bitmap/tick computation to the same pure logic

This makes Slice 5 about adding real functionality (async tick data fetcher) rather than splitting already-split logic.

### Design decisions

- **`@staticmethod` helpers, not a shared `build()` method**: The I/O orchestration (DB query → RPC fetch → token build → tick data fetch) must differ between sync and async because `await` vs non-`await` are different Python statements. Only the pure-logic steps can be shared. This is the same split the V2 family already uses.
- **No inheritance on async builders**: `AsyncV2PoolBuilder` doesn't inherit `V2BuilderBase` — it calls `V2BuilderBase.some_static_method()`. This plan follows the same pattern. Inheritance provides no value when the base has only `@staticmethod`s and no shared `__init__` state.
- **Frozen dataclasses for decoded results, not kwargs dicts**: `V3ImmutableData`, `V3Slot0Data`, `V3DbValues` etc. are frozen dataclasses carrying typed, named fields. This is more type-safe than `dict[str, Any]` and composes well with the registry-based subclass dispatch (V3 builders call `pool_type_registry.get_v3_class()` — different subclasses may have different constructor signatures, so a generic "construct kwargs" dict is fragile).
- **Pool construction stays in concrete builders**: `construct_v3_pool_kwargs()` returning `dict[str, Any]` was in the original plan. Removed because: (1) V3 dispatches to factory-registered subclasses with potentially different constructor signatures; (2) constructing a pool from a dict loses type safety; (3) the construction code is ~15 lines — not a big dedup win. The decode/extract helpers provide the real value.
- **`load_tick_snapshot()` is the highest-value extraction**: The ~40-line DB snapshot-loading block is the most painfully duplicated code across all four builders. A generic helper parameterized by the model class consolidates this from 4×40 = 160 lines to 1×40 + 4×3 = 52 lines.
- **`decode_slot0()` serves both `build()` and `update()`**: Currently V3 `slot0()` decode appears in both `build()` and `update()` of each builder — the same logic exists 4 times for V3 (sync build, sync update, async build, async update). A shared `@staticmethod` collapses this to 1 definition.
- **Tick data fetcher: add async, don't re-split pure logic**: `tick_data_fetcher.py` already splits pure logic from I/O — `_fetch_v3()` and `_fetch_v4()` handle the algorithm, `io.call()` handles I/O. The missing piece is async counterparts using `AsyncPoolIO`, not further splitting of logic that's already split.

## Files Involved

**Primary:**
- `src/degenbot/builders/v3_builder_base.py` — new file: shared V3 pure-logic helpers + frozen dataclasses
- `src/degenbot/builders/v4_builder_base.py` — new file: shared V4 pure-logic helpers + frozen dataclasses
- `src/degenbot/builders/v3_pool_builder.py` — inherit `V3BuilderBase`, replace inline decode/extract with calls to base class helpers
- `src/degenbot/builders/async_v3_pool_builder.py` — call `V3BuilderBase` static methods, replace inline decode/extract
- `src/degenbot/builders/v4_pool_builder.py` — inherit `V4BuilderBase`, replace inline decode/extract with calls to base class helpers
- `src/degenbot/builders/async_v4_pool_builder.py` — call `V4BuilderBase` static methods, replace inline decode/extract
- `src/degenbot/builders/tick_data_fetcher.py` — add `async_make_tick_data_fetcher()`, `_async_fetch_v3()`, `_async_fetch_v4()`

**Secondary:**
- `src/degenbot/builders/__init__.py` — export new base classes if needed
- `src/degenbot/builders/CONTEXT.md` — add `V3BuilderBase` / `V4BuilderBase` terminology

**No change needed:**
- `src/degenbot/builders/v2_builder_base.py` — already follows this pattern
- `src/degenbot/builders/v2_pool_builder.py` — already uses `V2BuilderBase`
- `src/degenbot/builders/async_v2_pool_builder.py` — already calls `V2BuilderBase` static methods
- `src/degenbot/builders/erc20_builder.py` — no sync/async pair (async has its own `AsyncErc20Builder`)

## Implementation Order

### Slice 1: Create `V3BuilderBase` with decode, extract, and snapshot helpers

1. Create `src/degenbot/builders/v3_builder_base.py`
2. Define frozen dataclasses: `V3ImmutableData`, `V3Slot0Data`, `V3DbValues`
3. Extract `decode_immutable_data()` as `@staticmethod` from the common decode logic in `V3PoolBuilder.build()` and `AsyncV3PoolBuilder.build()`
4. Extract `decode_slot0()` as `@staticmethod` (serves both `build()` and `update()`)
5. Extract `extract_db_values()` as `@staticmethod`
6. Extract `load_tick_snapshot()` as `@staticmethod` — the ~40-line DB snapshot block, parameterized by `pool_table_class`
7. Run: `just test-python` — expect no change yet (base class exists but isn't used)

### Slice 2: Refactor `V3PoolBuilder` to use `V3BuilderBase`

1. Make `V3PoolBuilder` inherit `V3BuilderBase`
2. Replace inline decode logic in `build()` with `self.decode_immutable_data(...)` / `self.decode_slot0(...)`
3. Replace inline DB extract with `self.extract_db_values(...)`
4. Replace inline snapshot-loading block with `self.load_tick_snapshot(...)`
5. Replace inline slot0 decode in `update()` with `self.decode_slot0(...)`
6. Run: `just test-python` — expect all tests green (behavior unchanged)

### Slice 3: Refactor `AsyncV3PoolBuilder` to call `V3BuilderBase` helpers

1. Do **not** make `AsyncV3PoolBuilder` inherit `V3BuilderBase` (follow `AsyncV2PoolBuilder` pattern)
2. Replace inline decode logic with `V3BuilderBase.decode_immutable_data(...)` / `V3BuilderBase.decode_slot0(...)`
3. Replace inline DB extract with `V3BuilderBase.extract_db_values(...)`
4. Replace inline snapshot-loading block with `V3BuilderBase.load_tick_snapshot(...)`
5. Replace inline slot0 decode in `update()` with `V3BuilderBase.decode_slot0(...)`
6. Run: `just test-python` — expect all tests green (behavior unchanged)

### Slice 4: Create `V4BuilderBase` and refactor both V4 builders

1. Create `src/degenbot/builders/v4_builder_base.py` with `V4Slot0Data`, `V4DbValues`, and `@staticmethod` helpers: `decode_slot0()`, `decode_protocol_fees()`, `extract_db_values()`, `load_tick_snapshot()`
2. Make `V4PoolBuilder` inherit `V4BuilderBase`, replace inline decode/extract/snapshot logic
3. Refactor `AsyncV4PoolBuilder` to call `V4BuilderBase` static methods (no inheritance)
4. Run: `just test-python` — expect all tests green

### Slice 5: Add async tick data fetcher — NOT VIABLE

**Not implemented.** The pool objects (`UniswapV3Pool`, `UniswapV4Pool`) store `_tick_data_fetcher` as `Callable[[int, int], None]` and call it synchronously during `external_update()`. An async fetcher returning `Coroutine` cannot be stored in this slot or called without `await`. Making this work requires redesigning the pool's tick-population mechanism (e.g., event-driven async tick loading, or a queue-based approach), which is out of scope for this plan. Async-built pools continue to pass `tick_data_fetcher=None`.

### Slice 6: Validate and clean up

1. Run `just lint` + `just test-all`
2. Update `src/degenbot/builders/CONTEXT.md` with `V3BuilderBase` / `V4BuilderBase` terminology
3. Verify no duplicated decode/extract logic remains by grepping for `eth_abi.abi.decode` in builder files — each decode type should appear once (in the base class)
4. Verify async tick data fetcher works by checking that async-built pools get a non-None `tick_data_fetcher`

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Slices 1–3 are the V3 refactoring; Slice 4 is V4; Slice 5 adds the async tick fetcher. No behavior change for Slices 1–4 — the same pools are constructed with the same parameters.

### New unit tests

```python
# tests/builders/test_v3_builder_base.py


def test_decode_immutable_data():
    """V3BuilderBase.decode_immutable_data decodes factory, tokens, fee, tick_spacing."""
    factory_result = eth_abi.abi.encode(["address"], [FACTORY])
    token0_result = eth_abi.abi.encode(["address"], [TOKEN0])
    token1_result = eth_abi.abi.encode(["address"], [TOKEN1])
    fee_result = eth_abi.abi.encode(["uint24"], [3000])
    tick_spacing_result = eth_abi.abi.encode(["int24"], [60])
    immutable = V3BuilderBase.decode_immutable_data(
        factory_result, token0_result, token1_result, fee_result, tick_spacing_result,
    )
    assert immutable.fee == 3000
    assert immutable.tick_spacing == 60


def test_decode_slot0():
    """V3BuilderBase.decode_slot0 decodes sqrt_price and tick."""
    slot0_result = eth_abi.abi.encode(
        ["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
        [SLOT0_VALUES],
    )
    data = V3BuilderBase.decode_slot0(slot0_result)
    assert data.sqrt_price_x96 == expected_sqrt_price
    assert data.tick == expected_tick


def test_load_tick_snapshot():
    """V3BuilderBase.load_tick_snapshot returns (bitmap, data, loaded=True) from a DB row with data."""
    # Create a fake DB row with initialization_maps + liquidity_positions
    bitmap, tick_data, loaded = V3BuilderBase.load_tick_snapshot(fake_pool_row, UniswapV3PoolTableBase)
    assert loaded is True
    assert len(bitmap) > 0
    assert len(tick_data) > 0


# tests/builders/test_v4_builder_base.py


def test_decode_slot0():
    """V4BuilderBase.decode_slot0 decodes sqrt_price, tick, protocol fees, lp fee."""
    slot0_result = eth_abi.abi.encode(
        ["uint160", "int24", "uint24", "uint24"],
        [PRICE, TICK, PROTOCOL_FEE, LP_FEE],
    )
    data = V4BuilderBase.decode_slot0(slot0_result)
    assert data.sqrt_price_x96 == PRICE
    assert data.protocol_fee_one_to_zero == PROTOCOL_FEE >> 12
    assert data.protocol_fee_zero_to_one == PROTOCOL_FEE & 0xFFF


def test_decode_protocol_fees():
    """V4BuilderBase.decode_protocol_fees extracts two uint12 fees from packed uint24."""
    one_to_zero, zero_to_one = V4BuilderBase.decode_protocol_fees(0xABC123)
    assert one_to_zero == 0xABC123 >> 12
    assert zero_to_one == 0xABC123 & 0xFFF
```

### Integration tests

Existing V3/V4 pool construction integration tests cover the `build()` path end-to-end. The base class extraction is behavior-preserving — these tests should pass unchanged.

## Benefits

- **Locality**: Bug fixes to decode logic apply once, not 2–4 times. Adding a new decoded field touches one file (the base class), not multiple builders.
- **Leverage**: ~150 lines of duplicated decode/extract logic collapses into shared helpers. The concrete sync/async builders lose their copy-pasted decode blocks.
- **Depth**: `load_tick_snapshot()` is a deep helper that eliminates the worst duplication — a 40-line block that was copy-pasted 4 times.
- **Consistency**: V3/V4 builders follow the same pattern already established by `V2BuilderBase` (`@staticmethod` helpers, async calls them without inheriting).
- **New functionality**: Async-built pools gain working tick data fetchers instead of `None`.

## Risks

- **Behavioral divergence during refactoring**: If the sync and async builders have subtly different decode logic (e.g., different error handling, different DB query logic), extracting shared helpers could accidentally merge divergent behavior. Mitigation: compare the two `build()` bodies line-by-line before extracting. Where they differ, parameterize the helper or keep the logic in the concrete builder.
- **`load_tick_snapshot()` parameterization complexity**: The snapshot block differs between V3 and V4 (`"pool_id"` vs `"managed_pool_id"`, different model classes). A single parameterized helper must handle both. Mitigation: the `pool_table_class` parameter and a `id_attr_name` parameter (defaulting to `"pool_id"`) capture the differences.
- **Async tick data fetcher signature**: The async fetcher returns a coroutine, changing the type of `tick_data_fetcher` on `UniswapV3Pool` / `UniswapV4Pool` from `Callable[[int, int], None]` to a union type. Mitigation: the pool's `tick_data_fetcher` attribute is already typed as `Callable | None` — it stores the callable, not the coroutine. The async fetcher will need to be `await`ed by its caller. Check that the pool's `update_tick_data()` call site inside the fetcher is compatible.
- **Constructor signature changes**: If the `UniswapV3Pool` or `UniswapV4Pool` constructor signature changes during this plan, both the base class and the concrete builders need updating. Mitigation: this plan doesn't change any pool constructor — it only reorganizes builder code.

## Relationship to Other Plans

- **Plan 043** (Extract V2 Variant Builders): Completed. Established `V2BuilderBase` pattern. This plan extends that pattern to V3/V4.
- **Plan 052** (Migrate V3/V4/Curve/ERC20 Builders to Full PoolIO): Completed. All builders use `PoolIO` for I/O. This plan is orthogonal — it reorganizes the internal structure of builders that already use PoolIO.
- **Plan 059** (Delete Deprecated `build_*` Pass-Throughs from Bot): Completed. A cleaner `Bot` surface makes the builder refactoring easier to reason about.
- **Plan 058** (Collapse Subscription Stubs): Completed. Orthogonal. Different module.

## Status

[x] Slice 1: Create `V3BuilderBase` with decode, extract, and snapshot helpers
[x] Slice 2: Refactor `V3PoolBuilder` to use `V3BuilderBase`
[x] Slice 3: Refactor `AsyncV3PoolBuilder` to call `V3BuilderBase` helpers
[x] Slice 4: Create `V4BuilderBase` and refactor both V4 builders
[ ] Slice 5: Add async tick data fetcher — NOT VIABLE (see note above)
[x] Slice 6: Validate and clean up
