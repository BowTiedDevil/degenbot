# Semantic Matching for Aave V3 Event Processing

## Overview

Semantic matching is an architectural approach for associating blockchain events with logical operations based on their **semantic relationships** (user, asset, operation type) rather than numeric comparisons or positional proximity.

## The Problem with Amount-Based Matching

### Unit Mismatch

Aave V3 events contain amounts in different units:

| Event Field | Unit | Example |
|-------------|------|---------|
| `debtToCover` (LIQUIDATION_CALL) | Underlying units | 8,414,088,469,488,124,792 |
| Burn event `amount` | Scaled units | 20,647,496 |
| Burn event `balanceIncrease` | Scaled units | 8,727 |

Direct comparison of these values fails because:
- Different assets have different decimals
- Scaled amounts require index-based conversion
- Pool revisions handle scaling differently

### Event Ordering Issues

In batch transactions, events may be emitted in any order:

```
LogIndex 350: DEBT_BURN (wstETH)
LogIndex 357: LIQUIDATION_CALL (GHO)
```

The burn precedes the liquidation call in log index order, making positional matching unreliable.

### False Negatives

Amount-based matching with tolerances can fail for:
- Bad debt liquidations (burn amount > debtToCover)
- Interest accrual (burn includes accrued interest)
- Multi-asset liquidations (debtToCover is for primary asset only)

## Semantic Matching Solution

### Core Principle

> If a debt burn exists for the same user and asset in the same transaction, it belongs to the liquidation.

This is based on the contract guarantee that:
1. Each user has at most one debt position per asset
2. Liquidations burn ALL debt positions for the user
3. No other operation burns debt in the same transaction

### Implementation

```python
def _collect_primary_debt_burns(...):
    for ev in scaled_events:
        # Filter by user
        if ev.user_address != user:
            continue
            
        # Filter by asset (token contract address)
        event_token_address = get_checksum_address(ev.event["address"])
        if event_token_address != debt_v_token_address:
            continue
            
        # Match! No amount comparison needed
        return ev
```

### Why This Works

1. **User uniqueness**: A user can't have two debt positions for the same asset
2. **Transaction isolation**: Events in the same transaction are causally related
3. **Contract logic**: The Pool contract burns ALL debts during liquidation

## Comparison: Amount-Based vs Semantic

### Scenario: Multi-Asset Liquidation

**Transaction:** User has GHO and wstETH debt, being liquidated

**Events:**
```
LogIndex 340: GHO_DEBT_BURN (primary)
LogIndex 350: DEBT_BURN wstETH (secondary)
LogIndex 357: LIQUIDATION_CALL debtAsset=GHO
```

**Amount-Based Matching:**
- ❌ GHO burn: 9,752,102,127,061,637 != 8,414,088,469,488,124,792 (debtToCover)
- ❌ wstETH burn: Skipped entirely (wrong asset)
- Result: Both burns unmatched, validation fails

**Semantic Matching:**
- ✅ GHO burn: User matches + Asset matches = Matched
- ✅ wstETH burn: User matches + Different asset = Secondary burn
- Result: All burns matched, validation passes

## Validation Strategy

Semantic matching separates concerns:

| Phase | Responsibility | Validation |
|-------|---------------|------------|
| **Matching** | Find related events | User + asset correspondence |
| **Processing** | Validate business logic | Amount sanity checks, balance updates |

Amount validation moves to processing:

```python
def _process_debt_burn(...):
    # Validate amounts make sense
    if burn_amount > position.balance * 2:
        logger.warning(f"Unusually large burn: {burn_amount}")
    
    # Apply the burn
    position.balance -= burn_amount
```

## Edge Cases and Considerations

### Multiple Burns Per Asset

**Assumption:** Only one burn per (user, asset) pair per transaction.

**If violated:** Second burn remains unmatched → validation error.

**Rationale:** Aave V3 contract burns the full debt balance in one operation. Multiple burns for the same asset would indicate unexpected contract behavior that should fail validation.

### Zero-Amount Burns

Some burns may have `amount=0` (e.g., pure interest accrual).

**Handling:** Semantic matching still applies. Zero-amount burns are matched and processed (no balance change).

### Reorgs and Event Ordering

Semantic matching is **reorg-safe** because it doesn't depend on log index proximity. As long as all events from the transaction are present, the matching succeeds regardless of order.

## Architecture Implications

### Benefits

1. **Simpler code**: No unit conversions or tolerance calculations
2. **More robust**: Works regardless of event ordering
3. **Easier debugging**: Clear semantic relationships vs complex math
4. **Future-proof**: Doesn't depend on revision-specific scaling behavior

### Trade-offs

1. **Less validation at match time**: Amount issues caught during processing
2. **Trusts contract behavior**: Assumes burns belong to liquidations if user+asset match
3. **Harder to detect partial liquidations**: Must check processing logic

## When to Use Semantic Matching

Use semantic matching when:
- Events have clear ownership (user + asset)
- Transaction context establishes causality
- Amount comparisons are unreliable (mixed units, revisions)
- Events may be out of order (batch transactions)

