# Issue: GHO REPAY Missing Discount Scaled Amount in Balance Update

## Date
2026-03-20

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...GHO...). User ... scaled balance (75403259930805440037139) does not match contract balance (75402927816733346332418) at block 18240233
```

**Difference**: 332,114,072,093,704,721 wei (0.333 GHO)

## Root Cause

The fix from Issue #0037 is incomplete. When processing GHO_REPAY operations where interest exceeds repayment, the code uses `enriched_event.scaled_amount` directly (bypassing the GHO processor), but this only accounts for the repayment amount, not the **discount amount** that also needs to be burned.

### GHO Repay Flow Analysis

In GHO vToken rev 2+, when interest exceeds repayment amount:

1. **Interest Accrual**: 46.601 GHO interest is accrued (reflected in index change)
2. **Repayment**: 24.044 GHO is repaid
3. **Discount Applied**: 22.557 GHO discount (from stkAAVE holdings) is minted to `balanceFromInterest`
4. **Net Effect**: The contract burns `scaled_repayment + discount_scaled` from the debt position

The mathematical relationship is:
```
Interest Accrued (46.601 GHO) = Repayment (24.044 GHO) + Discount (22.557 GHO)
```

### Event Analysis

**Mint Event** (emitted instead of Burn when interest > repayment):
- `value`: 22,556,776,016,625,358,317 (22.557 GHO) - The **discount amount**, not new debt
- `balanceIncrease`: 46,601,036,735,819,693,373 (46.601 GHO) - Interest accrued
- `index`: 1,003,370,062,812,789,211,000,554,929 ray

**Key Insight**: The Mint event's `value` field represents the **discount** going to `balanceFromInterest`, not a debt increase. This discount amount, when converted to scaled units, must ALSO be burned from the debt position.

### Current Code Behavior

In `src/degenbot/cli/aave.py` lines 3258-3264:
```python
if operation.operation_type == OperationType.GHO_REPAY:
    assert enriched_event.scaled_amount is not None
    debt_position.balance -= enriched_event.scaled_amount  # Only burns repayment!
```

The `enriched_event.scaled_amount` is calculated from the Repay event amount (24.044 GHO) using debt burn rounding, resulting in 23,963,502,211,524,933,093 scaled units.

However, the contract actually burns:
- Scaled repayment: 23,963,502,211,524,933,093
- Plus discount scaled: 332,114,072,093,704,721  
- **Total burned**: 24,295,616,283,618,637,814

The missing 332,114,072,093,704,721 scaled units (0.333 GHO at current index) is the **discount scaled amount** that the current code doesn't account for.

### Why This Happens

The GHO VariableDebtToken contract handles repayments differently when interest exceeds repayment:

1. The `_burnScaled` function accrues interest and applies discounts
2. When `balanceIncrease > amount` (repayment), it mints the discount to `balanceFromInterest` and emits a Mint event
3. The net debt change is burning `(scaled_repayment + discount_scaled)` - the repayment burns debt, and the discount "burns" by being redirected to interest balance

The GHO processor (GhoV2Processor.process_mint_event) correctly calculates this:
```python
# Partial repayment: burn (repayment_scaled + discount_scaled)
balance_delta = -(repayment_scaled + discount_scaled)
```

But the #0037 fix bypasses the processor for GHO_REPAY operations, missing the discount component.

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | `0xd08a1044fed4f8e998a2a97bed37362713803a64e1b56c4ef2e29a0057cf08f2` |
| **Block** | 18240233 |
| **Type** | GHO_REPAY (repayWithPermit) |
| **User** | `0x5b85B47670778b204041D6457dB8b5F5D36fa97a` |
| **Asset** | GHO (`0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f`) |
| **Pool** | `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` (rev 2) |
| **vGHO Token** | `0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B` (rev 2) |

### Event Breakdown

| Event | LogIndex | Key Fields |
|-------|----------|------------|
| **Transfer** | 221 | `from=0x0`, `to=user`, `value=22556776016625358317` (discount mint to user) |
| **Mint** | 222 | `value=22556776016625358317`, `balanceIncrease=46601036735819693373`, `index=...` |
| **ReserveDataUpdated** | 223 | `variableBorrowIndex=1003370062812878921100054929` |
| **Transfer** | 224 | `from=user`, `to=aToken`, `value=24044260719194335056` (repayment) |
| **Repay** | 225 | `amount=24044260719194335056`, `useATokens=false` |

### Balance Calculation

| Metric | Before | After (Expected) | After (Degenbot) | Difference |
|--------|--------|------------------|------------------|------------|
| **Scaled Balance** | 75,427,223,433,016,964,970,232 | 75,402,927,816,733,346,332,418 | 75,403,259,930,805,440,037,139 | +332,114,072,072,093,704,721 |
| **Actual Balance** | 75,681.417 GHO | 75,657.040 GHO | 75,657.374 GHO | +0.333 GHO |

**Actual Burned**: 24,295,616,283,618,637,814 scaled units  
**Degenbot Burned**: 23,963,502,211,524,933,093 scaled units  
**Missing**: 332,114,072,093,704,721 scaled units (the discount scaled amount)

## Fix

The fix requires modifying the GHO_REPAY handling to also burn the discount scaled amount.

### File: `src/degenbot/cli/aave.py`

**Location**: Lines 3258-3271 (in `_process_debt_mint_with_match`)

**Current Code**:
```python
if operation.operation_type == OperationType.GHO_REPAY:
    # Use enriched scaled_amount (calculated from Repay event in enrichment layer)
    # instead of calling processor which derives amount from Mint event fields.
    # This avoids 1 wei rounding errors from integer truncation in interest calculations.
    # See debug/aave/0037 - GHO REPAY Uses Mint Event Instead of Repay Event Amount.md
    assert enriched_event.scaled_amount is not None
    debt_position.balance -= enriched_event.scaled_amount
    _update_debt_position_index(...)
