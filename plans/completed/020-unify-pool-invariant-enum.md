# Plan 020: Unify the Dual PoolInvariant Enum

**Status: COMPLETE** ✅

## Overview

Eliminate the ambiguity of two `PoolInvariant` enums. `types/pool_type.py` had `PoolInvariant` (4 values, pool identity), while `types/hop_types.py` had a separate `PoolInvariant` (6 values, solver dispatch).

**Solution:** Rename the identity-level enum to `PoolFamily`. Keep the solver-dispatch enum as `PoolInvariant`. Add a backward-compatibility alias `PoolInvariant = PoolFamily` during migration.

## Changes Made

| File | Change |
|------|--------|
| `src/degenbot/types/pool_type.py` | Renamed `PoolInvariant` → `PoolFamily`. Added `PoolInvariant = PoolFamily` alias. Updated `derive_kind(invariant, variant)` → `derive_kind(family, variant)`. Updated `PoolTypeDescriptor.invariant` → `PoolTypeDescriptor.family`. |
| `src/degenbot/registry/pool_type.py` | Renamed `_derive_invariant()` → `_derive_family()`. Updated `PoolTypeDescriptor` construction. Updated `_RegistryEntry` field from `invariant` to `family`. Updated docstrings and comments. |
| `src/degenbot/bot.py` | All `PoolInvariant` references → `PoolFamily` in `build_pool()`, `_resolve_pool_type_by_probing()`, etc. |
| `tests/test_pool_type_resolution.py` | Updated to use `PoolFamily`. Fixed `registrations` iteration to match new tuple structure. |
| `tests/test_pool_type_registry.py` | Updated to use `PoolFamily`. |
| `tests/test_full_exchange_registration.py` | Updated to use `PoolFamily`. |

## Mapping

| `PoolFamily` (identity) | → `PoolInvariant` (solver dispatch) |
|---|---|
| `CONSTANT_PRODUCT` | `CONSTANT_PRODUCT` |
| `CONCENTRATED_LIQUIDITY` | `BOUNDED_PRODUCT` |
| `STABLESWAP` | `CURVE_STABLESWAP` or `SOLIDLY_STABLE` |
| `WEIGHTED` | `BALANCER_WEIGHTED` or `BALANCER_MULTI_TOKEN` |

## Definition of Done

- [x] `PoolInvariant` in `pool_type.py` renamed to `PoolFamily`
- [x] All `PoolTypeDescriptor.invariant` fields renamed to `family`
- [x] All `derive_kind(invariant, variant)` calls updated to `derive_kind(family, variant)`
- [x] `Bot.build_pool()` matches on `PoolFamily` values
- [x] `_derive_invariant()` in registry renamed to `_derive_family()`
- [x] All `pool_type.py` importers updated
- [x] Backward compatibility alias preserved (`PoolInvariant = PoolFamily`)
- [x] `just test-all` passes (2428 passed)
