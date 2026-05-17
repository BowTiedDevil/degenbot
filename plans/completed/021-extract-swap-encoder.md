# Plan 021: Extract SwapEncoder from UniswapLpCycle and Deprecate Legacy Path

**Status: READY**

## Overview

Extract the swap-calldata-building and V4-pool-key capabilities from `UniswapLpCycle` into a focused `SwapEncoder` module. Then deprecate `UniswapLpCycle` in favor of `ArbitragePath` + `SwapEncoder`, eliminating the duplicate arbitrage-path implementation.

## Files Involved

**Existing:**
- `src/degenbot/arbitrage/uniswap_lp_cycle.py` (~768 lines) — legacy arbitrage cycle
- `src/degenbot/arbitrage/path/arbitrage_path.py` (~377 lines) — current arbitrage path
- `src/degenbot/arbitrage/path/swap_amount_builder.py` — swap amount construction
- `src/degenbot/arbitrage/types.py` — `ArbitrageCalculationResult`, `AbstractSwapAmounts`, etc.

**New:**
- `src/degenbot/arbitrage/swap_encoder.py` — encoded swap calldata generation
- `src/degenbot/arbitrage/v4_pool_key.py` — V4 pool key encoding (if not already standalone)

**Modified:**
- `src/degenbot/arbitrage/uniswap_lp_cycle.py` — after extraction, mark as deprecated

**Tests:**
- `tests/arbitrage/test_swap_encoder.py` — new
- `tests/arbitrage/integration/test_uniswap_curve_cycle.py` — updated imports

## Problem

`UniswapLpCycle` and `ArbitragePath` both implement cyclic token-path arbitrage with the same core flow:

1. Validate pool token flow
2. Build swap vectors
3. Subscribe to state updates
4. Solve for optimal input
5. Build swap amounts
6. Encode swap calldata (UniswapLpCycle only)

`UniswapLpCycle` is the legacy 768-line class that does everything itself — including scipy-based optimization, ABI encoding for swap calldata, and explicit type matching in `_build_swap_amounts()`:

```python
# _build_swap_amounts — explicit type matching
match pool:
    case AerodromeV2Pool():
        ...
    case UniswapV2Pool():
        ...
    case UniswapV3Pool():
        ...
    case UniswapV4Pool():
        ...
```

`ArbitragePath` is the deeper module — it delegates to a swappable `Solver` protocol and uses `ArbitragePathPool` protocol instead of concrete types. But it lacks two capabilities that `UniswapLpCycle` has:

1. **Swap calldata encoding** — `UniswapLpCycle.generate_payloads()` constructs the actual on-chain transaction calldata (V2 `swap()`, V3 `exactInput()`, ERC-20 `approve()`, etc.). `ArbitragePath` only produces `SwapAmounts` — it stops at the calculation boundary.
2. **V4 pool key handling** — `UniswapLpCycle` has a `V4PoolKey` dataclass and V4 swap encoding that `ArbitragePath` doesn't.

Plan 011 addressed the solver duplication by making `_calculate()` delegate to `ArbSolver.solve()`. But the calldata-encoding capability remains trapped inside the legacy class. Understanding `UniswapLpCycle` requires bouncing between it and `ArbitragePath` to see what's shared vs. divergent, and the calldata encoding logic can't be reused by any other code path.

Applying the deletion test: if we deleted `UniswapLpCycle`, the duplicate logic (optimization) would vanish (already handled by `ArbitragePath`), but the swap encoding capability would reappear across callers that need on-chain execution. The swap encoding **earns its keep** — it just lives in the wrong module.

## Solution

### Step 1: Extract `SwapEncoder`

Create a focused module that takes `SwapAmounts` and produces encoded calldata for on-chain execution:

```python
# src/degenbot/arbitrage/swap_encoder.py

from __future__ import annotations

from dataclasses import dataclass
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3

from degenbot.arbitrage.types import (
    AbstractSwapAmounts,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
    UniswapV4PoolSwapAmounts,
)


# Function selectors (move from UniswapLpCycle)
UNISWAP_V2_SWAP_SELECTOR = Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
UNISWAP_V3_SWAP_SELECTOR = Web3.keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
ERC20_TRANSFER_SELECTOR = Web3.keccak(text="transfer(address,uint256)")[:4]


@dataclass(frozen=True, slots=True)
class V4PoolKey:
    """V4 pool identification for calldata encoding."""

    currency0: ChecksumAddress
    currency1: ChecksumAddress
    fee: int
    tick_spacing: int
    hooks: ChecksumAddress


@dataclass(frozen=True, slots=True)
class EncodedSwap:
    """An encoded swap calldata payload ready for on-chain submission."""

    to: ChecksumAddress
    data: bytes
    value: int = 0


def encode_v2_swap(
    *,
    pool_address: ChecksumAddress,
    amount0: int,
    amount1: int,
    recipient: ChecksumAddress,
) -> EncodedSwap:
    """Encode a Uniswap V2 swap() call."""
    data = UNISWAP_V2_SWAP_SELECTOR + Web3.codec.encode(
        types=["uint256", "uint256", "address", "bytes"],
        args=[amount0, amount1, recipient, b""],
    )
    return EncodedSwap(to=pool_address, data=data)


def encode_v3_swap(
    *,
    pool_address: ChecksumAddress,
    recipient: ChecksumAddress,
    zero_for_one: bool,
    amount_specified: int,
    sqrt_price_limit_x96: int,
) -> EncodedSwap:
    """Encode a Uniswap V3 swap() call."""
    data = UNISWAP_V3_SWAP_SELECTOR + Web3.codec.encode(
        types=["address", "bool", "int256", "uint160", "bytes"],
        args=[recipient, zero_for_one, amount_specified, sqrt_price_limit_x96, b""],
    )
    return EncodedSwap(to=pool_address, data=data)


def encode_erc20_transfer(
    *,
    token_address: ChecksumAddress,
    recipient: ChecksumAddress,
    amount: int,
) -> EncodedSwap:
    """Encode an ERC-20 transfer() call."""
    data = ERC20_TRANSFER_SELECTOR + Web3.codec.encode(
        types=["address", "uint256"],
        args=[recipient, amount],
    )
    return EncodedSwap(to=token_address, data=data)


def encode_swap_amounts(
    swap_amounts: AbstractSwapAmounts,
    *,
    recipient: ChecksumAddress,
) -> EncodedSwap:
    """
    Encode a single swap amount into on-chain calldata.

    Dispatches to the appropriate encoder based on swap amount type.
    """
    match swap_amounts:
        case UniswapV2PoolSwapAmounts():
            return encode_v2_swap(
                pool_address=swap_amounts.pool,
                amount0=swap_amounts.amounts_in[0] + swap_amounts.amounts_out[0],
                amount1=swap_amounts.amounts_in[1] + swap_amounts.amounts_out[1],
                recipient=recipient,
            )
        case UniswapV3PoolSwapAmounts():
            return encode_v3_swap(
                pool_address=swap_amounts.pool,
                recipient=recipient,
                zero_for_one=swap_amounts.zero_for_one,
                amount_specified=swap_amounts.amount_specified,
                sqrt_price_limit_x96=swap_amounts.sqrt_price_limit_x96,
            )
        case UniswapV4PoolSwapAmounts():
            # V4 encoding — more complex, uses PoolKey +Actions format
            # Extract from UniswapLpCycle._build_v4_swap_payload()
            ...
        case _:
            msg = f"Unsupported swap amount type: {type(swap_amounts).__name__}"
            raise ValueError(msg)


def generate_payloads(
    swap_amounts: tuple[AbstractSwapAmounts, ...],
    *,
    recipient: ChecksumAddress,
) -> list[EncodedSwap]:
    """
    Generate encoded swap payloads for all swaps in an arbitrage path.

    This is the replacement for UniswapLpCycle.generate_payloads().
    """
    return [encode_swap_amounts(swap, recipient=recipient) for swap in swap_amounts]
```

### Step 2: Deprecate UniswapLpCycle

After `SwapEncoder` is extracted and the solver delegation from Plan 011 is complete, `UniswapLpCycle` becomes a thin wrapper that only adds:

