# Bug #0022: SUPPLY Event Matching Fails with Zero onBehalfOf Address

**Date:** 2026-02-21

**Issue:** COLLATERAL_MINT events fail to match Pool SUPPLY events when using the Wrapped Token Gateway, causing ValueError and blocking Aave market updates.

---

## Symptom

```
No matching event found for COLLATERAL_MINT. Tried users: ['0x7FA5195595EFE0dFbc79f03303448af3FbE4ea91', '0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C'], reserve: 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2. Available pool events: 1
```

---

## Root Cause

The Aave V3 Pool's **SUPPLY** event structure has 4 indexed topics:
- `topics[0]`: Event signature
- `topics[1]`: Reserve (asset address)
- `topics[2]`: User (actual beneficiary)
- `topics[3]`: OnBehalfOf (who the supply is on behalf of)

When using the **Wrapped Token Gateway**:
- The Gateway calls `Pool.supply()` on behalf of the user
- The Pool sets:
  - `user` = actual beneficiary (e.g., 0x7FA5...)
  - `onBehalfOf` = **zero address** (0x0000...)

The old matching logic only checked `onBehalfOf`, which was zero, causing matches to fail even though the `user` topic contained the correct beneficiary address.

---

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | 0xa4a5f3993fd60bd01665f8389c1c5cded8cfed0007de913142cd9a8bb0f13117 |
| **Block** | 16496817 |
| **Chain** | Ethereum Mainnet |
| **Type** | ETH deposit via Wrapped Token Gateway |
| **User** | 0x7FA5195595EFE0dFbc79f03303448af3FbE4ea91 |
| **Gateway** | 0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C |
| **Asset** | 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2 (WETH) |

### Event Sequence

```
Log 0: WETH Deposit (ETH → WETH)
Log 1: ReserveDataUpdated (interest rates)
Log 2: WETH Transfer (Gateway → aWETH)
Log 3: aWETH Transfer (mint to user)
Log 4: aWETH Mint (COLLATERAL_MINT - caller=Gateway, onBehalfOf=User)
Log 5: ReserveUsedAsCollateralEnabled
Log 6: Pool Supply (SUPPLY - user=User, onBehalfOf=ZERO ADDRESS)
```

---

## Fix

Two changes were required to fix this issue:

### Fix 1: Zero Address Handling in SUPPLY Matching

**File:** `src/degenbot/cli/aave_event_matching.py`

**Location:** `_matches_pool_event()` method, SUPPLY handling (lines 376-394)

**Change:** Modified the SUPPLY event matching to handle both 4-topic and 3-topic formats, with special handling for zero onBehalfOf address:

```python
elif expected_type == AaveV3Event.SUPPLY:
    # SUPPLY: topics[1]=reserve, topics[2]=user, topics[3]=onBehalfOf (if present)
    # Old format (pre-V3): topics[1]=reserve, topics[2]=onBehalfOf (no user topic)
    event_reserve = _decode_address(pool_event["topics"][1])

    # Check if we have the new 4-topic format or old 3-topic format
    if len(pool_event["topics"]) >= 4:
        # New format: topics[2]=user, topics[3]=onBehalfOf
        event_user = _decode_address(pool_event["topics"][2])
        event_on_behalf_of = _decode_address(pool_event["topics"][3])
        # When onBehalfOf is zero address, match on user (handles Wrapped Token Gateway)
        # Otherwise match on onBehalfOf (standard supply flow)
        if event_on_behalf_of == "0x0000000000000000000000000000000000000000":
            return event_user == user_address and event_reserve == reserve_address
        return event_on_behalf_of == user_address and event_reserve == reserve_address
    else:
        # Old format: topics[2]=onBehalfOf (no separate user topic)
        event_on_behalf_of = _decode_address(pool_event["topics"][2])
        return event_on_behalf_of == user_address and event_reserve == reserve_address
```

### Fix 2: Event Order Handling (max_log_index)

**File:** `src/degenbot/cli/aave.py`

**Location:** `_process_collateral_mint_event()` method (lines 3046-3066)

**Issue:** The SUPPLY event comes **after** the Mint event in transaction logs (Mint at logIndex 434, SUPPLY at logIndex 436), but the code was passing `max_log_index=event["logIndex"]` which filtered out the SUPPLY event.

**Change:** Removed `max_log_index` restriction for deposit operations (when `event_amount >= balance_increase`), since SUPPLY events legitimately occur after Mint events:

```python
if event_amount >= balance_increase:
    # Standard deposit - SUPPLY is most likely
    # Note: SUPPLY events come AFTER Mint events in transaction logs, so we
    # don't restrict by max_log_index for deposits.
    result = matcher.find_matching_pool_event(
        event_type=ScaledTokenEventType.COLLATERAL_MINT,
        user_address=user.address,
        reserve_address=reserve_address,
        check_users=[caller_address],
        # No max_log_index - SUPPLY comes AFTER Mint event
    )
else:
    # balance_increase > value - interest accrual during withdraw
    # WITHDRAW events come BEFORE Burn events, so restrict by max_log_index
    result = matcher.find_matching_pool_event(
        event_type=ScaledTokenEventType.COLLATERAL_MINT,
        user_address=caller_address,
        reserve_address=reserve_address,
        check_users=[user.address],
        max_log_index=event["logIndex"],
    )
```

---

## Key Insight

The **Wrapped Token Gateway** is a helper contract that:
1. Receives ETH from users
2. Wraps it to WETH
3. Calls `Pool.supply()` on behalf of the user

In this flow, the Pool distinguishes between:
- **Direct supply**: `caller` supplies on their own behalf
- **Gateway/delegated supply**: `caller` (Gateway) supplies with `user` as beneficiary

The Pool's event semantics:
- `user` = actual beneficiary of the supply
- `onBehalfOf` = zero address when `caller` != `user` (indicates direct beneficiary)
- `onBehalfOf` = non-zero when supplying on behalf of a third party

The fix correctly handles both cases by checking if `onBehalfOf` is zero and falling back to matching on `user`.

---

## Testing

New tests added in `tests/cli/test_aave_supply_zero_address_matching.py`:
- `test_gateway_supply_with_zero_on_behalf_of_matches_user` - Verifies Gateway deposits work
- `test_direct_supply_with_non_zero_on_behalf_of_matches_on_behalf_of` - Verifies direct supplies still work
- `test_supply_with_different_user_and_on_behalf_of` - Tests delegated supplies
- `test_no_match_when_user_and_on_behalf_of_both_dont_match` - Verifies non-matching fails correctly

All 37 event matching tests pass (33 existing + 4 new).

---

## Refactoring

The EventMatcher class provides a centralized, declarative approach to Aave event matching. Future improvements could include:

1. **Event signature validation** - Verify topic lengths match expected event formats at startup
2. **Explicit handling patterns** - Document the various gateway/router patterns (Wrapped Token Gateway, ParaSwap adapters, etc.)
3. **Test coverage** - Add integration tests for each major flow type:
   - Direct supplies/withdrawals
   - Gateway ETH deposits
   - Adapter-based interactions
   - Liquidations
   - Repay with aTokens

---

## Related

- Transaction: [0xa4a5f399... on Etherscan](https://etherscan.io/tx/0xa4a5f3993fd60bd01665f8389c1c5cded8cfed0007de913142cd9a8bb0f13117)
- Wrapped Token Gateway V3: [0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C](https://etherscan.io/address/0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C)
- Aave V3 Pool: [0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2](https://etherscan.io/address/0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2)
