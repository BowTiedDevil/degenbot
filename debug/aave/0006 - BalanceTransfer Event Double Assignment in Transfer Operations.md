# 0006 - BalanceTransfer Event Double Assignment in Transfer Operations

**Issue:** BalanceTransfer Event Double Assignment in Transfer Operations

**Date:** 2026-03-14

## Symptom
```
Transaction validation failed:
Event at logIndex 472 assigned to multiple operations: 1 and 2. DEBUG NOTE: This event may need to be reusable. Investigate whether it can match multiple operations (e.g., LIQUIDATION_CALL or REPAY with useATokens).
```

## Root Cause
In `aave_transaction_operations.py::_create_transfer_operations()`, the code that checks if ERC20 Transfers to zero address are part of burns (lines 2199-2218) has a bug: it always `continue`s after the check, even when no Burn event is found. This prevents ERC20 Transfers to zero address from being processed as balance transfers.

When the ERC20 Transfer at logIndex 470 (transfer to zero address) is skipped, the matching BalanceTransfer at logIndex 472 is never paired with it. Instead, the BalanceTransfer is processed separately in:
1. The first loop (creating Operation 1)
2. The second "standalone" loop (creating Operation 2)

## Transaction Details
- **Hash:** 0x0c97184f68e957d619f2c3696ed7be447c2ff952ed1d0f68a6761821fa5ffca1
- **Block:** 21914037
- **Type:** aToken transfer (burn to zero address)
- **User:** 0xD40D51857AC0c4eD1ADe039d9902EB7FAea2C4C7
- **Asset:** rETH (0xae78736cd615f374d3085123a210448e74fc6393)
- **aToken:** 0xCc9EE9483f662091a1de4795249E24aC0aC2630f (rev_1)
- **Pool:** 0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2 (rev_6)

## Events
1. **logIndex 468** - ERC20 Transfer (interest accrual, amount=2)
2. **logIndex 469** - Mint event (interest accrual, balance_increase=2)
3. **logIndex 470** - ERC20 Transfer from user to 0x0...0 (burn, amount=913141676)
4. **logIndex 472** - BalanceTransfer from user to 0x0...0 (scaled amount=911746220)

## Fix
File: `src/degenbot/cli/aave_transaction_operations.py`
Lines: 2199-2218

**Primary Fix:** Only `continue` when a Burn event is actually found:
```python
if is_erc20_transfer and ev.target_address == ZERO_ADDRESS:
    # Check if there's a Burn event at the next log index for the same user
    is_part_of_burn = False
    for other_ev in scaled_events:
        if (
            other_ev.event["logIndex"] == ev.event["logIndex"] + 1
            and other_ev.event_type
            in {
                ScaledTokenEventType.DEBT_BURN,
                ScaledTokenEventType.COLLATERAL_BURN,
                ScaledTokenEventType.GHO_DEBT_BURN,
            }
            and other_ev.user_address == ev.from_address
        ):
            # This transfer is part of a burn, skip it
            is_part_of_burn = True
            local_assigned.add(ev.event["logIndex"])
            break
    if is_part_of_burn:
        continue
```

**Secondary Fix (lines 2296-2298):** When finding a BalanceTransfer in existing operations, mark it as assigned:
```python
# Found matching BalanceTransfer in existing operation
balance_transfer_event = bt_ev
local_assigned.add(bt_ev.event["logIndex"])  # Added line
break
```

## Key Insight
The `continue` statement was indented incorrectly (outside the inner loop), causing it to always execute when the target was ZERO_ADDRESS, regardless of whether a Burn event was found. This is a subtle control flow bug that only manifests when transfers are made to zero address without an associated Burn event (which happens when users directly transfer aTokens to zero address as a burn mechanism).

## Refactoring
1. **Control Flow Review:** When using nested loops with `break` and `continue`, carefully review the indentation to ensure control flow logic is correct.

2. **Test Coverage:** Add test cases for:
   - ERC20 Transfer to zero address without Burn event (direct aToken burn)
   - ERC20 Transfer to zero address with Burn event (protocol burn)
   - BalanceTransfer paired with ERC20 Transfer
   - Standalone BalanceTransfer events

3. **Code Clarity:** Consider using a boolean flag like `is_part_of_burn` to make the conditional logic explicit and avoid subtle indentation bugs.
