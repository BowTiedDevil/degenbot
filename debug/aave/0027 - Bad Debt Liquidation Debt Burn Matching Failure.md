# Issue 0027: Bad Debt Liquidation Debt Burn Matching Failure

## Date
2026-03-18

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). 
User AaveV3User(...) scaled balance (45332) does not match contract balance (0) at block 21929172
```

## Root Cause

In bad debt liquidations (when a DEFICIT_CREATED event is emitted), the debt burn event amount represents the **full debt balance** (borrowerReserveDebt), not just the `debtToCover` amount from the LiquidationCall event. The current matching logic uses a tolerance-based check that fails when the difference exceeds 1% or 1000 units.

### Transaction Breakdown

**Transaction:** 0x65cd903434d489b95c6e664a87775c68f9d6c940b64c67177c98ca3fc59d4116
**Block:** 21929172

| Field | Value |
|-------|-------|
| User | 0x14a7bce8e3f09393ffD42C816B10Db269A930a4d |
| Debt Asset | WBTC (0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599) |
| vToken | 0x40aAbEf1aa8f0eEc637E0E7d92fbfFB2F26A8b7B |
| debtToCover (LiquidationCall) | 42,358 satoshis |
| Burn event amount | 46,148 units |
| Balance increase (interest) | 168 units |
| Difference | 3,790 units |
| Tolerance | 1,000 units (max of 1% or 1000) |

### Event Sequence

1. **LogIndex 336:** DEBT_BURN - Burns 46,148 units (full debt)
2. **LogIndex 337:** DEFICIT_CREATED - Creates 3,958 satoshi deficit (bad debt)
3. **LogIndex 348:** LIQUIDATION_CALL - debtToCover=42,358

### The Bug

In `_create_liquidation_operation` (aave_transaction_operations.py:1897-1900):

```python
tolerance = max(int(debt_to_cover) * 0.01, 1000)  # = 1000
if amount_diff > tolerance:  # 3790 > 1000 = True
    continue  # Skip this burn event!
```

The debt burn (46,148) is **not matched** to the LIQUIDATION operation because:
- In bad debt liquidations, the burn amount = full debt balance
- The liquidator only pays `debtToCover` (42,358)
- The difference (3,790 = 46,148 - 42,358) is recorded as deficit
- This difference exceeds the 1,000 unit tolerance

### Consequence

1. The debt burn event remains **unassigned**
2. It gets classified as **INTEREST_ACCRUAL** operation
3. When processed, `_process_debt_burn_with_match` doesn't detect it as bad debt
   - Bad debt check only applies to LIQUIDATION/GHO_LIQUIDATION operations
4. The debt position balance is incorrectly calculated as:
   - Starting balance: 45,332 scaled units
   - Burn amount: 46,148
   - Calculated balance: 45,332 - 46,148 = -816 (or error)
5. Actual contract balance: 0 (full liquidation)

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | 0x65cd903434d489b95c6e664a87775c68f9d6c940b64c67177c98ca3fc59d4116 |
| **Block** | 21929172 |
| **Type** | Bad Debt Liquidation (Flash Loan) |
| **User** | 0x14a7bce8e3f09393ffD42C816B10Db269A930a4D |
| **Debt Asset** | WBTC |
| **Collateral Asset** | USDC |
| **Deficit Amount** | 3,958 satoshis (0.00003958 WBTC) |
| **Pool Revision** | 1 |
| **vToken Revision** | 1 |

## Fix

**File:** `src/degenbot/cli/aave_transaction_operations.py`

**Location:** `_create_liquidation_operation` function, debt burn matching logic (lines 1867-1905)

**Change:** Use `total_burn >= debt_to_cover` matching instead of tolerance-based matching:

```python
debt_burn: ScaledTokenEvent | None = None
for ev in scaled_events:
    if ev.event["logIndex"] in assigned_indices:
        continue
    if ev.user_address != user:
        continue
    if is_gho and ev.event_type != ScaledTokenEventType.GHO_DEBT_BURN:
        continue
    if not is_gho and ev.event_type != ScaledTokenEventType.DEBT_BURN:
        continue

    # Match debt burn events only if they belong to this liquidation's debt asset
    # This prevents incorrect matching when a user is liquidated multiple times
    # with different debt assets in the same transaction
    event_token_address = get_checksum_address(ev.event["address"])
    if debt_v_token_address is not None and event_token_address == debt_v_token_address:
        # Calculate total debt being cleared: principal + interest
        # The Burn event's `value` field is principal only, balance_increase is interest
        # Total burned = value + balance_increase
        total_burn = ev.amount + (ev.balance_increase or 0)

        # Match if total_burn >= debt_to_cover
        # - Normal liquidation: total_burn == debtToCover (within tolerance)
        # - Bad debt liquidation: total_burn > debtToCover (excess becomes deficit)
        if pool_revision >= SCALED_AMOUNT_POOL_REVISION:
            # Pool revision 9+ uses ray math with flooring, allow ±2 wei tolerance
            if total_burn < debt_to_cover - TOKEN_AMOUNT_MATCH_TOLERANCE:
                continue
        elif total_burn < debt_to_cover:
            continue

        debt_burn = ev
        break
