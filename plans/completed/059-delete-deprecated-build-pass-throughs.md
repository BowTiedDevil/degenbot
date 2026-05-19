# Plan 059: Delete Deprecated `build_*` Pass-Throughs and `get_web3`

## Overview

Delete the 4 deprecated `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, and `build_curve_pool` methods from `Bot`, plus `get_web3` from `Bot`, `AsyncBot`, `ConnectionManager`, and `AsyncConnectionManager`. These methods were deprecated in Plan 044 in favor of `build_pool()` and `get_provider()`, emit `DeprecationWarning` on every call, and duplicate logic already handled by the builder registry and type resolution.

**BREAKING CHANGE**: All 5 `Bot` methods and 2 `ConnectionManager` methods are removed. Callers must use `build_pool()` and `get_provider()` instead.

## Problem

### Deletion test

If you deleted `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`, and `get_web3` from `Bot`, nothing would break internally. `build_pool()` already handles every case these methods cover — V4 fast path, registry check, type resolution, builder dispatch. `get_provider()` replaces `get_web3()`. The deleted methods add zero behavior; they are pure pass-throughs with deprecation warnings.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| 4 deprecated `build_*` methods (~171 lines) | `bot.py` L355–576 | A reader of `Bot`'s public API sees 5 `build_*` methods and must read docstrings to determine which is canonical. The deprecated methods obscure the real entry point. |
| `build_v2_pool` re-implements factory→subclass dispatch | `bot.py` L383–407 | `build_v2_pool` calls `fetch_factory_from_chain()` then `pool_type_registry.get_v2_class()` and manually branches on `issubclass(AerodromeV2Pool)` / `issubclass(CamelotLiquidityPool)`. `build_pool()` handles this through the builder registry's MRO fallback. Dual maintenance risk. |
| `get_web3` deprecated pass-through | `bot.py` L580–586; `async_bot.py` L463–467 | Thin pass-throughs that delegate to `ConnectionManager.get_web3()`. Add noise to the API surface. `AsyncBot.get_web3` lacks its own `warnings.warn` — it relies on the ConnectionManager to emit the warning. |
| `ConnectionManager.get_web3` and `AsyncConnectionManager.get_web3` | `connection_manager.py` L56–85; `async_connection_manager.py` L56–85 | ~30 lines each. Exist only to support the Bot pass-throughs. Include a `provider_type != "web3"` guard that `get_provider()` does not replicate. |

### Line counts

| Method | File | Lines | Approx |
|--------|------|-------|--------|
| `build_v2_pool` | `bot.py` | L355–413 | 59 |
| `build_v3_pool` | `bot.py` | L465–501 | 37 |
| `build_v4_pool` | `bot.py` | L502–545 | 44 |
| `build_curve_pool` | `bot.py` | L546–576 | 31 |
| `Bot.get_web3` | `bot.py` | L580–586 | 7 |
| `AsyncBot.get_web3` | `async_bot.py` | L463–467 | 5 |
| `ConnectionManager.get_web3` | `connection_manager.py` | L56–85 | 30 |
| `AsyncConnectionManager.get_web3` | `async_connection_manager.py` | L56–85 | 30 |
| **Total** | | | **~243** |

## Solution

### Scope: no migration needed

Investigation confirms zero internal callers of the deprecated methods:

- **No test** calls `bot.build_v2_pool()`, `bot.build_v3_pool()`, `bot.build_v4_pool()`, or `bot.build_curve_pool()`. Test helpers named `_build_v2_pool` etc. already call `bot.build_pool()`.
- **No production code** in `src/` calls these methods (excluding the definitions themselves).
- The only code calling `cm.get_web3()` is `test_config.py` (2 assertions testing the deprecated method itself).
- `tests/helpers/bot_test_harness_prototype.py` references `build_curve_pool` but is an unevaluated design prototype, not a collected test.

Therefore, there are no callers to migrate — only the method definitions and one test of `get_web3` itself.

### Design decisions

- **Delete, don't deprecation-cycle further**: These methods have been deprecated since Plan 044. A further deprecation period adds no value — callers have had time to migrate.
- **Delete `get_web3` from ConnectionManagers too**: `ConnectionManager.get_web3()` is only used by `Bot.get_web3()`. It's not part of the builder-facing API (builders use `ProviderAdapter` / `PoolIO`). If any external code calls `bot.connections.get_web3()`, they should use `bot.get_provider()` instead.
- **Keep `build_pool` signature unchanged**: No need to modify the canonical method. Its signature already accepts all the kwargs the deprecated methods accepted.
- **`get_web3` type guard is lost**: `ConnectionManager.get_web3()` checks `provider.provider_type != "web3"` and raises `DegenbotValueError("Provider is not a Web3 provider.")`. `get_provider()` does not replicate this guard. After deletion, callers who need a raw `Web3` instance must do their own type narrowing: `provider = cm.get_provider(chain_id); w3 = provider.as_web3()`. This is consistent with ADR-001 (ProviderAdapter as the I/O primitive).
- **Historical doc references**: References to `build_v2_pool` etc. in completed plans (002, 006, 012) and architecture docs are historical — describing what existed at the time. These get "(removed by Plan 059)" annotations, consistent with the Plan 061 pattern for `EthereumProvider`.

### Imports that become unused

After deleting the 5 methods from `Bot`, these imports in `bot.py` become dead code:

| Import | Used only in | Action |
|--------|-------------|--------|
| `fetch_factory_from_chain` (from `type_resolution`) | `build_v2_pool` L380 | Remove from import block |
| `pool_type_registry` (from `registry.pool_type`) | `build_v2_pool` L382 | Remove from import block |
| `import warnings` | 4× `warnings.warn()` in deprecated methods | Remove |
| `Web3` (TYPE_CHECKING import) | `get_web3` return type L580 | Remove from TYPE_CHECKING block |

After deleting `get_web3` from `AsyncBot`, these imports in `async_bot.py` become dead:

| Import | Used only in | Action |
|--------|-------------|--------|
| `AsyncWeb3`, `AsyncBaseProvider` (TYPE_CHECKING) | `get_web3` return type L463 | Remove from TYPE_CHECKING block |

Note: `Web3` (the concrete class, not the TYPE_CHECKING import) is still used in `async_bot.py` for `Web3.keccak()` — do not remove.

## Files Involved

**Primary (method deletion):**
- `src/degenbot/bot.py` — delete `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`, `get_web3` (~178 lines). Remove unused imports: `fetch_factory_from_chain`, `pool_type_registry`, `warnings`, `Web3` (TYPE_CHECKING).
- `src/degenbot/async_bot.py` — delete `get_web3` (~5 lines). Remove unused TYPE_CHECKING imports: `AsyncWeb3`, `AsyncBaseProvider`.
- `src/degenbot/connection/connection_manager.py` — delete `get_web3` (~30 lines).
- `src/degenbot/connection/async_connection_manager.py` — delete `get_web3` (~30 lines).

**Secondary (test/doc updates):**
- `tests/test_config.py` — rewrite `get_web3` assertions (L33–37) as `get_provider()` + `as_web3()` equivalents.
- `docs/architecture/io-free-pools.md` — annotate `build_v2_pool` etc. references with "(removed by Plan 059)".
- `docs/adr/ADR-001-io-free-pools.md` — annotate `build_v2_pool` etc. references with "(removed by Plan 059)".
- `plans/completed/002-pool-class-registry.md` — annotate references with "(removed by Plan 059)".
- `plans/completed/006-universal-build-pool.md` — annotate references with "(removed by Plan 059)".
- `plans/completed/012-bot-session.md` — annotate references with "(removed by Plan 059)".

**No change needed:**
- `src/degenbot/builders/type_resolution.py` — `fetch_factory_from_chain` is still used by `_resolve_pool_type_impl()` and the async variant. Only the `bot.py` import is dead.
- `src/degenbot/registry/pool_type.py` — `pool_type_registry` is used extensively by builders. Only the `bot.py` import is dead.
- `src/degenbot/cli/exchange.py` — doesn't call deprecated methods.
- `tests/helpers/bot_test_harness_prototype.py` — unevaluated prototype; not worth updating.

## Implementation Order

### Slice 1: Delete deprecated methods and clean up imports

1. Delete `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`, `get_web3` from `bot.py`
2. Remove unused imports from `bot.py`: `fetch_factory_from_chain`, `pool_type_registry`, `warnings`, `Web3` (TYPE_CHECKING)
3. Delete `get_web3` from `async_bot.py`
4. Remove unused TYPE_CHECKING imports from `async_bot.py`: `AsyncWeb3`, `AsyncBaseProvider`
5. Delete `get_web3` from `ConnectionManager` and `AsyncConnectionManager`
6. Rewrite `test_config.py` L33–37: replace `cm.get_web3()` assertions with `cm.get_provider()` + `provider.as_web3()`; replace the `DegenbotValueError` test with `cm.get_provider(69)` raising `DegenbotValueError`
7. Run: `just test-python` — expect all tests green

### Slice 2: Annotate historical references and validate

1. Add "(removed by Plan 059)" annotations to `build_v2_pool`/`build_v3_pool`/`build_v4_pool`/`build_curve_pool`/`get_web3` references in:
   - `docs/architecture/io-free-pools.md`
   - `docs/adr/ADR-001-io-free-pools.md`
   - `plans/completed/002-pool-class-registry.md`
   - `plans/completed/006-universal-build-pool.md`
   - `plans/completed/012-bot-session.md`
2. Run: `just lint` + `just test-all`
3. Verify: `grep -rn "\.build_v2_pool\|\.build_v3_pool\|\.build_v4_pool\|\.build_curve_pool\|\.get_web3" src/ tests/` — expect zero results (references in docs/plans are not callable `.method` patterns)

## Testing

### Per-slice test runs

Slice 1 runs `just test-python`. Slice 2 runs `just lint` + `just test-all`.

### New unit tests

No new tests needed. The existing `build_pool()` tests cover all paths that the deprecated methods covered. The `test_config.py` rewrite preserves the underlying behavior test (provider lookup + type narrowing).

### Integration tests

Existing integration tests using `Bot.build_pool()` are unaffected.

## Benefits

- **Locality**: One entry point for pool construction. No ambiguity about which `build_*` method to call.
- **Leverage**: ~243 lines deleted. No more dual-maintenance risk where `build_v2_pool`'s factory dispatch diverges from the builder registry's MRO fallback.
- **Depth**: `Bot`'s public API surface shrinks from 5 `build_*` methods to 1, making the seam deeper — more behavior behind a smaller interface.

## Risks

- **External callers**: If any user code outside the `degenbot` package calls `bot.build_v2_pool()` etc., the deletion would break them. Mitigation: these methods have been deprecated with `DeprecationWarning` since Plan 044. This is a standard major-version deprecation removal. Commit message must include `BREAKING CHANGE: Removed build_v2_pool, build_v3_pool, build_v4_pool, build_curve_pool, and get_web3 from Bot/AsyncBot. Use build_pool() and get_provider() instead.`
- **`get_web3` on ConnectionManager**: If any code directly calls `bot.connections.get_web3()`, that would break. Mitigation: use `bot.get_provider(chain_id)` which returns a `ProviderAdapter`. This is consistent with ADR-001 (I/O-free pools, ProviderAdapter as the I/O primitive).
- **Lost `provider_type != "web3"` guard**: `ConnectionManager.get_web3()` validated that the provider was a Web3 provider before returning the raw `Web3` instance. After deletion, callers who need a raw `Web3` instance must do their own narrowing: `provider = cm.get_provider(chain_id); w3 = provider.as_web3()`. The `as_web3()` method returns `None` for non-Web3 providers, which is a safe fallback.

## Relationship to Other Plans

- **Plan 044** (Deprecate Bot Pass-Throughs): Completed. Introduced the deprecation warnings. This plan completes the deprecation cycle by deleting the methods.
- **Plan 006** (Universal `build_pool` with Type Resolution): Completed. Established `build_pool()` as the canonical entry point. This plan removes the legacy alternatives.
- **Plan 058** (Collapse Subscription Stubs): Orthogonal. Different module (`provider/` vs `bot.py`).
- **Plan 060** (Unify Sync/Async Builder Orchestration): Complementary. A cleaner `Bot` surface makes the builder refactoring easier to reason about.
- **Plan 061** (Delete `EthereumProvider` Alias): Same pattern — delete a deprecated entity, annotate historical references. Completed.

## Status

[x] Slice 1: Delete deprecated methods, clean up imports, rewrite test_config.py
[x] Slice 2: Annotate historical references and validate