Don't use when:
- Multiple operations of same type for same user/asset in one transaction
- Amount matching provides critical validation
- Event relationships are truly ambiguous

## Implementation Guidelines

### 1. Match by Immutable Identifiers

Always use identifiers that won't change:
- ✅ User address
- ✅ Token contract address
- ✅ Operation type

Avoid:
- ❌ Amounts (unit confusion, rounding)
- ❌ Log indices (ordering issues)
- ❌ Timestamps (within same block)

### 2. Separate Matching from Validation

```python
# Matching: Find the event
matched_event = find_event(user=user, asset=asset)

# Validation: Check if it makes sense
if matched_event.amount > expected_maximum:
    raise ValidationError("Amount too large")
```

### 3. Separate Enrichment from Matching

Amount-based validation in the enrichment layer is **NOT** the same as amount-based matching:

**Enrichment/Validation (KEEP THIS):**
- Validates that calculated amounts are within expected tolerances
- Handles revision-specific rounding differences
- Ensures mathematical correctness
- Example: `abs(calculated - expected) <= TOKEN_AMOUNT_MATCH_TOLERANCE`

**Amount-Based Matching (REMOVE THIS):**
- Uses amounts to correlate events with operations
- Fragile due to unit mismatches and ordering issues
- Example: `if burn_amount != debt_to_cover: skip_event()`

**Rule of Thumb:**
- Use **semantic matching** (user + asset) to find related events
- Use **amount validation** (tolerances) to verify correctness after matching
- Never skip events based on amount mismatches during matching phase

### 4. Fail Loud on Unmatched Events

Don't silently ignore unmatched events:

```python
if not matched:
    raise ValidationError(f"Expected burn for {user}/{asset} not found")
```

### 5. Document Assumptions

Every semantic match should document the contract guarantee it relies on:

```python
# Assumption: Pool contract burns ALL debts during liquidation
# Ref: Pool.rev_7.sol:executeLiquidationCall() lines 2490-2620
```

## Anti-Patterns to Avoid

### 1. Log Index Proximity Matching

❌ **DON'T:** Assume events at adjacent log indices are related

```python
# FRAGILE: Assumes burn is at logIndex + 1
if other_ev.logIndex == ev.logIndex + 1:
    match_events()
```

✅ **DO:** Match by semantic identifiers regardless of position

```python
# ROBUST: Matches by user and token, any position
if other_ev.user == ev.user and other_ev.token == ev.token:
    match_events()
```

**Why:** Batch transactions emit events in unpredictable order. Log index proximity is coincidental, not causal.

### 2. Proximity Thresholds

❌ **DON'T:** Use arbitrary thresholds like "within 3 log indices"

```python
# FRAGILE: Magic number threshold
if abs(log_index_1 - log_index_2) <= 3:
    match_events()
```

✅ **DO:** Match by complete semantic identifiers

```python
# ROBUST: Complete identifier matching
if ev1.token == ev2.token and ev1.from_addr == ev2.from_addr and ev1.to_addr == ev2.to_addr:
    match_events()
```

**Why:** If token/from/to all match, the events are the same transfer regardless of position.

### 3. Amount-Based Event Correlation

❌ **DON'T:** Skip events because amounts don't match

```python
# FRAGILE: Amount-based skipping
if burn_amount < expected_amount:
    continue  # Skip this event!
```

✅ **DO:** Match by semantic criteria, validate amounts later

```python
# ROBUST: Semantic matching
if burn.user == user and burn.token == token:
    matched_burn = burn  # Always match
    
# Validate during processing
if matched_burn.amount > position.balance:
    logger.warning(f"Large burn amount: {matched_burn.amount}")
```

**Why:** Amounts can differ due to interest, fees, rounding. The semantic relationship (user + asset) is the ground truth.

## Migration Guide

### Converting Positional to Semantic Matching

**Before (Positional):**
```python
# Look for burn at next log index
for other_ev in events:
    if other_ev.logIndex == ev.logIndex + 1 and other_ev.type == BURN:
        match = other_ev
        break
```

**After (Semantic):**
```python
# Look for burn by user and token, any position
for other_ev in events:
    if (other_ev.type == BURN and 
        other_ev.user == ev.user and 
        other_ev.token == ev.token):
        match = other_ev
        break
```

**Testing:**
1. Run against batch transactions (50+ operations)
2. Verify events are matched regardless of order
3. Check no events remain unmatched
4. Validate final balances match on-chain state

## References

- **Issue 0029**: Multi-Asset Liquidation Missing Secondary Debt Burns Fix
- **Issue 0028**: Multi-Asset Debt Liquidation Missing Secondary Debt Burns
- **Contract**: `Pool.rev_7.sol:executeLiquidationCall()`
- **File**: `src/degenbot/cli/aave/transaction_processor.py`

## Related Patterns

- **Event Sourcing**: Semantic matching is similar to event sourcing where events are linked by entity IDs
- **CQRS**: Separating matching (read) from processing (write) follows CQRS principles
- **Idempotency**: Semantic matching makes processing idempotent (same events → same result)

---

*Last updated: 2026-03-18*  
*Author: Issue 0029 investigation team*
