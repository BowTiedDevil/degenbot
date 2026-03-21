# MINT_TO_TREASURY Rounding Mode Mismatch for Pool Revision 1

## Issue

Balance verification failure when processing MINT_TO_TREASURY operations on Pool Revision 1. The calculated scaled balance is off by 1 wei due to using ceiling rounding when the contract uses half-up rounding.

## Date

2026-03-16

## Symptom

```
AssertionError: Balance verification failure for AaveV3Asset(...). 
User AaveV3User(...) scaled balance (64738475420603640) 
does not match contract balance (64738475420603639) at block 16516952

Difference: 1 wei
```

## Root Cause

The `_calculate_mint_to_treasury_scaled_amount` function always uses `ray_div_ceil` for the calculation, but this is only correct for Pool revisions 4+. Pool Revision 1 uses standard half-up rounding (equivalent to `ray_div`).

### The Problem

**Current Code (line 2694):**
```python
# Standard formula for other cases (Rev 9+)
logger.debug("MINT_TO_TREASURY: Using formula calculation")
previous_balance = ray_mul_floor(
    collateral_position.balance,
    collateral_position.last_index or 0,
)
next_balance = scaled_event.amount + previous_balance
X = ray_div_ceil(next_balance, scaled_event.index)  # <-- Always uses CEIL
scaled_amount = X - collateral_position.balance
```

**Contract Behavior:**

In AToken Revision 1 (`contract_reference/aave/AToken/rev_1.sol:2762`):
```solidity
function _mintScaled(...) internal returns (bool) {
    uint256 amountScaled = amount.rayDiv(index);  // Standard half-up rounding
    // ...
}
```

The `rayDiv` function in `WadRayMath` library:
```solidity
function rayDiv(uint256 a, uint256 b) internal pure returns (uint256 c) {
    assembly {
        c := div(add(mul(a, RAY), div(b, 2)), b)  // (a * RAY + b/2) / b = half-up
    }
}
```

### Calculation Verification

For the failing transaction:
- `next_balance = 64746517106584784`
- `index = 1000124218031532223928748283`

Using different rounding modes:
```python
ray_div_ceil(next_balance, index)  = 64738475420603640  # Our calculation (WRONG)
ray_div(next_balance, index)       = 64738475420603639  # Contract behavior (CORRECT)
ray_div_floor(next_balance, index) = 64738475420603639  # Would also work
```

**Why the formula uses different rounding for different pool revisions:**

- **Pool Revision 1-3:** Uses standard `rayDiv` (half-up rounding) in the AToken contract. No TokenMath library exists yet.
- **Pool Revision 4+:** Introduced explicit floor/ceil rounding in TokenMath library for protocol safety.

The MINT_TO_TREASURY formula was developed and tested for Pool Revision 4+ (where ceiling rounding is correct), but fails for earlier revisions where the contract uses half-up rounding.

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | `0xb718b71af633e582d9324740c1ed97f32d40712d77cfeafa27778542eb2c507a` |
| **Block** | 16516952 |
| **Type** | MINT_TO_TREASURY |
| **User** | 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c (Treasury) |
| **Asset** | WETH (aToken: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8) |
| **Pool Revision** | 1 |
| **aToken Revision** | 1 |

**Events in Transaction:**
- 5 MINTED_TO_TREASURY operations for different assets
- 5 Mint events (aToken mints)
- 5 Transfer events (ERC20 transfers)

The failure occurred specifically on the WETH (aWETH) mint operation #2.

## Investigation Findings

### Contract Analysis

**Pool Logic (`contract_reference/aave/Pool/rev_1.sol:3931-3956`):**
```solidity
function executeMintToTreasury(...) external {
    for (uint256 i = 0; i < assets.length; i++) {
        // ...
        uint256 accruedToTreasury = reserve.accruedToTreasury;
        if (accruedToTreasury != 0) {
            reserve.accruedToTreasury = 0;
            uint256 normalizedIncome = reserve.getNormalizedIncome();
            uint256 amountToMint = accruedToTreasury.rayMul(normalizedIncome);
            IAToken(reserve.aTokenAddress).mintToTreasury(amountToMint, normalizedIncome);
            emit MintedToTreasury(assetAddress, amountToMint);
        }
    }
}
```

**AToken Logic (`contract_reference/aave/AToken/rev_1.sol:183-188, 2756-2778`):**
```solidity
function mintToTreasury(uint256 amount, uint256 index) external virtual override onlyPool {
    if (amount == 0) { return; }
    _mintScaled(address(POOL), _treasury, amount, index);
}

function _mintScaled(...) internal returns (bool) {
    uint256 amountScaled = amount.rayDiv(index);  // <-- Half-up rounding
    require(amountScaled != 0, Errors.INVALID_MINT_AMOUNT);
    
    uint256 scaledBalance = super.balanceOf(onBehalfOf);
    uint256 balanceIncrease = scaledBalance.rayMul(index) -
        scaledBalance.rayMul(_userState[onBehalfOf].additionalData);
    
    _userState[onBehalfOf].additionalData = index.toUint128();
    _mint(onBehalfOf, amountScaled.toUint128());
    
    uint256 amountToMint = amount + balanceIncrease;
    emit Transfer(address(0), onBehalfOf, amountToMint);
    emit Mint(caller, onBehalfOf, amountToMint, balanceIncrease, index);
    
    return (scaledBalance == 0);
}
```

### Key Insight

**The MINT_TO_TREASURY formula's rounding mode must match the AToken contract's `rayDiv` implementation:**

