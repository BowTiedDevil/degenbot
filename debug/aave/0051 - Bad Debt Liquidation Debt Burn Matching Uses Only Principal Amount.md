# Issue 0051: Bad Debt Liquidation Debt Burn Matching Uses Only Principal Amount

## Date
2026-03-23

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...LUSD...). 
User AaveV3User(...) scaled balance (1546164357389661290) does not match contract balance (0) at block 22126921
```

## Root Cause

In bad debt liquidations with deficit creation, the `_collect_primary_debt_burns` function compares only the `ev.amount` (principal) field against `debt_to_cover`, ignoring the `balance_increase` (accrued interest). This causes the ratio check to fail and skip the debt burn event, leaving it unassigned and preventing the debt position from being properly cleared.

### Transaction Breakdown

**Transaction:** 0x93cf6341eca5fa5aa8401cca894f8b67d2472068eadc53b330b20c4c38ec30cc
**Block:** 22126921
**Type:** Bad Debt Liquidation with Deficit Creation

| Field | Value |
|-------|-------|
| User | 0xdE6E53Ad0c41C6014E4757b99Cf422b909B5E3a8 |
| Debt Asset | LUSD (0x5f98805A4E8be255a32880FDeC7F6728C6568bA0) |
| vToken | 0x33652e48e4B74D18520f11BfE58Edd2ED2cEc5A2 |
| debtToCover (LiquidationCall) | 13,536,057,044,592,977 (0.0135 ETH) |
| Burn event `value` (principal) | 1,682,476,190,976,739,244 (1.682 ETH) |
| Burn event `balanceIncrease` | 116,790,488,347,622,929 (0.117 ETH) |
| **Total Debt Cleared** | **1,799,266,679,324,362,173 (1.799 ETH)** |
| Ratio (value/debtToCover) | **124x** (exceeds 100x threshold) |

### Event Sequence

1. **LogIndex 416:** `Transfer` (vToken) - Transfer to zero address
2. **LogIndex 417:** `Burn` (vToken) - `value=1,682,476,190,976,739,244`, `balanceIncrease=116,790,488,347,622,929`
3. **LogIndex 418:** `ReserveDataUpdated` (Pool) - Interest rate update
4. **LogIndex 419:** `DeficitCreated` (Pool) - 1,785,730,622,279,769,196 LUSD deficit
5. **LogIndex 431:** `LiquidationCall` (Pool) - debtToCover=13,536,057,044,592,977

### The Bug

In `_collect_primary_debt_burns` (aave_transaction_operations.py:2022-2034):

```python
if debt_to_cover > 0 and ev.amount > 0:
    ratio = ev.amount / debt_to_cover  # BUG: Uses only principal, not total burn
    if ratio > DEBT_BURN_AMOUNT_MISMATCH_THRESHOLD:  # 100x threshold
        logger.debug(f"_collect_primary_debt_burns: Skipping burn at logIndex {log_index} "
                     f"(amount={ev.amount}) - {ratio:.0f}x debtToCover ({debt_to_cover})")
        continue  # Skip this burn event!
```

**Why this fails for bad debt liquidations:**

In Aave V3 bad debt liquidations, the debt burn clears the user's **entire debt balance** (principal + accrued interest), not just the `debtToCover` amount. The burn event structure is:

```solidity
event Burn(
    address indexed from,
    address indexed target,
    uint256 value,           // Principal burned
    uint256 balanceIncrease, // Accrued interest
    uint256 index
);
```

- `value` = principal debt reduction (1.682 ETH)
- `balanceIncrease` = accrued interest (0.117 ETH)  
- **Total cleared = value + balanceIncrease = 1.799 ETH**

The `debtToCover` in the `LiquidationCall` event (0.0135 ETH) represents only the portion the liquidator repays. In bad debt liquidations, this is much smaller than the total debt being cleared.

**Consequence:**

1. The ratio `ev.amount / debt_to_cover` = 1.682 / 0.0135 = **124x**
2. This exceeds the `DEBT_BURN_AMOUNT_MISMATCH_THRESHOLD` of 100x
3. The burn event is **skipped** with message: "Skipping burn at logIndex 417 (amount=1682476190976739244) - 124x debtToCover..."
4. The burn remains **unassigned** and gets classified as **INTEREST_ACCRUAL**
5. When processed, the debt position is NOT cleared
6. Local balance remains at 1,546,164,357,389,661,290 scaled units
7. Contract balance is **0** (verified via RPC)
8. Verification fails with balance mismatch

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | 0x93cf6341eca5fa5aa8401cca894f8b67d2472068eadc53b330b20c4c38ec30cc |
| **Block** | 22126921 |
| **Type** | Bad Debt Liquidation (Flash Loan via ClinicSteward) |
| **User** | 0xdE6E53Ad0c41C6014E4757b99Cf422b909B5E3a8 |
| **Debt Asset** | LUSD |
| **Collateral Asset** | wstETH |
| **Deficit Amount** | 1,785,730,622,279,769,196 (1.786 LUSD) |
| **Pool Revision** | 7 |
| **vToken Revision** | 1 |

## Fix

**File:** `src/degenbot/cli/aave_transaction_operations.py`

**Location:** `_collect_primary_debt_burns` function, lines 2022-2034

**Change:** Calculate total burn amount including `balance_increase` and use `>=` comparison:

```python
# For multi-liquidation scenarios (debug/aave/0050):
# Compare burn amount to debtToCover to find the best match.
# Allow for interest accrual (burn can be slightly higher than debtToCover).
# 
# IMPORTANT: Use total_burn = ev.amount + balance_increase, not just ev.amount.
# In bad debt liquidations, the burn clears the entire debt (principal + interest),
# which can be much larger than debtToCover. See debug/aave/0051.
if debt_to_cover > 0 and ev.amount > 0:
    # Calculate total debt cleared: principal + interest
    # The Burn event's `value` field is principal only, balance_increase is interest
    # Total burned = value + balance_increase
    total_burn = ev.amount + (ev.balance_increase or 0)
    
    # Match if total_burn >= debt_to_cover
    # - Normal liquidation: total_burn == debt_to_cover (within tolerance)
    # - Bad debt liquidation: total_burn > debt_to_cover (excess becomes deficit)
    if total_burn < debt_to_cover:
        log_index = ev.event["logIndex"]
        logger.debug(
            f"_collect_primary_debt_burns: Skipping burn at "
            f"logIndex {log_index} (total_burn={total_burn}) - "
            f"less than debtToCover ({debt_to_cover}), "
            f"likely belongs to different liquidation. "
            f"See debug/aave/0050"
        )
        continue

