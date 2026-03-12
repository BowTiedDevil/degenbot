# Issue: Debt Transfer Double Counting in Debt Swap

## Status
**RESOLVED** - Fix implemented and verified

## Date
2026-03-12

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), underlying_token=Erc20TokenTable(chain=1, address='0xdAC17F958D2ee523a2206206994597C13D831ec7', symbol=None), a_token=Erc20TokenTable(chain=1, address='0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a', symbol=None), v_token=Erc20TokenTable(chain=1, address='0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8', symbol=None)). User AaveV3User(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), address='0x16DBF7C8961c603cC92Bf94956aFd86559943B99', e_mode=0) scaled balance (3252026251) does not match contract balance (40050934) at block 23088665
```

## Root Cause

The processing code is incorrectly creating **BALANCE_TRANSFER operations** from ERC20 Transfer events that are part of debt burns. This causes the debt burn amount to be **double-counted**:

1. First as a BALANCE_TRANSFER that adds the unscaled amount to the user's balance
2. Then as an INTEREST_ACCRUAL (debt burn) that subtracts the scaled amount

### Why This Happens

During a debt swap via ParaSwapDebtSwapAdapterV3GHO, when USDT debt is repaid:

1. The Aave Pool calls `vToken.burn(user, amount, index)`
2. The vToken emits TWO events:
   - **ERC20 Transfer** (topic `0xddf252ad...`) from user to address(0), amount = 3,799,613,244
   - **Burn** event (topic `0x4cf25bc1...`) with value=3,799,613,244, balanceIncrease=386,755, index=1.183e27

3. The operation builder creates TWO separate operations:
   - **BALANCE_TRANSFER** (operation 5) from the ERC20 Transfer event (index=None)
   - **INTEREST_ACCRUAL** (operation 3) from the Burn event

4. During processing:
   - BALANCE_TRANSFER adds 3,799,613,244 (unscaled) to user's debt balance
   - INTEREST_ACCRUAL subtracts the scaled burn amount
   - Net effect: Balance is incorrectly inflated by ~3.2B

### Balance Calculation Breakdown

| Step | Operation | Amount | Scaled Amount | Position Balance |
|------|-----------|--------|---------------|------------------|
| Initial | - | - | - | 0 |
| 5 | BALANCE_TRANSFER | +3,799,613,244 | N/A (index=None) | 3,799,613,244 |
| 3 | INTEREST_ACCRUAL | -3,799,613,244 | 3,211,989,141 | 587,624,103 |
| 1 | BORROW | +3,287,390 | 3,211,989 | 590,836,092 |
| 6 | BALANCE_TRANSFER | +16,354 | N/A (index=None) | 590,852,446 |
| 4 | INTEREST_ACCRUAL | -16,354 | 15,973 | 590,836,473 |
| 0 | REPAY | -3,800,000,000 | N/A (no burn) | 590,836,473 |
| 2 | REPAY | -16,355 | N/A (no burn) | 590,836,473 |

**Expected**: ~40,050,934  
**Calculated**: 3,252,026,251  
**Discrepancy**: ~81x

### The Core Issue

The `_create_transfer_operations` function in `aave_transaction_operations.py` creates BALANCE_TRANSFER operations for unassigned ERC20 Transfer events. However, it doesn't filter out transfers that are part of debt burns (transfers to address(0) that have a corresponding Burn event).

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | 0x8054fbc3e481a37ad238384f5012ade5332d7a4b2469d982daf658c7893f97e3 |
| **Block** | 23088665 |
| **Type** | Debt Swap (ParaSwapDebtSwapAdapterV3GHO) |
| **User** | 0x16DBF7C8961c603cC92Bf94956aFd86559943B99 |
| **Asset** | USDT (Variable Debt) |
| **vToken** | 0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8 |
| **vToken Revision** | 4 |

### Event Sequence

| Log Index | Event Type | From | To | Amount | Notes |
|-----------|------------|------|-----|--------|-------|
| 519 | ERC20 Transfer | 0x16DBF7... | 0x0000... | 3,799,613,244 | **Part of burn** |
| 520 | Burn | 0x16DBF7... | 0x0000... | 3,799,613,244 | Principal + 386,755 interest |

## Fix Applied

**Files Modified**:

### 1. `src/degenbot/cli/aave_transaction_operations.py`

Added logic to skip ERC20 Transfer events to address(0) that are part of burns:

```python
# Skip ERC20 Transfer events to zero address that are part of burns
# These are handled by the Burn events, not as balance transfers
# ref: Issue #0003 - Debt Transfer Double Counting in Debt Swap
if is_erc20_transfer and ev.target_address == ZERO_ADDRESS:
    # Check if there's a Burn event at the next log index for the same user
    for other_ev in scaled_events:
        if (
            other_ev.event["logIndex"] == ev.event["logIndex"] + 1
            and other_ev.event_type
            in {
                ScaledTokenEventType.DEBT_BURN,
                ScaledTokenEventType.COLLATERAL_BURN,
                ScaledTokenEventType.GHO_DEBT_BURN,
            }
            and other_ev.user_address == ev.from_address
        ):
            # This transfer is part of a burn, skip it
            local_assigned.add(ev.event["logIndex"])
            break
    continue
