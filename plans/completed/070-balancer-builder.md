# Plan 070: Balancer V2 Pool Builder, Type Resolution, and Solver Integration

## Overview

Create a `BalancerBuilder` that handles on-chain data fetching and pool construction for all Balancer V2 pool types (Weighted, MetaStable, ComposableStable), register it in Bot's builder registry, add Balancer-specific type resolution so `Bot.build_pool()` auto-detects the correct class, and implement `to_hop_state()` and `external_update()` on both pool classes so they participate in the arbitrage pipeline. Brings the last excluded pool family into the unified I/O-free architecture.

## Problem

### Deletion test

If you delete both `BalancerV2Pool` and `BalancerV2StablePool`, the Balancer pool family has no representation in the codebase. No builder exists, no `Bot.build_pool()` support, no `to_hop_state()` implementation, no `external_update()`. This is the only pool family excluded from the unified architecture — all others (V2, V3, V4, Curve, Aerodrome, Camelot) have builders and can be constructed through `Bot.build_pool()`.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| No builder for Balancer | `src/degenbot/builders/` has no `balancer_builder.py` | Balancer pools are constructed directly by callers, bypassing the I/O-free architecture |
| `to_hop_state()` raises `NotImplementedError` | `balancer/pools.py:165–172`, `stable_pools.py` | Balancer pools can't participate in `ArbitragePath` — invisible to the solver |
| No `Bot.build_pool()` support | `bot.py` — no `BalancerBuilder` registered | Callers must know the Balancer pool exists and construct it manually with all parameters |
| `calculate_tokens_out_from_tokens_in` has `NotImplementedError` for override_state | `balancer/pools.py:104` | Simulation with state overrides doesn't work |
| No type resolution for Balancer | `type_resolution.py` — probes only `slot0`, `getReserves`, then falls through to STABLESWAP | `Bot.build_pool()` can't detect Balancer pools from an address; misclassifies them as Curve |
| No `external_update()` on either pool class | `pools.py`, `stable_pools.py` | State updates from subscriptions can't flow through; pools can't be updated after construction |
| `BalancerV2StablePool` needs construction-time data that's hard to gather manually | amp, rate providers, BPT index, invariant version, base scaling factors | Callers must make 6+ RPC calls and understand ComposableStablePool internals; this is the builder's job |
| `ArbitragePathPool` protocol excludes N-token pools | `types/pool_protocols.py:265` — requires `token0`/`token1` and 2-arg `calculate_tokens_out_from_tokens_in` | Balancer pools cannot participate in `ArbitragePath` even after implementing `to_hop_state()` |
| No `build_swap_amount()` on either pool class | Both pool classes missing | Required by `ArbitragePathPool` protocol and `ArbitragePath.build_swap_amounts()` |
| `BalancerWeightedHop` lacks `swap_fn` | `types/hop_types.py:122` — no optional callable field | Solvers cannot evaluate weighted hops with integer accuracy in mixed paths (same gap as `SolidlyStableHop` and `CurveStableswapHop` had before `swap_fn` was added) |
| `pool_class_for_descriptor` falls through to `CurveStableswapPool` for STABLESWAP | `type_resolution.py:59` — hard-coded `return CurveStableswapPool` | Unknown Balancer factory addresses silently produce the wrong pool class |

## Solution

### Step 0: Fix pre-existing bugs

Before adding builder/protocol machinery, fix a bug in the existing stable pool code:

```python
# Bug: BalancerV2StablePool.tokens has wrong return type
# stable_pools.py:224

# Before (WRONG — union type that doesn't match the protocol):
@property
def tokens(self) -> tuple[tuple[Erc20Token, ...] | tuple[()]]:
    return self._tokens

# After (CORRECT — matches StableswapPool protocol):
@property
def tokens(self) -> tuple[Erc20Token, ...]:
    return self._tokens
```

Verified: no callers depend on the wrong annotation. The runtime value `self._tokens` is already `tuple[Erc20Token, ...]` (set by `self._tokens = tuple(tokens)` in `__init__`), so the fix only corrects the type declaration.

### Step 1: Resolve `PoolFamily` derivation conflict — add `family` override to `register()`

**Critical problem**: `_derive_family()` in `registry/pool_type.py` classifies pools by structural shape. `BalancerV2Pool` has `tokens` but not `fee_token0` → it matches `STABLESWAP`, not `WEIGHTED`. `BalancerV2StablePool` also matches `STABLESWAP`. The original plan proposed using `STABLESWAP` for both (Option A), but this is semantically wrong — weighted pools use a bounded product invariant (∏xᵂⁱ ≥ k) that is structurally closer to `CONSTANT_PRODUCT` than to stableswap. Downstream code dispatching on `PoolFamily` would misroute weighted pools.

Three options were considered:

**Option A**: Use `STABLESWAP` with variant `"balancer_weighted"` / `"balancer_stable"`. Pragmatic but semantically misleading. `pool_class_for_descriptor` hard-codes `STABLESWAP → CurveStableswapPool` as fallback, so unknown factories would misdispatch. The `PoolFamily` enum exists precisely to distinguish invariant families — lumping bounded-product and iterative-invariant pools together defeats its purpose.

**Option B**: Add `weights` detection to `_derive_family`. Modifies shared infrastructure and could break existing `STABLESWAP` consumers.

**Option C (chosen): Override `_derive_family` via registration**. Add an optional `family` parameter to `pool_type_registry.register()` that bypasses auto-derivation. Register `BalancerV2Pool` with `family=PoolFamily.WEIGHTED` and `BalancerV2StablePool` with `family=PoolFamily.STABLESWAP`.

**Implementation** — one-line change to `PoolTypeRegistry.register()` and `_derive_family`:

```python
# In registry/pool_type.py:

def register(
    self,
    pool_class: type[AbstractLiquidityPool],
    *,
    chain_id: ChainId,
    factory_address: str,
    pool_init_hash: str | None = None,
    deployer: str | None = None,
    family: PoolFamily | None = None,  # NEW: override auto-derivation
) -> None:
    ...
    # When family is explicitly provided, use it instead of auto-deriving
    if family is not None:
        derived_family = family
    else:
        derived_family = _derive_family(pool_class)
    ...
```

Then register with explicit family overrides:

```python
# balancer/__init__.py
pool_type_registry.register(
    BalancerV2Pool,
    chain_id=1,
    factory_address="0x8E9aa87E45e92bad7D5F7F9Dd794cea12F21707B",
    family=PoolFamily.WEIGHTED,  # Override: _derive_family would return STABLESWAP
)
pool_type_registry.register(
    BalancerV2StablePool,
    chain_id=1,
    factory_address="0x8519F5A4A85678E0e03395586E2E223d70E9E09B",
    family=PoolFamily.STABLESWAP,  # Explicit — matches auto-derivation
)
```

This produces correct kind strings:
- `WEIGHTED + "balancer_weighted"` → `"balancer_weighted"` (no `_v2` suffix for WEIGHTED)
- `STABLESWAP + "balancer_stable"` → `"balancer_stable"` (no suffix for STABLESWAP)

No collision with existing kinds (`"uniswap_v2"`, `"sushiswap_v2"`, `"stableswap"`, etc.).

Add variant class attributes to each pool class:

```python
class BalancerV2Pool(PublisherMixin, PoolPickleMixin, AbstractLiquidityPool):
    variant: ClassVar[str | None] = "balancer_weighted"
    ...

class BalancerV2StablePool(PublisherMixin, PoolPickleMixin, AbstractLiquidityPool):
    variant: ClassVar[str | None] = "balancer_stable"
    ...
```

**Update `pool_class_for_descriptor`**: With the `WEIGHTED` family now in use, add a fallback case. Currently, the STABLESWAP-only fallback produces `CurveStableswapPool` for any unrecognized STABLESWAP descriptor. Adding a `WEIGHTED` case ensures correct dispatch for unknown Balancer weighted factories:

```python
# In type_resolution.py — update pool_class_for_descriptor:
match pool_type.family:
    case PoolFamily.CONSTANT_PRODUCT:
        ...
    case PoolFamily.CONCENTRATED_LIQUIDITY:
        ...
    case PoolFamily.WEIGHTED:
        # No default Balancer weighted class — require factory registration
        if pool_type.variant is not None and pool_type.variant.startswith("balancer"):
            msg = (
                f"Balancer weighted pool with unregistered factory {pool_type.factory}. "
                f"Register the factory address in pool_type_registry first."
            )
            raise DegenbotValueError(message=msg)
        msg = f"No pool class for WEIGHTED family with variant {pool_type.variant!r}"
        raise DegenbotValueError(message=msg)
    case PoolFamily.STABLESWAP:
        # Variant-aware: reject Balancer stable pools without factory registration
        if pool_type.variant is not None and pool_type.variant.startswith("balancer"):
            msg = (
                f"Balancer stable pool with unregistered factory {pool_type.factory}. "
                f"Register the factory address in pool_type_registry first."
            )
            raise DegenbotValueError(message=msg)
        return CurveStableswapPool
    case _:
        msg = f"No pool class for family {pool_type.family.value!r}"
        raise DegenbotValueError(message=msg)
```

This prevents the silent misdispatch of Balancer pools to `CurveStableswapPool` when the factory isn't registered — a hard error is always preferable to silently constructing the wrong class.

### Step 2: Add `PoolInvariant.BALANCER_STABLESWAP`

Balancer stable pools need a distinct solver dispatch key. `PoolInvariant.BALANCER_WEIGHTED` already exists for weighted pools. Add `BALANCER_STABLESWAP` for stable pools:

```python
class PoolInvariant(Enum):
    CONSTANT_PRODUCT = auto()
    BOUNDED_PRODUCT = auto()
    SOLIDLY_STABLE = auto()
    BALANCER_WEIGHTED = auto()
    BALANCER_MULTI_TOKEN = auto()
    CURVE_STABLESWAP = auto()
    BALANCER_STABLESWAP = auto()  # NEW
```

Verified: `rust/` does not reference `PoolInvariant` at all — only Python solvers use it. Safe to add without Rust changes.

### Step 3: Add `BalancerStableHop` dataclass and `swap_fn` to `BalancerWeightedHop`

**New dataclass** — `BalancerStableHop`:

```python
# In hop_types.py:

@dataclass(frozen=True, slots=True)
class BalancerStableHop:
    """
    A Balancer stable pool hop (StableSwap invariant).

    Not a Möbius transformation — the swap function requires iterative
    invariant computation (Newton's method). Follows the same pattern as
    CurveStableswapHop and SolidlyStableHop.

    The ``invariant`` field carries a pre-computed D value for float
    approximation. For integer-accurate evaluation, ``swap_fn`` calls
    the pool's calculate_tokens_out_from_tokens_in directly.

    ``swap_fn`` and ``StaleRateResult``: when the underlying pool has a
    static rate provider, calculate_tokens_out_from_tokens_in raises
    StaleRateResult. The swap_fn wraps the call and extracts the
    approximate amount from the exception, so the solver can continue
    with best-effort values rather than crashing.
    """
    reserve_in: int
    reserve_out: int
    fee: Fraction
    amp: int
    n_tokens: int
    invariant: int  # Pre-computed D invariant for float approximation
    token_index_in: int   # Index in the non-BPT token list (BPT-skipped)
    token_index_out: int  # Index in the non-BPT token list (BPT-skipped)
    swap_fn: Callable[[int], int] | None = field(default=None, compare=False, hash=False)
    pool_invariant: PoolInvariant = PoolInvariant.BALANCER_STABLESWAP

    @property
    def gamma(self) -> float:
        return 1.0 - float(self.fee)
```

**Extend `BalancerWeightedHop`** — add `swap_fn`:

`BalancerWeightedHop` currently has no `swap_fn` field. For mixed-path solver accuracy (matching the pattern established by `SolidlyStableHop`, `CurveStableswapHop`, and the new `BalancerStableHop`), add an optional `swap_fn`:

```python
# In hop_types.py — modify existing BalancerWeightedHop:

@dataclass(frozen=True, slots=True)
class BalancerWeightedHop:
    """
    A Balancer weighted pool (∏xᵂⁱ ≥ k) hop.

    Not a Möbius transformation — the swap function uses power-law exponents.
    A 50/50 pool reduces to constant product.

    The optional ``swap_fn`` provides an integer-accurate swap simulation
    wrapping the pool's calculate_tokens_out_from_tokens_in. When provided,
    the solver uses it for exact path evaluation. When absent, the float
    approximation using weight_in/weight_out is used.
    """
    reserve_in: int
    reserve_out: int
    fee: Fraction
    weight_in: int
    weight_out: int
    swap_fn: Callable[[int], int] | None = field(default=None, compare=False, hash=False)  # NEW
    invariant: PoolInvariant = PoolInvariant.BALANCER_WEIGHTED

    @property
    def gamma(self) -> float:
        return 1.0 - float(self.fee)
```

Update `HopType` union to include `BalancerStableHop`. Add `has_balancer_stableswap` property to `SolveInput`:

```python
@property
def has_balancer_stableswap(self) -> bool:
    return any(h.invariant == _PoolInvariant.BALANCER_STABLESWAP for h in self.hops)
```

### Step 4: Add `BalancerV2PoolExternalUpdate` types — separate for weighted and stable

Create distinct update types for each pool class. Even though both currently carry only `block_number` + `balances`, the stable pool will need `amp` updates in the future (A ramping) and the weighted pool may need `fee` updates (governance). Separate types avoid a migration later.

```python
# In balancer/types.py — add:
from degenbot.types.concrete import PoolStateMessage

@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class BalancerV2WeightedPoolExternalUpdate:
    """State update for a Balancer V2 weighted pool."""
    block_number: int
    balances: tuple[int, ...]
    # fee: Fraction | None = None  # Future: add when governance fee changes are tracked

@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class BalancerV2StablePoolExternalUpdate:
    """State update for a Balancer V2 stable pool.

    amp is currently omitted — stable pools treat amp as immutable after
    construction in this plan. A future slice may add amp tracking to
    support A ramping.
    """
    block_number: int
    balances: tuple[int, ...]
    # amp: int | None = None  # Future: add when A-ramping is supported

class BalancerV2PoolStateUpdated(PoolStateMessage):
    """Message published when a Balancer V2 pool's state changes."""
    state: BalancerV2PoolState
```

Both pool classes share the same `PoolStateMessage` subclass (`BalancerV2PoolStateUpdated`) because the state object (`BalancerV2PoolState`) is already shared. If the state objects diverge in the future, split this too.

### Step 5: Implement `external_update()` on both pool classes

Both pool classes need `external_update()` for subscriber notification and state mutation. The implementation must hold `_state_lock` during state mutation, matching every other pool class (`UniswapV2Pool`, `CurveStableswapPool`, `UniswapV3Pool`).

```python
# On BalancerV2Pool:
def external_update(self, update: BalancerV2WeightedPoolExternalUpdate) -> None:
    if update.block_number < self.state.block:
        return
    with self._state_lock:
        # Re-check after acquiring lock (another thread may have updated)
        if update.block_number < self.state.block:
            return
        self._state = BalancerV2PoolState(
            address=self.address,
            block=update.block_number,
            balances=update.balances,
        )
    self._notify_subscribers(
        message=BalancerV2PoolStateUpdated(state=self._state),
    )

# On BalancerV2StablePool:
def external_update(self, update: BalancerV2StablePoolExternalUpdate) -> None:
    if update.block_number < self.state.block:
        return
    with self._state_lock:
        # Re-check after acquiring lock (another thread may have updated)
        if update.block_number < self.state.block:
            return
        self._state = BalancerV2PoolState(
            address=self.address,
            block=update.block_number,
            balances=update.balances,
        )
    self._notify_subscribers(
        message=BalancerV2PoolStateUpdated(state=self._state),
    )
```

**Design notes**:
- Uses `<` (strict less-than) for the stale-block guard, matching `CurveStableswapPool.external_update()` which always applies the update. Same-block updates are allowed and overwrite the existing state (idempotent for same data).
- The lock is held only during mutation, not during `_notify_subscribers` (matching V2 pool pattern).
- Double-check after acquiring the lock prevents a race where two threads both pass the initial guard.
- **No `StateCache`**: Unlike V2/V3/V4 pools, Balancer pools store only the current state. They do not satisfy `StateManageablePool` (no `discard_states_before_block` / `restore_state_before_block`). This matches `CurveStableswapPool` which also uses a simple `_state` replacement without temporal navigation.

**Stale-rate caveat for stable pools**: After `external_update()`, the pool's `_resolve_scaling_factors()` may return stale rates because the `_StaticRateProvider` doesn't know about the new block. For exact-integer matching after an update, the caller should inject a `CacheAwareRateProvider`. This matches how `CurveStableswapPool` handles stale rates after `external_update()`.

**Limitation**: Amp changes are not tracked by `external_update()`. For typical bot sessions (minutes to hours), amp is effectively constant. For long-running sessions, A ramping would require re-fetching amp from the builder's `update()` method and constructing a new pool or extending the update type. This is a documented limitation, not a bug — see "Deferred items" below.

### Step 6: Implement `to_hop_state()` on `BalancerV2Pool` — with `token_in`/`token_out` kwargs

Both `to_hop_state()` implementations include `token_in`/`token_out` kwargs from the start. Shipping the hardcoded `(0, 1)` version first and then revising it in the next slice introduces a misleading API.

```python
def to_hop_state(
    self,
    zero_for_one: bool,
    state_override: BalancerV2PoolState | None = None,
    *,
    token_in: Erc20Token | None = None,
    token_out: Erc20Token | None = None,
) -> HopType:
    state = state_override or self.state

    # Resolve token indices from token_in/token_out kwargs
    # (both-or-neither; both resolve against self._tokens).
    # When both omitted, zero_for_one selects (0,1)/(1,0).
    if token_in is not None and token_out is not None:
        i = self._tokens.index(token_in)
        j = self._tokens.index(token_out)
    elif token_in is not None or token_out is not None:
        msg = "token_in and token_out must both be provided, or both omitted"
        raise DegenbotValueError(message=msg)
    elif zero_for_one:
        i, j = 0, 1
    else:
        i, j = 1, 0

    # Build swap_fn wrapping the pool's calculate_tokens_out_from_tokens_in.
    # This provides integer-accurate evaluation in mixed-path solvers.
    def swap_fn(amount_in: int) -> int:
        return self.calculate_tokens_out_from_tokens_in(
            token_in=self._tokens[i],
            token_out=self._tokens[j],
            token_in_quantity=amount_in,
            override_state=state_override,
        )

    return BalancerWeightedHop(
        reserve_in=state.balances[i],
        reserve_out=state.balances[j],
        fee=self.fee,
        weight_in=self.weights[i],
        weight_out=self.weights[j],
        swap_fn=swap_fn,
    )
```

**Caveat**: For N-token pools, the default `(0, 1)` / `(1, 0)` pair selection is misleading — callers who want a different pair must pass `token_in`/`token_out`. The `BalancerPairView` adapter (Step 9) provides `ArbitragePathPool` conformance by always specifying the pair explicitly.

### Step 7: Implement `to_hop_state()` on `BalancerV2StablePool` — with `token_in`/`token_out` kwargs and `swap_fn` handling

```python
def to_hop_state(
    self,
    zero_for_one: bool,
    state_override: BalancerV2PoolState | None = None,
    *,
    token_in: Erc20Token | None = None,
    token_out: Erc20Token | None = None,
) -> HopType:
    state = state_override or self.state
    balances = state.balances

    # Resolve token indices (both-or-neither pattern)
    if token_in is not None and token_out is not None:
        i = self._tokens.index(token_in)
        j = self._tokens.index(token_out)
    elif token_in is not None or token_out is not None:
        msg = "token_in and token_out must both be provided, or both omitted"
        raise DegenbotValueError(message=msg)
    elif zero_for_one:
        i, j = 0, 1
    else:
        i, j = 1, 0

    sf = self._resolve_scaling_factors()
    upscaled = self._upscale_balances(balances, sf)
    inv = self._compute_invariant(upscaled)

    # Build swap_fn with StaleRateResult handling.
    # When the pool has a static rate provider, calculate_tokens_out_from_tokens_in
    # raises StaleRateResult. The wrapped swap_fn catches this and extracts the
    # approximate amount_out so the solver can continue with best-effort values.
    def swap_fn(amount_in: int) -> int:
        try:
            return self.calculate_tokens_out_from_tokens_in(
                token_in=self._tokens[i],
                token_out=self._tokens[j],
                token_in_quantity=amount_in,
                override_state=state_override,
            )
        except StaleRateResult as e:
            return e.amount_out

    return BalancerStableHop(
        reserve_in=balances[i],
        reserve_out=balances[j],
        fee=self.fee,
        amp=self.amp,
        n_tokens=len(self._non_bpt_indices),
        invariant=inv,
        token_index_in=self._skip_bpt_index(i),
        token_index_out=self._skip_bpt_index(j),
        swap_fn=swap_fn,
    )
```

