# Plan 061: Delete `EthereumProvider` Backward-Compatibility Alias

## Overview

Delete the `EthereumProvider = ProviderBackend` type alias from `provider/interface.py` and all references to it across the codebase. The alias is a historical artifact from Plan 042 (which merged `EthereumProvider` + `_SyncProviderBackend` → `ProviderBackend`). Two names for the same concept force readers to check which is canonical. Zero external callers depend on the alias name.

## Problem

### Deletion test

If you deleted `EthereumProvider = ProviderBackend`, the following references need updating:

| # | Location | Type | Action |
|---|----------|------|--------|
| 1 | `provider/interface.py:146` | Alias definition | Delete |
| 2 | `provider/interface.py:1340` | `__all__` entry | Remove |
| 3 | `provider/__init__.py:42` | Import + re-export | Remove |
| 4 | `provider/__init__.py:443` | `__all__` entry | Remove |
| 5 | `provider/offline_provider.py:4` | Module docstring | Replace "EthereumProvider" → "ProviderBackend" |
| 6 | `provider/offline_provider.py:66` | Class docstring | Replace "EthereumProvider" → "ProviderBackend" |
| 7 | `provider/interface.py:56` | `ProviderBackend` class docstring | Clarify that alias was removed |
| 8 | `provider/__init__.py` | `__all__` comment | Update "Keep public API surface unchanged" → "Public API" |
| 9 | `connection/CONTEXT.md:8` | Domain doc | Remove "remains as a backward-compatible alias" sentence |
| 10 | `tests/provider/test_provider_backend.py:5,49,53` | Docstrings (historical) | Update to say "the former EthereumProvider" |
| 11 | `tests/provider/test_alloy_web3_feature_parity.py:155` | Comment | Replace "EthereumProvider" → "ProviderBackend" |
| 12 | `tests/provider/test_alloy_web3_feature_parity.py:388-389` | Class name + docstring | Rename `TestEthereumProviderProtocol` → `TestProviderBackendProtocolOnAlloy` |
| 13 | `tests/provider/test_alloy_web3_feature_parity.py:398` | Test docstring | Replace "EthereumProvider" → "ProviderBackend" |
| 14 | `tests/rust/test_alloy_provider.py:4` | Module docstring | Replace "EthereumProvider" → "ProviderBackend" |
| 15 | `tests/rust/test_provider_interface.py:45` | Test docstring | Replace "EthereumProvider" → "ProviderBackend" |

The alias has no callers that use `EthereumProvider` as a type annotation or import — all type annotations and `isinstance` checks use `ProviderBackend` directly. After these changes, `grep -rn "EthereumProvider" src/ tests/` should return only **historical references** (docstrings explaining what `ProviderBackend` replaced), which are appropriate to retain.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Two names for the same concept | `provider/interface.py` line 146 | `EthereumProvider = ProviderBackend` is a type alias that exists only for backward compatibility. A reader seeing both names must check which is canonical. The name `EthereumProvider` is misleading — it suggests Ethereum-specific behavior, while `ProviderBackend` correctly describes the role (a backend for the adapter pattern). |
| Stale docstring references | `provider/offline_provider.py` lines 4, 66 | The module docstring says "provides an EthereumProvider implementation" and the class docstring says "implements the EthereumProvider Protocol." Both should say `ProviderBackend`. |
| Re-export in `__init__.py` | `provider/__init__.py` lines 42, 443 | `EthereumProvider` is imported and re-exported in the provider package's public API. Callers who import `from degenbot.provider import EthereumProvider` get the alias. This is dead weight. |
| `__all__` listing | `provider/interface.py` line 1340 | `EthereumProvider` appears in `__all__`, advertising it as a public name. |
| Stale domain docs | `connection/CONTEXT.md` line 8 | States "EthereumProvider remains as a backward-compatible alias" — becomes false after deletion. |
| Stale test naming | `tests/provider/test_alloy_web3_feature_parity.py` | Class `TestEthereumProviderProtocol` and comment reference the deleted name. |

## Solution

### Step 1: Delete alias and clean up exports

1. Delete `EthereumProvider = ProviderBackend` from `provider/interface.py`
2. Remove `"EthereumProvider"` from `__all__` in `provider/interface.py`
3. Update the `__all__` comment from "Keep public API surface unchanged" to "Public API"
4. Remove the `EthereumProvider` import and re-export from `provider/__init__.py`
5. Remove `"EthereumProvider"` from `__all__` in `provider/__init__.py`

### Step 2: Update docstring references in provider module

1. Replace "EthereumProvider" with "ProviderBackend" in `provider/offline_provider.py` module docstring (line 4)
2. Replace "EthereumProvider" with "ProviderBackend" in `provider/offline_provider.py` class docstring (line 66)
3. Update `provider/interface.py` `ProviderBackend` class docstring to note the alias was removed (line 56–57)

### Step 3: Update domain documentation

1. Update `connection/CONTEXT.md` line 8: replace "remains as a backward-compatible alias for `ProviderBackend`" with "backward-compatibility alias was removed by Plan 061"

### Step 4: Update test references

1. `tests/provider/test_provider_backend.py`: Update docstrings to say "the former EthereumProvider" (lines 5, 49, 53)
2. `tests/provider/test_alloy_web3_feature_parity.py`:
   - Replace comment "Additional EthereumProvider protocol methods" → "Additional ProviderBackend protocol methods" (line 155)
   - Rename `TestEthereumProviderProtocol` → `TestProviderBackendProtocolOnAlloy` (avoids collision with existing `TestProviderBackendProtocol` in `test_provider_backend.py`)
   - Update class and test docstrings to say "ProviderBackend protocol" (lines 389, 398)
