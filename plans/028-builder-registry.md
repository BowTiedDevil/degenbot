# Plan 028: Builder Registry & Pool Class Restructuring

## Status: PENDING

## Overview

Two interdependent changes:

1. **Builder registry** — Replace `isinstance`-based builder dispatch in `Bot` with a `dict[type, PoolBuilder]` registry keyed on concrete pool class. Adding a new pool family goes from touching 5 places in `Bot` to 2: create the builder, register it.

2. **Pool class restructuring** — Replace the current inheritance-for-code-reuse pattern with a composition of three concerns: **state**, **calc**, and **identity**. Each concrete pool class inherits from `AbstractLiquidityPool` plus type-specific state and calc mixins. Sibling DEX variants (Sushi, PancakeSwap) are peers under the same state+calc mixins, not subclasses of `UniswapV2Pool`.

## Files Involved

**Primary:**
- `src/degenbot/bot.py` — replace `_builder_for_pool()` isinstance chain with dict lookup; remove `build_v2_pool`/`build_v3_pool`/`build_v4_pool`/`build_curve_pool` convenience methods; add `register_builder()`
- `src/degenbot/uniswap/v2_liquidity_pool.py` — extract `V2PoolState` and `UniswapV2PoolCalc` mixins; `UniswapV2Pool` becomes a composition
- `src/degenbot/uniswap/v3_liquidity_pool.py` — extract `V3PoolState` and `UniswapV3PoolCalc` mixins; `UniswapV3Pool` becomes a composition
- `src/degenbot/uniswap/v4_liquidity_pool.py` — extract `V4PoolState` and `UniswapV4PoolCalc` mixins; `UniswapV4Pool` becomes a composition
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — extract `StableswapPoolState` mixin (calc already externalized by Plans 026/027)
- `src/degenbot/aerodrome/pools.py` — extract `AerodromeV2PoolState` + `AerodromeV2PoolCalc`; eliminate `if self.stable` runtime dispatch

**Secondary:**
- `src/degenbot/calculations/` (new) — standalone pure-math functions extracted from pool classes and existing `functions.py` modules
- `src/degenbot/types/abstract/liquidity_pool.py` — remove `AbstractUniswapV2Pool`, `AbstractConcentratedLiquidityPool`, `AbstractAerodromeV2Pool`; replaced by protocols
- `src/degenbot/types/pool_protocols.py` — add `ConstantProductPool`, `ConcentratedLiquidityPool`, `StableswapPool` protocols
- `src/degenbot/registry/pool_type.py` — update `_derive_family()` to use protocols instead of isinstance
- `src/degenbot/builders/v2_pool_builder.py` — update `build()` to work with new type hierarchy
- `src/degenbot/builders/v3_pool_builder.py` — same
- `src/degenbot/builders/v4_pool_builder.py` — same
- `src/degenbot/builders/curve_pool_builder.py` — same
- `src/degenbot/sushiswap/pools.py` — `SushiswapV2Pool` becomes peer of `UniswapV2Pool` under same mixins
- `src/degenbot/pancakeswap/pools.py` — same
- `src/degenbot/swapbased/pools.py` — same
- `src/degenbot/camelot/pools.py` — extract `CamelotV2PoolState`; override calc where needed
- `src/degenbot/arbitrage/optimizers/solver_hop_builders.py` — update isinstance checks to protocol checks
- `src/degenbot/arbitrage/path/swap_amount_builder.py` — same
- All test files — replace `bot.build_v2_pool(...)` calls with `bot.build_pool(...)`; update isinstance checks

## Problem

### Problem 1: Bot is a 5-point wiring harness

Adding a new pool family currently requires:
1. Create the builder class
2. Add `self._xxx_builder = XxxPoolBuilder(...)` to `Bot.__init__`
3. Add `def build_xxx_pool(...)` method to `Bot` (pass-through to builder)
4. Add a `case PoolFamily.XXX:` branch in `Bot.build_pool()`
5. Add a `isinstance(pool, XxxPool)` check in `Bot._builder_for_pool()`

