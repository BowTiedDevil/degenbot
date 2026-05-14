# Plan 006: Universal `build_pool` with Type Resolution

**Status: COMPLETE** ✅

## Problem

Bot has five separate `build_*_pool` methods, one per invariant family:

- `build_v2_pool(address, ...)`
- `build_v3_pool(address, ...)`
- `build_v4_pool(pool_id, pool_manager_address, ...)`
- `build_curve_pool(address, ...)`
- (Camelot is handled as a branch inside `build_v2_pool`)

Callers must already know which invariant family a pool address belongs to before they can construct it. This forces the caller to hold type knowledge that Bot itself could discover from several available sources:

1. **Pool Registry** — the pool was already built in this session
2. **Database** — the `kind` column gives the exact polymorphic type
3. **Factory address** — via Plan 002's `PoolClassRegistry` or `FACTORY_DEPLOYMENTS`
4. **On-chain probing** — calling `slot0()`, `getReserves()`, `coins()`, etc. to identify the invariant

Bot should offer a single `build_pool()` entry point that resolves the type automatically and dispatches to the appropriate typed builder.

## Solution

### 1. Universal entry point

```python
def build_pool(
    self,
    address: str,
    *,
    pool_id: str | bytes | None = None,
    chain_id: ChainId | None = None,
    state_block: int | None = None,
    silent: bool = False,
    # Pass-through hints for builders that need them
    deployer_address: str | None = None,
    init_hash: str | None = None,
    tick_bitmap: Any | None = None,
    tick_data: Any | None = None,
) -> AbstractLiquidityPool:
```

The presence of `pool_id` is the **V4 discriminator**: when provided, `address` is interpreted as a PoolManager contract rather than a pool contract. Without it, `address` is the pool contract address and the type-resolver runs.

This preserves the ability to construct any pool through a single method while respecting the fundamental difference between address-identified and (pool_manager, pool_id)-identified pools.

### 2. PoolTypeDescriptor

The resolver returns a structured descriptor, not a class:

```python
class PoolInvariant(Enum):
    CONSTANT_PRODUCT = "constant_product"       # V2-family
    CONCENTRATED_LIQUIDITY = "concentrated_liquidity"  # V3-family
    STABLESWAP = "stableswap"                    # Curve V1-family
    WEIGHTED = "weighted"                        # Balancer (future)

@dataclass(frozen=True)
class PoolTypeDescriptor:
    invariant: PoolInvariant
    variant: str | None         # "sushiswap", "camelot", "aerodrome_v2", etc.
                                # None = canonical Uniswap variant
    factory: ChecksumAddress | None
```

### 3. Type resolution chain

```python
def _resolve_pool_type(
    self,
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
) -> PoolTypeDescriptor:
```

Sources consulted in order:

| Step | Source | What it provides | Notes |
|------|--------|------------------|-------|
| 1 | Pool Registry | Already-built `AbstractLiquidityPool` | Short-circuits: return the existing pool directly |
| 2 | Database `kind` column | Exact polymorphic identity (`sushiswap_v2`, `camelot_v2`, etc.) | Strictly more informative than factory lookup |
| 3 | `PoolClassRegistry` (Plan 002) | Class from factory address | Requires on-chain `factory()` call if DB miss |
| 4 | `FACTORY_DEPLOYMENTS` | Deployer init_hash from factory address | Supplemental, not type-resolving alone |
| 5 | On-chain probing | Invariant detection via contract calls | Last resort for unknown contracts |

**Step 2 is the primary path for DB-populated pools.** When the DB has a row, `kind` directly answers the question. The factory address is still read for pool construction (passed to `__init__`) but is not needed for type resolution.

**Step 3 is the primary path for new (un-persisted) pools.** The on-chain `factory()` call returns a factory address, and the PoolClassRegistry maps it to a class. The class's inheritance tells us the invariant.

**Step 5 requires multiple RPC calls with revert handling.** The probe sequence:

