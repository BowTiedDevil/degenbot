# Issue: Collateral Burn Amount Mismatch in REPAY_WITH_ATOKENS

## Issue ID
**0022**

## Date
2026-03-17

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (5518783115662313161) does not match contract balance (1) at block 20233465
```

**Balance Difference:** 5,518,783,115,662,313,160 wei (calculated balance is ~5.52 ETH higher than contract balance)

**Log Output:**
```
WITHDRAW: Looking for collateral burn for user=0x7F6f31e0603a459CC740E8e5a6F79dbb3304354b, amount=5667612229340587428
WITHDRAW: Total scaled events: 12
WITHDRAW: Assigned indices: {321, 325, 328, 331, 332, 337, 338, 341}
```

## Root Cause
The `_find_matching_collateral_burn` function returned the first unassigned COLLATERAL_BURN event for a user without validating that the burn amount matched the expected operation amount. This caused a collateral burn intended for a WITHDRAW operation to be incorrectly assigned to a REPAY_WITH_ATOKENS operation.

### Transaction Analysis

**Transaction:** `0x87c148f1379489e24b38a86aed0b4fab5409c5f8859622aa4899feb51fd497e5`

**Events in order:**
1. **Burn** (logIndex=321) - 9.61 USDC worth of aETH (for first REPAY_WITH_ATOKENS)
2. **REPAY_WITH_ATOKENS** (logIndex=325) - 9616261612 USDC
3. **BalanceTransfer** (logIndex=328) - 9.61 USDC worth of aETH
4. **Burn** (logIndex=331) - Additional collateral burn (interest component)
5. **REPAY_WITH_ATOKENS** (logIndex=332) - Another repayment
6. **Mint** (logIndex=337) - Interest accrual for collateral position
7. **Burn** (logIndex=338) - Debt burn
8. **REPAY** (logIndex=341) - Standard repayment
9. **Burn** (logIndex=348) - **5.67 ETH collateral burn** (INTENDED FOR WITHDRAW)
10. **WITHDRAW** (logIndex=350) - **5.67 ETH withdrawal** (MISSING BURN EVENT)

### The Problem

When `_create_repay_with_atokens_operation` processed the second REPAY_WITH_ATOKENS at logIndex=332:
- It called `_find_matching_collateral_burn(user=user, ...)` 
- The function found the unassigned burn at logIndex=348 (5.67 ETH) 
- **Without amount validation, this burn was incorrectly assigned to the REPAY_WITH_ATOKENS**
- The subsequent WITHDRAW at logIndex=350 found no matching burn (all burns already assigned)
- Result: The WITHDRAW operation had no collateral burn, so the balance wasn't reduced

**Key Insight:** The burn at logIndex=348 had `amount + balance_increase = 5667612229340587428`, which exactly matched the withdraw amount at logIndex=350 of `5667612229340587428`. This should have been matched to the WITHDRAW, not the REPAY_WITH_ATOKENS.

## Fix

### Location
`src/degenbot/cli/aave_transaction_operations.py`

### Changes

1. **Updated `_find_matching_collateral_burn` signature (lines 1718-1765):**
   - Added `expected_amount: int` parameter
   - Added `pool_revision: int` parameter
   - Added validation to check if `ev.amount + ev.balance_increase` matches expected amount
   - For pool revision 9+, allows ±2 wei tolerance due to ray math flooring

```python
@staticmethod
def _find_matching_collateral_burn(
    *,
    user: ChecksumAddress,
    expected_amount: int,
    scaled_events: list[ScaledTokenEvent],
    assigned_indices: set[int],
    pool_revision: int,
) -> ScaledTokenEvent | None:
    """
    Find the closest matching collateral burn event.

    Matches based on user address and burn amount (amount + balance_increase).
    For pool revision 9+, allows ±2 wei tolerance due to ray math rounding.
    """

    for ev in scaled_events:
        if ev.event["logIndex"] in assigned_indices:
            continue
        if ev.event_type != ScaledTokenEventType.COLLATERAL_BURN:
            continue
        if ev.user_address != user:
            continue
        if ev.index is None:
            continue
        if ev.balance_increase is None:
            continue

        # Calculate the total burn amount (principal + interest)
        total_burn = ev.amount + ev.balance_increase

        # Pool revision 9+ uses ray math with flooring, allow ±2 wei tolerance
        if pool_revision >= 9:
            if abs(total_burn - expected_amount) > 2:
                continue
        elif total_burn != expected_amount:
            continue

        return ev

    return None
```

2. **Updated caller in `_create_repay_with_atokens_operation` (line 1581):**
   - Added `expected_amount=repay_amount` parameter
   - Added `pool_revision=pool_revision` parameter

```python
collateral_burn_event = self._find_matching_collateral_burn(
    user=user,
    expected_amount=repay_amount,
    scaled_events=scaled_events,
    assigned_indices=assigned_indices,
    pool_revision=pool_revision,
)
```

### Why This Works

By requiring the burn amount to match the operation amount:
- The REPAY_WITH_ATOKENS at logIndex=332 (repay amount ~9.61 USDC worth) no longer matches the burn at logIndex=348 (5.67 ETH)
- The burn at logIndex=348 remains unassigned after REPAY_WITH_ATOKENS processing
- When the WITHDRAW at logIndex=350 is processed, it correctly matches the burn at logIndex=348 (both ~5.67 ETH)
- The balance is correctly reduced by the withdrawal amount

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x87c148f1379489e24b38a86aed0b4fab5409c5f8859622aa4899feb51fd497e5 |
| **Block Number** | 20233465 |
| **Chain** | Ethereum Mainnet |
| **User** | 0x7F6f31e0603a459CC740E8e5a6F79dbb3304354b |
| **Asset** | WETH (0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2) |
| **aToken** | aWETH (0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8) |
| **Pool** | 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 |
| **Pool Revision** | 3 |
| **aToken Revision** | 1 |

## Related Issues

- Issue #0012: V4 Withdraw Emits Mint When Interest Exceeds Withdrawal - Similar WITHDRAW operation handling
- Issue #0007: Interest Accrual Burn Amount Zeroed in Enrichment - Related burn event processing

## Files Involved

- `src/degenbot/cli/aave_transaction_operations.py` - Lines 1718-1765 (_find_matching_collateral_burn), line 1581 (caller)

## Verification Steps

After implementing fix:
```bash
# Run specific block range
uv run degenbot aave update --chunk 1

# Verify balance at block 20233465
uv run python3 -c "
from web3 import Web3
w3 = Web3(Web3.HTTPProvider('http://node:8545'))
# Check scaled balance
result = w3.eth.call({
    'to': '0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8',
    'data': '0x1da8d3680000000000000000000000007f6f31e0603a459cc740e8e5a6f79dbb3304354b',
}, block_identifier=20233465)
print('Scaled balance:', int(result.hex(), 16))
"
```

## Notes

- This issue occurs when a transaction contains multiple operations for the same user
- The burn amount matching is critical for correct operation-to-event assignment
- Pool revision 3 (used in this transaction) uses standard ray math without flooring, so exact matching is required
- Pool revision 9+ requires ±2 wei tolerance due to ray math flooring in the Pool contract
