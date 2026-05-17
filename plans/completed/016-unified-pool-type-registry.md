# Plan 016: Unified Pool Type Registry

**Status: COMPLETE** ✅

## Problem

Pool type identity and deployment data are scattered across five separate locations:

1. **`PoolClassRegistry`** (Plan 002) — maps `(chain_id, factory)` → pool class, split into V2/V3 dicts
2. **`FACTORY_DEPLOYMENTS`** — maps `(chain_id, factory)` → deployer + init_hash
3. **`_KIND_TO_DESCRIPTOR`** — maps DB `kind` string → `PoolTypeDescriptor`
4. **`_variant_from_class()`** — maps class → variant string via hard-coded dict
5. **`variant` class attribute** — each pool class carries its variant, but `_variant_from_class()` duplicates this knowledge

Adding a new DEX deployment requires touching all five places (plus `bot.py`). The variant is defined both as a class attribute and in the `_variant_from_class` mapping — a DRY violation. The deployment data (deployer, init_hash) lives separately from the class registration, even though they're always paired.

## Solution

A single **PoolTypeRegistry** that replaces all five with one registration call per deployment:

```python
pool_type_registry.register(
    SushiswapV2Pool,
    chain_id=1,
    factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
    pool_init_hash="0xe18a34eb...",
    deployer=None,  # defaults to factory_address
)
```

Identity (invariant, variant, kind) is auto-derived:

| Field | Source | Mechanism |
|-------|--------|-----------|
| `invariant` | Class hierarchy | `_derive_invariant()`: subclasses `AbstractUniswapV2Pool` → CONSTANT_PRODUCT, `AbstractConcentratedLiquidityPool` → CONCENTRATED_LIQUIDITY |
| `variant` | Class attribute | `pool_class.variant` — a `ClassVar[str | None]` on every pool class. `None` = canonical Uniswap variant |
| `kind` | Derived | `derive_kind(invariant, variant)` — pure function. CONSTANT_PRODUCT + "sushiswap" → "sushiswap_v2", CONCENTRATED_LIQUIDITY + None → "uniswap_v3" |

### Variant rule: bare DEX name, no suffix

The `variant` attribute is the bare DEX name (e.g. `"sushiswap"`, `"aerodrome"`), never including `_v2` or `_v3`. The invariant-derived suffix is added by `derive_kind()`:

- `variant="aerodrome"` + CONSTANT_PRODUCT → `kind="aerodrome_v2"` ✅
- `variant="aerodrome"` + CONCENTRATED_LIQUIDITY → `kind="aerodrome_v3"` ✅
- ~~`variant="aerodrome_v2"` + CONSTANT_PRODUCT → `kind="aerodrome_v2_v2"`~~ ❌ double-suffix bug

### PoolTypeDescriptor gains `kind` field

```python
@dataclass(frozen=True)
class PoolTypeDescriptor:
    invariant: PoolInvariant
    variant: str | None
    kind: str  # NEW: "sushiswap_v2", "camelot_v2", etc.
    factory: ChecksumAddress | None
```

The `kind` field makes the descriptor self-contained for DB polymorphic identity without a separate `_KIND_TO_DESCRIPTOR` lookup.

### PoolDeploymentData

A frozen dataclass for per-chain deployment data:

```python
@dataclass(frozen=True)
class PoolDeploymentData:
    factory_address: str
    deployer: str
    pool_init_hash: str | None
```

### Lookup API

```python
class PoolTypeRegistry:
    def register(pool_class, *, chain_id, factory_address, pool_init_hash=None, deployer=None) -> None
    def set_default_v2_class(pool_class) -> None
    def set_default_v3_class(pool_class) -> None
    def has_registration(chain_id, factory_address) -> bool
    def get_class(chain_id, factory_address) -> type | None
    def get_v2_class(chain_id, factory_address) -> type | None   # with default fallback
    def get_v3_class(chain_id, factory_address) -> type | None   # with default fallback
    def get_descriptor(chain_id, factory_address) -> PoolTypeDescriptor | None
    def get_deployment(chain_id, factory_address) -> PoolDeploymentData | None
```

### Exclusions

- **V4 deployments** — not keyed by factory (use PoolManager). Not covered by this registry.

## Implementation steps

### Phase 1: Core types ✅

1. ~~Add `kind` field to `PoolTypeDescriptor`~~
2. ~~Add `derive_kind()` pure function to `pool_type.py`~~
3. ~~Add `variant: ClassVar[str | None]` to all pool classes~~
4. ~~Fix Aerodrome variants: `"aerodrome_v2"`/`"aerodrome_v3"` → `"aerodrome"`~~