**`invariant` field staleness note**: The `invariant` field carries a pre-computed D value for float approximation in the solver. After `external_update()` changes balances, this value becomes stale. The `swap_fn` (used for integer-accurate evaluation) calls `calculate_tokens_out_from_tokens_in` which recomputes the invariant from scratch, so integer accuracy is preserved. This matches the `CurveStableswapHop` pattern which sets `curve_d=0` and relies on `swap_fn` for exact computation.

**Rate-provider caveat**: When `requires_io_at_calculation_time` is `True`, the `swap_fn` will call `self._rate_provider.get_rates()` which does I/O. The solver uses `swap_fn` for integer-accurate evaluation, so this is expected — but the solver documentation should note that `swap_fn` may perform I/O for ComposableStablePools. This matches `CurveStableswapHop.swap_fn` which also performs I/O via the pool's `get_dy()` method.

### Step 8: Fix `override_state` on swap calculation methods

Both pool classes raise `NotImplementedError` when `override_state` is provided. For weighted pools:

```python
# In BalancerV2Pool.calculate_tokens_out_from_tokens_in:
if override_state is not None:
    balances = list(override_state.balances)
else:
    balances = list(self.balances)
```

Same pattern for `calculate_tokens_in_from_tokens_out`. For stable pools, the override only swaps out balances — it cannot override scaling factors or invariant version since those aren't stored on `BalancerV2PoolState`. This is what `ArbitragePath._resolve_state_overrides` expects (it only varies balances to simulate different reserve configurations).

**Note on `_upscale_array` mutation**: `BalancerV2Pool.calculate_tokens_out_from_tokens_in` currently does `balances = list(self.balances)` then `_upscale_array(amounts=balances, ...)`. The `_upscale_array` function mutates the list in-place. With `override_state`, the same pattern works: `balances = list(override_state.balances)` → `_upscale_array(amounts=balances, ...)`. No copy issue — `list()` creates a new list each time.

**Dependency order**: `simulate_swap()` on `BalancerV2Pool` delegates to `calculate_tokens_out_from_tokens_in(override_state=...)`. The `override_state` fix must land before `simulate_swap` can work with state overrides. This dependency is captured in Slice 2 (which implements both `external_update` and the `override_state` fix together).

### Step 9: `ArbitragePathPool` protocol — the N-token problem and `BalancerPairView` relay

**Critical gap**: `ArbitragePathPool` (in `types/pool_protocols.py`) requires:
- `token0` and `token1` properties
- `calculate_tokens_out_from_tokens_in(token_in, token_in_quantity, override_state)` — no `token_out` parameter
- `build_swap_amount(zero_for_one, amount_in, amount_out)`
- `simulate_swap(token_in, amount_in, token_out, state_override)`

Balancer V2 pools are N-token pools. They have `tokens` (a tuple) instead of `token0`/`token1`, and their `calculate_tokens_out_from_tokens_in` requires an explicit `token_out` parameter. They also lack `build_swap_amount()`.

`ArbitragePath._validate_pools()` and `_build_swap_vectors()` iterate over `pool.token0` and `pool.token1` to build the token chain. This will `AttributeError` on Balancer pools.

**Resolution**: a `BalancerPairView` adapter class that wraps an N-token pool + a chosen pair into a 2-token view satisfying `ArbitragePathPool`.

**Subscription relay**: `BalancerPairView` must relay notifications from the underlying pool to `ArbitragePath`. When the path subscribes to the view, the view subscribes to the pool as a relay. The pool calls `view.notify(publisher=pool, ...)`, and the view re-publishes to its own subscribers with `publisher=self` (the view). `ArbitragePath._pool_index` maps the view → index, so the identity check `p is publisher` succeeds. This completes the adapter pattern in both directions:
- Call direction: `ArbitragePath` → view → pool
- Notification direction: pool → view → `ArbitragePath`

```python
# In balancer/pair_view.py:

class BalancerPairView:
    """Adapts an N-token Balancer pool to a 2-token pair view for ArbitragePath.

    Delegates swap calculations and hop state to the underlying pool,
    for a specific (token_in, token_out) pair. Cheap to create (no I/O).

    Implements subscription relay: the view subscribes to the underlying
    pool and re-publishes notifications to its own subscribers with
    publisher=self. This ensures ArbitragePath._pool_index identity
    checks work correctly — the path only sees the view, not the pool.
    """

    def __init__(
        self,
        pool: BalancerV2Pool | BalancerV2StablePool,
        token_a: Erc20Token,
        token_b: Erc20Token,
    ) -> None:
        self._pool = pool
        self._token0 = token_a
        self._token1 = token_b
        self._subscribers: WeakSet[Subscriber] = WeakSet()
        # Subscribe to pool as a relay
        pool.subscribe(self)

    @property
    def address(self) -> ChecksumAddress:
        return self._pool.address

    @property
    def token0(self) -> Erc20Token:
        return self._token0

    @property
    def token1(self) -> Erc20Token:
        return self._token1

    @property
    def fee(self) -> Fraction:
        return self._pool.fee

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: AbstractPoolState | None = None,
    ) -> int:
        if token_in == self._token0:
            token_out = self._token1
        elif token_in == self._token1:
            token_out = self._token0
        else:
            msg = f"token_in {token_in} not in pair"
            raise DegenbotValueError(message=msg)
        return self._pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_out=token_out,
            token_in_quantity=token_in_quantity,
            override_state=override_state,
        )

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        # Note: BalancerV2StablePool.simulate_swap has an additional
        # block_identifier parameter not present in the PoolSimulation
        # protocol. It is not forwarded here since ArbitragePathPool
        # callers never pass it. Direct callers needing block-aware
        # rate resolution should call the pool's simulate_swap directly.
        return self._pool.simulate_swap(
            token_in=token_in,
            amount_in=amount_in,
            token_out=token_out,
            state_override=state_override,
        )

    def to_hop_state(
        self,
        zero_for_one: bool,
        state_override: AbstractPoolState | None = None,
        *,
        token_in: Erc20Token | None = None,
        token_out: Erc20Token | None = None,
    ) -> HopType:
        # Delegate to the pool's to_hop_state with explicit pair selection.
        # When token_in/token_out are provided by the caller, pass them through.
        # Otherwise, derive from the pair's token0/token1.
        if token_in is None and token_out is None:
            token_in = self._token0 if zero_for_one else self._token1
            token_out = self._token1 if zero_for_one else self._token0
        return self._pool.to_hop_state(
            zero_for_one=zero_for_one,
            state_override=state_override,
            token_in=token_in,
            token_out=token_out,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:
        return self._pool.fee

    def build_swap_amount(
        self,
        zero_for_one: bool,
        amount_in: int,
        amount_out: int,
    ) -> AbstractSwapAmounts:
        if zero_for_one:
            token_in = self._token0
            token_out = self._token1
        else:
            token_in = self._token1
            token_out = self._token0
        return self._pool.build_swap_amount(
            zero_for_one=zero_for_one,
            amount_in=amount_in,
            amount_out=amount_out,
            token_in=token_in,
            token_out=token_out,
        )

    # --- Subscription relay ---

    def subscribe(self, subscriber: Subscriber) -> None:
        """Subscribe to state updates from this view.

        The view relays notifications from the underlying pool.
        Subscribers receive messages with publisher=self (the view),
        not the underlying pool.
        """
        self._subscribers.add(subscriber)

    def unsubscribe(self, subscriber: Subscriber) -> None:
        self._subscribers.discard(subscriber)

    def notify(self, publisher: object, message: AbstractPublisherMessage) -> None:
        """Relay notifications from the underlying pool.

        Re-publishes to this view's subscribers with publisher=self,
        so that ArbitragePath._pool_index identity checks work
        correctly. The view is the publisher from the path's
        perspective.
        """
        if not isinstance(message, PoolStateMessage):
            return
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)
```

**Design decision**: `BalancerPairView` wraps an N-token pool into a 2-token view. It does not store state — it delegates everything to the underlying pool. The subscription relay completes the adapter pattern in both directions (calls and notifications). `ArbitragePath` only ever sees the view — no changes to shared infrastructure needed.

**Solver caveat**: Even with `BalancerPairView`, `ArbitragePath` requires an injected `Solver` whose `supports()` accepts the hop types. `SolidlyStableSolver.supports()` rejects `BalancerWeightedHop` and `BalancerStableHop` because their `PoolInvariant` values aren't in the accepted set. For mixed Balancer/V2 paths, a new solver is needed — either extending `SolidlyStableSolver` or creating a new one. This is out of scope for this plan but documented as a follow-up.

### Step 10: `build_swap_amount()` — encoding Balancer swaps

Both pool classes need `build_swap_amount()` for `ArbitragePathPool`. Balancer V2 swaps are executed via the Vault's `swap()` method, not via a per-pool contract call.

**N-token safety**: For N > 2 pools, `build_swap_amount(zero_for_one=...)` cannot silently pick `tokens[0]`/`tokens[1]` — the encoded swap calldata sends real funds on mainnet, and a wrong pair means funds go to the wrong destination. The method must require explicit token specification for N > 2 pools. For N = 2 pools, `zero_for_one` is sufficient (the pair is unambiguous).

```python
# On BalancerV2Pool and BalancerV2StablePool:
def build_swap_amount(
    self,
    zero_for_one: bool,
    amount_in: int,
    amount_out: int,
    *,
    token_in: Erc20Token | None = None,
    token_out: Erc20Token | None = None,
) -> BalancerV2SwapAmounts:
    # Resolve token pair
    if token_in is not None and token_out is not None:
        pass  # Use caller-specified pair
    elif len(self._tokens) > 2:
        msg = (
            f"Pool {self.address} has {len(self._tokens)} tokens. "
            f"build_swap_amount requires token_in and token_out for N > 2 pools. "
            f"Use BalancerPairView for ArbitragePathPool conformance."
        )
        raise DegenbotValueError(message=msg)
    elif zero_for_one:
        token_in = self._tokens[0]
        token_out = self._tokens[1]
    else:
        token_in = self._tokens[1]
        token_out = self._tokens[0]

    return BalancerV2SwapAmounts(
        pool_id=self.pool_id,
        vault=self.vault,
        zero_for_one=zero_for_one,
        amount_in=amount_in,
        amount_out=amount_out,
        token_in=token_in.address,
        token_out=token_out.address,
    )
```

