# MINT_TO_TREASURY BalanceTransfer Amount Not Used

## Issue

When processing MINT_TO_TREASURY operations during liquidations, the code incorrectly calculates the scaled amount to add to the treasury. The Mint event at log 157 has `value == balanceIncrease`, which happens when the treasury's existing balance generated interest equal to the protocol fee. The current formula produces ~6.5M scaled units when it should produce ~0 (or a minimal amount for interest rounding).

## Date

2026-03-16

## Symptom

```
AssertionError: Balance verification failure for AaveV3Asset(...). 
User AaveV3User(...) scaled balance (4946218120415677886783) 
does not match contract balance (4946211624051502561950) at block 23110610

Difference: ~6,496,364 scaled units
```

## Root Cause

**Two-Part Problem:**

### Part 1: Pydantic Validation Error (Fixed)

The `IndexScaledEvent` model declared `scaled_amount: int` as a required field, but MINT_TO_TREASURY operations intentionally set it to `None` during enrichment because the calculation requires position data only available later in processing.

**Fix Applied:**
Changed `scaled_amount: int` to `scaled_amount: int | None` in `src/degenbot/aave/models.py` line 105.

### Part 2: Balance Verification Error (Current Issue)

After fixing the type error, the update now fails with a balance verification error. The MINT_TO_TREASURY calculation is incorrect.

**The Problem:**

Looking at the events in transaction 0x3efed6a8ec156b4aa1ad9b0166fbdccf5e975d8297d847949dd2013595fd194c, there are TWO separate treasury inflows:

1. **Log 159 (LIQUIDATION - BalanceTransfer):** `from=liquidated_user, to=treasury, value=2,121,610,532,076,104 (SCALED), index=1,050,710,055,779,947,702,744,999,399`
   - This is the liquidation protocol fee transferred from the borrower to treasury
   - This is correctly processed as part of the LIQUIDATION operation

2. **Log 157 (MINT_TO_TREASURY - Mint):** `value=6,825,795,165,022,410, balanceIncrease=6,825,795,165,022,410, index=1,050,710,055,779,947,702,744,999,399`
   - This is a separate mint of accumulated `accruedToTreasury` from reserve factor
   - When `value == balanceIncrease`, it means the treasury's existing balance generated interest equal to the protocol fee being minted
   - The actual scaled amount added should be approximately **0** (just the incremental interest from rounding)

**Current Code Behavior:**

The formula in `_process_collateral_mint_with_match()` calculates:
```python
previous_balance = ray_mul_floor(collateral_position.balance, collateral_position.last_index)
next_balance = scaled_event.amount + previous_balance
X = ray_div_ceil(next_balance, scaled_event.index)
scaled_amount = X - collateral_position.balance  # Results in ~6.5M
```

This produces 6,496,364 scaled units because:
- The MINT_TO_TREASURY runs AFTER the LIQUIDATION has already added 2.1M to the treasury
- The formula uses the updated balance (4,946,211,624,051,502,561,950)
- `scaled_event.amount` (6.8M underlying) gets converted using the complex formula
- Result is 6.5M scaled units instead of ~0

**Why the Formula Fails:**

When `value == balanceIncrease` in the Mint event:
- `amount = nextBalance - previousBalance` where both include interest
- The interest component cancels out in the calculation
- But the formula still produces a non-zero result due to using the updated position balance

## Transaction Details

- **Hash:** 0x3efed6a8ec156b4aa1ad9b0166fbdccf5e975d8297d847949dd2013595fd194c
- **Block:** 23110610
- **Type:** LIQUIDATION with MINT_TO_TREASURY
- **User (Liquidated):** 0x68880114a744cfB622862Fa8FDEb174Fd9259fC9
- **Treasury:** 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c
- **Asset:** WETH (aToken: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)
- **Pool Revision:** 9
- **aToken Revision:** 4

**Events in Transaction (aWETH only):**

| Log Index | Event Type | From | To | Amount | Notes |
|-----------|------------|------|-----|--------|-------|
| 153 | Transfer | 0x6888... (liquidated) | 0x0000...0000 | 465,882,104,566,816,967 | Collateral seized |
| 154 | Burn | 0x6888... (liquidated) | - | 465,882,104,566,816,967 | Collateral burned |
| 156 | Transfer | 0x0000...0000 | Treasury | 6,825,795,165,022,410 | Mint (underlying) |
| **157** | **Mint** | **Pool** | **Treasury** | **6,825,795,165,022,410** | **MINT_TO_TREASURY (value == balanceIncrease)** |
| 158 | Transfer | 0x6888... (liquidated) | Treasury | 2,229,197,520,501,007 | ERC20 transfer (underlying) |
| **159** | **BalanceTransfer** | **0x6888...** | **Treasury** | **2,121,610,532,076,104 (SCALED)** | **Liquidation fee - processed by LIQUIDATION** |
| 161 | LiquidationCall | Pool | - | - | Liquidation initiated |

**Treasury Position Flow:**

1. **Before any operations:**
   - Balance: 4,946,209,502,440,970,485,846
   - Index: 1,050,708,675,774,680,709,129,350,058

2. **After LIQUIDATION (log 159):**
   - Balance: 4,946,211,624,051,502,561,950 (+2,121,610,532,076,104 from liquidation fee)
   - Index updated to: 1,050,710,055,779,947,702,744,999,399

