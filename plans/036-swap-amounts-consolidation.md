# Plan 036: Consolidate SwapAmounts Dispatch into Self-Contained Subclasses

## Status: PROPOSED

## Overview

Give `AbstractSwapAmounts` virtual methods (`input_amount()`, `output_amount()`) so callers can extract amounts generically, and refactor `build_swap_amount()` from an isinstance chain to a protocol dispatch on pools. This eliminates the scattered match/case dispatch in `arbitrage_path.py` and the isinstance chain in `swap_amount_builder.py`, concentrating "how to build and destructure swap amounts for pool X" in the pool type itself.

**Dependency**: Plan 034 (delete legacy arbitrage cycles) should be implemented first. After 034, all `.amounts_in` / `.amounts_out` direct accesses outside `types.py` and `arbitrage_path.py` vanish, making the refactoring scope much smaller.

## Files Involved

**Primary:**
- `src/degenbot/arbitrage/types.py` — add `input_amount()` / `output_amount()` methods to `AbstractSwapAmounts`; implement on each subclass
- `src/degenbot/arbitrage/path/arbitrage_path.py` — replace `_extract_amount_in` / `_extract_amount_out` with `swap.input_amount()` / `swap.output_amount()`
- `src/degenbot/arbitrage/path/swap_amount_builder.py` — refactor isinstance chain into protocol dispatch

**Secondary (pool classes — add `build_swap_amount()` method):**
- `src/degenbot/uniswap/v2_liquidity_pool.py`
- `src/degenbot/uniswap/v3_liquidity_pool.py`
- `src/degenbot/uniswap/v4_liquidity_pool.py`
- `src/degenbot/aerodrome/pools.py`
- `src/degenbot/camelot/pools.py`

**Secondary (protocol):**
- `src/degenbot/types/pool_protocols.py` — add `build_swap_amount()` to `ArbitragePathPool` protocol

## Problem

Adding a new pool type requires touching at least four places for swap-amount handling:

1. **Define** a new `SwapAmounts` subclass in `arbitrage/types.py`
2. **Add a case** in `build_swap_amount()` in `swap_amount_builder.py` — an isinstance chain importing concrete pool classes
3. **Add cases** in `_extract_amount_in()` and `_extract_amount_out()` in `arbitrage_path.py` — match/case on concrete types
4. **Add the pool's** `to_hop_state()` and `extract_fee()` methods (separate concern)

`AbstractSwapAmounts` is a typed empty shell — no methods, no invariants. Each subclass stores amounts in a different shape:

- `UniswapV2PoolSwapAmounts`: `(amount_in, 0) or (0, amount_in)` as a tuple pair — requires `max()` to extract.
- `UniswapV3PoolSwapAmounts`: `amount_in` and `amount_out` as separate fields.
- `UniswapV4PoolSwapAmounts`: same shape as V3.
- `CurveStableSwapPoolSwapAmounts`: `amount_in` and `min_amount_out` as separate fields.

The **locality failure**: the knowledge of "how to extract the input and output amounts from a V2 swap amount" lives in `arbitrage_path.py`, not on `UniswapV2PoolSwapAmounts` where it belongs. The factory `build_swap_amount()` in `swap_amount_builder.py` imports concrete pool classes from `degenbot.uniswap`, coupling the arbitrage module to pool implementations.

### After Plan 034, the scope shrinks

All `.amounts_in` / `.amounts_out` direct accesses outside `types.py` itself are in the legacy cycle files (Plan 034 deletes them). The only remaining consumers are:
1. `UniswapV2PoolSwapAmounts` itself (`__post_init__`, `encode()`)
2. `_extract_amount_in` / `_extract_amount_out` in `arbitrage_path.py`

## Solution

### Step 1: Add `input_amount()` and `output_amount()` to `AbstractSwapAmounts`

Use `input_amount()` / `output_amount()` (not `amount_in()` / `amount_out()`) to avoid name collision with existing V3/V4 fields named `amount_in`.

```python
class AbstractSwapAmounts:
    """Base class for per-pool swap parameters and encoding."""

    def input_amount(self) -> int:
        """Return the input amount for this swap."""
        msg = f"{type(self).__name__} must implement input_amount()"
        raise NotImplementedError(msg)

    def output_amount(self) -> int:
        """Return the output amount for this swap."""
        msg = f"{type(self).__name__} must implement output_amount()"
        raise NotImplementedError(msg)

    def encode(self, *, recipient: ChecksumAddress) -> EncodedCall:
        """Encode this swap into an EVM call."""
        msg = f"{type(self).__name__} must implement encode()"
        raise NotImplementedError(msg)
```

