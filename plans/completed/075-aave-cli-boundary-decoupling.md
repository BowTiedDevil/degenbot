# Plan 075: Decouple Aave General Library from CLI Application Layer

## Overview

Reverse the inverted dependency where `degenbot/aave/` (general library) imports from
`degenbot/cli/` (application layer). Move domain data types (`ScaledTokenEvent`,
`Operation`, `TransactionOperations`, `TransactionValidationError`, `TokenType`)
out of CLI into the general Aave package, move `decode_address()` to the general
`contract/` package, and delete dead code (`AAVE_EVENT_TOPIC_TO_CATEGORY`,
`filter_scaled_events`/`find_first_scaled_event`) — so that `aave/` has zero CLI
imports and the dependency arrow points only downward: `cli/` → `aave/`.

## Problem

### Deletion test

If you deleted `degenbot/cli/` entirely, the `degenbot/aave/` package would break.
`aave/enrichment/` and `aave/liquidation_patterns.py` import `ScaledTokenEvent`,
`Operation`, and `decode_address` from `degenbot.cli.aave_transaction_operations` and
`degenbot.cli.aave_utils`. A general-purpose library should never depend on its
consumers — deleting the CLI application should leave the library intact.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| `aave/` imports `cli/` (inverted dependency) | `aave/enrichment/context.py:21`, `aave/liquidation_patterns.py:21-22` | Library cannot be used without pulling in the CLI package; circular-ish coupling |
| Domain parsing types trapped in CLI | `cli/aave_transaction_operations.py` → `ScaledTokenEvent`, `Operation` | These represent decoded on-chain event data — they are domain types, not application state |
| `decode_address()` trapped in CLI | `cli/aave_utils.py` | General-purpose ABI utility used by both `aave/` and `cli/` |
| `TokenType` enum trapped in CLI | `cli/aave/types.py` | `A_TOKEN`/`V_TOKEN`/`GHO_DISCOUNT` is a domain classification, not CLI state |
| Topic category map is dead code | `cli/aave/constants.py` → `AAVE_EVENT_TOPIC_TO_CATEGORY` | Defined but never imported anywhere; should be deleted |
| 16 enrichment handler files each import from `cli/` | `aave/enrichment/handlers/*.py` (all `TYPE_CHECKING` imports) | Every handler file is coupled to the CLI layer through its type hints |
| 13 test files import `ScaledTokenEvent` from `cli/` | `tests/aave/enrichment/handlers/test_*.py` | Test coupling mirrors production coupling; can't test enrichment without CLI |
| `aave_event_filtering.py` is dead code | `cli/aave_event_filtering.py` | Defines `filter_scaled_events`/`find_first_scaled_event` but they are never imported anywhere; should be deleted |

## Solution

### Step 1: Create `aave/operations.py` with pure domain types

Move `ScaledTokenEvent`, `Operation`, `TransactionOperations`, `TransactionValidationError`,
`TOKEN_AMOUNT_MATCH_TOLERANCE`, and `SCALED_AMOUNT_POOL_REVISION` from
`cli/aave_transaction_operations.py` into a new `aave/operations.py`.

These are frozen dataclasses and a validation container with no DB/Session/Provider
dependencies. Their only imports are other `aave/` modules (`events`, `operation_types`)
and `eth_abi`/`hexbytes`/`web3` (already standard dependencies).

```python
# aave/operations.py — NEW FILE
# Contains: ScaledTokenEvent, Operation, TransactionOperations, TransactionValidationError
# Moved from: cli/aave_transaction_operations.py
# No changes to class bodies — only the import paths of their internal references change
```

`TransactionOperationsParser` **stays** in `cli/aave_transaction_operations.py` because
it queries the database to resolve token types, asset addresses, and GHO configuration.
It imports `ScaledTokenEvent`/`Operation` from their new home in `aave/operations.py`.

### Step 2: Move `decode_address()` to `contract/decoding.py`