Five touch points for one new pool family. The builder registry collapses this to 2: create the builder, register it.

### Problem 2: Inheritance for code reuse, not identity

The current pool class hierarchy:

```
AbstractLiquidityPool (ABC)
├── AbstractUniswapV2Pool (ABC)
│   └── UniswapV2Pool ← SushiswapV2Pool, PancakeswapV2Pool, SwapbasedV2Pool, CamelotLiquidityPool
├── AbstractAerodromeV2Pool (ABC)
│   └── AerodromeV2Pool (standalone)
├── AbstractConcentratedLiquidityPool (ABC)
│   └── UniswapV3Pool ← SushiswapV3Pool, PancakeswapV3Pool, AerodromeV3Pool
│   └── UniswapV4Pool (standalone, but same ABC as V3)
└── CurveStableswapPool (standalone)
```

Problems with this hierarchy:

- **"Is-a" violation.** `SushiswapV2Pool(UniswapV2Pool)` means `isinstance(sushi, UniswapV2Pool)` returns `True`. Sushi uses the same math, but it *is not* a Uniswap pool.
- **V3/V4 collision.** Both share `AbstractConcentratedLiquidityPool`, but `_builder_for_pool()` must use `isinstance(pool, UniswapV3Pool) and not isinstance(pool, UniswapV4Pool)` to distinguish them. The awkward negation signals the hierarchy is wrong.
- **Hybrid pool dispatch.** `AerodromeV2Pool` has `if self.stable` scattered through its methods — the same address-dispatched problem Curve had before Plans 026/027, but triggered by instance state instead of address.
- **Calculation code locked inside classes.** The standalone `calc_d`, `calc_dp`, `calc_y` functions in Curve's pool class (already partially externalized by Plans 026/029) should live in a math library. Aerodrome's stable math duplicates logic from both `solidly_functions.py` and Curve.

## Solution

### Part 1: Builder Registry

#### Step 1: Add builder registry to Bot

```python
class Bot:
    def __init__(self, config: DegenbotConfig) -> None:
        # ... connections, db, registries ...
        
        self._erc20_builder = Erc20Builder(...)
        
        # Builder registry: concrete pool type → builder
        self._builders: dict[type, PoolBuilder] = {}
        
        # Register builders
        self.register_builder(UniswapV2Pool, V2PoolBuilder(...))
        self.register_builder(SushiswapV2Pool, V2PoolBuilder(...))  # V2 builder handles all V2 variants
        self.register_builder(UniswapV3Pool, V3PoolBuilder(...))
        self.register_builder(UniswapV4Pool, V4PoolBuilder(...))
        self.register_builder(CurveStableswapPool, CurvePoolBuilder(...))
        self.register_builder(AerodromeV2Pool, V2PoolBuilder(...))  # V2 builder also handles Aerodrome
        # ... etc for all concrete types
    
    def register_builder(self, pool_class: type, builder: PoolBuilder) -> None:
        self._builders[pool_class] = builder
```

#### Step 2: Replace `build_pool()` match statement with builder lookup

```python
def build_pool(self, address, *, pool_id=None, chain_id=None, ...):
    address = get_checksum_address(address)
    chain_id = chain_id or self.connections.default_chain_id
    
    # V4 fast path (pool_id discriminates V4 managed pools)
    if pool_id is not None:
        builder = self._builders[UniswapV4Pool]
        return builder.build(pool_id=pool_id, pool_manager_address=address, ...)
    
    # Check pool registry
    existing = self.pools.get(chain_id=chain_id, pool_address=address)
    if existing is not None:
        return existing
    
    # Resolve pool type → concrete class
    pool_type = self._resolve_pool_type(address, chain_id=chain_id)
    
    # Look up builder by concrete pool class
    pool_class = self._pool_class_for_descriptor(pool_type, chain_id=chain_id)
    builder = self._builders.get(pool_class)
    if builder is None:
        raise DegenbotValueError(message=f"No builder for pool class {pool_class.__name__}")
    
    return builder.build(address, chain_id=chain_id, ...)
```