### Step 2: Implement on each subclass

```python
# CurveStableSwapPoolSwapAmounts
def input_amount(self) -> int:
    return self.amount_in

def output_amount(self) -> int:
    return self.min_amount_out

# UniswapV2PoolSwapAmounts
def input_amount(self) -> int:
    return max(self.amounts_in)

def output_amount(self) -> int:
    return max(self.amounts_out)

# UniswapV3PoolSwapAmounts
def input_amount(self) -> int:
    return self.amount_in

def output_amount(self) -> int:
    return self.amount_out

# UniswapV4PoolSwapAmounts
def input_amount(self) -> int:
    return self.amount_in

def output_amount(self) -> int:
    return self.amount_out
```

### Step 3: Replace `_extract_amount_in` / `_extract_amount_out` with protocol calls

In `arbitrage_path.py`, `build_swap_amounts()` currently calls:

```python
input_amount = _extract_amount_in(input_swap)
output_amount = _extract_amount_out(output_swap)
```

Replace with:

```python
input_amount = input_swap.input_amount()
output_amount = output_swap.output_amount()
profit_amount = output_amount - input_amount
```

Delete the two `_extract_amount_in` / `_extract_amount_out` free functions (~25 lines).

### Step 4: Add `build_swap_amount()` to pool classes and protocol

Add a `build_swap_amount(swap_vector, amount_in, amount_out) -> AbstractSwapAmounts` method to the `ArbitragePathPool` protocol and implement on each pool class.

**Circular import consideration**: Pool classes are in `degenbot.uniswap`; `SwapAmounts` types are in `degenbot.arbitrage.types`. Currently, no pool module imports from `degenbot.arbitrage`. Adding `build_swap_amount()` would create `uniswap.* → arbitrage.types` — a new dependency. This is acceptable because:
- The dependency is one-directional (pools → arbitrage types, not vice versa)
- `SwapAmounts` types are pure data classes with no pool references
- The alternative (keeping the factory in `swap_amount_builder.py`) already has the same dependency in reverse (`arbitrage → uniswap.*` for isinstance checks)

However, if this dependency is undesirable, an alternative is to keep `build_swap_amount()` as a standalone function but make it dispatch via the `ArbitragePathPool` protocol rather than concrete isinstance checks. The protocol method would avoid importing concrete pool classes in `swap_amount_builder.py`. This is Option A below.

**Option A (recommended): Protocol-based dispatch, implementations on pools**

```python
# In pool_protocols.py — add to ArbitragePathPool:
def build_swap_amount(
    self,
    swap_vector: SwapVector,
    amount_in: int,
    amount_out: int,
) -> AbstractSwapAmounts: ...
```

Each pool implements it:

```python
# In UniswapV2Pool:
def build_swap_amount(self, swap_vector, amount_in, amount_out) -> UniswapV2PoolSwapAmounts:
    zfo = swap_vector.zero_for_one
    return UniswapV2PoolSwapAmounts(
        pool=self.address,
        amounts_in=(amount_in, 0) if zfo else (0, amount_in),
        amounts_out=(0, amount_out) if zfo else (amount_out, 0),
    )
```

Then `swap_amount_builder.py` simplifies to:

```python
def build_swap_amount(pool, swap_vector, amount_in, amount_out):
    return pool.build_swap_amount(swap_vector, amount_in, amount_out)
```

Or `ArbitragePath.build_swap_amounts()` calls `pool.build_swap_amount()` directly and the module-level function is deleted.

**Option B: Keep factory, use protocol dispatch only**

Keep `build_swap_amount()` in `swap_amount_builder.py` but replace isinstance checks with `hasattr` or protocol-based dispatch. This avoids pools importing from arbitrage.

Same circular-import concern in reverse — `swap_amount_builder.py` currently imports concrete pool classes. With protocol dispatch, it wouldn't need to.

**Why Option A**: Putting `build_swap_amount()` on the pool class is the most local design — the pool knows which `SwapAmounts` subclass it produces. Adding a new pool type is a one-class change. The circular import is one-directional and acceptable (pools import pure data types from arbitrage, not logic).

If the one-directional import is still undesirable, Option B is the fallback — protocol-based factory function that doesn't import concrete types.

### Step 5: Simplify `ArbitragePath.build_swap_amounts()`