### Phase 2: PoolTypeRegistry ✅

5. ~~Create `src/degenbot/registry/pool_type.py` with `PoolTypeRegistry`, `PoolDeploymentData`, `_derive_invariant`~~
6. ~~Create module-level singleton `pool_type_registry`~~
7. ~~Export from `src/degenbot/registry/__init__.py`~~

### Phase 3: DEX module self-registration ✅

8. ~~Migrate `uniswap/__init__.py` to register with `pool_type_registry` (and set defaults)~~
9. ~~Migrate `sushiswap/__init__.py` — 3 V2 + 3 V3 factories (added Base SushiswapV3)~~
10. ~~Migrate `pancakeswap/__init__.py` — 2 V2 + 2 V3 factories~~
11. ~~Migrate `aerodrome/__init__.py` — V3 only (V2 excluded)~~
12. ~~Migrate `camelot/__init__.py` — V2 on Arbitrum~~
13. ~~Add `swapbased/__init__.py` — V2 on Base (was previously unregistered)~~

Each DEX `__init__.py` currently dual-registers with both `pool_class_registry` (old) and `pool_type_registry` (new) during the transition.

### Phase 4: Bot migration ✅

14. ~~`_resolve_pool_type`: replaced `pool_class_registry.has_v2_registration`/`has_v3_registration` + manual descriptor construction with `pool_type_registry.get_descriptor()`~~
15. ~~`_resolve_pool_type_by_probing`: uses `pool_type_registry.get_descriptor()` for known factories, falls back to `variant=None` for unknown~~
16. ~~`build_v2_pool`: deployment data from `pool_type_registry.get_deployment()` instead of `FACTORY_DEPLOYMENTS`; class from `pool_type_registry.get_v2_class()`~~
17. ~~`build_v3_pool`: same migration as build_v2_pool~~
18. ~~Removed `_variant_from_class()` (variant now auto-derived from class attribute)~~
19. ~~Removed `pool_class_registry` and `FACTORY_DEPLOYMENTS` imports from bot.py~~

### Phase 5: Remove old infrastructure ✅

20. ~~Added `get_descriptor_by_kind()` to `PoolTypeRegistry` — reverse index built during `register()`~~
21. ~~Replaced `_KIND_TO_DESCRIPTOR` usage in `_resolve_pool_type` with `pool_type_registry.get_descriptor_by_kind()`~~
22. ~~Removed `_KIND_TO_DESCRIPTOR` from `pool_type.py`~~
23. ~~Removed `PoolClassRegistry`, its singleton, and all dual-registration calls from DEX `__init__.py` modules~~
24. ~~Removed `has_v2_registration`/`has_v3_registration` tests, migrated to `pool_type_registry` equivalents~~
25. ~~Removed `test_pool_class_registry.py` (superseded by `test_pool_type_registry.py` and `test_pool_type_registry_singleton.py`)~~

### Phase 6: `from_chain` classmethod ✅

25. ~~Add `from_chain` classmethod to CamelotLiquidityPool (fetches stableSwap/FEE_DENOMINATOR/fee percents from chain)~~
26. ~~Add `from_chain` classmethod to AerodromeV2Pool (fetches stable/fee from chain via `stable()` + `getFee(address,bool)`)~~
27. ~~Register AerodromeV2Pool in pool_type_registry (now possible via `from_chain`)~~
28. ~~Add `"aerodrome_v2"` to the kind reverse index~~
29. ~~Replace Camelot-specific branch in `build_v2_pool` with generic `from_chain` dispatch~~
30. ~~Remove `CamelotLiquidityPool` import from bot.py (no longer directly referenced)~~
31. ~~Add `deployer_address` kwarg to CamelotLiquidityPool.__init__ (forwarded to UniswapV2Pool)~~

### Phase 7: Public API for external callers ✅

29. ~~Add `PoolTypeRegistry` and `pool_type_registry` to top-level `__init__.py` exports~~
30. ~~Document the registration API in `PoolTypeRegistry` docstring with usage example~~
31. ~~Document `from_chain` classmethod convention for pools with non-standard constructors~~
32. ~~Document variant rule (bare DEX name, no suffix) in docstring~~

### Phase 8: Docs updates ✅

