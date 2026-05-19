# Plan 055: Delete Deprecated Fetcher Protocol Dead Code

## Overview

Delete the 8 deprecated `*Fetcher` protocol classes from `curve/types.py` and clean up all stale "fetcher" references across comments, error messages, docstrings, architecture docs, and CONTEXT files. These protocols were superseded by the `CurveDataProvider` protocol (Plan 040) and have zero production callers. Their continued presence clutters the module's interface and forces readers to determine which protocol is current.

## Problem

### Deletion test

If you deleted `VirtualPriceFetcher`, `TimestampFetcher`, `RedemptionPriceFetcher`, `AdminBalancesFetcher`, `DFetcher`, `GammaFetcher`, `PriceScaleFetcher`, and `LendingRateFetcher` from `types.py`, nothing in production would break. These protocols have zero import sites outside `types.py` itself. The only references are test files for their replacements (`test_curve_data_provider.py` has `TestDFetcher`, `TestGammaFetcher` — these test the `CurveDataProvider` methods, not the old protocols).

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| 8 deprecated protocols co-exist with their replacement | `curve/types.py` lines 265–340 | A reader scanning `types.py` must read the section comment `Fetcher Protocols (deprecated — use CurveDataProvider)` to know which protocol is current. The deprecated protocols add ~75 lines of dead code. |
| Deprecated protocols have no callers | Throughout `src/degenbot/` | `grep -rn "VirtualPriceFetcher\|TimestampFetcher\|..." src/` returns only `types.py` definitions. Zero call sites. |
| `types.py` is the module's public interface | `curve/types.py` | The file defines all Curve-specific types: state, enums, `DyCalculationInputs`, `DyCalculator`, `CurveDataProvider`, `PoolStrategies`, and the deprecated fetchers. The deprecated protocols are noise at the bottom of an otherwise important file. |
| Test class names are misleading | `tests/curve/test_curve_data_provider.py` line 238, 254 | `TestDFetcher` and `TestGammaFetcher` actually test `CurveDataProvider.D()` and `CurveDataProvider.gamma()`, not the old `DFetcher`/`GammaFetcher` protocols. The names create confusion about which interface is being tested. |
| Stale "fetcher" references in pool comments and error messages | `curve_stableswap_liquidity_pool.py` lines 103, 247, 509, 898, 908 | Comments and `MissingCurveData` error messages say "fetcher callbacks" or "Lending rate fetcher is required" — ambiguous with the removed `*Fetcher` protocol names. After deletion, a reader searching for "Fetcher" would expect zero hits in the Curve module but find 5. |
| Stale `LendingRateStyle` docstring | `curve/types.py` line 109 | Docstring says "Will be replaced by typed fetcher protocols in Plan 027." Plan 027 is completed; `LendingRateStyle` is the live enum, not a replacement target. |
| Stale architecture doc references | `docs/architecture/curve-stableswap-reference.md` lines 113–115 | Tables reference deleted protocols by name: "Fetched on-chain via `DFetcher`", "via `GammaFetcher`", "via `PriceScaleFetcher`". |
| Stale CONTEXT.md reference | `src/degenbot/types/CONTEXT.md` line 72 | "Curve pools call RateFetcher, VirtualPriceFetcher on-demand" — references a deleted class name and a non-existent `RateFetcher`. |
| Stale state-module docstring | `curve/stableswap_pool_state.py` line 14 | Says "Many fetcher callbacks for on-chain data" — outdated description. |

## Solution

### Step 1: Delete the 8 deprecated fetcher protocols from `types.py`

Remove the following from `curve/types.py`:
- `class VirtualPriceFetcher(Protocol)`
- `class TimestampFetcher(Protocol)`
- `class RedemptionPriceFetcher(Protocol)`
- `class AdminBalancesFetcher(Protocol)`
- `class DFetcher(Protocol)`
- `class GammaFetcher(Protocol)`
- `class PriceScaleFetcher(Protocol)`
- `class LendingRateFetcher(Protocol)`

Also remove the section comment `# ── Fetcher Protocols (deprecated — use CurveDataProvider) ──`.

After removal, the `# ── Data Classes ──` section follows directly after `CurveDataProvider`. Merge the two sections into a single `# ── Provider & State Types ──` header to give the remaining structure a coherent organization.

Verify that `Protocol` (from `typing`) still has callers after deletion — it does (`DyCalculator(Protocol)` on line ~218, `CurveDataProvider(Protocol)` on line ~228) — so no import changes are needed.

### Step 2: Update stale `LendingRateStyle` docstring in `types.py`

Replace:
```python
    Used by get_dy() to select which _stored_rates_from_*() method to call.
    Will be replaced by typed fetcher protocols in Plan 027.
```

With:
```python
    Used by get_dy() to select which stored-rate resolution path to call
    via CurveDataProvider.lending_rates().
```

### Step 3: Rename test classes in `test_curve_data_provider.py`