```
try slot0()          → CONCENTRATED_LIQUIDITY
try getReserves()    → CONSTANT_PRODUCT
  try stableSwap()   → variant="camelot"
  try coins()        → STABLESWAP (Curve)
any other heuristic  → ???
```

This is expensive and fragile. In practice, step 2 or 3 resolves almost every pool. Step 5 exists for pools whose factory is not in any registry and not in the DB — an extremely rare case.

### 4. Dispatch to typed builders

The descriptor selects the builder:

```python
def build_pool(self, address, *, pool_id=None, chain_id=None, ...):
    address = get_checksum_address(address)
    chain_id = chain_id or self.connections.default_chain_id

    # V4 fast path
    if pool_id is not None:
        return self.build_v4_pool(
            pool_id=pool_id,
            pool_manager_address=address,
            chain_id=chain_id,
            state_block=state_block,
            silent=silent,
        )

    # Check pool registry first
    existing = self.pools.get(pool_address=address, chain_id=chain_id)
    if existing is not None:
        return existing

    # Resolve type
    pool_type = self._resolve_pool_type(address, chain_id=chain_id)

    # Dispatch
    match pool_type.invariant:
        case PoolInvariant.CONSTANT_PRODUCT:
            return self._build_constant_product_pool(address, pool_type, chain_id=chain_id, ...)
        case PoolInvariant.CONCENTRATED_LIQUIDITY:
            return self._build_concentrated_liquidity_pool(address, pool_type, chain_id=chain_id, ...)
        case PoolInvariant.STABLESWAP:
            return self._build_stableswap_pool(address, pool_type, chain_id=chain_id, ...)
```

Each `_build_*_pool` private method contains the data-fetching and construction logic currently in `build_v2_pool`, `build_v3_pool`, `build_curve_pool`, and the Camelot branch. They receive the `PoolTypeDescriptor` so they know which variant class to instantiate.

### 5. kind → PoolTypeDescriptor mapping

A private mapping from DB `kind` values to `PoolTypeDescriptor`:

```python
_KIND_TO_DESCRIPTOR: dict[str, PoolTypeDescriptor] = {
    "uniswap_v2":      PoolTypeDescriptor(invariant=PoolInvariant.CONSTANT_PRODUCT, variant=None, factory=None),
    "sushiswap_v2":   PoolTypeDescriptor(invariant=PoolInvariant.CONSTANT_PRODUCT, variant="sushiswap", factory=None),
    "pancakeswap_v2": PoolTypeDescriptor(invariant=PoolInvariant.CONSTANT_PRODUCT, variant="pancakeswap", factory=None),
    "camelot_v2":     PoolTypeDescriptor(invariant=PoolInvariant.CONSTANT_PRODUCT, variant="camelot", factory=None),
    "aerodrome_v2":   PoolTypeDescriptor(invariant=PoolInvariant.CONSTANT_PRODUCT, variant="aerodrome_v2", factory=None),
    "swapbased_v2":   PoolTypeDescriptor(invariant=PoolInvariant.CONSTANT_PRODUCT, variant="swapbased", factory=None),
    "uniswap_v3":     PoolTypeDescriptor(invariant=PoolInvariant.CONCENTRATED_LIQUIDITY, variant=None, factory=None),
    "sushiswap_v3":   PoolTypeDescriptor(invariant=PoolInvariant.CONCENTRATED_LIQUIDITY, variant="sushiswap", factory=None),
    "pancakeswap_v3": PoolTypeDescriptor(invariant=PoolInvariant.CONCENTRATED_LIQUIDITY, variant="pancakeswap", factory=None),
    "aerodrome_v3":   PoolTypeDescriptor(invariant=PoolInvariant.CONCENTRATED_LIQUIDITY, variant="aerodrome_v3", factory=None),
}
```

The `factory` field is `None` in the mapping because DB rows carry the factory address separately. It's filled in during resolution.

## Relationship to Plan 002 (PoolClassRegistry)

Plan 002 is a **prerequisite** for the on-chain resolution path (step 3). It moves the `factory → class` mapping out of `bot.py` into a registry that each DEX module populates. The universal `build_pool` consumes that registry as one of its resolution sources.