```

**Fixed Code**:
```python
if operation.operation_type == OperationType.GHO_REPAY:
    # Use enriched scaled_amount (calculated from Repay event in enrichment layer)
    # instead of calling processor which derives amount from Mint event fields.
    # This avoids 1 wei rounding errors from integer truncation in interest calculations.
    # See debug/aave/0037 - GHO REPAY Uses Mint Event Instead of Repay Event Amount.md
    assert enriched_event.scaled_amount is not None
    
    # When interest exceeds repayment, the contract also burns the discount amount
    # that was minted to balanceFromInterest. We need to calculate and burn this too.
    # See debug/aave/0038 - GHO REPAY Missing Discount Scaled Amount in Balance Update.md
    if scaled_event.balance_increase is not None and scaled_event.index is not None:
        # Calculate the discount amount from the Mint event
        # value = discount (amount minted to balanceFromInterest)
        # balance_increase = total interest accrued
        discount_amount = scaled_event.amount  # This is the discount, not debt
        
        # Convert discount to scaled units (using same rounding as debt burn)
        gho_processor = TokenProcessorFactory.get_gho_debt_processor(
            debt_asset.v_token_revision
        )
        wad_ray_math = gho_processor.get_math_libraries()["wad_ray"]
        discount_scaled = wad_ray_math.ray_div_floor(
            a=discount_amount,
            b=scaled_event.index,
        )
        
        # Burn both repayment and discount
        total_burn = enriched_event.scaled_amount + discount_scaled
        debt_position.balance -= total_burn
        logger.debug(
            f"GHO_REPAY: burning repayment={enriched_event.scaled_amount} + "
            f"discount={discount_scaled} = total={total_burn}"
        )
    else:
        debt_position.balance -= enriched_event.scaled_amount
    
    _update_debt_position_index(...)
```

### Alternative Fix (Cleaner Architecture)

Instead of calculating the discount in the processing layer, use the GHO processor's existing logic:

```python
if operation.operation_type == OperationType.GHO_REPAY:
    # Get the effective discount from transaction context
    effective_discount = tx_context.user_discounts.get(user.address, user.gho_discount)
    
    # Process using GHO-specific processor
    gho_processor = TokenProcessorFactory.get_gho_debt_processor(
        debt_asset.v_token_revision
    )
    assert scaled_event.balance_increase is not None
    assert scaled_event.index is not None
    
    # Create event data with the actual repay amount from the Repay event
    # (extracted from enriched_event calculation)
    repay_amount = ... # Derive from enriched_event.scaled_amount
    
    gho_result = gho_processor.process_mint_event(
        event_data=DebtMintEvent(
            caller=scaled_event.caller_address or scaled_event.user_address,
            on_behalf_of=scaled_event.user_address,
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=enriched_event.scaled_amount,  # Pass actual repay amount
        ),
        previous_balance=debt_position.balance,
        previous_index=debt_position.last_index or 0,
        previous_discount=effective_discount,
        actual_repay_amount=repay_amount,  # New parameter
    )
    
    debt_position.balance += gho_result.balance_delta
    _update_debt_position_index(...)
