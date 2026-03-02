# Issue 0015: Interest Accrual Mint During Liquidation Not Processed

**Date:** 2026-03-01

## Symptom

```
AssertionError: User 0x09D86D566092bEc46D449e72087ee788937599D2: debt balance (1148052151540) does not match scaled token contract (1127419588420) @ 0x72E95b8931767C79bA4EeE721354d6E99a61D004 at block 19648924
```

The calculated debt balance was 20,632,563,120 units higher than the actual contract balance.

## Root Cause

During Aave liquidations, when a user's debt position is touched (e.g., during `liquidationCall`), the protocol first accrues interest by minting additional debt tokens to the user before burning the liquidated amount. This is handled internally in the `VariableDebtToken.burn()` function which calls `_accrueDebtOnAction()` before processing the burn.

The code in `aave_transaction_operations.py` was incorrectly skipping all `DEBT_MINT` events during liquidation transactions (line 1300-1301):

```python
if ev.event_type in {"DEBT_MINT", "GHO_DEBT_MINT"}:
    if has_liquidation:
        continue  # This was skipping interest accrual mints!
```

This meant that interest accrual mints during liquidations were not processed, causing the tracked debt balance to be higher than the actual contract balance (since interest was not added to the user's position).

## Transaction Details

- **Hash:** 0xcb087ea4d8d1b7c890318c3eccd7f730f24a1f1b55b25c156b9649e543de0588
- **Block:** 19648924
- **Type:** Liquidation
- **User:** 0x09D86D566092bEc46D449e72087ee788937599D2
- **Liquidator:** 0x8ce45e650ab17b6ca0dd6071f7c2b5c69b5b42b2
- **Debt Asset:** USDC (0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48)
- **Collateral Asset:** SNX (0xc011a73ee8576fb46f5e1c5751ca3b9fe0af2a6f)
- **Debt Token:** variableDebtUSDC (0x72E95b8931767C79bA4EeE721354d6E99a61D004)
- **Interest Mint Amount:** 11,627,951,177 (0x2b5147449)

### Event Sequence

1. `Transfer` event from address(0) to user (mint of 11,627,951,177 tokens)
2. `Mint` event (caller=user, onBehalfOf=user, value=11,627,951,177, balanceIncrease=33,823,939,319)
3. `ReserveDataUpdated` for USDC reserve
4. `LiquidationCall` event
5. Collateral burn and transfer events

The Mint event represented interest accrual that occurred when the debt position was accessed during liquidation.

## Fix

**File:** `src/degenbot/cli/aave_transaction_operations.py`
**Lines:** 1299-1313

Changed the logic to only skip non-interest-accrual mints during liquidations. Interest accrual mints (where `balance_increase >= amount`) should always be processed regardless of transaction type:

```python
if ev.event_type in {"DEBT_MINT", "GHO_DEBT_MINT"}:
    # Interest accrual: balance_increase >= amount
    is_interest_accrual = ev.balance_increase >= ev.amount
    if is_interest_accrual:
        # Process as INTEREST_ACCRUAL
        pass
    elif has_liquidation:
        # Skip non-interest mints during liquidation (e.g., flash borrows)
        continue
    elif has_borrow:
        # Skip DEBT_MINT during borrow (flash loan) if not interest accrual
        continue
    elif has_repay and not has_collateral_burn:
        # Skip DEBT_MINT during REPAY if not interest accrual
        continue
```

## Key Insight

Interest accrual in Aave happens whenever a user's position is touched, including during liquidations. The smart contract handles this internally by minting additional debt tokens to represent accrued interest before processing the principal operation (burn, transfer, etc.). These mint events must be processed to maintain accurate balance tracking.

The distinction between interest accrual and flash borrow is critical:
- **Interest accrual:** `balance_increase >= amount` (should always be processed)
- **Flash borrow:** `balance_increase < amount` (should be skipped during liquidations as it's part of flash loan mechanics)

## Refactoring

The current logic for handling DEBT_MINT events is complex and distributed across multiple conditionals. Consider refactoring to:

1. Create a dedicated `InterestAccrualClassifier` that determines if a mint event represents interest accrual, flash borrow, or other protocol operations
2. Move the classification logic out of `_create_interest_accrual_operations` into a separate function for better testability
3. Add explicit handling for liquidation-specific events with clear documentation of why certain mints are skipped vs processed
4. Consider using pattern matching for event classification instead of nested conditionals

## Test

Added test case to verify that interest accrual mints during liquidations are correctly processed:

```python
def test_liquidation_with_interest_accrual_mint(self):
    """Liquidation correctly processes interest accrual mint events."""
    # Regression test for issue #0015
    # Transaction: 0xcb087ea4d8d1b7c890318c3eccd7f730f24a1f1b55b25c156b9649e543de0588
    user = get_checksum_address("0x09D86D566092bEc46D449e72087ee788937599D2")
    collateral_asset = get_checksum_address("0xc011a73ee8576fb46f5e1c5751ca3b9fe0af2a6f")
    debt_asset = get_checksum_address("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")

    # Interest accrual mint (balance_increase >= amount)
    interest_mint_event = EventFactory.create_debt_mint_event(
        user=user,
        amount=11627951177,
        balance_increase=33823939319,  # >= amount, indicates interest accrual
        log_index=102,
    )

    liquidation_event = EventFactory.create_liquidation_call_event(
        collateral_asset=collateral_asset,
        debt_asset=debt_asset,
        user=user,
        debt_to_cover=22195988142,
        liquidated_collateral=9048585995794641865737,
        log_index=100,
    )

    collateral_burn_event = EventFactory.create_collateral_burn_event(
        user=user,
        amount=9048585995794641865737,
        balance_increase=11005547540567006197,
        log_index=104,
    )

    parser = TransactionOperationsParser(token_type_mapping=TEST_TOKEN_TYPE_MAPPING)
    tx_ops = parser.parse(
        [interest_mint_event, liquidation_event, collateral_burn_event],
        HexBytes("0x" + "00" * 32),
    )

    # Should have 2 operations: INTEREST_ACCRUAL and LIQUIDATION
    assert len(tx_ops.operations) == 2
    
    interest_op = tx_ops.operations[0]
    assert interest_op.operation_type == OperationType.INTEREST_ACCRUAL
    assert len(interest_op.scaled_token_events) == 1
    
    liquidation_op = tx_ops.operations[1]
    assert liquidation_op.operation_type == OperationType.LIQUIDATION
    assert len(liquidation_op.scaled_token_events) == 1  # collateral burn

    # Validation should pass
    tx_ops.validate([interest_mint_event, liquidation_event, collateral_burn_event])
```
