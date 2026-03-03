# Issue 0019: Treasury Liquidation Fee Transfer Amount Bug

**Date:** 2025-03-02

## Symptom

```
AssertionError: User 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c: collateral balance (591699257463037258842) does not match scaled token contract (591699276327750759963) @ 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 at block 20245383
```

## Root Cause

In `_process_collateral_transfer_with_match` (aave.py), ERC20 transfer amounts were being incorrectly converted using `calculate_collateral_transfer_scaled_amount()`, which applies `ray_div_ceil(amount, liquidity_index)`. However, for ERC20 transfers of aTokens, the `amount` field is **already the scaled balance** - no conversion is needed.

The code was treating ERC20 transfer amounts as if they were underlying amounts that needed to be converted to scaled amounts:

```python
# BUG: This incorrectly applies ray_div_ceil to an already-scaled amount
transfer_amount = pool_processor.calculate_collateral_transfer_scaled_amount(
    amount=scaled_event.amount,  # Already scaled!
    liquidity_index=liquidity_index,
)
```

## Transaction Details

- **Hash:** `0xe762a8ead8ccd682c95c50471486a0cbca6d1831b7edb71edb6b11fc536f5d81`
- **Block:** 20245383
- **Type:** MEV Bot Liquidation (4 positions)
- **Treasury:** `0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c` (Aave Treasury Collector V2)
- **Token:** `0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8` (aWETH)

### Liquidation Fee Transfers

The following ERC20 transfers sent liquidation fees to the treasury:

1. From `0x64a524...`: **56964777404410** (0.00005696 aWETH)
2. From `0xcd705d...`: **36134648872474** (0.00003613 aWETH)
3. From `0xcd705d...`: **55510648892147** (0.00005551 aWETH)
4. From `0x726a16...`: **57821049305951** (0.00005782 aWETH)

**Total:** 206431124474982 (0.00020643 aWETH)

## Fix

**File:** `src/degenbot/cli/aave.py`  
**Function:** `_process_collateral_transfer_with_match`  
**Lines:** 3277-3294

### Before

```python
# Standalone ERC20 Transfer without BalanceTransfer event
# Need to scale the amount using the current liquidity index
elif collateral_asset.a_token_revision >= 4:
    pool_processor = PoolProcessorFactory.get_pool_processor_for_token_revision(
        collateral_asset.a_token_revision
    )
    # Get liquidity index from the asset's reserve data
    liquidity_index = int(collateral_asset.liquidity_index)
    transfer_amount = pool_processor.calculate_collateral_transfer_scaled_amount(
        amount=scaled_event.amount,
        liquidity_index=liquidity_index,
    )
    transfer_index = liquidity_index
else:
    # Revision 1-3: standard ray_div using asset's liquidity index
    liquidity_index = int(collateral_asset.liquidity_index)
    transfer_amount = scaled_event.amount * liquidity_index // 10**27
    transfer_index = liquidity_index
```

### After

```python
# Standalone ERC20 Transfer without BalanceTransfer event
# The amount is the actual aToken amount (which IS the scaled balance)
elif collateral_asset.a_token_revision >= 4:
    # For ERC20 transfers, scaled_event.amount is already the scaled balance
    transfer_amount = scaled_event.amount
    transfer_index = int(collateral_asset.liquidity_index)
else:
    # Revision 1-3: ERC20 transfer amount is already the scaled balance
    transfer_amount = scaled_event.amount
    transfer_index = int(collateral_asset.liquidity_index)
```

Also fixed the BalanceTransfer handling (lines 3264-3276) to use the amount directly:

```python
# Standalone BalanceTransfer - the amount is already the scaled balance
# BalanceTransfer events contain the scaled amount directly
elif scaled_event.index > 0:
    transfer_amount = scaled_event.amount
    transfer_index = scaled_event.index
```

## Key Insight

**aToken amounts are always scaled amounts.**

- **ERC20 Transfer:** The `amount` field is the aToken amount (scaled balance)
- **BalanceTransfer:** The `amount` field is the scaled balance

Neither needs conversion via `ray_div` or `calculate_collateral_transfer_scaled_amount`. The confusion arose because:

1. Pool events (SUPPLY, WITHDRAW, etc.) report **underlying amounts**
2. Token events (Mint, Burn, Transfer) report **scaled amounts**
3. The conversion functions like `calculate_collateral_transfer_scaled_amount` are meant for converting underlying amounts to scaled amounts for Pool event processing, not for Token event processing.

## Refactoring

Consider:
1. Renaming `scaled_event.amount` to `scaled_event.scaled_amount` to make it clear this is already scaled
2. Adding type annotations to distinguish between underlying amounts and scaled amounts
3. Creating separate handling paths for Pool events (underlying) vs Token events (scaled)
4. Adding unit tests that verify exact balance changes for known transactions

## References

- Transaction: https://etherscan.io/tx/0xe762a8ead8ccd682c95c50471486a0cbca6d1831b7edb71edb6b11fc536f5d81
- Test: `tests/cli/test_aave_treasury_liquidation_fees.py`
- Related Files:
  - `src/degenbot/cli/aave.py` - Main processing logic
  - `src/degenbot/aave/libraries/token_math.py` - Math functions
