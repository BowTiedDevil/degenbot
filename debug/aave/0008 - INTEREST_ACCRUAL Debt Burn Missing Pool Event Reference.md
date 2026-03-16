# Issue: INTEREST_ACCRUAL Debt Burn Missing Pool Event Reference

## Status
**INVESTIGATED** - Root cause identified, fix strategy documented

## Date
2026-03-15

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), underlying_token=Erc20TokenTable(chain=1, address='0x514910771AF9Ca656af840dff83E8264EcF986CA', symbol=None), a_token=Erc20TokenTable(chain=1, address='0x5E8C8A7243651DB1384C0dDfDbE39761E8e7E51a', symbol=None), v_token=Erc20TokenTable(chain=1, address='0x4228F8895C7dDA20227F6a5c6751b8Ebf19a6ba8', symbol=None)). User AaveV3User(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), address='0x21e7824340C276735a033b1bC45652EbBe007193', e_mode=0) scaled balance (29905878934757052144577) does not match contract balance (29410404374552234237336) at block 23088593
```

**Balance Difference:** 495474560204817907241 (approximately 495 LINK in scaled units)

## Root Cause
When a debt burn event occurs during interest accrual (as part of a REPAY operation), the event is classified as an INTEREST_ACCRUAL operation without a pool_event reference. The enrichment layer at `src/degenbot/aave/enrichment.py:86-93` handles INTEREST_ACCRUAL operations by setting `scaled_amount = 0`, which is appropriate for pure interest accrual mints but incorrect for debt burns that need to reduce the scaled balance.

### Execution Flow
1. **Transaction Parser** (`aave_transaction_operations.py:1958-1974`): The debt burn event at logIndex 130 is created as an INTEREST_ACCRUAL operation with `pool_event=None` because it wasn't matched to the REPAY operation at logIndex 133.

2. **Enrichment Layer** (`enrichment.py:86-93`): When `pool_event is None` and operation type is INTEREST_ACCRUAL:
   ```python
   if operation.operation_type.name == "INTEREST_ACCRUAL":
       raw_amount = scaled_event.amount
       scaled_amount = 0  # INCORRECT for debt burns
   ```

3. **Debt Burn Processing** (`aave.py:3144-3155`): The DebtBurnEvent receives `scaled_amount=0` from enrichment:
   ```python
   _process_scaled_token_operation(
       event=DebtBurnEvent(
           value=scaled_event.amount,           # 499997055676410400534
           balance_increase=scaled_event.balance_increase,  # 2944323589599465
           index=scaled_event.index,            # 1009133546217410998733439284
           scaled_amount=scaled_amount,         # 0 (should be 495474560204817907241)
       ),
       ...
   )
   ```

4. **Token Processor** (`processors/debt/v4.py`): Since `scaled_amount` is 0, the processor falls back to reverse-calculating from event values, which introduces a 1 wei rounding error or completely misses the principal repayment.

### Why The Burn Wasn't Matched
The burn event at logIndex 130 has:
- `amount` (amountToBurn): 499997055676410400534
- `balance_increase`: 2944323589599465
- `index`: 1009133546217410998733439284

This represents a principal repayment plus accrued interest. The matching logic creates it as INTEREST_ACCRUAL because the burn event has `amount != balance_increase` (which would indicate pure interest), and it wasn't matched to the REPAY event that follows it at logIndex 133.

### Git History Context
Commit `a87f1fe4` ("refactor: remove `last_repay_amount` hack, attach interest accrual events to parent event if triggered") removed the workaround that stored `last_repay_amount` in TransactionContext for INTEREST_ACCRUAL debt burns to use. The commit intended to "attach interest accrual events to parent event" but the attachment mechanism doesn't provide the pool event reference needed for enrichment to calculate scaled_amount correctly.

## Transaction Details

| Field | Value |
|-------|-------|
| Hash | 0x121166f6d925e38e425a6dfa637a71cfa3bc6ed2d08653cf2aad146d2a6077c3 |
| Block | 23088593 |
| Type | REPAY + WITHDRAW |
| User | 0x21e7824340C276735a033b1bC45652EbBe007193 |
| Asset | LINK (0x514910771AF9Ca656af840dff83E8264EcF986CA) |
| vToken | 0x4228F8895C7dDA20227F6a5c6751b8Ebf19a6ba8 |
| Pool | 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 |
| vToken Revision | 4 |

**Operations Sequence:**
1. **REPAY** (logIndex 133) - Pool event with paybackAmount 500000000000000000000
2. **WITHDRAW** (logIndex 146) - Pool event with withdrawAmount 8634552914
3. **INTEREST_ACCRUAL** (logIndex 130) - Debt burn, INTEREST_ACCRUAL type, no pool_event
4. **INTEREST_ACCRUAL** (logIndex 137) - Collateral mint
5. **BALANCE_TRANSFER** (logIndex 138/139) - Collateral transfer

**Burn Event Values (logIndex 130):**
- `value` (amountToBurn): 499997055676410400534
- `balance_increase`: 2944323589599465
- `index`: 1009133546217410998733439284
- Expected `scaled_amount`: 495474560204817907241 (calculated via rayDivFloor(500e18, index))

**Balance Calculation:**
- Initial scaled balance: 29905878934757052144577
- Expected scaled burn: 495474560204817907241
- Expected final scaled balance: 29410404374552234237336
- Contract scaled balance: 29410404374552234237336 ✓
- Python calculated balance: 29905878934757052144577 ✗ (didn't apply burn)

## Fix Strategy

The fix requires ensuring INTEREST_ACCRUAL debt burns have access to the parent REPAY event's paybackAmount for scaled amount calculation. Two approaches:

### Option A: Attach Pool Event Reference
Modify `_create_interest_accrual_operations` in `aave_transaction_operations.py` to:
1. Check if an unassigned debt burn event is related to a REPAY operation in the same transaction
2. Attach the REPAY pool_event reference to the INTEREST_ACCRUAL operation
3. This allows enrichment to extract `raw_amount` from the pool event and calculate `scaled_amount` using TokenMath

### Option B: Pre-calculate During Operation Creation
In `_create_interest_accrual_operations`, when creating INTEREST_ACCRUAL operations for debt burns:
1. Look for a preceding REPAY event in the same transaction
2. If found, calculate `scaled_amount` using `TokenMath.get_debt_burn_scaled_amount(paybackAmount, index)`
3. Store this in the operation's extraction_data for enrichment to use

### Option C: Restore TransactionContext Storage (Minimal Change)
Restore the `last_repay_amount` field in TransactionContext (removed in commit a87f1fe4) and the associated pre-processing logic, but only for INTEREST_ACCRUAL debt burns:
```python
# In _process_debt_burn_with_match
if operation.operation_type == OperationType.INTEREST_ACCRUAL:
    if tx_context.last_repay_amount > 0:
        raw_amount = tx_context.last_repay_amount
        # Calculate scaled_amount using TokenMath
