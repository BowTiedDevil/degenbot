# Issue 0041: Treasury Fee Collection from Liquidations

## Date
2026-03-20

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...WETH...). 
User AaveV3User(...Treasury...) scaled balance (591699257463037258842) does not match 
contract balance (591699276327750759963) at block 20245383
```

**Balance Difference:** ~18,887,550,150,121 scaled units

## Root Cause

During batch liquidations, the Aave protocol collects liquidation fees and mints them to the treasury. This creates a complex event pattern that wasn't being handled correctly:

1. **Transfer event (logIndex 214)**: Mints aTokens from ZERO_ADDRESS to treasury (protocol fee)
2. **Mint event (logIndex 215)**: Emitted for interest accrual tracking (amount == balanceIncrease)

The code was incorrectly:
1. Treating the Mint event as MINT_TO_TREASURY (when it's actually interest accrual)
2. Not processing the Transfer event at all (skipping it as "part of a mint")
3. Not converting the Transfer amount from underlying to scaled units

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0xe762a8ead8ccd682c95c50471486a0cbca6d1831b7edb71edb6b11fc536f5d81 |
| **Block** | 20245383 |
| **Type** | Batch Liquidation (4 liquidations) |
| **Asset** | WETH aToken (aEthWETH) |
| **Treasury** | 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c |

### Event Sequence

| LogIndex | Event | From | To | Amount | Notes |
|----------|-------|------|-----|--------|-------|
| 214 | ERC20 Transfer | 0x0 (zero) | Treasury | 834,056,496,965,512 | Protocol fee mint |
| 215 | Mint | Pool | Treasury | 834,056,496,965,512 | Interest accrual (amount == balanceIncrease) |

## The Fix

### 1. Skip Interest Accrual Mint Events in MINT_TO_TREASURY
**File:** `src/degenbot/cli/aave_transaction_operations.py`
**Lines:** 2658-2666

```python
# Skip interest accrual events (amount == balance_increase means no new tokens minted)
# These are emitted during transfers/liquidations for tracking, not actual MINT_TO_TREASURY
if ev.amount == ev.balance_increase:
    logger.debug(
        f"Skipping interest accrual Mint event at logIndex {ev.event['logIndex']} - "
        f"amount ({ev.amount}) equals balance_increase ({ev.balance_increase})"
    )
    assigned_indices.add(ev.event["logIndex"])
    continue
```

### 2. Don't Skip Transfers Paired with Interest Accrual Mints
**File:** `src/degenbot/cli/aave_transaction_operations.py`
**Lines:** 2802-2825

```python
# Skip ERC20 Transfer events from zero address that are part of mints
if is_erc20_transfer and ev.from_address == ZERO_ADDRESS:
    is_part_of_mint = False
    ev_token_address = get_checksum_address(ev.event["address"])
    for other_ev in scaled_events:
        if (
            other_ev.event_type in {COLLATERAL_MINT, DEBT_MINT, GHO_DEBT_MINT}
            and other_ev.user_address == ev.target_address
            and get_checksum_address(other_ev.event["address"]) == ev_token_address
        ):
            # Only skip if this is an actual mint (amount != balance_increase)
            # If amount == balance_increase, it's interest accrual (tracking-only)
            if other_ev.balance_increase is not None and other_ev.amount != other_ev.balance_increase:
                is_part_of_mint = True
                local_assigned.add(ev.event["logIndex"])
                break
    if is_part_of_mint:
        continue
```

### 3. Process Standalone Transfers from Zero Address
**File:** `src/degenbot/cli/aave.py`
**Lines:** 3609-3617

```python
# Skip protocol mints (from zero address) - but only if they're part of SUPPLY or MINT_TO_TREASURY
if scaled_event.from_address == ZERO_ADDRESS:
    if operation and operation.operation_type == OperationType.BALANCE_TRANSFER:
        # This is a standalone BALANCE_TRANSFER operation (e.g., treasury fee collection)
        return False
    return True
```

### 4. Convert Underlying Amount to Scaled Units
**File:** `src/degenbot/cli/aave.py`
**Lines:** 3745-3752

```python
else:
    # Standalone ERC20 Transfer - convert to scaled units using liquidity index
    if scaled_event.from_address == ZERO_ADDRESS:
        # Protocol mint (e.g., treasury fee collection)
        scaled_amount = scaled_event.amount * 10**27 // collateral_asset.liquidity_index
    else:
        scaled_amount = scaled_event.amount
    transfer_index = collateral_asset.liquidity_index
```

## Key Insight

**Interest accrual Mint events (amount == balanceIncrease) are NOT actual mints.**

These events are emitted by the AToken contract during transfers/liquidations to track interest that has accrued since the user's last interaction. They do NOT represent new tokens being minted.

When you see:
- Transfer from ZERO_ADDRESS → actual token mint (protocol fees, supplies)
- Mint event with amount == balanceIncrease → tracking-only event (no state change)

The actual fee collection happens via the Transfer event, which must be processed separately.

## Verification

After applying all fixes:
```bash
$ uv run degenbot aave update
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) 
successfully updated to block 20,245,383
```

**Verification:**
- Block 20245383: ✅ Processed successfully
- Treasury balance: ✅ Matches contract state
- All liquidation operations: ✅ Processed correctly
- No regressions in other operations

## Summary

**Issue:** Balance verification failure due to incorrect handling of treasury fee collection during liquidations.

**Root Cause:** The Mint event at logIndex 215 was interest accrual (amount == balanceIncrease), not an actual mint. The Transfer event at logIndex 214 was the actual fee collection, but it was being skipped.

**Fix:** Four-part fix to:
1. Skip interest accrual Mint events when classifying MINT_TO_TREASURY
2. Don't skip Transfers paired with interest accrual Mints
3. Process standalone Transfers from zero address in BALANCE_TRANSFER operations
4. Convert underlying Transfer amounts to scaled units

**Impact:** Treasury fee collection during liquidations now correctly updates the treasury's scaled balance.