Move `decode_address()` from `cli/aave_utils.py` to `contract/decoding.py`. This is a
pure function: decode an ABI-encoded address bytes → `ChecksumAddress`. It has zero
Aave-specific logic and the `contract/` package already houses `addresses.py` for
deterministic address derivation, making it the natural home.

```python
# contract/decoding.py — NEW FILE
import eth_abi.abi
from eth_typing import ChecksumAddress
from degenbot.checksum_cache import get_checksum_address

def decode_address(input_: bytes) -> ChecksumAddress:
    (address,) = eth_abi.abi.decode(types=["address"], data=input_)
    return get_checksum_address(address)
```

After this move, `cli/aave_utils.py` becomes empty and should be deleted. All its
consumers (in both `aave/` and `cli/`) update their import to `contract.decoding`.

### Step 3: Move `TokenType` to `aave/types.py`

Move the `TokenType` enum (`A_TOKEN`, `V_TOKEN`, `GHO_DISCOUNT`) from
`cli/aave/types.py` to `aave/types.py`. This is a domain classification used by the
operations parser, the enrichment module's token-type resolution, and the event
filtering utility. It has no CLI-specific content.

### Step 4: Delete `AAVE_EVENT_TOPIC_TO_CATEGORY`

`AAVE_EVENT_TOPIC_TO_CATEGORY` is defined in `cli/aave/constants.py` but has **zero
imports** anywhere in the codebase. It is dead code. Delete it rather than moving it.

### Step 5: Delete `aave_event_filtering.py`

The module `cli/aave_event_filtering.py` defines `filter_scaled_events` and
`find_first_scaled_event`, but **neither is imported anywhere**. It is dead code.
Delete it rather than moving it.

### Step 6: Update all import paths

Systematic import path updates across production code and tests:

| Old import | New import |
|------------|------------|
| `from degenbot.cli.aave_transaction_operations import ScaledTokenEvent, Operation` | `from degenbot.aave.operations import ScaledTokenEvent, Operation` |
| `from degenbot.cli.aave_transaction_operations import TransactionOperations, TransactionValidationError` | `from degenbot.aave.operations import TransactionOperations, TransactionValidationError` |
| `from degenbot.cli.aave_utils import decode_address` | `from degenbot.contract.decoding import decode_address` |
| `from degenbot.cli.aave.types import TokenType` | `from degenbot.aave.types import TokenType` |
| `from degenbot.cli.aave.constants import AAVE_EVENT_TOPIC_TO_CATEGORY` | *(deleted — no consumers)* |
| `from degenbot.cli.aave_event_filtering import filter_scaled_events, find_first_scaled_event` | *(deleted — no consumers)* |

CLI-internal imports for `TransactionOperationsParser` remain pointed at
`cli/aave_transaction_operations` (renamed to `cli/aave/operations_parser.py` in
Slice 6); it imports the data classes from `aave/operations.py`.

### Step 7: Delete empty files and update `__init__.py` re-exports

- Delete `cli/aave_utils.py` (all contents moved)
- Re-export domain types from `aave/__init__.py`: `ScaledTokenEvent`, `Operation`,
  `TransactionOperations`, `TransactionValidationError`, `TokenType`. Do not re-export
  internal constants (`TOKEN_AMOUNT_MATCH_TOLERANCE`, `SCALED_AMOUNT_POOL_REVISION`) or
  `decode_address` (lives in `contract/`; use its own path).
- Update `cli/aave/types.py` to re-export `TokenType` from `aave.types` during a
  brief compatibility window, then remove in Slice 6
- Rename `cli/aave_transaction_operations.py` → `cli/aave/operations_parser.py`

### Design decisions

- **`TransactionOperationsParser` stays in `cli/`**: It queries the database (AaveV3Asset,
  AaveGhoToken, AaveV3Contract) to resolve token types and asset addresses. It is an
  application-level orchestrator, not a domain type. The parser imports the data classes
  from `aave/operations.py` and uses them; the data classes don't know about the parser.

- **`UserOperation` stays in `cli/aave/constants.py`**: This enum (`DEPOSIT`, `WITHDRAW`,
  `BORROW`, etc.) is display-oriented and only used by `token_processor.py` for logging.
  It's not a domain classification — domain operations use `OperationType` (already in
  `aave/operation_types.py`).

- **Revision/display constants stay in `cli/aave/constants.py`**: `GHO_DISCOUNT_DEPRECATION_REVISION`
  and `POSITION_RISK_DISPLAY_LIMIT` are application-level thresholds. `SCALED_AMOUNT_POOL_REVISION`
  is only used by the `TransactionOperationsParser` (via `aave/operations.py`); the duplicate
  in `cli/aave/constants.py` should import from `aave.operations` to eliminate the duplication.

- **`AAVE_EVENT_TOPIC_TO_CATEGORY` and `filter_scaled_events`/`find_first_scaled_event` are dead code**:
  Neither has any imports anywhere in the codebase. They are deleted, not moved. If future consumers
  are needed, they should be re-created in `aave/` at that time.

- **`decode_address` goes to `contract/decoding.py`, not `aave/decoding.py`**: It is a general-purpose
  ABI utility (decode `bytes` → `ChecksumAddress`) with zero Aave-specific logic. The `contract/`
  package already houses `addresses.py` for deterministic address derivation, making it the natural
  home. Placing it in `aave/` would create a semantic mismatch where non-Aave consumers would
  import from `degenbot.aave`.

- **No `TYPE_CHECKING` imports breakage**: All current `TYPE_CHECKING` imports of
  `ScaledTokenEvent`/`Operation` in `aave/enrichment/handlers/` become regular imports
  (they were only in `TYPE_CHECKING` to avoid circular imports with `cli/`). Once
  the types live in `aave/`, the circular dependency vanishes and these should be
  promoted to top-level imports.

- **`cli/aave/extraction.py` stays in CLI**: The user-address extraction functions
  (`extract_user_addresses_from_event`, `extract_user_addresses_from_transaction`) are
  used for batch pre-fetching users to avoid N+1 queries during transaction processing.
  This is a CLI optimization concern, not a domain extraction. The existing
  `aave/extraction.py` (raw amount extraction from Pool events) is a different concern
  and remains unchanged.

## Files Involved

**Primary (new files):**
- `src/degenbot/aave/operations.py` — NEW: `ScaledTokenEvent`, `Operation`, `TransactionOperations`, `TransactionValidationError`, `TOKEN_AMOUNT_MATCH_TOLERANCE`, `SCALED_AMOUNT_POOL_REVISION`
- `src/degenbot/contract/decoding.py` — NEW: `decode_address()`
- `src/degenbot/aave/types.py` — NEW: `TokenType` enum

**Primary (modified files):**
- `src/degenbot/cli/aave_transaction_operations.py` — Remove moved types; `TransactionOperationsParser` imports from `aave/operations`
- `src/degenbot/aave/liquidation_patterns.py` — Update imports: `aave.operations` + `contract.decoding`
- `src/degenbot/aave/enrichment/context.py` — Update import: `aave.operations`
- `src/degenbot/aave/enrichment/core.py` — Update import: `aave.operations`
- `src/degenbot/aave/enrichment/handlers/*.py` (16 files) — Update imports: `aave.operations`; promote `TYPE_CHECKING` imports to top-level; re-sort with ruff/isort
- `src/degenbot/cli/aave/types.py` — Remove `TokenType`; retains `TransactionContext`
- `src/degenbot/cli/aave/constants.py` — Delete `AAVE_EVENT_TOPIC_TO_CATEGORY`; import `SCALED_AMOUNT_POOL_REVISION` from `aave.operations`

**Secondary (import path updates only):**
- `src/degenbot/cli/aave/commands.py` — Update `TokenType` import if used
- `src/degenbot/cli/aave/event_handlers.py` — `decode_address` import
- `src/degenbot/cli/aave/extraction.py` — `decode_address` import
- `src/degenbot/cli/aave/liquidation_processor.py` — `Operation` import
- `src/degenbot/cli/aave/stkaave.py` — `decode_address` import
- `src/degenbot/cli/aave/token_processor.py` — `Operation`, `ScaledTokenEvent`, `decode_address`, `TokenType` imports
- `src/degenbot/cli/aave/transaction_processor.py` — `Operation`, `TransactionOperationsParser`, `decode_address` imports
- `src/degenbot/cli/aave/transfers.py` — `Operation`, `ScaledTokenEvent`, `decode_address` imports
- `src/degenbot/cli/aave/utils.py` — `decode_address` import
- `tests/aave/enrichment/handlers/test_*.py` (13 files) — `ScaledTokenEvent`, `Operation` imports

**Deleted:**
- `src/degenbot/cli/aave_utils.py` — Empty after `decode_address` move
- `src/degenbot/cli/aave_event_filtering.py` — Dead code (no consumers)
- `src/degenbot/cli/aave_transaction_operations.py` — Renamed to `cli/aave/operations_parser.py` (Slice 6)

**No change needed:**
- `src/degenbot/aave/events.py` — Already in `aave/`; no CLI imports
- `src/degenbot/aave/operation_types.py` — Already in `aave/`; no CLI imports
- `src/degenbot/aave/models.py` — Already in `aave/`; no CLI imports
- `src/degenbot/aave/calculator.py` — Already in `aave/`; no CLI imports
- `src/degenbot/aave/extraction.py` — Already in `aave/`; no CLI imports
- `src/degenbot/aave/pattern_types.py` — Already in `aave/`; no CLI imports
- `src/degenbot/aave/deployments.py` — Already in `aave/`; no CLI imports
- `src/degenbot/aave/libraries/` — Pure math; no CLI imports
- `src/degenbot/aave/processors/` — Stateless processors; no CLI imports
- `src/degenbot/aave/analysis/` — Already has I/O-free core + orchestrator seam; no CLI imports
- `src/degenbot/cli/aave/db_*.py` — DB helpers; unaffected
- `src/degenbot/cli/aave/verification.py` — Verification; unaffected
- `src/degenbot/cli/aave/erc20_utils.py` — ERC20 metadata; unaffected

## Implementation Order

### Slice 1: Move `decode_address` to `contract/decoding.py`

This is the simplest, widest-used extraction. It unblocks the `liquidation_patterns.py`
fix and establishes the pattern for subsequent slices.

1. Create `src/degenbot/contract/decoding.py` with `decode_address()` (moved from `cli/aave_utils.py`)
2. Update all 8 import sites across `aave/` and `cli/`:
   - `aave/liquidation_patterns.py`
   - `cli/aave_transaction_operations.py`
   - `cli/aave/event_handlers.py`
   - `cli/aave/extraction.py`
   - `cli/aave/stkaave.py`
   - `cli/aave/token_processor.py`
   - `cli/aave/transaction_processor.py`
   - `cli/aave/transfers.py`
   - `cli/aave/utils.py`
3. Delete `src/degenbot/cli/aave_utils.py` (the flat file in `cli/` — not `cli/aave/utils.py`, which is a different file and is unaffected)
4. Run: `just test-python` — expect all tests green

### Slice 2: Move `TokenType` to `aave/types.py`

1. Create `src/degenbot/aave/types.py` with `TokenType` enum (moved from `cli/aave/types.py`)
2. Update `cli/aave/types.py` to import `TokenType` from `aave.types` (re-export for
   compatibility during migration)
3. Update direct `TokenType` consumers:
   - `cli/aave_transaction_operations.py`
   - `cli/aave/token_processor.py`
   - `cli/aave/db_assets.py`
4. Run: `just test-python` — expect all tests green

### Slice 3: Delete dead code — `AAVE_EVENT_TOPIC_TO_CATEGORY` and `aave_event_filtering.py`

