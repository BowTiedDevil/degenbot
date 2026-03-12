# Issue: Interest Accrual Scaling Error in Enrichment

## Date
2026-03-12

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (10999837971837729360) does not match contract balance (10999837804590552304) at block 16498211
```

## Root Cause
The enrichment code in `degenbot/aave/enrichment.py` incorrectly handles INTEREST_ACCRUAL operations by treating the Mint event's `amount` field (which represents interest in underlying units) as the scaled amount directly. After investigating the Aave V3 aToken contract source code (rev_1), the Mint event for interest accrual is emitted for tracking purposes only and does NOT actually mint tokens or change the user's scaled balance.

### Technical Details

**Aave V3 Interest Accrual Mechanism:**

In the aToken contract's `_transfer` function (rev_1.sol:2825-2855):
```solidity
function _transfer(address sender, address recipient, uint256 amount, uint256 index) internal {
    // Calculate interest accrued since last interaction
    uint256 senderBalanceIncrease = senderScaledBalance.rayMul(index) -
        senderScaledBalance.rayMul(_userState[sender].additionalData);
    
    // Update the user's stored index (NOT their balance!)
    _userState[sender].additionalData = index.toUint128();
    _userState[recipient].additionalData = index.toUint128();
    
    // Perform the actual transfer of scaled balance
    super._transfer(sender, recipient, amount.rayDiv(index).toUint128());
    
    // Emit Mint event for tracking (NO actual mint occurs!)
    if (senderBalanceIncrease > 0) {
        emit Mint(_msgSender(), sender, senderBalanceIncrease, senderBalanceIncrease, index);
    }
}
```

**Key Insight:**
- Interest accrual only **updates the stored index** (`_userState[user].additionalData`)
- The Mint event is emitted for tracking/notification purposes only
- The user's **scaled balance does NOT change** from interest alone
- Interest is "virtual" - it exists in the event but doesn't mint new tokens

**The Problem:**
- The enrichment code treats INTEREST_ACCRUAL Mint events as actual mints that increase the scaled balance
- For this transaction:
  - Interest amount: 167,247,177,056 wei (underlying)
  - This was incorrectly added to the scaled balance
  - Scaled balance should NOT have increased from this event

**Impact:**
- The user's calculated balance was inflated by 167,247,177,056 (the interest amount in underlying units)
- The on-chain scaled balance does NOT include this interest as a balance increase
- This caused the balance verification to fail

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x4111ba18b284d459bceb74b7dc9a0ed7a56c02a612c06eb27271d8a52cc99cd7 |
| **Block Number** | 16498211 |
| **Chain** | Ethereum Mainnet |
| **Failing User** | 0xEB52371F5a251E93799A64844A88d3F56872AD00 |
| **Asset** | WETH (aToken: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8) |
| **Token Revision** | 1 |
| **Interest Amount** | 167,247,177,056 wei (0.000000167 WETH) |
| **Liquidity Index** | 1000014760459154860801583308 |

### Transaction Flow

1. **Before Transaction:** User balance = 19,999,704,962,418,970,004 scaled wei
2. **Operation 2 (INTEREST_ACCRUAL):** Interest accrual mint
   - LogIndex: 80
   - Event: `Mint(user=0xEB52371F5a251E93799A64844A88d3F56872AD00, amount=167247177056, balanceIncrease=167247177056, index=1000014760459154860801583308)`
   - **Bug:** Scaled amount calculated as 167247177056 instead of 167244708447
3. **Operation 3 (BALANCE_TRANSFER):** aToken transfer to Paraswap
   - LogIndex: 81 (transfer), 83 (BalanceTransfer)
   - Amount: 8,999,867,157,828,417,700 scaled wei
4. **After Transaction:** 
   - Calculated balance: 10,999,837,971,837,729,360 (incorrect)
   - Contract balance: 10,999,837,804,590,552,304 (correct)
   - Difference: 167,247,177,056 (the un-scaled interest amount)

## Fix

**File:** `src/degenbot/aave/enrichment.py`
**Lines:** 83-90

**Current Code:**
```python
if operation.operation_type.name == "INTEREST_ACCRUAL":
    # Interest accrual events don't have pool events and don't follow
    # standard TokenMath patterns. The event amount is the scaled amount
    # that was minted/burned due to interest accrual.
    # Use amount directly without validation since there's no Pool calculation to verify.
    raw_amount = scaled_event.amount
    scaled_amount = scaled_event.amount