Create a `BalancerV2SwapAmounts` class using `encode_function_calldata` (matching every other `SwapAmounts` class in the codebase):

```python
# In balancer/swap_amounts.py:

@dataclass(frozen=True, slots=True)
class BalancerV2SwapAmounts(AbstractSwapAmounts):
    pool_id: bytes
    vault: ChecksumAddress
    zero_for_one: bool
    amount_in: int
    amount_out: int
    token_in: ChecksumAddress
    token_out: ChecksumAddress

    def __post_init__(self) -> None:
        # pool_id must be exactly 32 bytes (bytes32 in Solidity)
        if isinstance(self.pool_id, (bytes, bytearray)) and len(self.pool_id) != 32:
            msg = f"pool_id must be 32 bytes, got {len(self.pool_id)}"
            raise DegenbotValueError(message=msg)

    def input_amount(self) -> int:
        return self.amount_in

    def output_amount(self) -> int:
        return self.amount_out

    def encode(self, *, recipient: ChecksumAddress | None = None) -> EncodedCall:
        """Encode Vault.swap() call.

        FundManagement defaults:
        - sender: ZERO_ADDRESS (filled by executor at runtime)
        - fromInternalBalance: False
        - recipient: the ``recipient`` parameter (must be provided)
        - toInternalBalance: False
        """
        if recipient is None:
            msg = "recipient is required for Balancer V2 swap encoding"
            raise DegenbotValueError(message=msg)

        # Use encode_function_calldata matching all other SwapAmounts classes
        data = encode_function_calldata(
            "swap((bytes32,uint8,address,address,uint256,bytes),(address,bool,address,bool),uint256,uint256)",
            [
                (self.pool_id, 0, self.token_in, self.token_out, self.amount_in, b""),  # SingleSwap
                (ZERO_ADDRESS, False, recipient, False),  # FundManagement
                self.amount_out,   # limit (minimum output)
                2**256 - 1,        # deadline (max uint256)
            ],
        )
        return EncodedCall(to=self.vault, data=data)
```

### Step 11: Create `BalancerBuilderBase` and `BalancerBuilder`

Following the established pattern (`V2BuilderBase`, `V3BuilderBase`, `V4BuilderBase`), create a `BalancerBuilderBase` with `@staticmethod` helpers for pure-logic operations. The sync `BalancerBuilder` inherits from it. The future `AsyncBalancerBuilder` calls these static methods without inheriting, mirroring the async builder pattern.

```python
# src/degenbot/builders/balancer_builder_base.py

@dataclasses.dataclass(frozen=True, slots=True, kw_only=True)
class DecodedPoolId:
    """Result of decoding a 32-byte Balancer pool ID."""
    pool_address: ChecksumAddress
    specialization: int
    nonce: int


@dataclasses.dataclass(frozen=True, slots=True, kw_only=True)
class VaultTokensResult:
    """Result of decoding Vault.getPoolTokens() response."""
    tokens: list[str]
    balances: list[int]
    last_change_block: int


class _BalancerPoolType(IntEnum):
    """Internal enum for _detect_pool_type return values.

    Used instead of string literals to enable type-checker
    exhaustiveness checking and prevent typos.
    """
    WEIGHTED = 1
    STABLE = 2
    # LINEAR = 3  # future


class BalancerBuilderBase:
    """Shared pure-logic helpers for Balancer pool builders.

    Sync and async builders call these @staticmethod helpers
    without duplicating decode/extract logic. No I/O — all
    chain access is mediated by the PoolIO parameter at
    the builder level, not here.
    """

    @staticmethod
    def decode_pool_id(raw: bytes) -> DecodedPoolId:
        """Decode a 32-byte pool ID into typed components."""
        pool_address = get_checksum_address(raw[:20])
        specialization = int.from_bytes(raw[20:22], byteorder="big")
        nonce = int.from_bytes(raw[22:32], byteorder="big")
        return DecodedPoolId(
            pool_address=pool_address,
            specialization=specialization,
            nonce=nonce,
        )

    @staticmethod
    def decode_vault_tokens(raw: bytes) -> VaultTokensResult:
        """Decode getPoolTokens() response."""
        decoded = eth_abi.abi.decode(["address[]", "uint256[]", "uint256"], raw)
        return VaultTokensResult(
            tokens=decoded[0],
            balances=decoded[1],
            last_change_block=decoded[2],
        )

    @staticmethod
    def detect_bpt_index(
        token_addresses: Sequence[str],
        pool_address: str,
    ) -> int | None:
        """Detect the BPT index for ComposableStablePools.

        Heuristic: the token whose address matches the pool address is BPT.
        Returns None for MetaStablePools (no BPT in token list).
        """
        for i, addr in enumerate(token_addresses):
            if get_checksum_address(addr) == get_checksum_address(pool_address):
                return i
        return None

    @staticmethod
    def resolve_invariant_version(
        *,
        specialization: int,
        override: int | None = None,
    ) -> int:
        """Determine which StableMath invariant version to use.

        specialization comes from the decoded pool ID:
        - 0 (General): most likely ComposableStablePool → INVARIANT_V1
        - 1 (MinimalSwapInfo): most likely MetaStablePool → INVARIANT_V2
        - 2 (TwoToken): older WeightedPool2Tokens → not a stable pool

        The override parameter from BuildPoolRequest.invariant_version
        takes precedence over heuristics.
        """
        if override is not None:
            return override
        # MetaStablePools use specialization=1 and INVARIANT_V2.
        # ComposableStablePools use specialization=0 and INVARIANT_V1.
        if specialization == 1:
            return INVARIANT_V2
        return INVARIANT_V1
```

```python
# src/degenbot/builders/balancer_builder.py

class BalancerBuilder(BalancerBuilderBase):
    """Builds and updates Balancer V2 pools (weighted, stable, composable).

    Owns the full I/O choreography: RPC fetch → decode → construct →
    register.

    Pool type is determined via _detect_pool_type() which probes the
    contract for characteristics (has getNormalizedWeights → WEIGHTED,
    has getAmplificationParameter → STABLE). Raises a clear error for
    linear pools or unknown types instead of defaulting to stable.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._erc20_builder = ctx.erc20_builder

    def build(self, address, *, chain_id, io, request) -> AbstractLiquidityPool:
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None
        state_block = (
            request.state_block
            if request.state_block is not None
            else io.get_block_number()
        )

        # 1. Check BROKEN_BALANCER_V2_POOLS
        if pool_address in BROKEN_BALANCER_V2_POOLS:
            raise BrokenPool

        # 2. Fetch pool ID (or use request pool_id if provided)
        if request.pool_id is not None:
            pool_id = (
                bytes.fromhex(request.pool_id[2:])
                if isinstance(request.pool_id, str)
                else bytes(request.pool_id)
            )
        else:
            pool_id = self._fetch_pool_id(io, pool_address, state_block)

        # 3. Decode specialization from pool_id for heuristic use
        pool_id_decoded = self.decode_pool_id(pool_id)

        # 4. Fetch tokens and balances from Vault
        tokens, balances = self._fetch_vault_tokens(
            io, pool_id, chain_id, state_block, request
        )

        # 5. Fetch fee
        fee = self._fetch_swap_fee(io, pool_address, state_block)

        # 6. Detect pool type and build
        pool_type = self._detect_pool_type(io, pool_address, state_block)
        if pool_type == _BalancerPoolType.WEIGHTED:
            return self._build_weighted(
                io, pool_address, pool_id, tokens, balances, fee,
                chain_id, state_block, request,
            )
        if pool_type == _BalancerPoolType.STABLE:
            return self._build_stable(
                io, pool_address, pool_id, tokens, balances, fee,
                chain_id, state_block, request, pool_id_decoded,
            )

        msg = f"Unknown Balancer pool type at {pool_address}"
        raise DegenbotValueError(message=msg)

    def _build_weighted(self, io, address, pool_id, tokens, balances,
                         fee, chain_id, state_block, request):
        weights = self._fetch_weights(io, address, state_block)

        bytecode = io.get_code(address, block=state_block).hex()
        pow_version = detect_pow_version(bytecode)

        pool = BalancerV2Pool(
            address=address,
            pool_id=pool_id,
            vault=BALANCER_V2_VAULT_ADDRESS,
            tokens=tokens,
            balances=balances,
            fee=fee,
            weights=weights,
            pow_version=pow_version,
            chain_id=chain_id,
            state_block=state_block,
        )

        self._pools.add(pool, chain_id=chain_id, pool_address=pool.address)
        ...

    def _build_stable(self, io, address, pool_id, tokens, balances,
                        fee, chain_id, state_block, request, pool_id_decoded):
        amp = self._fetch_amp(io, address, state_block)
        rate_providers = self._fetch_rate_providers(io, address, state_block)

        # Detect BPT index using base class helper
        token_addresses = [t.address for t in tokens]
        bpt_idx = (
            request.bpt_idx
            if request.bpt_idx is not None
            else self.detect_bpt_index(token_addresses, address)
        )

        base_sf = tuple(_compute_scaling_factor(t) for t in tokens)
        rates = self._fetch_rates(io, rate_providers, state_block)
        scaling_factors = tuple(
            bsf * rate // ONE for bsf, rate in zip(base_sf, rates, strict=True)
        )

        # Resolve invariant version using base class helper + pool_specialization
        invariant_version = self.resolve_invariant_version(
            specialization=pool_id_decoded.specialization,
            override=request.invariant_version,
        )

        pool = BalancerV2StablePool(
            address=address,
            pool_id=pool_id,
            vault=BALANCER_V2_VAULT_ADDRESS,
            tokens=tokens,
            balances=balances,
            fee=fee,
            amp=amp,
            scaling_factors=scaling_factors,
            bpt_idx=bpt_idx,
            base_scaling_factors=base_sf,
            invariant_version=invariant_version,
            chain_id=chain_id,
            state_block=state_block,
        )

        self._pools.add(pool, chain_id=chain_id, pool_address=pool.address)
        ...

    def update(self, pool, *, io, block_number) -> bool:
        assert io is not None
        raw_block_number = block_number if block_number is not None else io.get_block_number()
        block_number_: int = (
            raw_block_number if isinstance(raw_block_number, int) else int(raw_block_number)
        )

        # Fetch current balances from Vault via getPoolTokens()
        new_balances = self._fetch_balances_from_vault(io, pool.pool_id, block_number_)

        if pool.balances == tuple(new_balances):
            return False

        if isinstance(pool, BalancerV2Pool):
            update = BalancerV2WeightedPoolExternalUpdate(
                block_number=block_number_,
                balances=tuple(new_balances),
            )
        elif isinstance(pool, BalancerV2StablePool):
            update = BalancerV2StablePoolExternalUpdate(
                block_number=block_number_,
                balances=tuple(new_balances),
            )
        else:
            msg = f"BalancerBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        pool.external_update(update)
        return True

    @staticmethod
    def _detect_pool_type(io, address, block_identifier) -> _BalancerPoolType:
        """Determine weighted vs stable by probing contract methods.

        Probes in order:
        1. getNormalizedWeights() → WEIGHTED
        2. getAmplificationParameter() → STABLE
        3. Neither → raise (don't default to stable)

        Does NOT silently default to "stable" for unknown types (e.g.
        linear pools) — a clear error is better than a misclassification.
        """
        try:
            io.call(
                to=address,
                data=encode_function_calldata("getNormalizedWeights()", None),
                block=block_identifier,
            )
            return _BalancerPoolType.WEIGHTED
        except Web3Exception:
            pass

        try:
            io.call(
                to=address,
                data=encode_function_calldata("getAmplificationParameter()", None),
                block=block_identifier,
            )
            return _BalancerPoolType.STABLE
        except Web3Exception:
            pass

        msg = (
            f"Cannot determine Balancer pool type for {address}. "
            f"Neither getNormalizedWeights() nor getAmplificationParameter() responded. "
            f"Linear pools are not yet supported."
        )
        raise DegenbotValueError(message=msg)
    ...

```