```

## Key Insight
The AGENTS.md lesson on INTEREST_ACCRUAL ("Interest accrual Mint events are emitted for tracking purposes ONLY and do NOT increase the scaled balance") is only half correct. While true for collateral mints during interest accrual, **debt burns during interest accrual DO reduce the scaled balance** and require proper scaled_amount calculation.

The confusion stems from assuming all INTEREST_ACCRUAL events are pure index updates. In reality:
- **Collateral mints** during interest accrual: scaled_amount = 0 (index update only)
- **Debt burns** during interest accrual: scaled_amount > 0 (actual balance reduction)

## Refactoring
1. **Operation Attachment**: Ensure INTEREST_ACCRUAL operations that represent debt burns are linked to their triggering REPAY event so enrichment can access paybackAmount.

2. **Enrichment Logic**: Modify `enrichment.py` to distinguish between:
   - Pure interest accrual (mint events where amount == balance_increase): scaled_amount = 0
   - Debt burns during repayment (burn events where amount != balance_increase): calculate scaled_amount from paybackAmount

3. **TokenMath Factory**: Ensure V4+ tokens use TokenMath for all INTEREST_ACCRUAL scaled amount calculations, not just direct REPAY operations.

4. **Transaction Processing**: Consider processing pool events (REPAY, WITHDRAW) before creating INTEREST_ACCRUAL operations to enable proper parent-child linking.

## Related Issues
- Issue #0001: V4 Debt Burn Rounding Error (original fix that was removed)
- Commit a87f1fe4: "refactor: remove `last_repay_amount` hack, attach interest accrual events to parent event if triggered"

## Files Involved
- `src/degenbot/aave/enrichment.py` - Lines 86-93 (INTEREST_ACCRUAL handling)
- `src/degenbot/cli/aave_transaction_operations.py` - Lines 1958-1974 (debt burn INTEREST_ACCRUAL creation)
- `src/degenbot/cli/aave.py` - Lines 3144-3155 (debt burn processing)
- `src/degenbot/cli/aave_types.py` - TransactionContext.last_repay_amount (removed field)