The `_pool_class_for_descriptor()` method resolves a `PoolTypeDescriptor` to a concrete pool class by consulting `pool_type_registry` (existing) or falling back to the default class for the family.

#### Step 3: Replace `_builder_for_pool()` with dict lookup

```python
def update(self, pool, *, block_number=None):
    builder = self._builders.get(type(pool))
    if builder is None:
        msg = f"No builder registered for pool type {type(pool).__name__}"
        raise TypeError(msg)
    return builder.update(pool, block_number=block_number)
```

#### Step 4: Remove `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`

Hard cutover. All callers use `build_pool()`. The builder's `build()` method still accepts the full set of type-specific kwargs.

### Part 2: Pool Class Restructuring

#### Step 5: Create `calculations/` module

Standalone pure-math functions. No `self`, no class references, no I/O.

```
src/degenbot/calculations/
├── __init__.py
├── constant_product.py     # get_amount_out, get_amount_in, fee calc
├── stableswap.py           # calc_d, calc_y, calc_dp + variants (moved from Curve pool class)
├── concentrated_liquidity.py  # tick_math, sqrt_price_math, fee_growth
├── camelot.py               # k_camelot, get_y_camelot, f_camelot
└── solidly_stable.py        # general_calc_d, general_calc_k, general_calc_exact_in_stable
```

Existing `aerodrome/functions.py`, `camelot/functions.py`, `solidly/solidly_functions.py`, and the nested `calc_*` functions inside `curve_stableswap_liquidity_pool.py` move here.

#### Step 6: Extract state and calc mixins for V2 family

New structure for V2-style pools:

```python
# ── State: data attributes (immutable + mutable) ──

class V2PoolState:
    """State for V2-style constant-product pools (Uniswap V2 contract)."""
    
    # Immutable
    _token0: Erc20Token
    _token1: Erc20Token
    _fee_token0: Fraction
    _fee_token1: Fraction
    
    # Mutable
    _reserves_token0: int
    _reserves_token1: int
    _block_number_last: int
    
    @property
    def token0(self) -> Erc20Token: return self._token0
    @property
    def token1(self) -> Erc20Token: return self._token1
    # ... etc


# ── Calc: read-only methods operating on state ──

class UniswapV2PoolCalc:
    """Calculation methods matching the Uniswap V2 contract.
    
    All methods operate on state held by the concrete class via V2PoolState.
    Subclasses override as needed for contract-specific differences.
    """
    
    RESERVES_STRUCT_TYPES: ClassVar[tuple[str, ...]] = ("uint112", "uint112", "uint32")
    FEE: ClassVar[Fraction] = Fraction(3, 1000)
    
    def simulate_swap(self, token_in, amount_in, token_out, state_override=None): ...
    def external_update(self, update): ...
    def _encode_swap(self, ...): ...


# ── Concrete pools ──

class UniswapV2Pool(AbstractLiquidityPool, V2PoolState, UniswapV2PoolCalc):
    variant: ClassVar[str | None] = "uniswap"
    type DatabasePoolType = UniswapV2PoolTable

class SushiswapV2Pool(AbstractLiquidityPool, V2PoolState, UniswapV2PoolCalc):
    variant: ClassVar[str | None] = "sushiswap"
    type DatabasePoolType = SushiswapV2PoolTable

class PancakeswapV2Pool(AbstractLiquidityPool, V2PoolState, UniswapV2PoolCalc):
    variant: ClassVar[str | None] = "pancakeswap"
    type DatabasePoolType = PancakeswapV2PoolTable
    
    FEE: ClassVar[Fraction] = Fraction(25, 10000)
    RESERVES_STRUCT_TYPES: ClassVar[tuple[str, ...]] = ("uint112", "uint112", "uint32")
```