```

**Key changes:**
1. Calculate total debt cleared: `total_burn = ev.amount + (ev.balance_increase or 0)`
2. Match if `total_burn >= debt_to_cover` (handles both normal and bad debt liquidations)
3. Remove the 1000 unit / 1% tolerance clause
4. Keep 2 wei tolerance for pool revision 9+ (standard pattern)

**Why this works:**
- Normal liquidation: `total_burn ≈ debt_to_cover` (within 2 wei for rev 9+)
- Bad debt liquidation: `total_burn > debt_to_cover` (excess is the deficit)
- Both cases satisfy `total_burn >= debt_to_cover`

## Key Insight

**Burn event semantics:**

From the Aave V3 VariableDebtToken contract (line 1733):
> `@param value The scaled-up amount being burned (user entered amount - balance increase from interest)`

This means:
- `value` field = principal debt being burned
- `balanceIncrease` field = accrued interest
- **Total debt cleared = value + balanceIncrease**

**Matching strategy:**

- **Normal liquidation:** `total_burn ≈ debt_to_cover` (within 2 wei tolerance for rev 9+)
- **Bad debt liquidation:** `total_burn > debt_to_cover` (excess becomes deficit)
- Both cases match with `total_burn >= debt_to_cover`

**Architectural lesson:** Use intrinsic event data (`balance_increase`) before extrinsic signals (DEFICIT_CREATED). The burn event already contains all information needed for matching.

## Verification

**Test Results:**

```
Processing operation 1: LIQUIDATION
Processing _process_operation for tx at block 21929172
Processing _process_debt_burn_with_match at block 21929172
_process_debt_burn_with_match: handling with standard debt processor
_process_debt_burn_with_match: scaled_event.amount = 46148
_process_debt_burn_with_match: scaled_event.balance_increase = 168
_process_debt_burn_with_match: scaled_event.index = 1021708047393027229971381351
_process_debt_burn_with_match: Bad debt liquidation detected for user 0x14a7bce8e3f09393ffD42C816B10Db269A930a4D
_process_debt_burn_with_match: BAD DEBT LIQUIDATION - setting balance to 0 (was 45332)
...
AaveV3Market successfully updated to block 21,929,172
```

✅ **Block 21929172: Passed** (original failing block)

**Code Quality:**
- Lint: ✅ All checks passed
- Type check: ✅ No issues found

## Refactoring

**Future improvements:**

1. **Audit all tolerance-based matching** - Replace arbitrary tolerances with semantic matching where possible
2. **Document event field semantics** - Add contract references for event field meanings (e.g., Burn event `value` vs `balanceIncrease`)
3. **Consider stricter matching** - Once semantic matching is proven, consider removing the 2 wei tolerance for pool revision 9+

## Related Issues

- Issue 0018: Bad Debt Liquidation Burns Full Debt Balance (handling after matching)
- Issue 0026: Liquidation Debt Burn Unit Mismatch in Matching

## References

- Contract: `contract_reference/aave/VariableDebtToken/rev_1.sol` (line 1733: Burn event definition)
- File: `src/degenbot/cli/aave_transaction_operations.py` (lines 1867-1905)
- File: `src/degenbot/cli/aave.py` (line ~3081, bad debt handling)
- Transaction: 0x65cd903434d489b95c6e664a87775c68f9d6c940b64c67177c98ca3fc59d4116
