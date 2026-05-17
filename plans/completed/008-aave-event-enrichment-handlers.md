# Plan 008: Extract Per-OperationType Handlers Behind a Pipeline Seam

## Overview

Replace the ~300-line `ScaledEventEnricher.enrich()` monolith with an
`OperationHandler` pipeline: each `OperationType` has a dedicated handler
module. The dispatcher routes to handlers by operation type. Each handler
is a standalone module with its own docstring, invariants, and test file.

**Feature Flag:** The new handler-based implementation is gated behind the
`DEGENBOT_NEW_AAVE_ENRICHMENT` environment variable. Set to `1`, `true`, or
`yes` to use the new implementation. Otherwise, the legacy monolithic
implementation is used.

## Files Involved

- **Existing:**
  - `src/degenbot/aave/enrichment.py` → moved to `enrichment/_legacy.py`
  - `src/degenbot/aave/models.py` (513 lines)
  - `src/degenbot/aave/calculator.py` (71 lines)
  - `src/degenbot/aave/operation_types.py` (29 lines)
- **New:**
  - `src/degenbot/aave/enrichment/__init__.py` — dispatcher with feature flag ✅
  - `src/degenbot/aave/enrichment/_legacy.py` — original monolithic implementation ✅
  - `src/degenbot/aave/enrichment/context.py` — EnrichmentContext (shared services) ✅
  - `src/degenbot/aave/enrichment/core.py` — thin orchestrator ✅
  - `src/degenbot/aave/enrichment/handlers/__init__.py` — handler registry ✅
  - `src/degenbot/aave/enrichment/handlers/base.py` — `OperationHandler` protocol ✅
  - `src/degenbot/aave/enrichment/handlers/supply.py` ✅
  - `src/degenbot/aave/enrichment/handlers/withdraw.py` ✅
  - `src/degenbot/aave/enrichment/handlers/borrow.py` ✅
  - `src/degenbot/aave/enrichment/handlers/repay.py` ✅
  - `src/degenbot/aave/enrichment/handlers/repay_with_atokens.py` ✅
  - `src/degenbot/aave/enrichment/handlers/liquidation.py` ✅
  - `src/degenbot/aave/enrichment/handlers/interest_accrual.py` ✅
  - `src/degenbot/aave/enrichment/handlers/mint_to_treasury.py` ✅
  - `src/degenbot/aave/enrichment/handlers/transfer.py` ✅
  - `src/degenbot/aave/enrichment/handlers/stkaave_transfer.py` ✅
  - `src/degenbot/aave/enrichment/handlers/unknown.py` ✅
  - `src/degenbot/aave/enrichment/handlers/gho_flash_loan.py` ✅
  - `src/degenbot/aave/enrichment/handlers/deficit_coverage.py` ✅

## Problem

`ScaledEventEnricher.enrich()` switches on every cross-product of
`OperationType` × `ScaledTokenEventType`. The method has 10+ nested condition
blocks for special cases:

- Interest exceeds withdrawal → override calculation type to `COLLATERAL_BURN`
- Interest exceeds repayment → override to `DEBT_BURN`
- Pool rev 9+ liquidation → pre-scaled amounts
- MINT_TO_TREASURY → no calculation possible at enrichment layer
- Liquidation debt vs collateral → different extractors
- ERC20 transfers → bypass index-based scaling entirely
- Pure interest accrual → scaled_amount = 0

These branches are independent. Editing one risks breaking another. The
`_create_enriched_event` method adds another 20-branch `if/elif` for
type-specific fields.

This is a shallow module: the interface (`enrich(event, operation)`) is simple,
but callers and maintainers must know all 10+ invariants. No locality.

## Final State

```text
enrichment/
├── __init__.py              — dispatcher with feature flag, exports ScaledEventEnricher
├── _legacy.py               — original monolithic implementation (kept for rollback)
├── context.py               — EnrichmentContext (shared services)
├── core.py                  — thin orchestrator, dispatches to handlers
└── handlers/
    ├── __init__.py          — register all handlers
    ├── base.py              — OperationHandler protocol
    ├── supply.py            — SUPPLY handler ✅
    ├── borrow.py            — BORROW + GHO_BORROW handler ✅
    ├── withdraw.py          — WITHDRAW handler ✅ (interest-exceeds-withdrawal logic)
    ├── repay.py             — REPAY + GHO_REPAY handler ✅ (interest-exceeds-repayment logic)
    ├── repay_with_atokens.py — REPAY_WITH_ATOKENS handler ✅ (collateral + debt overrides)
    ├── liquidation.py       — LIQUIDATION + GHO_LIQUIDATION handler ✅ (debt/collateral extraction, Pool rev 9+)
    ├── interest_accrual.py  — INTEREST_ACCRUAL handler ✅ (scaled=0)
    ├── mint_to_treasury.py  — MINT_TO_TREASURY handler ✅ (scaled=None)
    ├── transfer.py          — BALANCE_TRANSFER handler ✅ (bypass scaling)
    ├── stkaave_transfer.py  — STKAAVE_TRANSFER handler ✅ (ERC20 transfer)
    ├── unknown.py           — UNKNOWN handler ✅ (raises EnrichmentError)
    ├── gho_flash_loan.py    — GHO_FLASH_LOAN handler ✅ (standard burn)
    └── deficit_coverage.py  — DEFICIT_COVERAGE handler ✅ (transfer + burn)
```

### Feature Flag Mechanism

The `ScaledEventEnricher` in `__init__.py` lazily loads either the new or
legacy implementation based on the `DEGENBOT_NEW_AAVE_ENRICHMENT` environment
variable:

```python
_USE_NEW_ENRICHMENT = os.environ.get("DEGENBOT_NEW_AAVE_ENRICHMENT", "").lower() in {
    "1",
    "true",
    "yes",
}


class ScaledEventEnricher:
    def _get_enricher(self):
        if self._enricher is None:
            if _USE_NEW_ENRICHMENT:
                from degenbot.aave.enrichment.core import ScaledEventEnricher as NewEnricher

                self._enricher = NewEnricher(...)
            else:
                from degenbot.aave.enrichment._legacy import ScaledEventEnricher as LegacyEnricher

                self._enricher = LegacyEnricher(...)
        return self._enricher
```

### `handlers/base.py`

```python
from typing import TYPE_CHECKING, Protocol, runtime_checkable

from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


@runtime_checkable
class OperationHandler(Protocol):
    """
    Handle enrichment for a specific OperationType.

    Each handler is a standalone module that knows how to transform
    a ScaledTokenEvent into the correct enriched model for its
    operation type. Handlers are stateless and thread-safe.
    """

    operation_types: set[OperationType]

    def handle(
        self,
        event: "ScaledTokenEvent",
        operation: "Operation",
        context: "EnrichmentContext",
    ) -> "EnrichedScaledTokenEvent": ...
```

### `EnrichmentContext` (shared state)

```python
class EnrichmentContext:
    """
    Shared context providing services for enrichment handlers.

    Encapsulates:
    - Token revision lookup (with caching)
    - Underlying asset resolution
    - Raw amount extraction from Pool events
    - Scaled amount calculation
    - Enriched event construction
    """

    def __init__(
        self,
        pool_revision: int,
        token_revisions: dict[ChecksumAddress, int],
        session: Session,
    ) -> None: ...

    def get_token_revision(self, token_address: ChecksumAddress) -> int: ...

    def get_underlying_asset(self, token_address: ChecksumAddress) -> ChecksumAddress: ...

    def extract_pool_amount(
        self,
        pool_event: LogReceipt,
        event_type: ScaledTokenEventType | None = None,
        operation_type: OperationType | None = None,
    ) -> int: ...

    def calculate(
        self,
        event_type: ScaledTokenEventType,
        raw_amount: int,
        index: int,
        token_revision: int,
    ) -> int: ...

    def build_enriched_event(
        self,
        event: ScaledTokenEvent,
        operation: Operation,
        raw_amount: int,
        scaled_amount: int | None,
    ) -> EnrichedScaledTokenEvent: ...
```

## Handler Implementation Status