primary_burns.append(ev)
assigned_indices.add(ev.event["logIndex"])
if ev.index is not None and ev.index > 0:
    assigned_indices.add(ev.index)
break  # Only one primary burn expected per (user, asset) pair
```

**Key changes:**
1. Calculate total debt cleared: `total_burn = ev.amount + (ev.balance_increase or 0)`
2. Compare `total_burn` to `debt_to_cover` instead of just `ev.amount`
3. Match if `total_burn >= debt_to_cover` (handles both normal and bad debt liquidations)
4. Remove the ratio-based threshold check
5. Keep the `< debt_to_cover` check to filter burns that are too small

**Why this works:**
- Normal liquidation: `total_burn ≈ debt_to_cover` (within reasonable tolerance)
- Bad debt liquidation: `total_burn > debt_to_cover` (excess is the deficit)
- Both cases satisfy `total_burn >= debt_to_cover`
- Burns that are too small (`total_burn < debt_to_cover`) are correctly skipped

## Key Insight

**Burn event semantics in Aave V3:**

From the Aave V3 VariableDebtToken contract:
- `value` field = principal debt being burned
- `balanceIncrease` field = accrued interest since last user interaction
- **Total debt cleared = value + balanceIncrease**

In bad debt liquidations:
- The entire debt balance is burned (principal + interest)
- The `debtToCover` represents only what the liquidator repays
- The difference between total debt and `debtToCover` becomes the deficit
- Using only `value` for matching misses the interest component

**Matching strategy:**

- **Normal liquidation:** `total_burn ≈ debt_to_cover` (slightly higher due to interest)
- **Bad debt liquidation:** `total_burn > debt_to_cover` (excess becomes deficit)
- Both cases match with `total_burn >= debt_to_cover`

**Architectural lesson:** Always include `balance_increase` when calculating the total effect of Burn events. The `value` field alone does not represent the full debt reduction.

## Verification

**Expected behavior after fix:**

1. The debt burn event at logIndex 417 will be correctly matched to the LIQUIDATION operation
2. The `_process_debt_burn_with_match` function will detect this as a bad debt liquidation
3. The debt position will be set to 0
4. Balance verification will pass

**Test command:**
```bash
uv run degenbot aave update
```

Expected output:
```
Processing operation 2: LIQUIDATION
_process_debt_burn_with_match: handling with standard debt processor
_process_debt_burn_with_match: scaled_event.amount = 1682476190976739244
_process_debt_burn_with_match: scaled_event.balance_increase = 116790488347622929
_process_debt_burn_with_match: Bad debt liquidation detected for user 0xdE6E53Ad0c41C6014E4757b99Cf422b909B5E3a8
_process_debt_burn_with_match: BAD DEBT LIQUIDATION - setting balance to 0 (was 1546164357389661290)
...
AaveV3Market successfully updated to block 22,126,921
```

## Refactoring

### Architectural Insight: Primary/Secondary Burn Classification is Technical Debt

**Finding:** The `primary_burns` vs `secondary_burns` distinction adds no semantic value. It was introduced as a workaround for multi-asset liquidations but creates unnecessary complexity.

**History:**
- Originally, the code only matched one debt burn per liquidation
- Issue 0028/0029: Added "secondary burns" to handle additional debt assets being liquidated
- Issue 0043: Added `user_liquidation_count > 1` check to disable secondary collection for multi-liquidation scenarios
- The distinction exists only because the original design didn't anticipate multi-asset liquidations

**Current State:**
- Primary burns: match `debt_v_token_address` + amount-based disambiguation
- Secondary burns: all other burns for the same user (skipped when `user_liquidation_count > 1`)
- Both are processed identically in `_process_debt_burn_with_match`

**The Real Problem:** The primary/secondary split conflates two concerns:
1. Multi-liquidation disambiguation (same user/asset, multiple liquidations)
2. Multi-asset liquidation handling (same user, multiple debt assets)

**Cleaner Architecture:**

```python
def _collect_debt_burns(
    self,
    *,
    user: ChecksumAddress,
    debt_v_token_address: ChecksumAddress | None,
    debt_to_cover: int,
    scaled_events: list[ScaledTokenEvent],
    assigned_indices: set[int],
    is_gho: bool,
    liquidation_analysis: dict,  # Pre-analyzed liquidation context
) -> list[ScaledTokenEvent]:
    """
    Collect ALL debt burns for the liquidated user.
    
    No primary/secondary distinction - just semantic matching (user + asset).
    Multi-liquidation scenarios are handled through pre-analysis.
    """
    burns: list[ScaledTokenEvent] = []
    
    # Check if this is a multi-liquidation scenario for this user/asset
    is_multi_liquidation = liquidation_analysis.get((user, debt_v_token_address), 0) > 1
    
    for ev in scaled_events:
        if ev.event["logIndex"] in assigned_indices:
            continue
        if ev.user_address != user:
            continue
        if is_gho and ev.event_type != ScaledTokenEventType.GHO_DEBT_BURN:
            continue
        if not is_gho and ev.event_type != ScaledTokenEventType.DEBT_BURN:
            continue
            
        event_token_address = get_checksum_address(ev.event["address"])
        if debt_v_token_address is not None and event_token_address != debt_v_token_address:
            # This is a burn for a different debt asset (multi-asset liquidation)
            # Include it regardless of is_multi_liquidation
            burns.append(ev)
            continue
            
        # For the primary debt asset
        if is_multi_liquidation:
            # Multi-liquidation: use amount-based disambiguation
            total_burn = ev.amount + (ev.balance_increase or 0)
            if total_burn < debt_to_cover:
                continue  # Belongs to different liquidation
        
        burns.append(ev)
        assigned_indices.add(ev.event["logIndex"])
    
    return burns