3. **After MINT_TO_TREASURY (expected):**
   - Balance: ~4,946,211,624,051,502,561,950 (+~0, just interest rounding)
   - Should remain approximately the same

4. **After MINT_TO_TREASURY (actual - wrong):**
   - Balance: 4,946,218,120,415,677,886,783 (+6,496,364,175,324,834 incorrectly added)

## Investigation Findings

**Contract Analysis:**

From `Pool/rev_9.sol` lines 123-129:
```solidity
uint256 accruedToTreasury = reserve.accruedToTreasury;
if (accruedToTreasury != 0) {
    reserve.accruedToTreasury = 0;
    uint256 normalizedIncome = reserve.getNormalizedIncome();
    uint256 amountToMint = accruedToTreasury.getATokenBalance(normalizedIncome);
    IAToken(reserve.aTokenAddress).mintToTreasury(accruedToTreasury, normalizedIncome);
    emit IPool.MintedToTreasury(assetAddress, amountToMint);
}
```

The Pool passes `accruedToTreasury` (in **scaled** units) directly to the AToken. The AToken's `_mintScaled` function:
1. Emits a Mint event with `amountToMint` (underlying units)
2. When `value == balanceIncrease`, the treasury had an existing balance that accrued interest

**Key Insight:**

When `value == balanceIncrease` in a Mint event during MINT_TO_TREASURY:
- The actual scaled amount added is **NOT** directly available from the Mint event
- The Mint event shows underlying units that include interest on the existing balance
- The correct scaled amount would be `accruedToTreasury` which is stored in the Pool's reserve data (not available from events)
- The scaled amount should be approximately **0** (or a minimal rounding amount)

## Proposed Fix

For MINT_TO_TREASURY operations where `value == balanceIncrease`:
1. **Option A:** Set `scaled_amount = 0` (simplest, most likely correct)
2. **Option B:** Calculate the minimal interest amount: `ray_div_floor(amount, index) - ray_div_floor(amount - small_buffer, index)`
3. **Option C:** Query the Pool's `accruedToTreasury` directly before it was reset (not practical)

**Recommended Approach (Option A):**

When `scaled_event.amount == scaled_event.balance_increase` for MINT_TO_TREASURY:
```python
if operation.operation_type.name == "MINT_TO_TREASURY":
    if scaled_event.amount == scaled_event.balance_increase:
        # When value == balanceIncrease, the treasury's existing balance
        # generated interest equal to the protocol fee. The actual scaled
        # amount added is approximately 0 (just rounding).
        scaled_amount = 0
    else:
        # Use standard formula for other cases
        ...
```

## Refactoring

**Immediate Fix:**
1. Detect when `value == balanceIncrease` in MINT_TO_TREASURY Mint events
2. Set `scaled_amount = 0` in this case (or minimal rounding amount)
3. Document why this special case exists

**Long-term Improvements:**
1. **Contract Integration:** Consider querying the Pool's reserve data to get the actual `accruedToTreasury` value before processing
2. **Documentation:** Add detailed comments explaining the MINT_TO_TREASURY flow and why `value == balanceIncrease` means ~0 scaled amount
3. **Test Coverage:** Add test cases for MINT_TO_TREASURY with `value == balanceIncrease`

## Verification

After applying the fix:
1. Run `uv run degenbot aave update --chunk 1`
2. Transaction 0x3efed6a8ec156b4aa1ad9b0166fbdccf5e975d8297d847949dd2013595fd194c should process successfully
3. Treasury balance after MINT_TO_TREASURY should remain approximately 4,946,211,624,051,502,561,950
4. Balance verification should pass with no discrepancy

## Resolution

**Fix Applied:**

Modified `src/degenbot/cli/aave.py:2682-2714` to handle the special case when `value == balanceIncrease`:

```python
# Special case: when amount == balanceIncrease, the treasury's existing
# aTokens accrued interest equal to the accruedToTreasury amount.
# No new scaled tokens are minted - the existing balance simply appreciated.
if scaled_event.amount == scaled_event.balance_increase:
    scaled_amount = 0
    logger.debug(f"MINT_TO_TREASURY: amount == balanceIncrease, setting scaled_amount = 0")
else:
    # Use standard formula for other cases
    ...
```

**Verification:**

✅ Fix tested successfully on 2026-03-16
- Run: `uv run degenbot aave update --chunk 1`
- Result: Update completed successfully to block 23,110,610
- Treasury balance verification passed
- No balance discrepancy reported

## References

- Issue 0014: MINT_TO_TREASURY AccruedToTreasury Calculation Error
- Contract: `contract_reference/aave/Pool/rev_9.sol:109-134` (mintToTreasury)
- Contract: `contract_reference/aave/AToken/rev_4.sol:2782-2805` (_mintScaled)
- Transaction: `0x3efed6a8ec156b4aa1ad9b0166fbdccf5e975d8297d847949dd2013595fd194c`
- Files:
  - `src/degenbot/aave/models.py` (line 105 - type fix, validator order)
  - `src/degenbot/cli/aave_transaction_operations.py` (operation creation)
  - `src/degenbot/cli/aave.py` (MINT_TO_TREASURY processing - fix applied at lines 2682-2714)