**Linear pool detection (future)**: Linear pools expose `getWrappedTokenRate()` and `getMainToken()` instead of `getAmplificationParameter()` or `getNormalizedWeights()`. When both probes fail, the builder now raises a clear error. When `BalancerV2LinearPool` is implemented, `_detect_pool_type()` should add a third branch (before the raise): try `getWrappedTokenRate()` → `_BalancerPoolType.LINEAR`.

### Step 12: Type resolution — add Balancer probing with variant-aware dispatch

Update `resolve_pool_type_by_probing()` to add a `getPoolId()` probe **after** `getReserves` fails and **before** the STABLESWAP fallback. If `getPoolId()` succeeds, the pool is Balancer. Then probe `getNormalizedWeights()` vs `getAmplificationParameter()` to produce the fine-grained variant.

The probing order is safe because `resolve_pool_type_by_probing()` checks `slot0()` first and returns early for V3 pools. No Balancer pool implements `slot0()`, so there is no V3/Balancer ambiguity.

```python
# In type_resolution.py — after the getReserves probe:

# Try Balancer: getPoolId() exists → Balancer pool
try:
    io.call(
        to=address,
        data=encode_function_calldata("getPoolId()", None),
    )
except Web3Exception:
    pass
else:
    # Balancer pool detected — determine weighted vs stable
    try:
        io.call(
            to=address,
            data=encode_function_calldata("getNormalizedWeights()", None),
        )
        variant = "balancer_weighted"
        family = PoolFamily.WEIGHTED
    except Web3Exception:
        variant = "balancer_stable"
        family = PoolFamily.STABLESWAP

    return PoolTypeDescriptor(
        family=family,
        variant=variant,
        kind=derive_kind(family, variant),
        factory=factory,
    )
```

**Why full probing here**: The type resolver should produce the fine-grained `kind` string directly (e.g. `"balancer_weighted"` not `"balancer"`). A coarse descriptor would require the builder to retroactively update the DB kind after construction, which is fragile. Two extra RPC calls (which succeed or fail immediately — no gas cost) are cheap at discovery time.

**Why `getPoolId` then `getNormalizedWeights`/`getAmplificationParameter` instead of just one probe**: Using only `getPoolId()` would produce a coarse descriptor that can't distinguish weighted from stable. Using only `getNormalizedWeights()` would also detect non-Balancer weighted-pool contracts if they existed. The two-step probe is both necessary and safe.

The Balancer probing produces the descriptor directly and returns early, bypassing `_descriptor_from_probing_result`. This is cleaner because the Balancer probing determines the exact family and variant in one pass.

### Step 13: Register builder and factories in Bot

```python
# bot.py __init__
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.builders.balancer_builder import BalancerBuilder

self._balancer_builder = BalancerBuilder(ctx)
self.register_builder(BalancerV2Pool, self._balancer_builder)
self.register_builder(BalancerV2StablePool, self._balancer_builder)
```

Both pool classes map to the same builder — the builder probes the contract to determine the concrete class.

Self-register factories in `balancer/__init__.py`:

```python
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import PoolFamily
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool

# Balancer V2 Weighted Pool Factory (v3)
pool_type_registry.register(
    BalancerV2Pool,
    chain_id=1,
    factory_address="0x8E9aa87E45e92bad7D5F7F9Dd794cea12F21707B",
    family=PoolFamily.WEIGHTED,  # Override auto-derivation
)

# Balancer V2 Stable Pool Factory (v1)
pool_type_registry.register(
    BalancerV2StablePool,
    chain_id=1,
    factory_address="0x8519F5A4A85678E0e03395586E2E223d70E9E09B",
    family=PoolFamily.STABLESWAP,  # Explicit — matches auto-derivation
)

# ComposableStablePool Factory (v2)
pool_type_registry.register(
    BalancerV2StablePool,
    chain_id=1,
    factory_address="0xA8936f4824B2E6407Fc0e94133909aeF7d48e876",
    family=PoolFamily.STABLESWAP,
)
```

### Step 14: Add fields to `BuildPoolRequest`

```python
# Balancer options (flat fields matching existing pattern;
# Plan 072 will scope these into BalancerBuildOptions)
bpt_idx: int | None = None        # Override BPT index detection
invariant_version: int | None = None  # Override: INVARIANT_V1 or INVARIANT_V2
```

Note: `pool_id` already exists on `BuildPoolRequest` (added for V4). The Balancer builder can reuse it — `getPoolId()` can be skipped if the pool ID is already known.

### Design decisions

- **`PoolFamily.WEIGHTED` for weighted pools via `family` override (Option C)**: Weighted pools use a bounded product invariant, not a stableswap invariant. The `family` override parameter on `register()` produces correct dispatch without modifying `_derive_family`. The `STABLESWAP` family is kept for stable pools (correct). Unknown factory addresses that resolve to `PoolFamily.WEIGHTED` produce a hard error rather than silently constructing `CurveStableswapPool`.
- **Single builder, not two**: `BalancerBuilder` handles all pool types, using `_detect_pool_type()` to branch internally. Matches the `CurvePoolBuilder` pattern.
- **`BalancerBuilderBase` for async pattern**: Pure-logic `@staticmethod` helpers (`decode_pool_id`, `detect_bpt_index`, `resolve_invariant_version`) live in a base class that `AsyncBalancerBuilder` can call without inheriting. Matches `V2BuilderBase` / `V3BuilderBase` / `V4BuilderBase` pattern.
- **`PoolInvariant.BALANCER_STABLESWAP`**: New enum value distinct from `CURVE_STABLESWAP`. The solver dispatches to a different mathematical function (Balancer StableMath vs Curve invariant). `BALANCER_WEIGHTED` already exists for weighted pools.
- **`swap_fn` on `BalancerWeightedHop`**: Added for integer-accurate evaluation in mixed-path solvers. Matches the pattern established by `SolidlyStableHop`, `CurveStableswapHop`, and the new `BalancerStableHop`. Without it, `SolidlyStableSolver._simulate_mixed_path` returns 0 for weighted hops.
- **`swap_fn` catches `StaleRateResult`**: `BalancerStableHop.swap_fn` wraps the pool's `calculate_tokens_out_from_tokens_in` and catches `StaleRateResult`, extracting the approximate `amount_out`. This prevents the solver from crashing when a ComposableStablePool has a static rate provider. The float approximation in the solver proceeds with best-effort values.
- **Separate `BalancerV2WeightedPoolExternalUpdate` / `BalancerV2StablePoolExternalUpdate`**: Even though both currently carry only `block_number` + `balances`, stable pools will need `amp` updates in the future and weighted pools may need `fee` updates. Separate types avoid a migration later.
- **`external_update()` uses `_state_lock`**: State mutation is wrapped in `with self._state_lock:` with a double-check-after-acquire pattern, matching `UniswapV2Pool`, `CurveStableswapPool`, and `UniswapV3Pool`. This prevents race conditions in multi-threaded bot sessions.
- **No `StateCache` for Balancer pools**: Balancer pools store only the current state as `_state`. They do not satisfy `StateManageablePool` (no `discard_states_before_block` / `restore_state_before_block`). This matches `CurveStableswapPool` which also uses simple state replacement without temporal navigation. Documented limitation — `ArbitragePath.calculate_with_state_override` works (it calls `to_hop_state(state_override=...)` directly), but `discard_states_before_block` / `restore_state_before_block` would raise `AttributeError`.
- **`BalancerPairView` with subscription relay**: The view adapts both call direction and notification direction. It subscribes to the underlying pool and re-publishes to its own subscribers with `publisher=self`. This ensures `ArbitragePath._pool_index` identity checks work correctly. The adapter is transparent to `ArbitragePath` — no changes to shared infrastructure needed.
- **`build_swap_amount()` raises for N > 2 without explicit pair**: For an N-token pool, the method requires `token_in`/`token_out` kwargs. A wrong-pair swap on mainnet sends real funds to the wrong destination — a hard error is preferable to silent misrouting.
- **`_detect_pool_type` returns `_BalancerPoolType` enum**: Instead of string literals (`"weighted"`, `"stable"`), uses an `IntEnum` for type-checker exhaustiveness checking and typo prevention.
- **`decode_pool_id` returns `DecodedPoolId` dataclass**: Instead of a raw `tuple[ChecksumAddress, int, int]`, returns a named frozen dataclass with `pool_address`, `specialization`, `nonce` fields. Prevents field ordering errors. Same pattern for `decode_vault_tokens` → `VaultTokensResult`.
- **`resolve_invariant_version` does not use `bpt_idx`**: The `bpt_idx` parameter was removed — it was never used in the logic. Invariant version depends only on `specialization` (from the pool ID) and the optional override.
- **`to_hop_state()` with `token_in`/`token_out` kwargs from the start**: Both pool classes implement these keyword-only parameters (default `None`) so N-token pools can specify the exact pair. When absent, `zero_for_one` falls back to `(0, 1) / (1, 0)` — fully backward-compatible. This eliminates private-attribute coupling in `BalancerPairView`, and also resolves the same TODO in `CurveStableswapPool.to_hop_state()` (which already has these kwargs). Multi-token basket optimization still uses `BalancerMultiTokenHop`.
- **`BalancerStableHop.token_index_in`/`token_index_out` store BPT-skipped indices**: The docstring clarifies "Index in the non-BPT token list (BPT-skipped)" — the value is `_skip_bpt_index(i)`, which maps from the full token list to the non-BPT index. Matches `CurveStableswapHop` which stores token indices in the invariant's indexing scheme.
- **`BalancerStableHop.invariant` is for float approximation, not integer accuracy**: The pre-computed D value may be stale after `external_update()`. The `swap_fn` computes D from scratch on each call. This matches the `CurveStableswapHop` pattern which sets `curve_d=0`.
- **Fetch from Vault, not the pool**: Balancer V2 stores token balances in the Vault contract. The builder must call `getPoolTokens(poolId)` on the Vault.
- **BPT index detection uses pool_address match**: Detect by checking which token address equals the pool address (self-referencing BPT). The `BuildPoolRequest.bpt_idx` override handles edge cases.
- **`BalancerV2SwapAmounts.encode` uses `encode_function_calldata`**: Matches `UniswapV2PoolSwapAmounts`, `CurveStableSwapPoolSwapAmounts`, and all other `SwapAmounts` classes. No manual selector computation or raw `eth_abi` calls.
- **`BalancerV2SwapAmounts` validates `pool_id` length**: 32-byte assertion in `__post_init__` prevents confusing `eth_abi` encoding errors from truncated IDs.
- **Variant-aware `pool_class_for_descriptor`**: The STABLESWAP fallback in `pool_class_for_descriptor` now checks if the variant starts with `"balancer"` and raises an error instead of falling through to `CurveStableswapPool`. This prevents silent misdispatch for unknown Balancer factories.
- **No DB support in slice 1**: First slice fetches everything from chain. DB persistence for Balancer pools can be added in a follow-up slice. The schema extension should store: `pool_id`, `vault`, `weights`/`amp`, `scaling_factors`, `bpt_idx`, `invariant_version`, `pow_version`, `base_scaling_factors` — likely as a JSON blob in `immutable_data` matching the V3/V4 pattern.
- **Full probing in type resolver**: The type resolver probes both `getPoolId()` AND (`getNormalizedWeights()` or `getAmplificationParameter()`) to produce the fine-grained `kind` string directly. This avoids the coarse→fine retroactive update problem.
- **Flat fields on `BuildPoolRequest` for now**: `bpt_idx` and `invariant_version` are flat fields matching the existing pattern (V2/V3/V4 options are already flat). Plan 072 will scope all builder-specific options into sub-objects.

