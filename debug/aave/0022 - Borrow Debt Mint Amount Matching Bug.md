# Issue 0022: Borrow Debt Mint Amount Matching Bug

**Date:** 2026-03-03

## Symptom

```
AssertionError: User 0xB22e3d2418C2B909C14883F35EA0BDcBA566e9c6: debt balance (2060594563394477418258) does not match scaled token contract (2647567112895583751348) @ 0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE at block 20363588
```

The user's tracked debt balance was approximately 587 scaled units lower than the actual contract balance.

## Root Cause

The `_create_borrow_operation` function in `aave_transaction_operations.py` was matching DEBT_MINT events to BORROW pool events based only on:
1. User address (onBehalfOf)
2. Token contract (matching reserve to debt token)

This worked for simple transactions with one borrow, but failed in **flash loan scenarios** where the same user has multiple debt mints in the same transaction. The function would match the first unassigned debt mint to the borrow, regardless of whether the amounts matched.

### Transaction Analysis

**Transaction:** 0x37416a998da98779737e6c62607defcf9d0a7fbfd38651e54b8c058710eb3992  
**Block:** 20363588

The transaction contained:
- **Log 182:** DEBT_MINT with value=266163817852323386 (small interest accrual)
- **Log 187:** DEBT_MINT with value=614800334026855555114 (flash loan borrow)
- **Log 189:** BORROW event with amount=614800334026855555114

### The Problem

The BORROW at log 189 (amount=614.80 WETH) was incorrectly matched with the DEBT_MINT at log 182 (value=0.266 WETH) because:
1. Both were for the same user
2. Both were for the same token (variableDebtWETH)
3. The mint at log 182 appeared first in the iteration

The correct match should have been:
- BORROW log 189 (amount=614.80) → DEBT_MINT log 187 (value=614.80)
- DEBT_MINT log 182 (value=0.266) → Should be processed as INTEREST_ACCRUAL

## Fix

**File:** `src/degenbot/cli/aave_transaction_operations.py`  
**Function:** `_create_borrow_operation` (lines 904-977)

### Changes

The fix adds amount-based matching to ensure the correct debt mint is paired with each borrow:

1. **Decode borrow amount:** Extract the borrow amount from the BORROW event data
2. **Exact amount matching:** When iterating through DEBT_MINT events, check if `ev.amount == borrow_amount` for an exact match
3. **Fallback mechanism:** If no exact match is found, fall back to the first matching mint to maintain backward compatibility

### Code Changes

```python
# Decode borrow amount from event data
_, borrow_amount, _, _ = decode(
    types=["address", "uint256", "uint8", "uint256"],
    data=borrow_event["data"],
)

# Find debt mint with amount-based matching
debt_mint = None
fallback_mint = None  # Fallback for when exact amount match fails

for ev in scaled_events:
    # ... existing checks ...
    
    if self.debt_token_to_reserve:
        # ... token matching ...
        if reserve_asset and reserve_asset.lower() == reserve.lower():
            # First try exact amount match
            if ev.amount == borrow_amount:
                debt_mint = ev
                break
            # Store first matching mint as fallback
            if fallback_mint is None:
                fallback_mint = ev
    else:
        # Fallback to old behavior
        if ev.amount == borrow_amount:
            debt_mint = ev
            break
        if fallback_mint is None:
            fallback_mint = ev

# Use fallback if no exact match found
if debt_mint is None and fallback_mint is not None:
    debt_mint = fallback_mint
```

## Transaction Details

- **Hash:** 0x37416a998da98779737e6c62607defcf9d0a7fbfd38651e54b8c058710eb3992
- **Block:** 20363588
- **Type:** Flash loan leveraged yield farming
- **User:** 0xB22e3d2418C2B909C14883F35EA0BDcBA566e9c6
- **Asset:** WETH (variable debt)
- **Debt Token:** 0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE (variableDebtWETH)
- **Debt Token Revision:** 5

## Key Insight

When multiple debt mints exist for the same user in a transaction (common in flash loans and complex operations), matching must consider the mint value, not just the user and token. The BORROW event's amount should match the corresponding DEBT_MINT's value for accurate pairing.

## Refactoring Recommendations

1. **Add transaction-level event grouping:** Group related events by transaction before matching to avoid cross-transaction interference
2. **Add more context-aware matching:** Consider the full event sequence when matching (e.g., mints after borrows, burns before repays)
3. **Add validation warnings:** When a fallback match is used (no exact amount match), log a warning for debugging
4. **Add comprehensive test coverage:** Create unit tests for complex scenarios with multiple borrows/repays

## Test

Added test case in `tests/cli/test_aave_transaction_operations.py`:

```python
def test_borrow_matches_debt_mint_by_amount(self):
    """BORROW operation matches DEBT_MINT with same amount.
    
    Regression test for issue #0022.
    When multiple debt mints exist for the same user/token,
    the borrow should match the mint with the same amount.
    """
    user = get_checksum_address("0xB22e3d2418C2B909C14883F35EA0BDcBA566e9c6")
    weth_reserve = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
    variable_debt_weth = get_checksum_address("0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE")
    
    # Small interest accrual mint (log 182 in original tx)
    small_mint = EventFactory.create_debt_mint_event(
        user=user,
        amount=266163817852323386,
        balance_increase=266164750831695501,
        log_index=182,
        contract_address=variable_debt_weth,
    )
    
    # Large flash loan borrow mint (log 187 in original tx)
    large_mint = EventFactory.create_debt_mint_event(
        user=user,
        amount=614800334026855555114,
        balance_increase=0,
        log_index=187,
        contract_address=variable_debt_weth,
    )
    
    # Borrow event (log 189 in original tx)
    borrow_event = EventFactory.create_borrow_event(
        reserve=weth_reserve,
        on_behalf_of=user,
        amount=614800334026855555114,
        log_index=189,
    )
    
    parser = TransactionOperationsParser(
        token_type_mapping={variable_debt_weth: "vToken"},
        debt_token_to_reserve={variable_debt_weth: weth_reserve},
    )
    tx_ops = parser.parse(
        [small_mint, large_mint, borrow_event],
        HexBytes("0x" + "00" * 32),
    )
    
    # Should have 2 operations: BORROW and INTEREST_ACCRUAL
    borrow_ops = [op for op in tx_ops.operations 
                  if op.operation_type == OperationType.BORROW]
    assert len(borrow_ops) == 1
    
    borrow_op = borrow_ops[0]
    # Should have exactly 1 scaled token event
    assert len(borrow_op.scaled_token_events) == 1
    
    # Should match the large mint (same amount as borrow)
    matched_mint = borrow_op.scaled_token_events[0]
    assert matched_mint.amount == 614800334026855555114
    
    # The small mint should be in a separate INTEREST_ACCRUAL operation
    interest_ops = [op for op in tx_ops.operations 
                    if op.operation_type == OperationType.INTEREST_ACCRUAL]
    assert len(interest_ops) == 1
    assert interest_ops[0].scaled_token_events[0].amount == 266163817852323386
```

## References

- Transaction: https://etherscan.io/tx/0x37416a998da98779737e6c62607defcf9d0a7fbfd38651e54b8c058710eb3992
- Aave Pool Contract: 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4e2
- Variable Debt WETH: 0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE
