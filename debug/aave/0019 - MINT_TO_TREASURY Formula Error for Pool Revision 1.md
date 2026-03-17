# MINT_TO_TREASURY Formula Error for Pool Revision 1

## Issue

Balance verification failure when processing MINT_TO_TREASURY operations on Pool Revision 1. The calculation formula used was incorrect for Pool revisions 1-3, producing wrong scaled amounts.

## Date

2026-03-16

## Symptom

```
AssertionError: Balance verification failure for AaveV3Asset(...). 
User AaveV3User(...) scaled balance (2334868167681700143) 
does not match contract balance (2335013626111045582) at block 16594588

Difference: ~145 trillion wei (145458429345439)
```

## Root Cause

The `_calculate_mint_to_treasury_scaled_amount` function was using the same complex formula for all pool revisions, but Pool Revision 1-3 requires a much simpler formula.

### The Problem

**Original Code (wrong for Rev 1-3):**
```python
previous_balance = ray_mul_floor(
    collateral_position.balance,
    collateral_position.last_index or 0,
)
next_balance = scaled_event.amount + previous_balance
X = ray_div(next_balance, scaled_event.index)
scaled_amount = X - collateral_position.balance
```

This formula was designed for Pool Revision 4+ to reverse the contract's floor rounding (see Issue 0014). However, Pool Revision 1-3 uses standard half-up rounding in `rayMul` and `rayDiv`, so the formula produces incorrect results.

### Contract Behavior (Pool Rev 1)

In AToken Revision 1 (`contract_reference/aave/AToken/rev_1.sol:2756-2778`):
```solidity
function _mintScaled(...) internal returns (bool) {
    uint256 amountScaled = amount.rayDiv(index);  // Standard half-up rounding
    
    uint256 scaledBalance = super.balanceOf(onBehalfOf);
    uint256 balanceIncrease = scaledBalance.rayMul(index) -  // Half-up
        scaledBalance.rayMul(_userState[onBehalfOf].additionalData);  // Half-up
    
    _userState[onBehalfOf].additionalData = index.toUint128();
    _mint(onBehalfOf, amountScaled.toUint128());
    
    uint256 amountToMint = amount + balanceIncrease;
    emit Mint(caller, onBehalfOf, amountToMint, balanceIncrease, index);
    
    return (scaledBalance == 0);
}
```

And in Pool Revision 1 (`contract_reference/aave/Pool/rev_1.sol:3931-3956`):
```solidity
function executeMintToTreasury(...) external {
    uint256 accruedToTreasury = reserve.accruedToTreasury;
    if (accruedToTreasury != 0) {
        reserve.accruedToTreasury = 0;
        uint256 normalizedIncome = reserve.getNormalizedIncome();
        uint256 amountToMint = accruedToTreasury.rayMul(normalizedIncome);  // Half-up
        IAToken(reserve.aTokenAddress).mintToTreasury(amountToMint, normalizedIncome);
        emit MintedToTreasury(assetAddress, amountToMint);
    }
}
```

### Why the Formulas Differ

**Pool Revision 1-3 (Simple Formula):**
- All calculations use half-up rounding (standard `rayMul` and `rayDiv`)
- No TokenMath library exists
- The calculation is straightforward:
  1. `balance_increase = rayMul(balance, index) - rayMul(balance, last_index)`
  2. `amount_from_pool = Mint.amount - balance_increase`
  3. `scaled_amount = rayDiv(amount_from_pool, index)`

**Pool Revision 4+ (Complex Formula):**
- TokenMath library introduces floor/ceil rounding for protocol safety
- Collateral mints use floor rounding (never mint more than supplied)
- The complex formula reverses the floor division using ceiling rounding
- See Issue 0014 for detailed explanation

### Calculation Verification

For the failing transaction at block 16594588:

**Original (Wrong) Formula:**
```
previous_balance = ray_mul_floor(317517235048390953, 1000118049507356325074809392) = 317554717801565558
next_balance = 2018685674484120043 + 317554717801565558 = 2336240392285685601
X = ray_div(2336240392285685601, 1000587709671569142671842629) = 2334868167681700143
scaled_amount = 2334868167681700143 - 317517235048390953 = 2017350932633309190
```
Result: Off by 145,458,429,345,439 wei ❌

**Correct Formula (Rev 1-3):**
```
balance_increase = ray_mul(317517235048390953, 1000587709671569142671842629) - ray_mul(317517235048390953, 1000118049507356325074809392) = 3581280082051
principal = 2018685674484120043 - 3581280082051 = 2018682093204037992
scaled_amount = ray_div(2018682093204037992, 1000587709671569142671842629) = 2017496391062654629
```
Result: Matches contract exactly ✅

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | `0xf23b599f4960504bfb13c1294c6b48389a838609d100f77b5dfbe9f00d770f2e` |
| **Block** | 16594588 |
| **Type** | MINT_TO_TREASURY |
| **User** | 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c (Treasury) |
| **Asset** | WETH (aToken: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8) |
| **Pool Revision** | 1 |
| **aToken Revision** | 1 |

**Events in Transaction:**
- 7 MINTED_TO_TREASURY operations for different assets
- 7 Mint events (aToken mints)
- 7 Transfer events (ERC20 transfers)

The failure occurred on multiple assets in this transaction.

## Fix

**File:** `src/degenbot/cli/aave.py`

**Changes applied:**

### 1. Import `ray_mul` (line 32)
```python
# Before:
from degenbot.aave.libraries.wad_ray_math import ray_div, ray_div_ceil, ray_mul_floor, wad_mul

# After:
from degenbot.aave.libraries.wad_ray_math import ray_div, ray_div_ceil, ray_mul, ray_mul_floor, wad_mul
```

### 2. Update `_calculate_mint_to_treasury_scaled_amount` function (lines 2693-2727)
```python
# Use appropriate calculation based on pool revision
# - Rev 1-3: Simple formula - the contract calculates
#   balance_increase = scaled_balance * index - scaled_balance * last_index
#   amount (from Pool) = Mint.amount - balance_increase
#   scaled_amount = amount / index (rayDiv with half-up rounding)
# - Rev 4+: Complex formula to reverse floor rounding (see Issue 0014)
logger.debug("MINT_TO_TREASURY: Using formula calculation")
if pool_revision <= 3:
    # Simple formula for Pool Rev 1-3 (see Issue 0019)
    balance_increase = ray_mul(
        collateral_position.balance,
        scaled_event.index,
    ) - ray_mul(
        collateral_position.balance,
        collateral_position.last_index or 0,
    )
    principal = scaled_event.amount - balance_increase
    scaled_amount = ray_div(principal, scaled_event.index)
    logger.debug(f"MINT_TO_TREASURY (Rev 1-3): balance_increase={balance_increase}")
    logger.debug(f"MINT_TO_TREASURY (Rev 1-3): principal={principal}")
    logger.debug(f"MINT_TO_TREASURY (Rev 1-3): scaled_amount={scaled_amount}")
    return scaled_amount

# Complex formula for Rev 4+
previous_balance = ray_mul_floor(
    collateral_position.balance,
    collateral_position.last_index or 0,
)
next_balance = scaled_event.amount + previous_balance
X = ray_div_ceil(next_balance, scaled_event.index)
scaled_amount = X - collateral_position.balance
logger.debug(f"MINT_TO_TREASURY (Rev 4+): previous_balance={previous_balance}")
logger.debug(f"MINT_TO_TREASURY (Rev 4+): next_balance={next_balance}")
logger.debug(f"MINT_TO_TREASURY (Rev 4+): X={X}")
logger.debug(f"MINT_TO_TREASURY (Rev 4+): scaled_amount={scaled_amount}")
return scaled_amount
```

**Rationale:**
1. Pool Revision 1-3 uses standard half-up rounding for all operations
2. Pool Revision 4+ introduced the TokenMath library with floor/ceil rounding
3. Each revision requires a different formula to correctly calculate the inverse

## Verification

### Before Fix (Block 16594588)
```
WETH:
- Our calculation: 2017350932633309190 scaled units
- Expected:       2017496391062654629 scaled units
- Difference:     145458429345439 scaled units (~$0.23 worth of aWETH)
```

### After Fix (Block 16594588)
```
WETH:
- Our calculation: 2017496391062654629 scaled units
- Expected:       2017496391062654629 scaled units
- Difference:     0

All 7 assets in the mintToTreasury transaction:
- Calculated balance matches contract balance exactly
- No balance verification errors
```

**Test Results:**
```bash
$ uv run degenbot aave update --chunk 1
Updating Aave Ethereum Market (chain 1): block range 16,594,588 - 16,594,588
...
MINT_TO_TREASURY (Rev 1-3): balance_increase=3581280082051
MINT_TO_TREASURY (Rev 1-3): principal=2018682093204037992
MINT_TO_TREASURY (Rev 1-3): scaled_amount=2017496391062654629
...
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) successfully updated to block 16,594,588
```

✅ Block 16594588 processed successfully with no balance verification errors.

## Key Insight

**Different pool revisions require different calculation formulas.**

The MINT_TO_TREASURY formula was originally developed for Pool Revision 4+ where:
1. The TokenMath library explicitly uses floor rounding for collateral mints
2. The formula uses complex inversion to reverse the floor division

For Pool Revision 1-3:
1. The AToken contract uses standard half-up rounding throughout
2. The calculation is straightforward: `(Mint.amount - balance_increase) / index`
3. Using the Rev 4+ formula produces catastrophically wrong results

**Lesson:** When implementing inverse calculations for contract operations, always verify that both the formula structure AND rounding modes match the contract's actual implementation for the specific revision being processed.

## Refactoring

**Immediate Fix:**
1. Add pool revision check to determine which formula to use
2. Use simple formula for pool revisions 1-3: `(Mint.amount - balance_increase) / index`
3. Keep complex formula for pool revisions 4+

**Long-term Improvements:**
1. **TokenMathFactory integration:** Consider using TokenMathFactory to get the appropriate math implementation for each pool revision
2. **Documentation:** Add detailed comments explaining why different revisions use different formulas
3. **Test Coverage:** Add test cases for MINT_TO_TREASURY on different pool revisions

## Related Issues

- Issue 0014: MINT_TO_TREASURY AccruedToTreasury Calculation Error
- Issue 0015: MINT_TO_TREASURY BalanceTransfer Amount Not Used
- Issue 0017: MINT_TO_TREASURY Validation Error on Pool Revision 8

## References

- Contract: `contract_reference/aave/Pool/rev_1.sol` (executeMintToTreasury, lines 3931-3956)
- Contract: `contract_reference/aave/AToken/rev_1.sol` (_mintScaled, lines 2756-2778)
- Transaction: `0xf23b599f4960504bfb13c1294c6b48389a838609d100f77b5dfbe9f00d770f2e` (Block 16594588)
- Files:
  - `src/degenbot/cli/aave.py` (MINT_TO_TREASURY calculation logic, lines 32, 2693-2727)
  - `src/degenbot/aave/libraries/wad_ray_math.py` (ray_div, ray_mul implementations)