```

**Benefits of Cleaner Architecture:**
1. **No primary/secondary distinction** - simpler mental model
2. **Explicit multi-liquidation handling** - pre-analysis makes intent clear
3. **Handles both issues (0050 and 0051)** - no ratio threshold, proper total_burn calculation
4. **Removes DEBT_BURN_AMOUNT_MISMATCH_THRESHOLD constant** - no magic numbers
5. **Single responsibility** - one function collects all burns, processing handles semantics

### Recommended Fix

**Phase 1:** Fix the immediate issue (0051) by using `total_burn = ev.amount + balance_increase`

**Phase 2:** Refactor to remove primary/secondary split entirely:
1. Add liquidation analysis phase to detect multi-liquidation scenarios
2. Consolidate `_collect_primary_debt_burns` and `_collect_secondary_debt_burns` into single function
3. Remove `DEBT_BURN_AMOUNT_MISMATCH_THRESHOLD` constant
4. Update documentation

**Future improvements:**

1. **Consolidate burn matching logic** - Remove primary/secondary split, use pre-analysis
2. **Document event field semantics** - Add contract references explaining that Burn events use `value + balanceIncrease` for total debt reduction
3. **Remove ratio threshold** - The `>= debt_to_cover` check with pre-analysis is more robust
4. **Add test coverage** - Include test cases for bad debt liquidations where `total_burn >> debt_to_cover`

## Related Issues

- Issue 0027: Bad Debt Liquidation Debt Burn Matching Failure (similar root cause, different manifestation)
- Issue 0050: Multi-Liquidation Same Debt Asset Burn Misassignment (ratio threshold originally added for this)
- Issue 0046: WBTC Debt Burn Misclassified as INTEREST_ACCRUAL (consequence of skipped burns)

## References

- Contract: `contract_reference/aave/VariableDebtToken/rev_1.sol` (Burn event definition)
- File: `src/degenbot/cli/aave_transaction_operations.py` (lines 2022-2034)
- Transaction: 0x93cf6341eca5fa5aa8401cca894f8b67d2472068eadc53b330b20c4c38ec30cc
- Investigation report: `/tmp/aave_liquidation_0051.md`
