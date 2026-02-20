# Aave Debug Progress

## Issue: V4 Collateral Burn Events Use Incorrect Rounding

**Date:** 2025-02-19

**Symptom:** 
```
AssertionError: User 0xADC0A53095A0af87F3aa29FE0715B5c28016364e: collateral balance (1) does not match scaled token contract (0) @ 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c at block 23088590
```

**Root Cause:** 
The `CollateralV4Processor` class in `src/degenbot/aave/processors/collateral/v4.py` did not override the `process_burn_event()` method from `CollateralV1Processor`. Since V4 aTokens use the `TokenMath` library which applies **ceiling division** for burn operations (via `getATokenBurnScaledAmount()`), but V1-V3 use **half-up rounding**, the inherited V1 method was calculating incorrect balance deltas.

**The calculation:**
- Balance transfer added: `21,945,410,670` scaled tokens
- Withdraw amount: `25,000,000,000` USDC
- Index: `1139190347206618673677188955`
- Scaled amount = (25,000,000,000 * 10^27) / 1,139,190,347,206,618,673,677,188,955 = 21,945,410,669.999...
- V1 `ray_div` (half-up): `21,945,410,669` (leaves 1 wei remaining)
- V4 `ray_div_ceil` (ceiling): `21,945,410,670` (burns entire balance)

**Transaction Details:**
- **Hash:** 0xb201f849aa3f5ca789543ba31d92f111a5a38bd422811055178d18b04780f010
- **Block:** 23088590
- **Type:** WITHDRAW (full balance withdrawal)
- **User:** 0xADC0A53095A0af87F3aa29FE0715B5c28016364e
- **Asset:** aUSDC (0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c, revision 4)
- **Events:** 
  - BalanceTransfer at logIndex 98: 21,945,410,670 scaled tokens received
  - Burn at logIndex 102: 25,000,000,000 USDC withdrawn

**Fix:**
1. Added `process_burn_event()` method override to `CollateralV4Processor` in `src/degenbot/aave/processors/collateral/v4.py`:

```python
def process_burn_event(
    self,
    event_data: CollateralBurnEvent,
    previous_balance: int,  # noqa: ARG002
    previous_index: int,  # noqa: ARG002
) -> BurnResult:
    """
    Process a collateral burn event.

    Burn events are triggered by WITHDRAW operations.
    Revision 4 uses ceiling division (ray_div_ceil) to match
    TokenMath.getATokenBurnScaledAmount behavior.
    """
    wad_ray_math = self._math_libs["wad_ray"]

    # uint256 amountToBurn = amount + balanceIncrease;
    requested_amount = event_data.value + event_data.balance_increase

    # uint256 amountScaled = amount.rayDiv(index);
    # Use ceiling division to match TokenMath.getATokenBurnScaledAmount
    balance_delta = -wad_ray_math.ray_div_ceil(
        a=requested_amount,
        b=event_data.index,
    )

    return BurnResult(
        balance_delta=balance_delta,
        new_index=event_data.index,
    )
```

2. Added `ray_div_ceil` to the `WadRayMathLibrary` protocol in `src/degenbot/aave/processors/base.py`:

```python
class WadRayMathLibrary(Protocol):
    """Protocol for Wad/Ray math operations."""

    def ray_div(self, a: int, b: int) -> int: ...
    def ray_div_ceil(self, a: int, b: int) -> int: ...  # Added
    def ray_div_floor(self, a: int, b: int) -> int: ...
    def ray_mul(self, a: int, b: int) -> int: ...
```

**Key Insight:**
Different Aave aToken revisions use different rounding strategies for different operations:
- **Revisions 1-3**: Use `WadRayMath.rayDiv` - half-up rounding for all operations
- **Revision 4+**: Use `TokenMath` library with operation-specific rounding:
  - **Mints (SUPPLY)**: Floor division via `getATokenMintScaledAmount()`
  - **Burns (WITHDRAW)**: Ceiling division via `getATokenBurnScaledAmount()`

The V4 processor already had the correct floor rounding for mints (SUPPLY), but was missing the ceiling rounding for burns (WITHDRAW).

**Refactoring:**
Consider reviewing all V4+ processors to ensure they override methods that need different rounding behavior from their parent classes. Create a documentation table showing which rounding mode each revision uses for each operation type.