- **Pool Rev 1-3:** AToken uses `rayDiv` (half-up) → Formula should use `ray_div` (half-up)
- **Pool Rev 4+:** AToken uses `rayDivFloor` (floor) → Formula should use `ray_div_ceil` to reverse

The formula inverts the contract's floor rounding by using ceiling rounding. This works for Rev 4+ because:
- Contract: `scaled = floor(amount * RAY / index)`
- Formula: `X = ceil(next_balance * RAY / index)`

But for Rev 1-3, the contract uses half-up rounding, so the formula should also use half-up rounding to maintain mathematical consistency.

## Fix

**File:** `src/degenbot/cli/aave.py`

**Changes applied:**

### 1. Import `ray_div` (line 32)
```python
# Before:
from degenbot.aave.libraries.wad_ray_math import ray_div_ceil, ray_mul_floor, wad_mul

# After:
from degenbot.aave.libraries.wad_ray_math import ray_div, ray_div_ceil, ray_mul_floor, wad_mul
```

### 2. Update `_calculate_mint_to_treasury_scaled_amount` function (lines 2687-2709)
```python
# Standard formula for other cases (Rev 4+)
logger.debug("MINT_TO_TREASURY: Using formula calculation")
previous_balance = ray_mul_floor(
    collateral_position.balance,
    collateral_position.last_index or 0,
)
next_balance = scaled_event.amount + previous_balance

# Use appropriate rounding based on pool revision
# - Rev 1-3: Half-up rounding (standard ray_div)
# - Rev 4+: Ceiling rounding (ray_div_ceil) to reverse contract's floor rounding
if pool_revision <= 3:
    X = ray_div(next_balance, scaled_event.index)
else:
    X = ray_div_ceil(next_balance, scaled_event.index)

scaled_amount = X - collateral_position.balance
logger.debug(f"MINT_TO_TREASURY: previous_balance={previous_balance}")
logger.debug(f"MINT_TO_TREASURY: next_balance={next_balance}")
logger.debug(f"MINT_TO_TREASURY: X={X}")
logger.debug(f"MINT_TO_TREASURY: scaled_amount={scaled_amount}")
return scaled_amount
```

**Rationale:**
1. Pool Revision 1-3 uses standard `rayDiv` (half-up) in the AToken contract
2. The MINT_TO_TREASURY formula must use matching rounding to calculate the inverse correctly
3. Pool Revision 4+ introduced floor rounding in the TokenMath library, so the formula uses ceiling rounding to reverse it

## Verification

### Before Fix
```
WETH:
- Our calculation: 64738475420603640
- Expected:       64738475420603639
- Difference:     1 wei
```

### After Fix
```
WETH:
- Our calculation: 64738475420603639
- Expected:       64738475420603639
- Difference:     0 wei
```

**Test Results:**
```bash
$ uv run degenbot aave update --chunk 1
Updating Aave Ethereum Market (chain 1): block range 16,516,952 - 16,516,952
...
MINT_TO_TREASURY: Using formula calculation
MINT_TO_TREASURY: previous_balance=0
MINT_TO_TREASURY: next_balance=64746517106584784
MINT_TO_TREASURY: X=64738475420603639
MINT_TO_TREASURY: scaled_amount=64738475420603639
...
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) successfully updated to block 16,516,952
```

✅ Block 16516952 processed successfully with no balance verification errors.

## Key Insight

**Rounding modes must match between formula and contract implementation.**

The MINT_TO_TREASURY formula was originally developed for Pool Revision 4+ where:
1. The TokenMath library explicitly uses floor rounding for collateral mints
2. The formula uses ceiling rounding to reverse the floor division
3. This ensures mathematical consistency: `ceil(floor(x)) = x` (approximately)

For Pool Revision 1-3:
1. The AToken contract uses standard half-up rounding
2. The formula should also use half-up rounding
3. Using ceiling rounding introduces a 1 wei error in edge cases

**Lesson:** When implementing inverse calculations for contract operations, always verify that the rounding mode matches the contract's actual implementation for the specific revision being processed.

## Refactoring

**Immediate Fix:**
1. Add pool revision check to determine rounding mode
2. Use `ray_div` (half-up) for pool revisions 1-3
3. Keep `ray_div_ceil` for pool revisions 4+

**Long-term Improvements:**
1. **TokenMathFactory integration:** Consider using TokenMathFactory to get the appropriate math implementation for each pool revision instead of hardcoding rounding modes
2. **Documentation:** Add comments explaining why different revisions use different rounding modes
3. **Test Coverage:** Add test cases for MINT_TO_TREASURY on different pool revisions to catch rounding mismatches

## Related Issues

- Issue 0014: MINT_TO_TREASURY AccruedToTreasury Calculation Error
- Issue 0015: MINT_TO_TREASURY BalanceTransfer Amount Not Used
- Issue 0017: MINT_TO_TREASURY Validation Error on Pool Revision 8

## References

- Contract: `contract_reference/aave/Pool/rev_1.sol` (executeMintToTreasury)
- Contract: `contract_reference/aave/AToken/rev_1.sol` (_mintScaled, rayDiv)
- Transaction: `0xb718b71af633e582d9324740c1ed97f32d40712d77cfeafa27778542eb2c507a`
- Files:
  - `src/degenbot/cli/aave.py` (MINT_TO_TREASURY calculation logic)
  - `src/degenbot/aave/libraries/wad_ray_math.py` (ray_div implementations)
  - `src/degenbot/aave/libraries/token_math.py` (TokenMath revision mapping)