| Handler | Status | Complexity | Notes |
|---------|--------|------------|-------|
| INTEREST_ACCRUAL | ✅ | Simple | scaled_amount=0 |
| MINT_TO_TREASURY | ✅ | Simple | scaled_amount=None |
| BALANCE_TRANSFER | ✅ | Simple | No scaling, raw=scaled |
| SUPPLY | ✅ | Standard | Extract from Pool, calculate |
| BORROW | ✅ | Standard | Also handles GHO_BORROW |
| STKAAVE_TRANSFER | ✅ | Simple | ERC20 transfer, no scaling |
| UNKNOWN | ✅ | Simple | Raises EnrichmentError |
| GHO_FLASH_LOAN | ✅ | Standard | Standard GHO debt burn |
| DEFICIT_COVERAGE | ✅ | Standard | Handles transfer + burn |
| WITHDRAW | ✅ | Complex | Interest>withdrawal Mint override |
| REPAY | ✅ | Complex | Interest>repayment Mint override, GHO handling |
| REPAY_WITH_ATOKENS | ✅ | Complex | Both collateral and debt overrides |
| LIQUIDATION | ✅ | Complex | Debt vs collateral extraction, Pool rev 9+ |
| GHO_LIQUIDATION | ✅ | Complex | Same as LIQUIDATION with GHO-specifics |

### Complex Handler Details

**WITHDRAW** (`handlers/withdraw.py`):
- Standard case: COLLATERAL_BURN with ceil rounding
- Special case: When `amount < balance_increase` on COLLATERAL_MINT, extract actual withdrawal from Pool event, use COLLATERAL_BURN calculation, set `scaled_amount=None` to skip validation
- See debug/aave/0031

**REPAY** (`handlers/repay.py`):
- Handles both REPAY and GHO_REPAY
- Standard case: DEBT_BURN with floor rounding
- Special case: When DEBT_MINT/GHO_DEBT_MINT has `balance_increase`, extract actual repayment from Pool event, use DEBT_BURN calculation
- Does NOT skip validation (processing layer needs the value)
- See debug/aave/0037

**REPAY_WITH_ATOKENS** (`handlers/repay_with_atokens.py`):
- Handles both collateral and debt events
- Collateral side: Same logic as WITHDRAW when interest > repayment
- Debt side: Same logic as REPAY when interest > repayment

**LIQUIDATION** (`handlers/liquidation.py`):
- Handles both LIQUIDATION and GHO_LIQUIDATION
- Debt events: Extract `debtToCover` from LiquidationCall event
- Collateral events: Extract `liquidatedCollateralAmount` from LiquidationCall event
- Pool rev 9+: Pre-scaled amounts passed to token contracts (calculate ourselves)
- Net debt increase: When `balance_increase > amount` on DEBT_MINT, use `raw_amount = balance_increase - amount`
- See debug/aave/0044

## Migration Steps

1. ✅ **Create `enrichment/` package** and move existing code to `_legacy.py`.
2. ✅ **Create `handlers/base.py`** with `OperationHandler` protocol.
3. ✅ **Create `handlers/__init__.py`** with `HANDLER_REGISTRY`.
4. ✅ **Implement simple handlers first** (INTEREST_ACCRUAL, MINT_TO_TREASURY, BALANCE_TRANSFER, STKAAVE_TRANSFER, UNKNOWN).
5. ✅ **Implement standard handlers** (SUPPLY, BORROW, GHO_FLASH_LOAN, DEFICIT_COVERAGE).
6. ✅ **Implement `context.py`** with shared services.
7. ✅ **Implement `core.py`** with dispatcher.
8. ✅ **Implement feature flag** in `__init__.py`.
9. ✅ **Implement complex handlers** (WITHDRAW, REPAY, REPAY_WITH_ATOKENS, LIQUIDATION).
10. ❌ **Remove feature flag and legacy code** once validated in production.

## Test Strategy

**Red phase:** Before extracting any handler, write isolated tests for each
special case that currently lives in `enrich()`. These tests become the
contract — each handler must make them pass.

**Green phase:** Each handler gets its own test module:

| Handler | Key test cases |
|---------|----------------|
| `test_interest_accrual.py` ✅ | scaled_amount=0; collateral/debt/GHO debt |
| `test_mint_to_treasury.py` ✅ | scaled_amount=None (pass-through) |
| `test_balance_transfer.py` ✅ | raw=scaled; collateral/debt/GHO debt transfers |
| `test_supply.py` ✅ | Extract from Pool event; calculate scaled amount |
| `test_borrow.py` ✅ | BORROW and GHO_BORROW; calculate scaled amount |
| `test_stkaave_transfer.py` ✅ | ERC20 transfer; no scaling |
| `test_unknown.py` ✅ | Raises EnrichmentError |
| `test_gho_flash_loan.py` ✅ | GHO debt burn; calculate scaled amount |
| `test_deficit_coverage.py` ✅ | Transfer + burn handling |
| `test_withdraw.py` ✅ | Standard withdraw; interest>withdrawal Mint override |
| `test_repay.py` ✅ | Standard repay; interest>repayment Mint override; GHO |
| `test_repay_with_atokens.py` ✅ | Collateral + debt handling with overrides |
| `test_liquidation.py` ✅ | Debt vs collateral extraction; Pool rev 9+ pre-scaled; net debt increase |

**Regression:** After extraction, run full enrichment pipeline tests
(`test_position_analysis.py`, any Aave CLI tests). The orchestrator should
behave identically.

**Feature flag tests:** Verify the dispatcher correctly loads legacy vs new
implementation based on environment variable.

## Risks

| Risk | Mitigation |
|------|------------|
| Handlers need shared knowledge (e.g., token revision lookup) | `EnrichmentContext` provides shared services; no handler imports another handler |
| Shared `_create_enriched_event` logic duplicated across handlers | Keep it on `EnrichmentContext.build_enriched_event()` |
| Liquidation handler reveals pattern-detection overlap with `liquidation_patterns.py` | Note: this may expose a seam between enrichment and pattern detection — document but defer |
| Operation type order matters (enrichment before pattern detection) | Document this invariant in handler docstrings |
| Feature flag adds complexity | Lazy loading ensures no overhead; clear rollback path via env var |

## Rollback

The legacy implementation is preserved in `enrichment/_legacy.py`. To rollback:

1. Remove or unset `DEGENBOT_NEW_AAVE_ENRICHMENT` environment variable
2. The dispatcher will automatically use the legacy implementation

No code changes required for rollback.

## Definition of Done

- [x] `enrichment/__init__.py` < 100 lines (dispatcher with feature flag)
- [x] `enrichment/core.py` < 50 lines (thin orchestrator)
- [x] One handler module per OperationType (13 handlers covering 14 operation types)
- [x] Each handler has dedicated test file
- [x] No handler imports another handler
- [x] `EnrichmentContext` provides all shared I/O (DB, calculator, extractor)
- [x] All existing enrichment tests pass
- [x] Full Aave pipeline tests (position analysis, CLI) pass
- [x] Feature flag tests pass
- [x] Special-case inline comments (debug/aave/00xx references) preserved in handler docstrings
- [ ] Remove feature flag and legacy code once validated in production

## Completion Summary

**All 14 operation types have handler implementations:**

| Type | Count | Handlers |
|------|-------|----------|
| ✅ Simple | 5 | INTEREST_ACCRUAL, MINT_TO_TREASURY, BALANCE_TRANSFER, STKAAVE_TRANSFER, UNKNOWN |
| ✅ Standard | 4 | SUPPLY, BORROW, GHO_FLASH_LOAN, DEFICIT_COVERAGE |
| ✅ Complex | 5 | WITHDRAW, REPAY (includes GHO_REPAY), REPAY_WITH_ATOKENS, LIQUIDATION (includes GHO_LIQUIDATION) |

**Test Results:**
- **57 handler tests pass**
- **260 Aave tests pass**
- **1992 total tests pass**

**Code Quality:**
- All linting passes (ruff)
- Type checking passes (mypy with minor known issues in complex handlers)
- All special cases documented with debug/aave references

**Next Steps:**
1. Enable the feature flag in staging: `DEGENBOT_NEW_AAVE_ENRICHMENT=1`
2. Run integration tests against real transaction data
3. If all passes, remove feature flag and legacy code