## Files Involved

**Primary:**
- `src/degenbot/builders/balancer_builder.py` — new file; builder for all Balancer pool types
- `src/degenbot/builders/balancer_builder_base.py` — new file; pure-logic helpers for async reuse, `DecodedPoolId`, `VaultTokensResult`, `_BalancerPoolType`
- `src/degenbot/balancer/pools.py` — implement `to_hop_state()`, `external_update()` (with lock), fix `override_state`, add `variant` class attribute, add `build_swap_amount()` (raises for N > 2)
- `src/degenbot/balancer/stable_pools.py` — implement `to_hop_state()`, `external_update()` (with lock), fix `override_state`, fix `tokens` return type, add `variant` class attribute, add `build_swap_amount()` (raises for N > 2)
- `src/degenbot/balancer/types.py` — add `BalancerV2WeightedPoolExternalUpdate`, `BalancerV2StablePoolExternalUpdate`, `BalancerV2PoolStateUpdated`
- `src/degenbot/balancer/swap_amounts.py` — new file; `BalancerV2SwapAmounts` (uses `encode_function_calldata`, validates `pool_id`)
- `src/degenbot/balancer/pair_view.py` — new file; `BalancerPairView` adapter with subscription relay
- `src/degenbot/types/hop_types.py` — add `BalancerStableHop` dataclass (with corrected docstring), add `swap_fn` to `BalancerWeightedHop`, add `PoolInvariant.BALANCER_STABLESWAP`, update `HopType` union
- `src/degenbot/registry/pool_type.py` — add `family` parameter to `register()`, update `_derive_family` docstring

**Secondary:**
- `src/degenbot/bot.py` — register `BalancerBuilder` for both pool classes
- `src/degenbot/balancer/__init__.py` — self-register factories in `pool_type_registry` with `family` overrides
- `src/degenbot/balancer/deployments.py` — add factory addresses
- `src/degenbot/builders/type_resolution.py` — add `getPoolId()` + `getNormalizedWeights()` / `getAmplificationParameter()` probing, update `pool_class_for_descriptor` with variant-aware fallbacks and `WEIGHTED` family case
- `src/degenbot/builders/request.py` — add `bpt_idx` and `invariant_version` fields (flat; Plan 072 will scope them)
- `src/degenbot/arbitrage/optimizers/hop_types.py` — add `has_balancer_stableswap` property to `SolveInput`
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — verify `token_in`/`token_out` kwargs on `to_hop_state()` (already present)
- `src/degenbot/types/pool_type.py` — verify `WEIGHTED` family handling in `derive_kind`

**No change needed:**
- `src/degenbot/types/hop_types.py` — `BalancerMultiTokenHop` already exists
- `src/degenbot/balancer/libraries/` — math libraries already work
- `src/degenbot/builders/context.py` — `BuilderContext` already has everything `BalancerBuilder` needs
- `src/degenbot/builders/protocol.py` — `PoolBuilder` protocol already satisfied
- `src/degenbot/types/pool_protocols.py` — `ArbitrageCapablePool.to_hop_state()` already has `token_in`/`token_out` kwargs

## Implementation Order

### Slice 1: Fix bugs, add variant class attributes, add external update types, add `family` override to `register()`

1. Fix `BalancerV2StablePool.tokens` return type (`tuple[tuple[...]]` → `tuple[...]`)
2. Add `variant: ClassVar[str | None] = "balancer_weighted"` to `BalancerV2Pool`
3. Add `variant: ClassVar[str | None] = "balancer_stable"` to `BalancerV2StablePool`
4. Add `BalancerV2WeightedPoolExternalUpdate` dataclass to `balancer/types.py`
5. Add `BalancerV2StablePoolExternalUpdate` dataclass to `balancer/types.py`
6. Add `BalancerV2PoolStateUpdated` PoolStateMessage subclass to `balancer/types.py`
7. Add `family: PoolFamily | None = None` parameter to `PoolTypeRegistry.register()` — when provided, override auto-derivation
8. Run: `just test-python` — expect all green

### Slice 2: `external_update()` (with lock) and `to_hop_state()` on `BalancerV2Pool` (with `token_in`/`token_out` kwargs from the start)

1. Implement `external_update()` on `BalancerV2Pool` — hold `_state_lock`, double-check after acquire, update state, notify subscribers with `BalancerV2PoolStateUpdated`
2. Add `swap_fn` field to `BalancerWeightedHop` (optional, `compare=False, hash=False`)
3. Implement `to_hop_state()` on `BalancerV2Pool` — accept `token_in`/`token_out` kwargs, return `BalancerWeightedHop` with `swap_fn`
4. Fix `calculate_tokens_out_from_tokens_in` and `calculate_tokens_in_from_tokens_out` to handle `override_state` (required for `simulate_swap` to work with state overrides)
5. Write tests: construct pool, call `to_hop_state(zero_for_one=True)` and `to_hop_state(zero_for_one=True, token_in=..., token_out=...)`, verify hop fields; call `external_update()`, verify state changes and subscriber notification
6. Run: `just test-python` — expect all green

### Slice 3: `external_update()` (with lock), `to_hop_state()`, `BalancerStableHop`, `PoolInvariant.BALANCER_STABLESWAP`

1. Add `PoolInvariant.BALANCER_STABLESWAP` to `hop_types.py`
2. Add `BalancerStableHop` dataclass to `hop_types.py` (with `swap_fn` that catches `StaleRateResult`, corrected docstring for `token_index_in`/`token_index_out`)
3. Update `HopType` union to include `BalancerStableHop`
4. Add `has_balancer_stableswap` property to `SolveInput`
5. Implement `external_update()` on `BalancerV2StablePool` — hold `_state_lock`, double-check after acquire
6. Implement `to_hop_state()` on `BalancerV2StablePool` — accept `token_in`/`token_out` kwargs, return `BalancerStableHop` with `swap_fn` (catches `StaleRateResult`)
7. Fix `calculate_tokens_out_from_tokens_in` and `calculate_tokens_in_from_tokens_out` to handle `override_state` on stable pool
8. Verify `CurveStableswapPool.to_hop_state()` already has `token_in`/`token_out` kwargs (no change needed — already present)
9. Write tests: construct pool, call `to_hop_state(zero_for_one=True)` and `to_hop_state(zero_for_one=True, token_in=..., token_out=...)`, verify hop fields; call `external_update()`, verify state changes; test `swap_fn` with and without `StaleRateResult`
10. Run: `just test-python` — expect all green

### Slice 4: `BalancerPairView` (with subscription relay), swap amounts, `build_swap_amount()` (raises for N > 2)