Both have zero imports across the entire codebase. Delete rather than move.

1. Remove `AAVE_EVENT_TOPIC_TO_CATEGORY` from `cli/aave/constants.py`
2. Delete `src/degenbot/cli/aave_event_filtering.py` (defines `filter_scaled_events`/`find_first_scaled_event` — unused)
3. Run: `just test-python` — expect all tests green

### Slice 4: Move `ScaledTokenEvent`, `Operation`, `TransactionOperations`, `TransactionValidationError` to `aave/operations.py`

This is the core structural change.

1. Create `src/degenbot/aave/operations.py` containing:
   - `ScaledTokenEvent` (frozen dataclass)
   - `Operation` (frozen dataclass)
   - `TransactionOperations` (validation container)
   - `TransactionValidationError` (exception)
   - `TOKEN_AMOUNT_MATCH_TOLERANCE` (constant)
   - `SCALED_AMOUNT_POOL_REVISION` (constant)
2. Update `cli/aave_transaction_operations.py`:
   - Remove the moved classes/constants
   - Import them from `aave.operations`
   - `TransactionOperationsParser` stays; its imports of `ScaledTokenEvent`/`Operation` now
     point at `aave.operations`
3. Update all `aave/` consumers — promote `TYPE_CHECKING` imports to top-level and re-sort (ruff/isort):
   - `aave/enrichment/context.py`
   - `aave/enrichment/core.py`
   - `aave/enrichment/handlers/*.py` (16 files, including `__init__.py`)
   - `aave/liquidation_patterns.py`
4. Update all `cli/aave/` consumers:
   - `cli/aave/transaction_processor.py`
   - `cli/aave/token_processor.py`
   - `cli/aave/transfers.py`
   - `cli/aave/liquidation_processor.py`
5. Update `cli/aave/constants.py` — replace duplicate `SCALED_AMOUNT_POOL_REVISION` with import from `aave.operations`
6. Update all test files:
   - `tests/aave/enrichment/handlers/test_*.py` (13 files)
7. Run: `just test-python` — expect all tests green

### Slice 5: Validate, clean up, and remove compatibility shims

1. Remove the `TokenType` re-export from `cli/aave/types.py` (all consumers should
   now import directly from `aave.types`)
2. Remove any re-exports from `aave_transaction_operations.py` (all consumers should
   import `ScaledTokenEvent`/`Operation` from `aave.operations`)
3. Rename `cli/aave_transaction_operations.py` → `cli/aave/operations_parser.py`
4. Update all imports of `TransactionOperationsParser` to use the new path
5. Verify `aave/` has zero `cli/` imports:
   ```bash
   grep -rn 'from degenbot\.cli' src/degenbot/aave/
   ```
   Expect: no output
6. Add boundary invariant smoke test:
   ```python
   # tests/aave/test_no_cli_imports.py

   def test_aave_package_has_no_cli_imports():
       """Verify that degenbot.aave never imports from degenbot.cli."""
       import subprocess
       result = subprocess.run(
           ["grep", "-rn", "from degenbot.cli", "src/degenbot/aave/"],
           capture_output=True,
       )
       assert result.returncode != 0, (
           f"aave/ leaks CLI dependency:\n{result.stdout.decode()}"
       )
   ```
7. Run: `just lint` + `just test-all`
8. Update `aave/CONTEXT.md` to document the new module layout
9. Update `cli/AGENTS.md` to reflect new file locations

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Every slice must leave the suite green before
proceeding. All import path changes are mechanical; no logic changes.

### New unit tests

No new test logic is required — this is a structural refactoring (moving modules,
updating import paths). The existing test suite fully covers all moved types:

- `tests/aave/enrichment/handlers/test_*.py` — exercises `ScaledTokenEvent`/`Operation` construction
- `tests/aave/processors/test_unified_processor.py` — exercises `CollateralMintEvent`/`DebtBurnEvent`
- `tests/aave/test_models_unified.py` — exercises `EnrichedScaledTokenEvent` validation
- `tests/aave/libraries/` — exercises `TokenMath`/`WadRayMath` (underlying calculation layer)