Aerodrome and Camelot define their own state and calc:

```python
class AerodromeV2PoolState:
    _token0: Erc20Token
    _token1: Erc20Token
    _fee: Fraction          # unidirectional
    _stable: bool           # selects calculation path
    _reserves_token0: int
    _reserves_token1: int
    _block_number_last: int

class AerodromeV2PoolCalc:
    """Aerodrome V2 calculations — dispatches to stableswap or constant_product
    based on the stable flag set at construction time."""
    
    def __init__(self, ...):  # or set in pool __init__
        pass

class AerodromeV2Pool(AbstractLiquidityPool, AerodromeV2PoolState, AerodromeV2PoolCalc):
    variant: ClassVar[str | None] = "aerodrome"
    type DatabasePoolType = AerodromeV2PoolTable
    
    def __init__(self, ..., stable: bool):
        # Wire calculation at construction — no runtime if self.stable dispatch
        self._dy_calculator = calc_stableswap_dy if stable else calc_constant_product_dy
        self._fee_calculator = calc_stableswap_fee if stable else calc_constant_product_fee
```

```python
class CamelotV2PoolState:
    _token0: Erc20Token
    _token1: Erc20Token
    _fee_token0: int
    _fee_token1: int
    _fee_denominator: int
    _stable_swap: bool
    _reserves_token0: int
    _reserves_token1: int
    _block_number_last: int

class CamelotV2PoolCalc:
    """Camelot calculations — constant-product or camelot_k."""
    
    # Inherits external_update, encode_swap from UniswapV2PoolCalc
    # (or re-implements if different)
    
    def simulate_swap(self, ...):  # overrides UniswapV2PoolCalc
        ...

class CamelotLiquidityPool(AbstractLiquidityPool, CamelotV2PoolState, CamelotV2PoolCalc):
    variant: ClassVar[str | None] = "camelot"
    type DatabasePoolType = CamelotV2PoolTable
```

#### Step 7: Extract state and calc mixins for V3/V4 family

Same pattern. V3 and V4 have distinct state (V4 adds pool_key, pool_manager) and distinct calc (V4 uses unlock/callback pattern).

```python
class V3PoolState:
    _token0: Erc20Token
    _token1: Erc20Token
    _fee: int
    _liquidity: int
    _sqrt_price_x96: int
    _tick: int
    _tick_spacing: int
    _tick_bitmap: dict[int, Any]
    _tick_data: dict[int, Any]
    # ...

class UniswapV3PoolCalc:
    """Calculation methods matching the Uniswap V3 contract."""
    SLOT0_STRUCT_TYPES: ClassVar[tuple[str, ...]] = (...)
    # ...

class V4PoolState:
    # All V3 state plus:
    _pool_key: UniswapV4PoolKey
    _pool_manager_address: ChecksumAddress
    # ...

class UniswapV4PoolCalc:
    """Calculation methods matching the Uniswap V4 contract."""
    # ...

class UniswapV3Pool(AbstractLiquidityPool, V3PoolState, UniswapV3PoolCalc):
    variant: ClassVar[str | None] = "uniswap"

class UniswapV4Pool(AbstractLiquidityPool, V4PoolState, UniswapV4PoolCalc):
    variant: ClassVar[str | None] = "uniswap_v4"
```

#### Step 8: Replace abstract base classes with protocols

`AbstractUniswapV2Pool`, `AbstractConcentratedLiquidityPool`, and `AbstractAerodromeV2Pool` in `types/abstract/liquidity_pool.py` are replaced by `runtime_checkable` protocols in `types/pool_protocols.py`:

```python
@runtime_checkable
class ConstantProductPool(Protocol):
    """Any pool using x*y=k invariant with directional fees."""
    token0: Erc20Token
    token1: Erc20Token
    fee_token0: Fraction
    fee_token1: Fraction
    reserves_token0: int
    reserves_token1: int

@runtime_checkable
class ConcentratedLiquidityPool(Protocol):
    """Any pool using concentrated liquidity (tick-based)."""
    token0: Erc20Token
    token1: Erc20Token
    fee: int
    liquidity: int
    sqrt_price_x96: int
    tick: int
    tick_spacing: int

@runtime_checkable
class StableswapPool(Protocol):
    """Any pool using the StableSwap invariant."""
    # ... as needed
```

This collapses the type hierarchy to two layers of inheritance (`AbstractLiquidityPool` + mixins) instead of three. Duck typing via protocols replaces the intermediate abstract classes.

#### Step 9: Update `pool_type_registry._derive_family()`

Replace `issubclass(pool_class, AbstractUniswapV2Pool)` checks with protocol-based checks:

```python
def _derive_family(pool_class: type[AbstractLiquidityPool]) -> PoolFamily:
    if issubclass(pool_class, ConcentratedLiquidityPool):
        return PoolFamily.CONCENTRATED_LIQUIDITY
    if issubclass(pool_class, ConstantProductPool):
        return PoolFamily.CONSTANT_PRODUCT
    # ... etc
```

## Implementation Order

The two parts interleave. The pool class restructuring is the larger change and should be done first (it changes the type key used by the builder registry).

### Phase 1: Standalone calculations (no behaviour change)

1. **Create `calculations/` module** — move existing standalone functions from `solidly/solidly_functions.py`, `aerodrome/functions.py`, `camelot/functions.py`, `uniswap/v3_libraries/functions.py`
2. **Move Curve's nested `calc_*` functions** from inside `_get_d()` to `calculations/stableswap.py`
3. **Update imports** — no behaviour change, just where the functions live
4. **Run all tests** — verify no regression

### Phase 2: State and calc mixins (incremental, one pool family at a time)

5. **Extract `V2PoolState` and `UniswapV2PoolCalc`** from `UniswapV2Pool` — restructure the class body into mixins
6. **Restructure `SushiswapV2Pool`, `PancakeswapV2Pool`, `SwapbasedV2Pool`** as peers under the same mixins
7. **Restructure `AerodromeV2Pool`** with its own state+calc, eliminating `if self.stable`
8. **Restructure `CamelotLiquidityPool`** with its own state+calc
9. **Extract `V3PoolState` and `UniswapV3PoolCalc`** from `UniswapV3Pool`
10. **Restructure `SushiswapV3Pool`, `PancakeswapV3Pool`, `AerodromeV3Pool`** as peers
11. **Extract `V4PoolState` and `UniswapV4PoolCalc`** from `UniswapV4Pool`
12. **Extract `StableswapPoolState`** from `CurveStableswapPool` (calc already externalized)
13. **Run all tests after each step**

### Phase 3: Protocols (replace ABCs)

14. **Define `ConstantProductPool`, `ConcentratedLiquidityPool`, `StableswapPool` protocols** in `pool_protocols.py`
15. **Update `_derive_family()`** to use protocol checks
16. **Remove `AbstractUniswapV2Pool`, `AbstractConcentratedLiquidityPool`, `AbstractAerodromeV2Pool`** from `types/abstract/liquidity_pool.py`
17. **Update all `isinstance` checks** in `arbitrage/`, `builders/`, etc. to use protocols
18. **Run all tests**

### Phase 4: Builder registry (depends on Phase 2 for stable type keys)

19. **Add `register_builder()` and `_builders` dict to Bot** — alongside existing builder attributes
20. **Replace `_builder_for_pool()` isinstance chain** with `_builders[type(pool)]` dict lookup
21. **Replace `build_pool()` match statement** with builder lookup via `_pool_class_for_descriptor()`
22. **Remove `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`** — hard cutover to `build_pool()`
23. **Update all callers** — replace `bot.build_v2_pool(...)` with `bot.build_pool(...)`
24. **Run all tests**

## Testing

### Per-step test runs