```

**Corrected Understanding:**
The Mint event for interest accrual is emitted for **tracking purposes only**. The interest does NOT increase the user's scaled balance - it only updates the stored index. Therefore, the correct scaled_amount should be **0** (no balance change).

**Fixed Code:**
```python
if operation.operation_type.name == "INTEREST_ACCRUAL":
    # Interest accrual events don't have pool events.
    # The Mint event for interest accrual is emitted for tracking purposes only.
    # Interest accrual does NOT mint tokens or increase the scaled balance - 
    # it only updates the user's stored index.
    # See Aave V3 aToken contract _transfer function (rev_1.sol:2844-2846)
    raw_amount = scaled_event.amount
    scaled_amount = 0  # Interest accrual does not change scaled balance
```

## Key Insight

**The Fundamental Issue:** Aave V3 interest accrual works differently than expected:

1. **Interest is calculated** when a user interacts with their position (transfer, withdraw, etc.)
2. **The stored index is updated** (`_userState[user].additionalData = index`)
3. **A Mint event is emitted** for tracking purposes (lines 2844-2846 in rev_1.sol)
4. **The scaled balance does NOT change** - no tokens are actually minted

**The Mint Event:**
```solidity
emit Mint(_msgSender(), sender, senderBalanceIncrease, senderBalanceIncrease, index);
```
- `amount` = interest in underlying units (for tracking)
- `balanceIncrease` = same as amount (for tracking)
- **NO** call to `_mint()` - balance does not change

**The Real Balance Change:**
The user's "effective" balance increases because:
- `balance = scaledBalance * index / RAY`
- When `index` increases (interest accrual), the effective balance increases
- But `scaledBalance` remains constant

**Lesson Learned:** Read the contract source code! The Mint event for interest accrual is informational only and does not represent an actual token mint.

## Refactoring

1. **Standardize Interest Accrual Handling:** Create a dedicated processor for INTEREST_ACCRUAL operations that correctly converts underlying amounts to scaled amounts using the appropriate index.

2. **Add Unit Validation:** Add validation in the enrichment layer to verify that amounts are within expected ranges for scaled vs underlying units. Scaled amounts are typically much smaller than underlying amounts when the index > 1.

3. **Improve Documentation:** Add clear documentation distinguishing between:
   - `raw_amount`: The amount from the Pool event (underlying units)
   - `scaled_amount`: The amount divided by the index (scaled units)
   - `event.amount`: The raw event field value (context-dependent)

4. **Test Coverage:** Add test cases for:
   - Pure interest accrual without deposits/withdrawals
   - Interest accrual combined with transfers
   - Edge cases with very small interest amounts
   - Cross-verification with on-chain balances

5. **Consider Refactoring Enrichment Logic:** The current enrichment code has special cases for INTEREST_ACCRUAL that bypass standard TokenMath calculations. Consider unifying the logic so that all operations use the same scaling calculations.

## Verification

**Status:** ✅ FIXED

After applying the fix:
```bash
uv run degenbot aave update --chunk 1
```

**Result:**
```
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) successfully updated to block 16,498,212
```

The transaction at block 16498211 now processes without errors, and the balance verification passes.

**Balance reconciliation:**
- Starting balance: 19,999,704,962,418,970,004 scaled wei
- Transfer deducted: 8,999,867,157,828,417,700 scaled wei  
- Interest accrual: 0 scaled wei (Mint event is tracking-only)
- Final balance: 10,999,837,804,590,552,304 scaled wei ✓
- Matches on-chain balance exactly ✓

**Key Fix:**
The critical change was setting `scaled_amount = 0` for INTEREST_ACCRUAL operations, because the Mint event emitted during interest accrual is for **tracking purposes only** and does not actually mint tokens or change the scaled balance.