Rename `TestDFetcher` → `TestD` and `TestGammaFetcher` → `TestGamma` to match the existing `Test<PropertyName>` convention in the same file (`TestVirtualPrice`, `TestBlockTimestamp`, `TestRedemptionPrice`, `TestPriceScale`, `TestAdminBalances`).

### Step 4: Update stale "fetcher" references in `curve_stableswap_liquidity_pool.py`

| Line | Current text | Replacement |
|------|-------------|-------------|
| 103 | `# On-chain data access (replaces 13 individual fetcher callbacks)` | `# On-chain data access (replaces 13 individual callback parameters)` |
| 247 | `# I/O is done via fetcher callbacks injected by Bot.build_pool()` | `# I/O is done via data_provider injected by Bot.build_pool()` |
| 509 | `"Lending rate fetcher is required for pools with"` | `"Data provider is required for pools with"` |
| 898 | `Returns rate_multipliers for NONE, or calls the lending rate fetcher` | `Returns rate_multipliers for NONE, or calls the data provider` |
| 908 | `"Lending rate fetcher is required for pools with lending tokens. "` | `"Data provider is required for pools with lending tokens. "` |

### Step 5: Update stale references in architecture doc and CONTEXT files

**`docs/architecture/curve-stableswap-reference.md` lines 113–115:**

Replace:
```
| `D` | Current invariant value | Fetched on-chain via `DFetcher` |
| `gamma` | Curve shape parameter | Fetched on-chain via `GammaFetcher` |
| `price_scale` | Current prices of volatile assets | Fetched on-chain via `PriceScaleFetcher` |
```

With:
```
| `D` | Current invariant value | Fetched on-chain via `CurveDataProvider.D()` |
| `gamma` | Curve shape parameter | Fetched on-chain via `CurveDataProvider.gamma()` |
| `price_scale` | Current prices of volatile assets | Fetched on-chain via `CurveDataProvider.price_scale()` |
```

**`src/degenbot/types/CONTEXT.md` line 72:**

Replace:
```
- A **Pool State** may be updated via **Fetcher Callbacks** (e.g., Curve pools call RateFetcher, VirtualPriceFetcher on-demand)
```

With:
```
- A **Pool State** may be updated via **data_provider** (e.g., Curve pools call `CurveDataProvider` methods on-demand)
```

**`src/degenbot/curve/stableswap_pool_state.py` line 14:**

Replace:
```
- Many fetcher callbacks for on-chain data
```

With:
```
- CurveDataProvider for on-chain data access
```

### Design decisions

- **Delete, don't deprecate**: These protocols have zero callers. A deprecation period would add noise without benefit. The `CurveDataProvider` protocol is the documented seam (Plan 040, ADR-001).
- **No migration path needed**: Nothing imports these protocols. No adapter pattern conversion is required.
- **Full stale-reference cleanup in one pass**: The word "fetcher" (lowercase) appears across comments, error messages, docstrings, architecture docs, and CONTEXT files. Cleaning these alongside the protocol deletion ensures that post-deletion `grep -ri fetcher` returns zero hits in the Curve module, rather than leaving ambiguous references that a reader might confuse with the removed classes.
- **Test class naming follows file convention**: `TestD` and `TestGamma` match the existing pattern (`TestVirtualPrice`, `TestBlockTimestamp`, etc.) rather than introducing the inconsistent "Method" suffix.

## Files Involved

**Primary:**
- `src/degenbot/curve/types.py` — delete 8 deprecated protocol classes (lines ~265–340), update `LendingRateStyle` docstring, reorganize section headers

**Secondary:**
- `tests/curve/test_curve_data_provider.py` — rename `TestDFetcher` → `TestD`, `TestGammaFetcher` → `TestGamma`
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — update 5 stale "fetcher" references in comments/error messages
- `docs/architecture/curve-stableswap-reference.md` — update 3 table rows referencing deleted protocol class names
- `src/degenbot/types/CONTEXT.md` — update 1 stale relationship line referencing `RateFetcher` and `VirtualPriceFetcher`
- `src/degenbot/curve/stableswap_pool_state.py` — update 1 stale docstring line

**No change needed:**
- `src/degenbot/curve/data_provider_impl.py` — implements `CurveDataProvider`, not the deprecated protocols; its module docstring says "Replaces the closure-based CurveFetcherFactory" which is a historical reference to the factory class, not the protocols
- `src/degenbot/curve/_pool_strategies.py` — uses `PoolStrategies` and `SwapStyle`/`LendingRateStyle`, not deprecated fetchers
- `src/degenbot/curve/__init__.py` — does not export any `*Fetcher` names

## Implementation Order

### Slice 1: Delete deprecated protocols + fix types.py docstrings

1. Delete the 8 deprecated protocol class definitions from `curve/types.py`
2. Remove the `# ── Fetcher Protocols (deprecated — use CurveDataProvider) ──` section comment
3. Merge the orphaned `# ── Data Classes ──` section into `# ── Provider & State Types ──`
4. Fix `LendingRateStyle` docstring: remove stale Plan 027 reference
5. Run: `just test-python` — expect all tests green