33. ~~Update `AGENTS.md` — replaced PoolClassRegistry → Pool Type Registry~~
34. ~~Update `CONTEXT-MAP.md` — replaced all Pool Class Registry references, updated cross-module relationships, updated example dialogue, updated ambiguity rulings~~
35. ~~Update `registry/CONTEXT.md` — rewrote for Pool Type Registry, added Pool Class Registry vs Pool Type Registry ambiguity ruling, documented `from_chain` convention~~
36. ~~Update `types/CONTEXT.md` — fixed Pool Variant definition (bare DEX name, no suffix), added `from_chain` and `kind` terms~~

## Metrics

| Metric | Before | After |
|--------|--------|-------|
| Registration sites per deployment | 5 (PoolClassRegistry, FACTORY_DEPLOYMENTS, _KIND_TO_DESCRIPTOR, _variant_from_class, variant attribute) | 1 (`pool_type_registry.register()`) |
| Places to edit when adding a new DEX | 5+ | 1 (the DEX module's `__init__.py`) |
| Where variant is defined | Class attribute + `_variant_from_class()` dict | Class attribute only |
| Where deployment data lives | `FACTORY_DEPLOYMENTS` (separate from class mapping) | Same `register()` call as the class |
| `_variant_from_class()` in bot.py | ~15 lines + 6 imports | Removed |
| Bot.py imports from old registries | `pool_class_registry`, `FACTORY_DEPLOYMENTS`, `_FACTORY_DEPLOYMENTS` | `pool_type_registry` only |
| `_KIND_TO_DESCRIPTOR` mapping | 51 lines in `pool_type.py` | Removed — superseded by `get_descriptor_by_kind()` reverse index |
| `PoolClassRegistry` / `pool_class_registry` | ~93 lines in `registry/pool_class.py` | Removed |
| Camelot-specific branch in `build_v2_pool` | ~50 lines of chain fetch + constructor | 0 — replaced by `from_chain` dispatch |
| AerodromeV2Pool registration | Not registered (excluded) | Registered via `from_chain` |
| Built-in registrations | 19 | 20 (added AerodromeV2) |

## Test coverage

| Test file | Tests | What it validates |
|-----------|-------|-------------------|
| `test_pool_type_registry.py` | 29 | Kind derivation, invariant derivation, variant from class attribute, registration/lookup, deployment data, default fallback, descriptor shape, kind reverse lookup |
| `test_full_exchange_registration.py` | 128 | All 20 built-in deployments registered correctly (class, invariant, variant, kind, deployment data, descriptors). Cross-checks vs FACTORY_DEPLOYMENTS. **AerodromeV2 now registered**. Default fallback. |
| `test_pool_type_registry_singleton.py` | 143 | Module-level singleton correctly populated by DEX self-registration. 20 registrations across 3 chains. Cross-checks init_hash and deployer vs FACTORY_DEPLOYMENTS. Kind reverse lookup on singleton (incl. `aerodrome_v2`). |
| `test_pool_type_resolution.py` | 29 | _resolve_pool_type, PoolTypeDescriptor, build_pool dispatch. `variant="aerodrome"` produces correct kinds. `aerodrome_v2` kind resolvable. |
| `test_from_chain.py` | 13 | `from_chain` classmethod on CamelotLiquidityPool and AerodromeV2Pool. Fake provider with ABI-encoded responses. Fee/fraction/stable assertions. deployer_address forwarding. UniswapV2Pool/AerodromeV3Pool do NOT have `from_chain`. |
| `test_pool_subclass_selection.py` | 4 | Manager-level subclass selection. |

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| AerodromeV2Pool has different constructor signature | `from_chain` classmethod encapsulates the non-standard chain fetches (`stable()`, `getFee()`). Bot dispatches via `hasattr(pool_class, "from_chain")`. |
| `variant="aerodrome"` produces `"aerodrome_v3"` for both V2 and V3 classes | Correct behavior: invariant distinguishes them. Both share the same variant because they're the same DEX. |
| Module-level singleton means import order matters | Registration is in `__init__.py`, which is the standard import path. Tests validate the singleton. |
| AerodromeV2 kind string `"aerodrome_v2"` not in registry | Resolved — AerodromeV2 is now registered via `from_chain`. `get_descriptor_by_kind("aerodrome_v2")` returns the correct descriptor. |

## Dependencies

- **Plan 002** (PoolClassRegistry): Superseded by this plan. PoolTypeRegistry replaces it.
- **Plan 006** (Universal build_pool): Already uses `PoolTypeDescriptor` which now has the `kind` field. `_resolve_pool_type` migrated to use `pool_type_registry`.