Without Plan 002, step 3 would still need to reference the hard-coded class maps in `bot.py`, which is the exact coupling Plan 002 removes.

Plan 002 can be implemented independently and first. This plan then layers the type-resolver and dispatch on top of it.

## Implementation steps

### Phase 1: Define the type system

1. Create `src/degenbot/types/pool_type.py` with `PoolInvariant` enum and `PoolTypeDescriptor` dataclass.
2. Create `_KIND_TO_DESCRIPTOR` mapping from DB `kind` values to descriptors.

### Phase 2: Implement the type resolver

3. Add `_resolve_pool_type(address, *, chain_id)` to `Bot` with the 5-step chain.
4. Currently steps 4–5 (on-chain probing for completely unknown factories) can raise an error with a clear message ("factory 0x... not recognized; specify the pool type explicitly"). This avoids building fragile heuristic probing in this plan. It can be added later as a separate effort.

### Phase 3: Add `build_pool` entry point

5. Add `build_pool()` as the universal entry point with V4 discriminator (`pool_id`).
6. Implement dispatch to the existing `build_v2_pool`, `build_v3_pool`, `build_curve_pool` methods. Initially, `build_pool` delegates rather than replaces — the typed `build_*_pool` methods remain public as explicit paths for callers who already know the type.

### Phase 4: Wire DB `kind` into resolution

7. When `pool_from_db is not None`, use `_KIND_TO_DESCRIPTOR[kind]` to get the descriptor instead of the factory → class-map lookup.
8. Pass the descriptor into the builder so it knows which variant class to instantiate without re-deriving it.

### Phase 5: Tests

9. Test `_resolve_pool_type` for each step: registry hit, DB hit (each `kind`), PoolClassRegistry hit, unknown factory error.
10. Test `build_pool` dispatch: V2, V3, V4 (via `pool_id`), Curve.
11. Test that `build_pool` returns an existing pool from the registry without re-building.
12. Verify existing `tests/test_pool_subclass_selection.py` still passes.

### Phase 6: Update call sites

13. Update internal Bot methods and examples to use `build_pool` where appropriate.
14. The typed `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool` methods remain public for backward compatibility and explicit use.

## Metrics

| Metric | Before | After |
|--------|--------|-------|
| Entry points for pool construction | 4 separate methods (`build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`) | 1 universal entry point + 4 explicit paths |
| Caller must know pool type in advance | Yes | No (for V2/V3/Curve with DB or known factory) |
| DB `kind` column used for type resolution | No (only factory address is used from DB) | Yes (direct, more informative) |
| Unknown factory handling | Silent fallback to UniswapV2Pool / UniswapV3Pool | Explicit error with clear message |

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| On-chain probing (step 5) is fragile and expensive | Don't implement it in this plan. Raise a clear error for unknown factories. Probing can be added as a separate effort. |
| `build_pool` signature accumulates pass-through kwargs from all builders | Accept it for now. The alternative (builder-specific `**kwargs`) loses type safety. Can be refined when builders are extracted (Plan 001). |
| V4 pools require `pool_id` — callers might pass just an address expecting it to work | The universal entry point documents this clearly: without `pool_id`, `address` is treated as a pool contract. A PoolManager address without `pool_id` is meaningless for V4. |
| AerodromeV2Pool has a different constructor signature (stable, fee) | The `_build_constant_product_pool` method handles AerodromeV2 as an internal branch (same as today's exclusion from the class map). The `variant="aerodrome_v2"` descriptor triggers that branch. |
| DB `kind` and factory-based lookup could disagree | `kind` is authoritative (written at persistence time from the same source). If they disagree, it's a data bug, not a resolution bug. |

## Dependencies

- **Plan 002** (PoolClassRegistry): Required for step 3 of the resolution chain. Without it, on-chain `factory()` → class lookup has no clean home.
- **Plan 001** (Pool builders): Optional. If Plan 001 is also implemented, the dispatch targets are the extracted builder classes rather than private methods on Bot. This plan is compatible with either.
