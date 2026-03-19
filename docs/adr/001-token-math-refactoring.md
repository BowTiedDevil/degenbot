# ADR-001: TokenMath Refactoring

**Status:** Accepted  
**Date:** 2026-03-19  
**Deciders:** Refactoring Team  

## Context

The Aave V3 processing system had accumulated technical debt around math calculations:

1. **Confusing naming:** `TokenMathV1`, `TokenMathV4`, `TokenMathV5` didn't correspond to Pool revisions
2. **Mixed concerns:** Pool-level operations (MINT_TO_TREASURY) were conflated with Token-level operations
3. **Direct imports:** CLI code imported `ray_div`, `ray_div_ceil`, `wad_mul` directly from `wad_ray_math`
4. **No clear separation:** Pool and Token revisions were treated as the same thing

## Problem Statement

The existing architecture made it difficult to:
- Understand which rounding mode to use for which operation
- Debug rounding errors (Issues 0034, 0036)
- Add new pool/token revision combinations
- Maintain the codebase as Aave V3 evolved

## Decision

We decided to:

1. **Rename TokenMath classes** to behavioral names (`HalfUpRoundingMath`, `ExplicitRoundingMath`)
2. **Create PoolMath** for Pool-level operations
3. **Create GhoMath** for GHO-specific calculations
4. **Use factories** for revision-to-implementation mapping
5. **Eliminate direct imports** of ray/wad math in CLI

## Consequences

### Positive

- **Clear intent:** Method names describe behavior, not version numbers
- **Proper separation:** Pool and Token math are now distinct
- **Easier debugging:** Single source of truth for each calculation type
- **Type safety:** Protocol-based design with proper type hints
- **Testability:** Easier to test individual math functions

### Negative

- **Breaking change:** Old `TokenMathV1/V4/V5` names removed immediately
- **Learning curve:** New architecture takes time to understand
- **More files:** Three math modules instead of one

## Implementation

### Phase 1: Rename TokenMath Classes

Changed from version-based naming:
```python
class TokenMathV1:  # Removed
class TokenMathV4:  # Removed
class TokenMathV5:  # Removed
```

To behavioral naming:
```python
class HalfUpRoundingMath:  # Revs 1-3
class ExplicitRoundingMath:  # Revs 4+
```

### Phase 2: Create PoolMath

Created `src/degenbot/aave/libraries/pool_math.py`:
```python
class PoolMath:
    @staticmethod
    def get_treasury_mint_amount(accrued, index, pool_revision) -> int
    @staticmethod
    def underlying_to_scaled_collateral(amount, index, pool_revision) -> int
    @staticmethod
    def underlying_to_scaled_debt(amount, index, pool_revision) -> int
```

PoolMath handles revision-specific rounding:
- Rev 1-8: Half-up rounding
- Rev 9+: Floor/ceil rounding

### Phase 3: Create GhoMath

Created `src/degenbot/aave/libraries/gho_math.py`:
```python
class GhoMath:
    @staticmethod
    def calculate_discount_rate(debt, discount_token) -> int
    @staticmethod
    def calculate_discounted_balance(debt, discount_token) -> int
    @staticmethod
    def calculate_effective_debt_balance(debt, discount_token) -> int
```

### Phase 7: Add Reverse Methods

Added to TokenMath protocol:
```python
def get_scaled_from_underlying_collateral(underlying, index) -> int
def get_scaled_from_underlying_debt(underlying, index) -> int
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    CLI Layer                                │
├─────────────────────────────────────────────────────────────┤
│  calculate_gho_discount_rate()                              │
│  _calculate_mint_to_treasury_scaled_amount()               │
└──────────────────┬──────────────────────────────────────────┘
                   │
         ┌─────────┴──────────┐
         │                    │
         ▼                    ▼
┌────────────────┐  ┌─────────────────┐  ┌─────────────────┐
│   GhoMath      │  │   PoolMath      │  │  TokenMath      │
├────────────────┤  ├─────────────────┤  ├─────────────────┤
│calculate_      │  │get_treasury_    │  │get_collateral_  │
│discount_rate() │  │mint_amount()    │  │mint_scaled_     │
│                │  │                 │  │amount()         │
│calculate_      │  │underlying_to_   │  │                 │
│discounted_     │  │scaled_...()     │  │get_debt_mint_   │
│balance()       │  │                 │  │scaled_amount()  │
└────────────────┘  └─────────────────┘  └─────────────────┘
         │                    │                    │
         └────────────────────┼────────────────────┘
                              │
                              ▼
                    ┌─────────────────────┐
                    │  wad_ray_math       │
                    ├─────────────────────┤
                    │ray_mul, ray_div     │
                    │ray_mul_floor, etc.  │
                    └─────────────────────┘
```

## References

- Refactoring Plan: `docs/refactoring/TOKEN_MATH_REFACTOR_PLAN.md`
- Issue 0034: Pool Rev 9 MINT_TO_TREASURY rounding
- Issue 0036: Pool Rev 8 MINT_TO_TREASURY rounding