Each step in Phases 1–4 runs `just test-python` to verify no regression. The pool class restructuring is incremental — one family at a time, tests pass after each.

### New unit tests

- `test_builder_registry.py`: registering a builder, updating via the registry, KeyError for unregistered type
- `test_v2_pool_state.py`: V2PoolState mixin provides correct properties
- `test_v2_pool_calc.py`: UniswapV2PoolCalc produces correct simulation results
- `test_aerodrome_dispatch.py`: AerodromeV2Pool wires stable vs volatile calc at construction time
- `test_protocols.py`: `isinstance(pool, ConstantProductPool)` for each V2 variant, `isinstance(pool, ConcentratedLiquidityPool)` for each V3 variant

### Integration tests

- All existing pool construction tests work via `build_pool()` (no more typed methods)
- `Bot.update()` dispatches correctly for every registered pool type
- V4 `pool_id` fast path still works

## Benefits

- **Adding a pool family goes from 5 → 2 touch points.** Create the builder, register it. No changes to Bot's methods.
- **No "is-a" violations.** `SushiswapV2Pool` is a peer of `UniswapV2Pool`, not a subclass. `isinstance(sushi, UniswapV2Pool)` returns `False`.
- **No isinstance in Bot.** `update()` uses `dict[type, PoolBuilder]` lookup. `build_pool()` uses builder lookup from `pool_type_registry` → concrete class → builder.
- **Hybrid pools are clean.** Aerodrome's stable mode wires `calc_stableswap_dy` at construction, no `if self.stable` in any method.
- **Calculation code is testable in isolation.** `calculations/stableswap.py` functions accept `(xp, amp, ...)` and return results — no pool object needed.
- **Library users can extend without modifying library code.** Define pool class + state + calc, define builder, register both. No enum changes, no Bot edits.

## Risks

- **Large scope.** This plan touches every pool class, every builder, every test that calls `build_v2_pool`, and every `isinstance` check in the arbitrage code. Phasing it carefully (one pool family at a time) mitigates this.
- **MRO complexity.** Each pool class now has 3+ base classes (Abstract + State + Calc). Python's MRO resolves this deterministically, but it's more to hold in your head. The tradeoff: more bases but each one has a single clear purpose (data, calculation, identity).
- **Protocol vs ABC tradeoff.** Protocols use duck typing (`isinstance` checks structural compatibility). If a pool accidentally has a `token0` property but isn't a constant-product pool, it would satisfy `ConstantProductPool`. This is mitigated by the protocol being narrow (only the V2-signature methods/properties) and the registration system (only registered types get builder dispatch).
- **Aerodrome state divergence.** AerodromeV2Pool's state (`fee: Fraction` undirectional, `stable: bool`) is significantly different from V2's (directional `fee_token0`/`fee_token1`). The separate `AerodromeV2PoolState` mixin handles this, but the arbitrage optimizer's `isinstance` checks need to switch from `AbstractAerodromeV2Pool` to `ConstantProductPool` protocol + specific handling for the stable case.

## Relationship to Other Plans

- **Plans 026/027/029** (Curve strategy objects, lending-rate fetchers, variant groups): Complete. This plan extends the same pattern (bind calculations at construction, dispatch via configuration) to all pool families, not just Curve.
- **Plan 013** (Curve I/O-Free): Complete. The `calculations/stableswap.py` module extracts the nested `calc_*` functions that Plan 026/029 left inside the Curve pool class as closures inside `_get_d()`.
- **Plan 001** (Pool Builders): Complete. This plan builds on the builder extraction — the builders exist, we're just changing how Bot wires them.
- **Plan 016** (Unified Pool Type Registry): Complete. `_pool_class_for_descriptor()` will consult `pool_type_registry` to map `PoolTypeDescriptor` → concrete class. No changes to the registry itself.
- **ADR-001** (I/O-Free Pools): Continued. The `Calculations/` module and `Calc` mixins are pure calculation — no I/O. The `State` mixins hold data. Both are I/O-free by construction.