3. `tests/rust/test_alloy_provider.py`: Replace "EthereumProvider" → "ProviderBackend" in module docstring (line 4)
4. `tests/rust/test_provider_interface.py`: Replace "EthereumProvider" → "ProviderBackend" in test docstring (line 45)

### Step 5: Validate zero live references remain

1. `grep -rn "EthereumProvider" src/ tests/` — remaining hits should only be historical docstrings explaining what `ProviderBackend` replaced
2. Run `just lint` + `just test-all`
3. Verify `ProviderBackend` still appears in both `__all__` lists and all re-exports

### Design decisions

- **Delete, don't deprecate**: The alias has zero callers as a type annotation or import. A deprecation warning on a type alias is awkward (it's used at import time, not call time). Clean deletion is simpler and consistent with Plan 055 (which deleted dead protocol classes with zero callers).
- **BREAKING CHANGE**: Removing a name from the public API is semver-major. The commit must include `BREAKING CHANGE: remove EthereumProvider backward-compatibility alias` in its footer per AGENTS.md conventions.
- **Historical references retained**: Docstrings that say "replaces the former EthereumProvider" are historical context, not forward references. They remain but are clarified where needed (e.g., "was removed by Plan 061").
- **Don't rename `offline_provider.py`'s class references**: `OfflineProvider` structurally satisfies `ProviderBackend` but doesn't inherit it. The docstring update is sufficient.
- **Test class renamed to avoid collision**: `TestEthereumProviderProtocol` → `TestProviderBackendProtocolOnAlloy` (the existing `TestProviderBackendProtocol` in `test_provider_backend.py` tests different concerns).

## Files Involved

**Primary:**
- `src/degenbot/provider/interface.py` — delete alias line, remove from `__all__`, update class docstring
- `src/degenbot/provider/__init__.py` — remove import and re-export, remove from `__all__`
- `src/degenbot/provider/offline_provider.py` — update 2 docstring references

**Domain docs:**
- `src/degenbot/connection/CONTEXT.md` — remove stale alias statement

**Tests:**
- `tests/provider/test_provider_backend.py` — update 3 historical docstrings
- `tests/provider/test_alloy_web3_feature_parity.py` — rename class, update comment + 2 docstrings
- `tests/rust/test_alloy_provider.py` — update module docstring
- `tests/rust/test_provider_interface.py` — update test docstring

**No change needed:**
- `src/degenbot/provider/interface.py` (protocol definition) — `ProviderBackend` is the canonical name
- `src/degenbot/connection/connection_manager.py` — uses `ProviderAdapter`, not `EthereumProvider`
- `src/degenbot/builders/` — use `PoolIO`, not `ProviderBackend` directly

## Implementation Order

### Slice 1: Delete alias, update all references

1. Delete `EthereumProvider = ProviderBackend` from `interface.py`
2. Remove from both `__all__` lists
3. Remove import and re-export from `__init__.py`
4. Update `offline_provider.py` docstrings
5. Update `interface.py` class docstring
6. Update `connection/CONTEXT.md`
7. Update all test docstrings and class name

### Slice 2: Validate

1. `grep -rn "EthereumProvider" src/ tests/` — remaining hits should only be historical references
2. Run `just lint` + `just test-all`
3. Verify `ProviderBackend` still appears in `__all__` and all re-exports

## Testing

### Per-slice test runs

Slice 1 runs `just test-all`. The alias deletion doesn't affect any runtime behavior.

### New unit tests

No new tests needed. The alias has no callers.

### Integration tests

No changes needed. No integration test imports `EthereumProvider`.

## Benefits

- **Locality**: One name for one concept. No more "is `EthereumProvider` the same as `ProviderBackend`?"
- **Minimal risk**: Similar to Plan 055 (deleting dead protocol classes with zero callers). Low risk but broader scope than initially estimated — ~12 changes across 7 files (code, docstrings, domain docs, tests). No runtime behavior changes.
- **Consistency**: The provider module's public API no longer advertises a misleading name.

## Risks

- **External consumers**: If any user code outside the `degenbot` package imports `EthereumProvider`, the deletion would break them. Mitigation: `EthereumProvider` was a public name but was always documented as the "provider backend protocol." The canonical name `ProviderBackend` has been available since Plan 042. This is a standard deprecation removal. The commit includes `BREAKING CHANGE:` per AGENTS.md conventions.

## Relationship to Other Plans

- **Plan 042** (Collapse Provider Adapter Mirror): Completed. Merged `EthereumProvider` + `_SyncProviderBackend` → `ProviderBackend` and left `EthereumProvider` as a backward-compatibility alias. This plan completes the cleanup by deleting the alias.
- **Plan 058** (Collapse Subscription Stubs): Orthogonal. Both touch `provider/interface.py` but on different methods. Execute 061 first (smaller, lower risk) or in parallel.
- **Plan 059** (Delete Deprecated `build_*` Pass-Throughs): Orthogonal. Different module.
- **Plan 062** (Extract Chainlink into Package): Orthogonal. Different module entirely.

## Status

[x] Slice 1: Delete alias, update all references
[x] Slice 2: Validate