```

### 2. `src/degenbot/cli/aave_types.py`

Changed `last_repay_amount: int` to `repay_amounts_by_asset: dict[ChecksumAddress, int]` to support multiple REPAY operations in a single transaction.

### 3. `src/degenbot/cli/aave_event_matching.py`

Updated `_match_interest_accrual` to use the stored paybackAmount from `repay_amounts_by_asset` keyed by token address.

### 4. `src/degenbot/cli/aave.py`

- Updated operation sorting to use pool event logIndex when available (ensures REPAY operations are processed before INTEREST_ACCRUAL)
- Updated `_process_debt_burn_with_match` to look up paybackAmount by reserve address instead of vToken address
- Updated pre-processing and processing loops to use `repay_amounts_by_asset` dict

## Key Insight

Aave vToken burn operations emit **both** an ERC20 Transfer event (for ERC20 compatibility) **and** a specialized Burn event (for Aave-specific data like balanceIncrease and index). The processing code must recognize these as a single operation, not two separate operations.

**Two separate issues were fixed:**

1. **Double Counting**: ERC20 Transfer events to address(0) were being processed as BALANCE_TRANSFER operations, adding the unscaled amount to the user's debt balance. Then the Burn event was processed as INTEREST_ACCRUAL, subtracting the scaled amount. This double-counted the burn.

2. **Wrong Asset Key**: The paybackAmount lookup was using vToken address as the key, but the dict was keyed by reserve (underlying) address. This caused the lookup to fail, falling back to reverse-calculated amounts with rounding errors.

**Transaction Context State Management**: When multiple REPAY operations exist in a single transaction (e.g., debt swaps), a single `last_repay_amount` variable gets overwritten. Using a dict keyed by asset address ensures each INTEREST_ACCRUAL operation gets the correct paybackAmount.

## Refactoring

1. **Event Pairing Logic**: Consider creating a unified event pairing system at parse time that recognizes related events and creates single operations:
   - Mint: ERC20 Transfer(from=0) + Mint
   - Burn: ERC20 Transfer(to=0) + Burn  
   - Transfer: ERC20 Transfer + BalanceTransfer

2. **Asset Address Normalization**: Ensure consistent use of vToken vs reserve addresses throughout the codebase. Consider adding type hints or wrapper classes to prevent mixing them up.

3. **Operation Sorting**: The current sorting logic is complex. Consider a more explicit dependency graph approach where operations declare their dependencies (e.g., INTEREST_ACCRUAL depends on REPAY).

4. **Test Coverage**: Add test cases for:
   - Debt swap transactions with multiple burns
   - Multi-asset operations in single transactions
   - Edge cases with 1 wei rounding differences

## Verification

After the fix, the balance calculation should be:

| Step | Operation | Scaled Amount | Position Balance |
|------|-----------|---------------|------------------|
| Initial | - | - | 0 |
| 3 | INTEREST_ACCRUAL | -3,211,989,141 | 3,211,989,141 |
| 1 | BORROW | +3,211,989 | 3,215,201,130 |
| 4 | INTEREST_ACCRUAL | -15,973 | 40,050,934 |
| **Final** | - | - | **40,050,934** |

This matches the on-chain scaled balance exactly.