1. Pool type validation (already in `ArbitragePath` via `_check_pool_compatibility`)
2. Swap vector construction (identical to `ArbitragePath._build_swap_vectors`)
3. The `name` string (cosmetic)

Mark it as deprecated:

```python
class UniswapLpCycle(PublisherMixin, AbstractArbitrage):
    """
    Legacy arbitrage cycle implementation.

    .. deprecated::
        Use :class:`ArbitragePath` for solving and :func:`generate_payloads`
        for swap encoding. This class will be removed in a future version.
    """

    ...
```

### Migration path for existing users

```python
# BEFORE (legacy):
cycle = UniswapLpCycle(
    input_token=WETH,
    swap_pools=[pool_a, pool_b],
)
result = cycle.calculate()
payloads = cycle.generate_payloads(recipient=bot_address)

# AFTER (new):
path = ArbitragePath(
    pools=[pool_a, pool_b],
    input_token=WETH,
    solver=ArbSolver(),
)
result = path.calculate()
calc_result = path.build_swap_amounts(result)
payloads = generate_payloads(
    calc_result.swap_amounts,
    recipient=bot_address,
)
```

## Implementation Steps

### Phase 1: Extract SwapEncoder (TDD)

1. **Red:** Write tests for `encode_v2_swap()`, `encode_v3_swap()`, `encode_erc20_transfer()`:
   ```python
   def test_encode_v2_swap():
       encoded = encode_v2_swap(
           pool_address="0xPool...",
           amount0=1000,
           amount1=0,
           recipient="0xRecipient...",
       )
       assert encoded.to == "0xPool..."
       assert encoded.data.startswith(UNISWAP_V2_SWAP_SELECTOR)
       assert encoded.value == 0
   ```
2. **Green:** Create `src/degenbot/arbitrage/swap_encoder.py` with encoding functions.
3. **Red:** Write tests for `encode_swap_amounts()` dispatch:
   ```python
   def test_encode_swap_amounts_v2():
       swap = UniswapV2PoolSwapAmounts(
           pool="0xPool...",
           amounts_in=(1000, 0),
           amounts_out=(0, 900),
       )
       encoded = encode_swap_amounts(swap, recipient="0xRecipient...")
       assert encoded.to == "0xPool..."
   ```
4. **Green:** Implement `encode_swap_amounts()` with match/case dispatch.
5. **Red:** Write tests for `generate_payloads()`:
   ```python
   def test_generate_payloads_multi_swap():
       swaps = (v2_swap, v3_swap)
       payloads = generate_payloads(swaps, recipient="0xRecipient...")
       assert len(payloads) == 2
   ```
6. **Green:** Implement `generate_payloads()`.

### Phase 2: Extract V4PoolKey

1. Move `V4PoolKey` dataclass from `uniswap_lp_cycle.py` to `swap_encoder.py`.
2. Extract V4 swap encoding from `UniswapLpCycle._build_v4_swap_payload()` into `encode_v4_swap()`.
3. Run V4-specific tests.

### Phase 3: Verify parity with UniswapLpCycle

1. **Red:** Write parity tests proving `generate_payloads()` produces identical output to `UniswapLpCycle.generate_payloads()`:
   ```python
   def test_swap_encoder_matches_legacy_v2():
       cycle = UniswapLpCycle(input_token=FAKE_WETH, swap_pools=[FAKE_V2_POOL_0, FAKE_V2_POOL_1])
       result = cycle.calculate()
       legacy_payloads = cycle.generate_payloads(recipient=FAKE_RECIPIENT)

       # New path
       calc_result = path.build_swap_amounts(solver_result)
       new_payloads = generate_payloads(calc_result.swap_amounts, recipient=FAKE_RECIPIENT)

       assert len(legacy_payloads) == len(new_payloads)
       for legacy, new in zip(legacy_payloads, new_payloads):
           assert legacy["to"] == new.to
           assert legacy["data"] == new.data
   ```
2. Run parity tests for V2, V3, V4 paths.

### Phase 4: Deprecate UniswapLpCycle