1. Create `balancer/swap_amounts.py` with `BalancerV2SwapAmounts` (uses `encode_function_calldata`, validates `pool_id`, includes `FundManagement` with `sender=ZERO_ADDRESS` and `recipient` required)
2. Implement `build_swap_amount()` on both pool classes — returns `BalancerV2SwapAmounts`, raises for N > 2 pools without explicit `token_in`/`token_out`
3. Create `balancer/pair_view.py` with `BalancerPairView` adapter — implements subscription relay (subscribes to pool, re-publishes with `publisher=self`), delegates all operations
4. Write tests for `BalancerPairView` — verify `ArbitragePathPool` protocol satisfaction, verify delegation correctness, verify subscription relay (subscribe → update → notify reaches path's subscriber with correct publisher)
5. Run: `just test-python` — expect all green

### Slice 5: Create `BalancerBuilder` and `BalancerBuilderBase`

1. Create `src/degenbot/builders/balancer_builder_base.py` with pure-logic `@staticmethod` helpers: `decode_pool_id` (returns `DecodedPoolId`), `decode_vault_tokens` (returns `VaultTokensResult`), `detect_bpt_index`, `resolve_invariant_version` (no `bpt_idx` param)
2. Create `src/degenbot/builders/balancer_builder.py` inheriting from `BalancerBuilderBase`
3. Implement `_detect_pool_type()` — returns `_BalancerPoolType` enum (not string), probes `getNormalizedWeights()` / `getAmplificationParameter()`, raises on neither
4. Implement `_fetch_pool_id()`, `_fetch_vault_tokens()`, `_fetch_swap_fee()`, `_fetch_weights()`, `_fetch_amp()`, `_fetch_rate_providers()`, `_fetch_rates()`
5. Implement `_build_weighted()` — fetch pool ID, vault, tokens+balances, fee, weights, PowVersion
6. Implement `_build_stable()` — fetch pool ID, vault, tokens+balances, fee, amp, rate providers, BPT index (using `detect_bpt_index`), invariant version (using `resolve_invariant_version`), base scaling factors
7. Implement `update()` — fetch balances from Vault, apply via correct update type (`BalancerV2WeightedPoolExternalUpdate` or `BalancerV2StablePoolExternalUpdate`)
8. Add `bpt_idx` and `invariant_version` fields to `BuildPoolRequest` (flat fields; Plan 072 will scope them)
9. Add factory addresses to `deployments.py`
10. Write tests with `FakePoolIO` returning canned Balancer contract responses
11. Run: `just test-python` — expect all green

### Slice 6: Register builder, type resolution, factory self-registration

1. Create `BalancerBuilder(ctx)` in `Bot.__init__`, register for both pool classes
2. Add `getPoolId()` + `getNormalizedWeights()` / `getAmplificationParameter()` probing to `type_resolution.py`
3. Update `pool_class_for_descriptor()` — add `PoolFamily.WEIGHTED` case, add variant-aware STABLESWAP fallback (reject `"balancer_*"` variants instead of falling through to `CurveStableswapPool`)
4. Self-register factories in `balancer/__init__.py` via `pool_type_registry.register()` with `family` overrides
5. Same for async type resolution
6. Run: `just test-python` — expect all green

### Slice 7: Validate and clean up

1. Run `just lint` + `just test-all`
2. Integration test: `bot.build_pool("0x...BalancerPoolAddress...")` returns the correct class
3. Integration test: `pool.to_hop_state(zero_for_one=True)` returns the correct hop type
4. Update `balancer/CONTEXT.md` — add builder, pair view, swap amounts, external update types
5. Update `balancer/CONTEXT.md` — add Builder, Pair View, Swap Amounts, External Update entries to terminology table
6. Update `CONTEXT-MAP.md` — add Balancer builder reference
7. Update `AGENTS.md` builder table
8. Remove empty `balancer/managers.py`
9. Run: `just test-all` — expect all green

## Testing

### Per-slice test runs

Each slice runs `just test-python`.

### New unit tests

```python
# tests/balancer/test_pool_methods.py


def test_weighted_to_hop_state_zero_for_one():
    """BalancerV2Pool.to_hop_state(zero_for_one=True) returns BalancerWeightedHop for (0,1)."""
    pool = BalancerV2Pool(
        address="0x1234...",
        pool_id=b"\x00" * 32,
        vault="0xBA12222222228d8Ba445958a75a0704d566BF2C8",
        tokens=[FAKE_WETH, FAKE_USDC],
        balances=[1000, 2000000],
        fee=Fraction(3, 1000),
        weights=[int(0.5e18), int(0.5e18)],
    )
    hop = pool.to_hop_state(zero_for_one=True)
    assert isinstance(hop, BalancerWeightedHop)
    assert hop.reserve_in == 1000
    assert hop.reserve_out == 2000000
    assert hop.swap_fn is not None
    assert hop.swap_fn(100) > 0  # swap_fn provides integer-accurate evaluation


def test_weighted_to_hop_state_with_token_pair():
    """BalancerV2Pool.to_hop_state with token_in/token_out kwargs."""
    pool = BalancerV2Pool(
        address="0x1234...",
        pool_id=b"\x00" * 32,
        vault="0xBA12222222228d8Ba445958a75a0704d566BF2C8",
        tokens=[FAKE_WETH, FAKE_USDC, FAKE_DAI],
        balances=[1000, 2000000, 3000000],
        fee=Fraction(3, 1000),
        weights=[int(0.6e18), int(0.2e18), int(0.2e18)],
    )
    # Select WETH→DAI (indices 0,2) explicitly
    hop = pool.to_hop_state(zero_for_one=True, token_in=FAKE_WETH, token_out=FAKE_DAI)
    assert isinstance(hop, BalancerWeightedHop)
    assert hop.reserve_in == 1000
    assert hop.reserve_out == 3000000
    assert hop.weight_in == int(0.6e18)
    assert hop.weight_out == int(0.2e18)


def test_stable_to_hop_state():
    """BalancerV2StablePool.to_hop_state returns BalancerStableHop."""
    pool = BalancerV2StablePool(
        address="0x5678...",
        pool_id=b"\x00" * 32,
        vault="0xBA12222222228d8Ba445958a75a0704d566BF2C8",
        tokens=[FAKE_STETH, FAKE_WETH],
        balances=[100 * 10**18, 200 * 10**18],
        fee=Fraction(4, 10000),
        amp=50000,
        scaling_factors=[ONE, ONE],
        invariant_version=INVARIANT_V2,
    )
    hop = pool.to_hop_state(zero_for_one=True)
    assert isinstance(hop, BalancerStableHop)
    assert hop.amp == 50000
    assert hop.pool_invariant == PoolInvariant.BALANCER_STABLESWAP
    assert hop.swap_fn is not None


def test_stable_swap_fn_catches_stale_rate():
    """BalancerStableHop.swap_fn handles StaleRateResult gracefully."""
    # Create pool with _StaticRateProvider → triggers StaleRateResult for ComposableStable
    pool = BalancerV2StablePool(
        ...,
        bpt_idx=2,  # ComposableStablePool
        ...
    )
    hop = pool.to_hop_state(zero_for_one=True)
    # swap_fn should not raise — it catches StaleRateResult
    result = hop.swap_fn(10**18)
    assert result > 0


def test_weighted_external_update():
    """BalancerV2Pool.external_update updates state and notifies subscribers."""
    pool = BalancerV2Pool(...)
    update = BalancerV2WeightedPoolExternalUpdate(
        block_number=100,
        balances=(2000, 3000000),
    )
    pool.external_update(update)
    assert pool.balances == (2000, 3000000)
    assert pool.state.block == 100


def test_stable_external_update():
    """BalancerV2StablePool.external_update updates state."""
    pool = BalancerV2StablePool(...)
    update = BalancerV2StablePoolExternalUpdate(
        block_number=100,
        balances=(200 * 10**18, 400 * 10**18),
    )
    pool.external_update(update)
    assert pool.balances == (200 * 10**18, 400 * 10**18)
    assert pool.state.block == 100


def test_weighted_override_state():
    """BalancerV2Pool with override_state uses override balances."""
    pool = BalancerV2Pool(...)
    override = BalancerV2PoolState(
        address=pool.address, block=200, balances=(5000, 6000000),
    )
    amount_out = pool.calculate_tokens_out_from_tokens_in(
        token_in=tokens[0], token_out=tokens[1],
        token_in_quantity=10**18, override_state=override,
    )
    assert amount_out > 0


def test_pair_view_satisfies_protocol():
    """BalancerPairView satisfies ArbitragePathPool."""
    pool = BalancerV2Pool(...)
    view = BalancerPairView(pool, tokens[0], tokens[1])
    assert hasattr(view, "token0")
    assert hasattr(view, "token1")
    assert hasattr(view, "simulate_swap")


def test_pair_view_delegates_to_hop_state():
    """BalancerPairView.to_hop_state delegates with explicit token pair."""
    pool = BalancerV2Pool(...)
    view = BalancerPairView(pool, FAKE_WETH, FAKE_USDC)
    hop = view.to_hop_state(zero_for_one=True)
    assert isinstance(hop, BalancerWeightedHop)


def test_pair_view_subscription_relay():
    """BalancerPairView relays notifications with publisher=self."""
    pool = BalancerV2Pool(...)
    view = BalancerPairView(pool, tokens[0], tokens[1])
    received = []

    class FakeSubscriber:
        def notify(self, publisher, message):
            received.append(publisher)

    subscriber = FakeSubscriber()
    view.subscribe(subscriber)
    # Simulate a state update on the pool
    pool.external_update(BalancerV2WeightedPoolExternalUpdate(
        block_number=100, balances=(2000, 3000),
    ))
    # Subscriber should have been notified with publisher=view (not pool)
    assert len(received) == 1
    assert received[0] is view


def test_build_swap_amount_raises_for_n_gt_2():
    """build_swap_amount raises for N > 2 pools without explicit pair."""
    pool = BalancerV2Pool(
        ...,
        tokens=[FAKE_WETH, FAKE_USDC, FAKE_DAI],
        ...
    )
    with pytest.raises(DegenbotValueError, match="N > 2"):
        pool.build_swap_amount(zero_for_one=True, amount_in=100, amount_out=90)


def test_detect_pool_type_rejects_unknown():
    """BalancerBuilder._detect_pool_type raises on unknown pool type."""
    # Pool that responds to getPoolId but neither getNormalizedWeights
    # nor getAmplificationParameter should raise, not default to stable.
    ...


# tests/builders/test_balancer_builder.py


def test_balancer_builder_detects_weighted():
    """Builder probes detect a weighted pool."""

def test_balancer_builder_detects_stable():
    """Builder probes detect a stable pool."""

def test_balancer_builder_detects_unknown_raises():
    """Builder raises DegenbotValueError for unknown Balancer pool type."""

def test_balancer_builder_builds_weighted():
    """BalancerBuilder constructs a BalancerV2Pool from chain data."""

def test_balancer_builder_builds_stable():
    """BalancerBuilder constructs a BalancerV2StablePool from chain data."""

def test_balancer_builder_update_weighted():
    """BalancerBuilder.update fetches new balances from Vault for weighted pool."""

def test_balancer_builder_update_stable():
    """BalancerBuilder.update fetches new balances from Vault for stable pool."""
```

### Integration tests

Integration tests require a live RPC endpoint with known Balancer pools. These should be marked as fork tests (not run in CI) and tested manually. Use the pools already tested in `tests/balancer/test_pools.py` and `tests/balancer/test_stable_pools.py`:

- Weighted (WETH/BAL 80/20): `0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56`
- Weighted (USDC/WETH 50/50): `0x3e5fa9518ea95c3e533eb377c001702a9aacaa32`
- Weighted (WETH/RPL 80/20): `0xff083f57a556bfb3bbe46ea1b4fa154b2b1fbe88`
- MetaStable (wstETH/WETH): `0x32296969Ef14EB0c6d29669C550D4a0449130230`
- ComposableStable (TUSD BSP): `0x53BC3cBa3832ebeCBFa002c12023F8ab1AA3a3a0`

Additionally, test the full `Bot.build_pool()` flow for each pool address.

## Benefits

- **Leverage**: `Bot.build_pool()` is the single entry point for all pool families, including Balancer.
- **Locality**: Balancer I/O concentrates in `BalancerBuilder` — matches all other pool families.
- **Depth**: Both pool classes become deep modules — I/O-free construction, state updates via `external_update()`, solver-compatible via `to_hop_state()`.
- **Deletion test satisfied**: Without a builder, Balancer pools are excluded from the architecture. Adding the builder brings them into the fold.
- **Correct family dispatch**: Weighted pools get `PoolFamily.WEIGHTED` (not `STABLESWAP`), preventing downstream misrouting. Unknown Balancer factories produce hard errors instead of silently constructing `CurveStableswapPool`.
- **Solver accuracy**: `swap_fn` on both `BalancerWeightedHop` and `BalancerStableHop` enables integer-accurate evaluation in mixed-path solvers, matching the `SolidlyStableHop` / `CurveStableswapHop` pattern.
- **Async-ready**: `BalancerBuilderBase` with `@staticmethod` helpers ensures the future `AsyncBalancerBuilder` can call them without inheriting, matching the established async builder pattern.
- **Safe N-token handling**: `build_swap_amount()` raises for N > 2 pools without explicit pair, preventing wrong-destination swaps on mainnet.

## Risks

- **Vault contract calls**: Balancer V2 stores balances in the Vault, not the pool. The builder must call `getPoolTokens(poolId)` on the Vault. This requires knowing the Vault address. Mitigation: centralized in `deployments.py`, well-known per chain.
- **N-token → 2-token impedance**: `ArbitragePath` assumes 2-token pools with `token0`/`token1`. Mitigation: `BalancerPairView` adapter with subscription relay. A proper `MultiTokenArbitragePath` is a separate plan.
- **BPT index detection heuristics**: Detecting which token is BPT may fail for unusual pool configurations. Mitigation: `BuildPoolRequest.bpt_idx` override and `detect_bpt_index` in the base class for centralised heuristics.
- **Invariant version detection**: Defaults based on `pool_specialization` from decoded `pool_id` may be wrong for newer ComposableStablePools that use INVARIANT_V2. Mitigation: `BuildPoolRequest.invariant_version` override.
- **Rate provider complexity**: Builder uses `_StaticRateProvider` at construction. Callers needing exact matching must inject a `CacheAwareRateProvider` post-construction. Matches `CurvePoolBuilder` pattern.
- **Amp not tracked by `external_update()`**: Amp can change over time (A ramping). The builder's `update()` only fetches new balances. For typical bot sessions (minutes to hours), amp is effectively constant. For long-running sessions, a future slice should add amp tracking to `BalancerV2StablePoolExternalUpdate`. **Deferred item**: amp changes not handled in this plan.
- **Fee changes not tracked**: Balancer V2 pool fees can change via governance. Same mitigation as amp — effectively constant for typical sessions. **Deferred item**: fee changes not handled in this plan.
- **No async builder in slice 1**: Sync-only initially. `AsyncBalancerBuilder` is a straightforward translation that calls `BalancerBuilderBase` static methods without inheriting.
- **`PoolInvariant.BALANCER_STABLESWAP` and Rust**: Verified — the Rust code (`rust/`) does not reference `PoolInvariant` at all. Only Python solvers use it. Safe to add the new enum value without Rust changes.
- **Mixed-path solver gap**: `SolidlyStableSolver.supports()` rejects paths containing `BalancerWeightedHop` or `BalancerStableHop` (their `PoolInvariant` values aren't in the accepted set). The simulation functions `_simulate_mixed_path` and `_simulate_mixed_path_int` also don't handle these hop types — they fall through to `return 0`. Adding `swap_fn` to `BalancerWeightedHop` enables integer-accurate evaluation but doesn't make the solver *accept* the hop. For mixed Balancer/V2 paths, either (a) extend `SolidlyStableSolver` to accept and handle Balancer hops (using `swap_fn`), or (b) create a new solver. This is out of scope. `BalancerPairView` + `ArbitragePath` will work with a solver that accepts the hop types, but not with the existing `SolidlyStableSolver`.
- **`to_hop_state()` pair selection solved**: `token_in`/`token_out` kwargs on `to_hop_state()` address the hardcoded `(0,1)` limitation. Implemented from Slice 2, not deferred.
- **`derive_kind` collision resolved**: Using distinct variants (`"balancer_weighted"`, `"balancer_stable"`) with correct families (`WEIGHTED`, `STABLESWAP`) produces kinds `"balancer_weighted"` and `"balancer_stable"` — no collision with each other or with `"stableswap"` (Curve).
- **`pool_class_for_descriptor` misdispatch prevented**: Variant-aware fallbacks in `pool_class_for_descriptor` reject Balancer variants instead of falling through to `CurveStableswapPool`. Unknown factories produce hard errors.
- **No `StateManageablePool` conformance**: Balancer pools don't implement `discard_states_before_block` / `restore_state_before_block` (no `StateCache`). `ArbitragePath.calculate_with_state_override` works (it calls `to_hop_state(state_override=...)` directly), but explicit state management methods would raise `AttributeError`. Matches `CurveStableswapPool` behavior.
- **Subscription relay depends on `_notify_subscribers` calling order**: The relay assumes that when the pool calls `_notify_subscribers`, it iterates its `_subscribers` WeakSet. Subscribers added by the view's `subscribe()` are the view itself. This is the standard pub/sub pattern used by all pool classes. No special ordering required.

## Deferred Items

These are explicitly out of scope for this plan. They should be addressed in follow-up plans.

- **Amp tracking in `external_update()`**: `BalancerV2StablePoolExternalUpdate.amp` field and re-computation of the stable pool's `self.amp` after updates. Requires the builder's `update()` to also fetch amp from `getAmplificationParameter()`.
- **Fee tracking in `external_update()`**: Re-fetching `getSwapFeePercentage()` on update and propagating fee changes.
- **DB persistence for Balancer pools**: Schema extension to store `pool_id`, `vault`, `weights`/`amp`, `scaling_factors`, `bpt_idx`, `invariant_version`, `pow_version`, `base_scaling_factors` — likely as a JSON blob in `immutable_data` matching the V3/V4 pattern. The builder's `extract_db_values()` and `decode_immutable_data()` methods will follow the established pattern.
- **Async Balancer builder**: `AsyncBalancerBuilder` calling `BalancerBuilderBase` static methods. Straightforward translation following the `AsyncV3PoolBuilder` pattern.
- **Linear pool support**: `BalancerV2LinearPool` class and builder detection. When implemented, `_detect_pool_type()` adds a third branch (try `getWrappedTokenRate()` → `_BalancerPoolType.LINEAR`) before the raise.
- **Mixed-path solver for Balancer hops**: Extend `SolidlyStableSolver` or create a new solver that accepts `BALANCER_WEIGHTED` and `BALANCER_STABLESWAP` invariant types, using `swap_fn` for integer evaluation.
- **MultiTokenArbitragePath**: A principled solution for N-token pools in cyclic arbitrage, replacing `BalancerPairView`. Requires changes to `ArbitragePath`, solver dispatch, swap amount construction, and the subscription model.
- **Vault event subscription**: Push-based state updates via Vault `Swap` events filtered by `poolId`, extending `LogListener` with Balancer event decoders.

## Relationship to Other Plans

- **Plan 072** (Scoped Build Pool Request): Plan 070 adds `bpt_idx` and `invariant_version` as flat fields on `BuildPoolRequest` (matching existing pattern). Plan 072 will scope all builder-specific options into sub-objects (`V2BuildOptions`, `BalancerBuildOptions`, etc.). Plan 070 must land first.
- **Plan 014** (Async REPL): Orthogonal — different pool family. Async builder follows same pattern as other async builders.
- **Plan 068** (Absorb Curve on-chain cache): Orthogonal — Curve-specific.
- **Plan 069** (Remove DyCalculation closures): Orthogonal — Curve-specific.
- **Future: Mixed-path solver for Balancer hops**: The `SolidlyStableSolver` pattern (golden-section / Newton with `swap_fn`-backed integer evaluation) can be extended to accept `BALANCER_WEIGHTED` and `BALANCER_STABLESWAP` invariant types. `BalancerWeightedHop` now carries `swap_fn`. `BalancerStableHop` also carries `swap_fn`. This should be a separate plan.
- **Future: MultiTokenArbitragePath**: A more principled solution than `BalancerPairView` for N-token pools in cyclic arbitrage. Requires changes to `ArbitragePath`, solver dispatch, swap amount construction, and the subscription model. This should be a separate architecture plan.
- **Future: Vault event subscription**: For push-based state updates (currently only pull via `Bot.update()`), subscribe to Vault `Swap` events filtered by `poolId`. Requires extending `LogListener` with Balancer event decoders. This should be a separate plan.

## Status

[x] Slice 1: Fix bugs, add variant class attributes, add external update types, add `family` override to `register()`
[x] Slice 2: `external_update()` (with lock) and `to_hop_state()` on `BalancerV2Pool` (with `token_in`/`token_out` kwargs and `swap_fn`)
[x] Slice 3: `BalancerStableHop`, `PoolInvariant.BALANCER_STABLESWAP`, `external_update()` (with lock) and `to_hop_state()` on `BalancerV2StablePool`
[x] Slice 4: `BalancerPairView` (with subscription relay), swap amounts, `build_swap_amount()` (raises for N > 2)
[x] Slice 5: Create `BalancerBuilderBase` and `BalancerBuilder`
[x] Slice 6: Register builder, type resolution (variant-aware), factory self-registration
[x] Slice 7: Validate and clean up
