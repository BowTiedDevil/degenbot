# Aave Debug Progress

## Issue: SUPPLY Events Use Incorrect Rounding for Scaled Amount Calculation

**Date:** 2025-02-19

**Symptom:**
```
AssertionError: User 0xDC3C277932f44A1bcB3956164bb9a2a177c63F46: collateral balance (1426681888411580079023) does not match scaled token contract (1426681888411580079022) @ 0x5E8C8A7243651DB1384C0dDfDbE39761E8e7E51a at block 23088589
```

**Root Cause:**
The aToken at 0x5E8C8A7243651DB1384C0dDfDbE39761E8e7E51a was upgraded from revision 3 to revision 4 between blocks 23,088,000 and 23,099,000. The `calculate_scaled_amount()` method in `CollateralV1Processor` uses `ray_div()` (half-up rounding), but revision 4 tokens use the `TokenMath` library which applies **floor division** for mint operations via `getATokenMintScaledAmount()`.

The calculation for SUPPLY events:
- Raw amount: 244080826347865820530 LINK
- Index: 1000786317914685407870667881
- Exact value: 243889052017069157741.668...
- **Wrong (v1 half-up rounding)**: 243889052017069157742
- **Correct (v4 floor division)**: 243889052017069157741

**Transaction Details:**
- **Hash:** 0xea19ed9304575f498e00e9770e69f705c0a9038b2a3865c82d62464acf2514e9
- **Block:** 23088589
- **Type:** SUPPLY (deposit)
- **User:** 0xDC3C277932f44A1bcB3956164bb9a2a177c63F46
- **Asset:** aToken (0x5E8C8A7243651DB1384C0dDfDbE39761E8e7E51a, revision 4)
- **Amount:** 244.08 LINK

**Fix:**
1. Reverted `src/degenbot/aave/processors/collateral/v1.py` to keep `ray_div()` (half-up rounding) for revisions 1-3:

```python
def calculate_scaled_amount(self, raw_amount: int, index: int) -> int:
    """Calculate scaled amount from raw underlying amount.

    Uses half-up rounding (ray_div) to match revision 1-3 AToken
    behavior. These versions use WadRayMath.rayDiv which rounds
    half up, not floor division.
    """
    return self._math_libs["wad_ray"].ray_div(
        a=raw_amount,
        b=index,
    )
```

2. Added override in `src/degenbot/aave/processors/collateral/v4.py` to use `ray_div_floor()` for revision 4:

```python
def calculate_scaled_amount(self, raw_amount: int, index: int) -> int:
    """Calculate scaled amount from raw underlying amount.

    Uses floor division (ray_div_floor) to match revision 4 AToken
    behavior. This version uses TokenMath.getATokenMintScaledAmount
    which rounds down, unlike revision 1-3 which rounds half up.
    """
    return self._math_libs["wad_ray"].ray_div_floor(
        a=raw_amount,
        b=index,
    )
```

3. Added `ray_div_floor` to the `WadRayMathLibrary` protocol in `src/degenbot/aave/processors/base.py`:

```python
class WadRayMathLibrary(Protocol):
    """Protocol for Wad/Ray math operations."""

    def ray_div(self, a: int, b: int) -> int: ...
    def ray_div_floor(self, a: int, b: int) -> int: ...  # Added
    def ray_mul(self, a: int, b: int) -> int: ...
```

**Key Insight:**
Different Aave aToken revisions use different rounding strategies:
- **Revisions 1-3**: Use `WadRayMath.rayDiv` - half-up rounding for all operations
- **Revision 4+**: Use `TokenMath` library:
  - **Mints (SUPPLY)**: Floor division via `getATokenMintScaledAmount()`
  - **Burns (WITHDRAW)**: Ceiling division via `getATokenBurnScaledAmount()`

The `ray_div_floor()` and `ray_div_ceil()` functions were already implemented in the math libraries but weren't exposed through the protocol or used by the appropriate processor versions.

**Refactoring:**
Consider adding a `rounding` parameter to `calculate_scaled_amount()` to explicitly specify the rounding mode, making the intent clearer and reducing the risk of using the wrong rounding strategy for different operation types.
