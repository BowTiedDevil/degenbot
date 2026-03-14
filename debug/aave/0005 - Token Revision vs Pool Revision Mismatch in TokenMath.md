# 0005 - Token Revision vs Pool Revision Mismatch in TokenMath

### Issue
Scaled balance verification failure when processing Aave V3 deposit transactions where the Pool revision and aToken revision differ.

### Date
2026-03-14

### Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User 0x29dc870abD9EaAe7CD13620a59b018c490F29895 scaled balance (9259833) does not match contract balance (9259834) at block 20398680
```

### Root Cause
The `ScaledAmountCalculator` and validation logic incorrectly used the **Pool revision** to determine which TokenMath implementation to use, instead of using the **aToken revision**.

When a Pool is upgraded to a newer revision (e.g., rev 4), the older aToken contracts that were deployed before the upgrade continue using their original revision's math logic (e.g., rev 1). The scaled balance calculation is performed by the aToken contract itself, not the Pool contract.

In this case:
- **Pool revision**: 4 → TokenMathV4 uses `ray_div_floor` → expected 9,259,833
- **aToken revision**: 1 → TokenMathV1 uses `ray_div` (half-up) → actual 9,259,834

### Transaction Details
- **Hash**: 0x90cc8be2f046fcd7e32d50f8573ce9851a71ebc03bb95a08766425285eb2b1b4
- **Block**: 20398680
- **Type**: Supply (deposit)
- **User**: 0x29dc870abD9EaE7CD13620a59b018c490F29895
- **Asset**: USDT (Tether)
- **Amount**: 10,000,000 (10 USDT with 6 decimals)
- **Index**: 1,079,932,968,296,243,384,762,497,884

### Calculation Details

Solidity aToken contract (_mintScaled function, rev_1.sol:2762):
```solidity
uint256 amountScaled = amount.rayDiv(index);
```

Solidity rayDiv implementation uses half-up rounding:
```solidity
c := div(add(mul(a, RAY), div(b, 2)), b)
```

Python calculation:
```python
# Expected using contract's math (TokenMathV1)
ray_div(10,000,000, 1,079,932,968,296,243,384,762,497,884)
= ((10,000,000 * 10**27) + (index // 2)) // index
= 9,259,834

# Incorrect using Pool's math (TokenMathV4)
ray_div_floor(10,000,000, 1,079,932,968,296,243,384,762,497,884)
= (10,000,000 * 10**27) // index
= 9,259,833
```

### Fix

**File**: `src/degenbot/aave/calculator.py:21`

Changed from:
```python
self.token_math = TokenMathFactory.get_token_math(pool_version=pool_revision)
```

To:
```python
self.token_math = TokenMathFactory.get_token_math_for_token_revision(token_revision)
```

**File**: `src/degenbot/aave/models.py:158`

Changed from:
```python
token_math = TokenMathFactory.get_token_math(pool_rev)
```

To:
```python
token_math = TokenMathFactory.get_token_math_for_token_revision(token_rev)
```

### Key Insight
**The aToken contract performs the scaled balance calculation, not the Pool contract.** Even after a Pool upgrade, existing aToken contracts continue using their deployed revision's math logic. The token revision (ATOKEN_REVISION) determines the correct TokenMath implementation, not the pool revision.

### Refactoring

1. **Clarify TokenMath ownership**: Document that TokenMath choice depends on the token contract performing the calculation, not the pool calling it.

2. **Add validation layer**: Consider adding domain validation to ensure `TokenMathFactory.get_token_math_for_token_revision` is correctly used in the enrichment layer.

3. **Test expansion**: Add test cases for pools upgraded to revision 4+ with revision 1-3 tokens deployed before the upgrade to catch this class of bugs.
