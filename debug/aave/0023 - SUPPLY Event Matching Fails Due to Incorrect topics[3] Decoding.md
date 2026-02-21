---
title: SUPPLY Event Matching Fails Due to Incorrect topics[3] Decoding
category: aave
tags:
  - event-matching
  - supply
  - bug
complexity: standard
related_files:
  - ../../src/degenbot/cli/aave_event_matching.py
  - ../../src/degenbot/cli/aave.py
---

## Issue

SUPPLY event matching fails when processing collateral mint events because the code incorrectly decodes `topics[3]` as an address when it is actually the referral code.

## Date

2025-02-21

## Symptom

```
ValueError: No matching Pool event for collateral mint in tx 6c6bfbc0b41fdbb1f4cb5bff1f47e3ff42d86e7aa299605abcb387d3e8bce123. User: 0x9cCf93089cb14F94BAeB8822F8CeFfd91Bd71649, Reserve: 0x6B175474E89094C44Da98b954EedeAC495271d0F. Available: ['2b627736bc', 'b3d084820f']
```

## Root Cause

The Aave V3 SUPPLY event has the following structure:

```solidity
event Supply(
    address indexed reserve,        // topics[1]
    address indexed onBehalfOf,     // topics[2]
    uint16 indexed referralCode,    // topics[3] - NOT an address!
    address caller,                 // data (offset 0)
    uint256 amount                  // data (offset 32)
);
```

The buggy code at `aave_event_matching.py:376-394` incorrectly assumed:
1. `topics[2]` was the user address
2. `topics[3]` was the `onBehalfOf` address

In reality:
1. `topics[2]` is `onBehalfOf` (the user address to match against)
2. `topics[3]` is `referralCode` (uint16, not an address)

When the code tried to decode `topics[3]` as an address, it produced `0x0000000000000000000000000000000000000040` (referral code 64), which failed to match the expected user address.

## Transaction Details

- **Hash:** `0x6c6bfbc0b41fdbb1f4cb5bff1f47e3ff42d86e7aa299605abcb387d3e8bce123`
- **Block:** 16497464
- **Type:** DSProxy Recipe Execution (DeFi Saver)
- **User:** `0x9cCf93089cb14F94BAeB8822F8CeFfd91Bd71649`
- **Asset:** `0x6B175474E89094C44Da98b954EedeAC495271d0F` (DAI)
- **Operation:** Supply 90 DAI

The SUPPLY event at log index 0x75 had:
- `topics[1]`: `0x6B175474E89094C44Da98b954EedeAC495271d0F` (DAI reserve)
- `topics[2]`: `0x9cCf93089cb14F94BAeB8822F8CeFfd91Bd71649` (onBehalfOf = user)
- `topics[3]`: `0x0000000000000000000000000000000000000000000000000000000000000040` (referralCode = 64)

## Fix

**File:** `src/degenbot/cli/aave_event_matching.py`  
**Lines:** 376-394

**Before:**
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

**After:**
```python
elif expected_type == AaveV3Event.SUPPLY:
    # SUPPLY: topics[1]=reserve, topics[2]=onBehalfOf, topics[3]=referralCode
    # Aave V3 Supply event format (4 topics):
    #   Supply(address indexed reserve, address indexed onBehalfOf,
    #          uint16 indexed referralCode, address caller, uint256 amount)
    # topics[3] is referralCode (uint16), NOT an address - do not decode as address!
    event_reserve = _decode_address(pool_event["topics"][1])
    event_on_behalf_of = _decode_address(pool_event["topics"][2])
    return event_on_behalf_of == user_address and event_reserve == reserve_address
```

## Key Insight

When debugging event matching issues, always verify the exact event signature and topic structure against the deployed contract ABI. The Aave V3 SUPPLY event has 4 indexed topics, where `topics[3]` is a `uint16` referral code, not an address. Decoding non-address data as addresses produces incorrect values that fail matching.

## Refactoring

1. **Add ABI-based validation:** Consider validating event topic counts and types against the contract ABI to catch mismatches early.

2. **Add event structure documentation:** Document the exact topic structure for each Aave V3 event type in code comments to prevent future confusion.

3. **Add regression tests:** Create unit tests that verify event matching logic using actual on-chain event data to catch regressions.

4. **Improve error messages:** Include decoded topic values in error messages to make debugging easier when matching fails.