A smoke test is added in Slice 5 to verify the boundary invariant (see implementation order).
It uses a `grep`-based check rather than `pkgutil.walk_packages` because the latter does not
detect `TYPE_CHECKING`-guarded imports and may miss lazily-loaded submodules.

### Integration tests

No existing CLI-specific integration tests exist (`tests/cli/` only has `test_pool_updater_configs.py`
and `test_cli.py`, neither of which is Aave-specific). The CLI Aave updater is tested manually
via the `degenbot aave update` command.

## Benefits

- **Locality**: Domain data types (`ScaledTokenEvent`, `Operation`, `TokenType`) live
  alongside the domain enums (`ScaledTokenEventType`, `OperationType`) that classify them
- **Depth**: The `aave/` package becomes a self-contained Aave domain library; removing
  `cli/` would not break it
- **Leverage**: Any future Aave consumer (async updater, API server, notebook analysis)
  can import `degenbot.aave` without pulling in Click, SQLAlchemy, and the full CLI stack
- **Seam**: `aave/operations.py` becomes a clean boundary — pure data types in, parsed
  operations out. The DB-aware `TransactionOperationsParser` is a separate seam in `cli/`
- **Simplicity**: Eliminates 16+ `TYPE_CHECKING`-guarded circular import workarounds in
  `aave/enrichment/handlers/`

## Risks

- **Import path churn**: ~40 files change import paths. Mitigated by mechanical
  find-and-replace with immediate test validation per slice. Each slice is independently
  shippable and leaves the test suite green.
- **`TransactionOperationsParser` coupling**: The parser imports `TokenType` from
  `aave.types` and `ScaledTokenEvent`/`Operation` from `aave.operations` but still lives
  in `cli/`. This is the correct boundary — the parser is an application-level orchestrator
  that uses domain types. No risk, just clarity.
- **File rename in Slice 5**: `cli/aave_transaction_operations.py` → `cli/aave/operations_parser.py`
  changes the import path for `TransactionOperationsParser`. All consumers (currently
  `cli/aave/transaction_processor.py`) must update. This is a small, mechanical change
  — one import site — but it touches a different file than the other slices. Bundled
  with cleanup to keep the rename in a single commit.

## Relationship to Other Plans

- **Plan 007** (Collapse Aave Token Processor Revision Matrix): COMPLETE. Established the
  `aave/processors/` module with strategy-based rounding. This plan continues the
  separation by moving the types those processors consume (`ScaledTokenEvent`,
  `Operation`) out of CLI.
- **Plan 008** (Aave Event Enrichment Handlers): COMPLETE. Established the
  `aave/enrichment/handlers/` module. This plan removes those handlers' inverted
  dependency on `cli/`.
- **Plan 009** (Aave Position Analysis I/O-Free): COMPLETE. Established the
  `analysis/protocols.py` seam. This plan follows the same pattern — pushing types
  down to the library layer so application code depends on library, not vice versa.
- **Plan 010** (Aave Event Models Parameterized): COMPLETE. Established the unified
  `EnrichedScaledTokenEvent` model in `aave/models.py`. This plan completes the
  decoupling by moving `ScaledTokenEvent` (the input type) alongside it.

Independent of all non-Aave plans.

## Status

- [x] Slice 1: Move `decode_address` to `contract/decoding.py`
- [x] Slice 2: Move `TokenType` to `aave/types.py`
- [x] Slice 3: Delete dead code (`AAVE_EVENT_TOPIC_TO_CATEGORY`, `aave_event_filtering.py`)
- [x] Slice 4: Move `ScaledTokenEvent`, `Operation`, `TransactionOperations`, `TransactionValidationError` to `aave/operations.py`
- [x] Slice 5: Validate, clean up, remove compatibility shims, and rename `aave_transaction_operations.py` → `aave/operations_parser.py`
