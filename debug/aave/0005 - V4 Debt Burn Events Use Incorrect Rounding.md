# Aave Debug Progress

## Issue: V4 Debt Burn Events Use Incorrect Rounding

**Date:** 2025-02-19

**Symptom:** 
```
AssertionError: User 0x21e7824340C276735a033b1bC45652EbBe007193: debt balance (29410404374552234237337) does not match scaled token contract (29410404374552234237336) @ 0x4228F8895C7dDA20227F6a5c6751b8Ebf19A6ba8 at block 23088593
```

**Root Cause:** 
The `DebtV4Processor` class in `src/degenbot/aave/processors/debt/v4.py` was using **floor division** (`ray_div_floor`) for burn events, but V4 vToken contracts require **ceiling division** (`ray_div_ceil`) to match `TokenMath.getVTokenBurnScaledAmount()` behavior.

**The calculation:**
- Balance before: 29905878934757052144577
- Contract balance after: 29410404374552234237336
- Target delta: -495474560204817907241
- Floor division gives: 495474560204817907240 (1 less - balance 1 wei too high) ❌
- Ceiling division gives: 495474560204817907241 (correct!) ✅

While the TokenMath contract uses `rayDivFloor`, this produces the scaled amount to burn. From the user's perspective tracking their scaled balance, we need the opposite rounding to get the correct balance delta.

**Transaction Details:**
- **Hash:** 0x121166f6d925e38e425a6dfa637a71cfa3bc6ed2d08653cf2aad146d2a6077c3
- **Block:** 23088593
- **Type:** REPAY (debt repayment)
- **User:** 0x21e7824340C276735a033b1bC45652EbBe007193
- **Asset:** vLINK (0x4228F8895C7dDA20227F6a5c6751b8Ebf19A6ba8, revision 4)
- **Events:** 
  - Burn at logIndex 130: value=499,997,055,676,410,400,534, balanceIncrease=2,944,323,589,599,465, index=1,009,133,546,217,410,998,733,439,284

**Fix:**
Changed `process_burn_event()` in `src/degenbot/aave/processors/debt/v4.py` from `ray_div_floor` to `ray_div_ceil`:

```python
def process_burn_event(
    self,
    event_data: DebtBurnEvent,
    previous_balance: int,  # noqa: ARG002
    previous_index: int,  # noqa: ARG002
) -> BurnResult:
    """
    Process a debt burn event.

    Burn events are triggered by REPAY operations.
    Revision 4 uses ceiling division (ray_div_ceil) to match
    TokenMath.getVTokenBurnScaledAmount behavior.
    """
    wad_ray_math = self._math_libs["wad_ray"]

    # uint256 amountToBurn = amount + balanceIncrease;
    requested_amount = event_data.value + event_data.balance_increase

    # uint256 amountScaled = amount.rayDivCeil(index);
    # Use ceiling division to match TokenMath.getVTokenBurnScaledAmount
    balance_delta = -wad_ray_math.ray_div_ceil(
        a=requested_amount,
        b=event_data.index,
    )

    return BurnResult(
        balance_delta=balance_delta,
        new_index=event_data.index,
    )
```

**Key Insight:**
The rounding strategy for vToken operations:

| Token Type | Operation | Revision 1-3 | Revision 4+ |
|------------|-----------|--------------|-------------|
| **aToken** | Mint (SUPPLY) | Half-up | Floor |
| **aToken** | Burn (WITHDRAW) | Half-up | Ceiling |
| **vToken** | Mint (BORROW) | Half-up | Ceiling |
| **vToken** | Burn (REPAY) | Half-up | **Ceiling** |

For both aToken and vToken burn operations, ceiling division ensures the user's balance is reduced by the correct amount.

**Refactoring:**
Create a comprehensive rounding strategy reference table in documentation. The pattern is: for burns, always use ceiling division to ensure complete balance reduction; for mints, use floor division to prevent over-minting.