```

**Note**: The alternative fix requires modifying the processor interface to accept the actual repay amount, avoiding the 1 wei rounding issue from #0037 while still using the processor's discount logic.

## Key Insight

**When processing GHO_REPAY operations, the enriched scaled_amount only represents the repayment portion. The discount amount (from the Mint event's `value` field) must also be converted to scaled units and burned from the debt position.**

The Mint event semantics in GHO are different from standard Aave debt tokens:
- Standard debt: `Mint.value` = new debt added
- GHO debt: `Mint.value` = discount minted to `balanceFromInterest` (not debt)
- In both cases, the interest is already reflected in the index change

The GHO contract burns `(scaled_repayment + discount_scaled)` in a single operation, so our processing must do the same.

## Alternative Solutions

### Option 1: Calculate Discount in Processing Layer (Recommended)

As shown in the "Fixed Code" above, calculate the discount_scaled from the Mint event's `amount` field (which is the discount) and add it to the burn amount.

**Pros**: Minimal changes, fixes the issue immediately  
**Cons**: Duplicates logic that exists in the GHO processor

### Option 2: Modify GHO Processor Interface

Add an `actual_repay_amount` parameter to `process_mint_event` that overrides the derived amount from Mint event fields.

**Pros**: Keeps all GHO logic in the processor, cleaner architecture  
**Cons**: Requires interface changes, more testing

### Option 3: Skip GHO Processor Bypass

Remove the special case for GHO_REPAY in #0037 and let it go through the normal GHO processor flow. Handle the 1 wei rounding issue differently (e.g., tolerance in verification).

**Pros**: Simpler code path, no special cases  
**Cons**: Reintroduces the rounding issue from #0037

## Refactoring

1. **Document GHO-specific semantics**: Create clear documentation explaining that GHO Mint events have different semantics than standard debt tokens, particularly for REPAY operations where interest exceeds repayment.

2. **Unified GHO processing**: Consider refactoring GHO processing to always go through the processor, with the enrichment layer passing the "source of truth" amounts from Pool events rather than bypassing the processor entirely.

3. **Test coverage**: Add test cases for GHO_REPAY where interest exceeds repayment, verifying that the discount amount is correctly burned.

## Related Issues

- Issue #0037: GHO REPAY Uses Mint Event Instead of Repay Event Amount (incomplete fix)
- Issue #0031: REPAY Debt Mint Validation Rounding Error (related to rounding)
- Issue #0016: REPAY with Interest Exceeding Repayment Uses Wrong Rounding

## Files Referenced

- `src/degenbot/cli/aave.py` - Main processing logic (`_process_debt_mint_with_match`)
- `src/degenbot/aave/enrichment.py` - Event enrichment layer
- `src/degenbot/aave/processors/debt/gho/v2.py` - GHO V2 processor with discount logic
- `src/degenbot/aave/processors/debt/gho/v1.py` - GHO V1 processor (similar logic)
- `contract_reference/aave/GhoVariableDebtToken/rev_2.sol` - GHO vToken contract

---

*Report Generated: March 20, 2026*  
*Issue ID: 0038*  
*Status: ✅ FIXED - Implementation Complete*

## Implementation Summary

The fix was implemented across all GHO processor versions (V1, V2, V4, V5) and the processing layer:

1. **Base Protocol** (`src/degenbot/aave/processors/base.py`):
   - Added `actual_repay_amount: int | None = None` parameter to `GhoDebtTokenProcessor.process_mint_event()`

2. **GHO Processors** (`v1.py`, `v2.py`, `v4.py`, `v5.py`):
   - Updated method signatures to accept `actual_repay_amount` parameter
   - Modified REPAY logic to use `actual_repay_amount` when provided, otherwise fall back to deriving from Mint event fields
   - This preserves the discount logic (repayment_scaled + discount_scaled) from the original processors

3. **Processing Layer** (`src/degenbot/cli/aave.py`):
   - Removed the bypass code from #0037 that used `enriched_event.scaled_amount` directly
   - Now extracts repay amount from Repay event and passes it to processor via `actual_repay_amount` parameter
   - The processor handles the full logic including discount calculations

**Verification**: 
- Transaction `0xd08a1044fed4f8e998a2a97bed37362713803a64e1b56c4ef2e29a0057cf08f2` at block 18240233 now processes successfully
- Balance verification passes
- Aave update completes: `successfully updated to block 18,240,234`