### Slice 2: Rename test classes + update pool comments and error messages

1. Rename `TestDFetcher` → `TestD` and `TestGammaFetcher` → `TestGamma` in test file
2. Update 5 stale "fetcher" references in `curve_stableswap_liquidity_pool.py` (3 comments, 2 error messages)
3. Run: `just test-python` — expect all tests green

### Slice 3: Update architecture docs and CONTEXT files

1. Update `docs/architecture/curve-stableswap-reference.md` — 3 table rows
2. Update `src/degenbot/types/CONTEXT.md` — 1 relationship line
3. Update `src/degenbot/curve/stableswap_pool_state.py` — 1 docstring line
4. Run: `just test-python` — expect all tests green (documentation-only changes)

### Slice 4: Validate and clean up

1. Run `just lint` + `just test-all`
2. Run case-sensitive grep for deleted class names: `grep -rn "VirtualPriceFetcher\|TimestampFetcher\|RedemptionPriceFetcher\|AdminBalancesFetcher\|DFetcher\|GammaFetcher\|PriceScaleFetcher\|LendingRateFetcher" src/ docs/` — expect zero results
3. Run case-insensitive grep for lingering "fetcher" references in the Curve module: `grep -rni "fetcher" src/degenbot/curve/ docs/architecture/curve-stableswap-reference.md src/degenbot/types/CONTEXT.md` — expect zero results (or only historical references like `data_provider_impl.py`'s "Replaces the closure-based CurveFetcherFactory" which is acceptable)
4. Verify `Protocol` import in `types.py` still has callers (should: `DyCalculator`, `CurveDataProvider`)

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Slices 1–3 should be green — the deleted protocols have no callers, and the remaining changes are doc/comment/message text only.

### New unit tests

No new unit tests required. The existing `FakeCurveDataProvider` and `CurveDataProviderImpl` tests cover the replacement protocol.

### Integration tests

No changes needed. The Curve pool uses `CurveDataProvider`, not the deprecated fetchers.

## Benefits

- **Leverage**: The `CurveDataProvider` protocol is the single seam for on-chain data access — one protocol with 13 methods replaces 8 separate protocols. Deleting the old protocols eliminates the question "which one do I use?"
- **Locality**: `types.py` defines only the current, documented types. A reader doesn't need to scroll past 75 lines of dead code or parse deprecation comments.
- **Depth**: The `CurveDataProvider` seam is already deep — 13 methods behind one protocol. The deprecated fetchers were shallow (1 method each), offering no leverage over their replacement.
- **Terminology consistency**: Post-deletion, `grep -ri fetcher` returns zero hits in the Curve module. The word "fetcher" no longer has an ambiguous dual meaning (the removed protocols vs. the generic concept of fetching data). Error messages say "data provider" consistently with the `CurveDataProvider` name.

## Risks

- **External consumers**: If any user code outside the `degenbot` package imports these protocols, the deletion would break them. Mitigation: these are `Protocol` classes in an internal module, not part of the public API. The package's `__all__` doesn't export them. Verified: `grep -rn "from degenbot.curve.types import.*Fetcher"` returns zero results.
- **Error message string change**: Two `MissingCurveData` error messages change from "Lending rate fetcher is required" to "Data provider is required". If any test asserts on the exact message text, those assertions will break. Mitigation: check for message-asserting tests during Slice 2.
- **Minimal risk**: This is the lowest-risk plan in the set. The deletion is straightforward and well-contained.

## Relationship to Other Plans

- **Plan 040** (Curve Data Provider): Completed. Established `CurveDataProvider` as the replacement. This plan cleans up the residual dead code from that replacement.
- **Plan 027** (Curve Lending-Rate Fetchers): Completed. Introduced `LendingRateFetcher` protocol. This plan deletes that protocol class (superseded by `CurveDataProvider.lending_rates()`) and removes the stale "Will be replaced by typed fetcher protocols in Plan 027" docstring from `LendingRateStyle`.
- **Plan 054** (Consolidate Curve On-Chain Caches): Orthogonal. Plan 054 reorganizes the pool's cache fields; this plan deletes dead protocol types and cleans up stale references. No dependency between them.
- **Plan 053** (Delete Old Optimizer Hierarchy): Orthogonal. Different module entirely.
- **Plan 057** (Document Curve Pool's Partial I/O Status): Complementary. This plan removes stale fetcher references; Plan 057 adds explicit I/O-boundary documentation. Executing 055 first gives Plan 057 a cleaner surface to document.

## Status

[x] Slice 1: Delete deprecated protocols + fix types.py docstrings
[x] Slice 2: Rename test classes + update pool comments and error messages
[x] Slice 3: Update architecture docs and CONTEXT files
[x] Slice 4: Validate and clean up