After Steps 3 and 4:

```python
def build_swap_amounts(self, result, state_overrides=None):
    ...
    for pool, sv in zip(self._pools, self._swap_vectors, strict=True):
        ...
        swap_amounts.append(pool.build_swap_amount(sv, token_in_quantity, token_out_quantity))
        token_in_quantity = token_out_quantity

    input_amount = swap_amounts[0].input_amount()
    output_amount = swap_amounts[-1].output_amount()
    profit_amount = output_amount - input_amount
    ...
```

## Implementation Order

1. **Step 1–2**: Add `input_amount()` / `output_amount()` to `AbstractSwapAmounts` and all subclasses (one commit)
2. **Step 3**: Replace `_extract_amount_in/out` in `arbitrage_path.py` (one commit)
3. **Step 4**: Add `build_swap_amount()` to `ArbitragePathPool` protocol and each pool class (one commit)
4. **Step 5**: Simplify `ArbitragePath.build_swap_amounts()` and delete/simplify `swap_amount_builder.py` (one commit)

Run `just test-python` after each step.

**Precondition**: Plan 034 should be done first to eliminate the legacy `.amounts_in` / `.amounts_out` call sites.

## Testing

### Per-subclass tests

For each `SwapAmounts` subclass, test:
- `input_amount()` returns the correct value
- `output_amount()` returns the correct value
- Edge cases: zero amounts, directional tuple orientation (V2)
- `encode()` still works (unchanged)

### Integration test

- `ArbitragePath.build_swap_amounts()` produces correct `ArbitrageCalculationResult` for V2, V3, V4, mixed paths
- `profit_amount` calculation matches expected values

### Regression test

- All existing solver tests pass
- `generate_payloads()` produces correct `EncodedCall`s

## Benefits

- **Locality**: Adding a new pool type's swap amounts means: (1) add a `SwapAmounts` subclass with `input_amount()`, `output_amount()`, `encode()`, and (2) implement `build_swap_amount()` on the pool class. Two files touched, no match/case dispatchers to update.
- **Leverage**: `ArbitragePath` calls `swap.input_amount()` / `swap.output_amount()` and `pool.build_swap_amount()` generically — no knowledge of concrete swap-amount types needed.
- **Testability**: Each `SwapAmounts` subclass is self-contained and testable without constructing a full arbitrage path.
- **Encapsulation**: V2's directional tuple representation hidden behind `input_amount()` / `output_amount()`.
- **Reduced coupling**: `swap_amount_builder.py` no longer imports concrete pool classes — protocol dispatch replaces isinstance checks.

## Risks

- **One-directional import (pools → arbitrage.types)**: Adding `build_swap_amount()` to pool classes means pools import `SwapAmounts` types from `degenbot.arbitrage.types`. This is a new `uniswap → arbitrage` dependency. It's acceptable because the imported types are pure data classes with no logic. If this is still undesirable, Option B (protocol-based factory function) avoids it.
- **Field name collision**: `input_amount()` / `output_amount()` avoids the collision with V3/V4's `amount_in` / `amount_out` fields. The naming is slightly inconsistent (method is `input_amount()` but field is `amount_in`), but the semantic mapping is clear.
- **Curve pools excluded from `ArbitragePathPool`**: Curve pools use `MultiTokenSwapCalculation` protocol, not `ArbitragePathPool`. They don't need `build_swap_amount()` — `CurveStableSwapPoolSwapAmounts` is built separately in `CurveCycle` code. This is the existing split and is not changed by this plan.
- **Protocol method on every arbitrage-participating pool**: Adding `build_swap_amount()` to `ArbitragePathPool` means every pool that joins an arbitrage path must implement it. Currently V2, V3, V4, Aerodrome, Camelot. The scope is bounded and the implementation is straightforward (10 lines per class).

## Relationship to Other Plans

- **Plan 033** (Consolidate Dual Pool-to-Hop Conversion): Independent but complementary. 033 consolidates pool→hop; this plan consolidates pool→swap-amounts. Together they make adding a new pool type a purely local change.
- **Plan 034** (Delete Legacy Arbitrage Cycles): Should be done first. Eliminates all legacy `.amounts_in` / `.amounts_out` direct accesses, shrinking the refactoring scope.
- **Plan 021** (Extract SwapEncoder): Complete. Created `encoding.py` with `generate_payloads()`. This plan improves the `SwapAmounts` data classes that `generate_payloads()` consumes.