1. Add deprecation warning to `UniswapLpCycle.__init__()`:
   ```python
   import warnings

   warnings.warn(
       "UniswapLpCycle is deprecated. Use ArbitragePath + SwapEncoder instead.",
       DeprecationWarning,
       stacklevel=2,
   )
   ```
2. Update docstring.
3. Run all tests (deprecation warnings should appear but not fail).

### Phase 5: Verify and clean up

1. `grep -rn "generate_payloads" src/degenbot/` — ensure only `SwapEncoder` and the deprecated `UniswapLpCycle` have this method.
2. `just test-all`.
3. `just lint`.

## What Stays the Same

- `ArbitragePath` — not modified. It already delegates solving to `Solver` protocol.
- `ArbitragePath.build_swap_amounts()` — not modified. Returns `ArbitrageCalculationResult` with `SwapAmounts`.
- The solver stack — not modified.
- All existing `ArbitragePath` tests.

## What Changes

| Before | After |
|---|---|
| Swap encoding lives inside 768-line `UniswapLpCycle` | Swap encoding lives in `swap_encoder.py` (~150 lines) |
| `UniswapLpCycle.generate_payloads()` | `generate_payloads(swap_amounts, recipient=...)` |
| `V4PoolKey` defined in `uniswap_lp_cycle.py` | `V4PoolKey` defined in `swap_encoder.py` |
| Can't reuse swap encoding without `UniswapLpCycle` | `SwapEncoder` usable independently |
| `UniswapLpCycle` is the only way to get encoded calldata | `ArbitragePath` + `SwapEncoder` is the recommended path |
| `UniswapLpCycle` emitters deprecation silence | `UniswapLpCycle` emits `DeprecationWarning` |

## Metrics

| Metric | Before | After |
|---|---|---|
| `UniswapLpCycle` lines | 768 | 768 (deprecated, unchanged) |
| Swap encoding as standalone module | No | Yes (`swap_encoder.py`) |
| Reusable swap encoding without legacy class | No | Yes |
| Testable swap encoding independently | No | Yes (unit tests with `SwapAmounts` fixtures) |
| Paths to get encoded calldata | 1 (legacy only) | 2 (legacy + `SwapEncoder`) |

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| V4 swap encoding is complex (PoolKey + Actions format) | The extraction is a 1:1 move — no logic changes. Parity tests prove identical output. |
| Existing callers of `UniswapLpCycle.generate_payloads()` | Deprecation warning guides them to `SwapEncoder`. The legacy method continues to work. |
| `SwapEncoder` depends on `web3` for ABI encoding | Acceptable — this module is inherently about on-chain encoding. Construction/simulation paths don't depend on it. |
| Not removing `UniswapLpCycle` immediately creates dual maintenance | After Plan 011 (solver delegation) and this plan (swap encoding extraction), `UniswapLpCycle` is a thin wrapper. The only remaining code is vector construction — nearly identical to `ArbitragePath`. Full removal can happen in a subsequent cleanup. |

## Dependencies on Other Plans

- **Plan 011** (ArbSolver delegation) — `UniswapLpCycle._calculate()` should delegate to `ArbSolver.solve()`. If not yet done, the swap encoding extraction can still proceed independently — `SwapEncoder` doesn't depend on the optimization path.
- **Plan 017** (V2/V3 I/O-free) — independent. Pool construction changes don't affect swap encoding.

## Definition of Done

- [ ] `src/degenbot/arbitrage/swap_encoder.py` created
- [ ] `encode_v2_swap()` implemented and tested
- [ ] `encode_v3_swap()` implemented and tested
- [ ] `encode_v4_swap()` implemented and tested
- [ ] `encode_erc20_transfer()` implemented and tested
- [ ] `encode_swap_amounts()` dispatch implemented and tested
- [ ] `generate_payloads()` implemented and tested
- [ ] `V4PoolKey` moved to `swap_encoder.py`
- [ ] Parity tests prove identical output to legacy `generate_payloads()`
- [ ] `UniswapLpCycle` marked deprecated with `DeprecationWarning`
- [ ] All arbitrage tests pass
- [ ] `just test-all` passes
