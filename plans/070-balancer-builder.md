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

## Solution

### Step 0: Fix pre-existing bugs

Before adding builder/protocol machinery, fix two bugs in the existing stable pool code:

```python
# Bug 1: BalancerV2StablePool.tokens has wrong return type
# stable_pools.py:224

# Before (WRONG — tuple of one element containing another tuple):
@property
def tokens(self) -> tuple[tuple[Erc20Token, ...] | tuple[()]]:
    return self._tokens

# After (CORRECT):
@property
def tokens(self) -> tuple[Erc20Token, ...]:
    return self._tokens
```

### Step 1: Resolve `PoolFamily` derivation conflict

**Critical problem**: `_derive_family()` in `registry/pool_type.py` classifies pools by structural shape. `BalancerV2Pool` has `tokens` but not `fee_token0` → it matches `STABLESWAP`, not `WEIGHTED`. `BalancerV2StablePool` also matches `STABLESWAP`. Using `PoolFamily.WEIGHTED` with variant `"balancer"` as the plan originally proposed is **not possible** without structural changes.

Three options:

**Option A (recommended): Use `STABLESWAP` with variant `"balancer_weighted"` / `"balancer_stable"`**

This is the path of least resistance. `_derive_family` already returns `STABLESWAP` for both classes. We lean into it by differentiating with distinct variants:

```python
class BalancerV2Pool(PublisherMixin, PoolPickleMixin, AbstractLiquidityPool):
    variant: ClassVar[str | None] = "balancer_weighted"
    ...

class BalancerV2StablePool(PublisherMixin, PoolPickleMixin, AbstractLiquidityPool):
    variant: ClassVar[str | None] = "balancer_stable"
    ...
```

This produces kinds `"balancer_weighted"` and `"balancer_stable"` — no collision. The type resolver produces `PoolFamily.STABLESWAP` + the appropriate variant. The builder dispatches on the variant or on the pool class directly.

**Option B: Add weights detection to `_derive_family`**

Add a check for a `weights` attribute on the class. If `hasattr(pool_class, "weights")`, return `WEIGHTED`. This requires modifying `pool_type.py` and could break existing `STABLESWAP` consumers.

**Option C: Override `_derive_family` via registration**

Add a `family` override parameter to `pool_type_registry.register()` that bypasses auto-derivation. Register Balancer classes with explicit `family=PoolFamily.WEIGHTED`.

**Decision: Option A.** It works without modifying shared infrastructure, produces non-colliding kind strings, and the `STABLESWAP` family assignment is defensible — both Balancer pool types use iterative invariant math that is structurally more similar to Curve stableswap than to constant-product AMMs. The `variant` field provides the distinguishing information for builder dispatch.

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

### Step 3: Add `BalancerStableHop` dataclass

```python
# In hop_types.py:

@dataclass(frozen=True, slots=True)
class BalancerStableHop:
    """
    A Balancer stable pool hop (StableSwap invariant).

    Not a Möbius transformation — the swap function requires iterative
    invariant computation (Newton's method). Follows the same pattern as
    CurveStableswapHop and SolidlyStableHop.

    The optional ``swap_fn`` provides an integer-accurate swap simulation.
    When provided, the solver uses it for exact path evaluation.
    """
    reserve_in: int
    reserve_out: int
    fee: Fraction
    amp: int
    n_tokens: int
    invariant: int  # Pre-computed D invariant for the current state
    token_index_in: int  # Non-BPT-adjusted index
    token_index_out: int  # Non-BPT-adjusted index
    swap_fn: Callable[[int], int] | None = field(default=None, compare=False, hash=False)
    pool_invariant: PoolInvariant = PoolInvariant.BALANCER_STABLESWAP

    @property
    def gamma(self) -> float:
        return 1.0 - float(self.fee)
```

Update `HopType` union and `SolveInput.has_balancer_stableswap` property. Also update `SolveInput` in `arbitrage/optimizers/hop_types.py` to add:

```python
@property
def has_balancer_stableswap(self) -> bool:
    return any(h.invariant == _PoolInvariant.BALANCER_STABLESWAP for h in self.hops)
```

### Step 4: Implement `external_update()` and `BalancerV2PoolStateUpdated` on both pool classes

Both pool classes need `external_update()` and a `PoolStateMessage` subclass for subscriber notification.

```python
# In balancer/types.py — add:
from degenbot.types.concrete import PoolStateMessage

@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class BalancerV2PoolExternalUpdate:
    block_number: int
    balances: tuple[int, ...]

class BalancerV2PoolStateUpdated(PoolStateMessage):
    """Message published when a Balancer V2 pool's state changes."""
    state: BalancerV2PoolState
```

```python
# On both pool classes:
def external_update(self, update: BalancerV2PoolExternalUpdate) -> None:
    if update.block_number <= self.state.block:
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

**Note**: The original plan's code passed `self` instead of a proper `PoolStateMessage` — this is a bug. All pool classes use `PoolStateMessage` subclasses (e.g. `UniswapV2PoolStateUpdated`, `CurveStableSwapPoolStateUpdated`).

### Step 5: Implement `to_hop_state()` on `BalancerV2Pool`

```python
def to_hop_state(
    self,
    zero_for_one: bool,
    state_override: BalancerV2PoolState | None = None,
) -> HopType:
    state = state_override or self.state

    # N-token pool: zero_for_one selects pair (0, 1) or (1, 0).
    # For non-trivial pair selection, callers should use
    # the multi-token solver directly.
    if zero_for_one:
        i, j = 0, 1
    else:
        i, j = 1, 0

    return BalancerWeightedHop(
        reserve_in=state.balances[i],
        reserve_out=state.balances[j],
        fee=self.fee,
        weight_in=self.weights[i],
        weight_out=self.weights[j],
    )
```

**Critical caveat**: This works for 2-token weighted pools but is misleading for N-token pools where `zero_for_one=True` always picks indices 0 and 1 regardless of which tokens the caller actually wants. The `ArbitragePath` uses `token0`/`token1` to determine direction, but `BalancerV2Pool` has no `token0`/`token1` — it has `tokens`. This means **N-token Balancer pools cannot participate in `ArbitragePath`**. See Step 8.

### Step 6: Implement `to_hop_state()` on `BalancerV2StablePool` — same `token_in`/`token_out` pattern

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

    if token_in is not None and token_out is not None:
        i = self._tokens.index(token_in)
        j = self._tokens.index(token_out)
    elif zero_for_one:
        i, j = 0, 1
    else:
        i, j = 1, 0

    sf = self._resolve_scaling_factors()
    upscaled = self._upscale_balances(balances, sf)
    inv = self._compute_invariant(upscaled)

    return BalancerStableHop(
        reserve_in=balances[i],
        reserve_out=balances[j],
        fee=self.fee,
        amp=self.amp,
        n_tokens=len(self._non_bpt_indices),
        invariant=inv,
        token_index_in=self._skip_bpt_index(i),
        token_index_out=self._skip_bpt_index(j),
        swap_fn=lambda amount_in: self.calculate_tokens_out_from_tokens_in(
            token_in=self._tokens[i],
            token_out=self._tokens[j],
            token_in_quantity=amount_in,
        ),
    )
```

**Important**: The `swap_fn` closure captures `self`, `i`, and `j`. This prevents the `BalancerStableHop` from being hashable (the `compare=False, hash=False` on `swap_fn` handles that). But the closure also holds a reference to `self` which keeps the pool alive — this matches the `SolidlyStableHop` and `CurveStableswapHop` patterns.

**Rate-provider caveat**: When `requires_io_at_calculation_time` is `True`, the `swap_fn` will call `self._rate_provider.get_rates()` which does I/O. The solver uses `swap_fn` for integer-accurate evaluation, so this is expected — but the solver documentation should note that `swap_fn` may perform I/O for ComposableStablePools. This matches `CurveStableswapHop.swap_fn` which also performs I/O via the pool's `get_dy()` method.

### Step 7: Fix `override_state` on `calculate_tokens_out_from_tokens_in` / `calculate_tokens_in_from_tokens_out`

Both pool classes raise `NotImplementedError` when `override_state` is provided. For weighted pools:

```python
# In BalancerV2Pool.calculate_tokens_out_from_tokens_in:
if override_state is not None:
    balances = list(override_state.balances)
else:
    balances = list(self.balances)
```

Same pattern for `calculate_tokens_in_from_tokens_out`. For stable pools, the override must also flow into `_resolve_scaling_factors` and `_compute_invariant` — but stable pools don't have per-block rates or invariant values stored on the state object. The override only swaps out balances, which is what `ArbitragePath._resolve_state_overrides` expects.

**Note on `_upscale_array` mutation**: `BalancerV2Pool.calculate_tokens_out_from_tokens_in` currently does `balances = list(self.balances)` then `_upscale_array(amounts=balances, ...)`. The `_upscale_array` function mutates the list in-place with a `for` loop. With `override_state`, the same pattern works: `balances = list(override_state.balances)` → `_upscale_array(amounts=balances, ...)`. No copy issue — `list()` creates a new list each time.

### Step 8: `ArbitragePathPool` protocol — the N-token problem

**Critical gap the original plan missed**: `ArbitragePathPool` (in `types/pool_protocols.py`) requires:
- `token0` and `token1` properties
- `calculate_tokens_out_from_tokens_in(token_in, token_in_quantity, override_state)` — no `token_out` parameter
- `build_swap_amount(zero_for_one, amount_in, amount_out)`

Balancer V2 pools are N-token pools. They have `tokens` (a tuple) instead of `token0`/`token1`, and their `calculate_tokens_out_from_tokens_in` requires an explicit `token_out` parameter. They also lack `build_swap_amount()`.

`ArbitragePath._validate_pools()` and `_build_swap_vectors()` iterate over `pool.token0` and `pool.token1` to build the token chain. This will `AttributeError` on Balancer pools.

**The existing `MultiTokenSwapCalculation` protocol** was designed for exactly this case — it has `calculate_tokens_out_from_tokens_in(token_in, token_out, token_in_quantity)`. But `ArbitragePathPool` intentionally *excludes* multi-token pools from cyclic arbitrage.

**Resolution for this plan**: Implement `to_hop_state()` and `external_update()` (Steps 5–6) so that Balancer pools are compatible with the *solver infrastructure*. But they **cannot participate in `ArbitragePath`** without either:

(a) A new `MultiTokenArbitragePath` class that uses `MultiTokenSwapCalculation`, or
(b) A wrapper/adapter that binds an N-token Balancer pool to a specific (token_in, token_out) pair, exposing `token0`/`token1` for the pair and delegating `calculate_tokens_out_from_tokens_in` to the N-token pool.

**This plan implements option (b)** — a `BalancerPairView` adapter class. A separate `MultiTokenArbitragePath` is a larger design that should be its own plan.

```python
# In balancer/pools.py or a new balancer/pair_view.py:

class BalancerPairView:
    """Adapts an N-token Balancer pool to a 2-token pair view for ArbitragePath.

    Delegates swap calculations and hop state to the underlying pool,
    for a specific (token_in, token_out) pair.
    """

    def __init__(
        self,
        pool: BalancerV2Pool | BalancerV2StablePool,
        token_a: Erc20Token,
        token_b: Erc20Token,
    ) -> None:
        self._pool = pool
        self._token0 = token_a  # First token in the pair
        self._token1 = token_b  # Second token in the pair

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

    def to_hop_state(
        self,
        zero_for_one: bool,
        state_override: AbstractPoolState | None = None,
    ) -> HopType:
        # Delegate to the pool's to_hop_state with explicit pair selection.
        # No private attribute access needed — the pool's to_hop_state()
        # accepts token_in/token_out kwargs for N-token pair selection.
        token_in = self._token0 if zero_for_one else self._token1
        token_out = self._token1 if zero_for_one else self._token0
        return self._pool.to_hop_state(
            zero_for_one=zero_for_one,  # Ignored when token_in/token_out provided
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
        # Need Balancer-specific swap amounts type
        ...

    def subscribe(self, subscriber: Subscriber) -> None:
        self._pool.subscribe(subscriber)

    def unsubscribe(self, subscriber: Subscriber) -> None:
        self._pool.unsubscribe(subscriber)
```

**Design decision**: `BalancerPairView` wraps an N-token pool into a 2-token view. It does not store state — it delegates everything to the underlying pool. It satisfies `ArbitragePathPool` so it can be used in `ArbitragePath`. The view is cheap to create (no I/O) and can be created for any pair of tokens in the pool.

**Solver caveat**: Even with `BalancerPairView`, `ArbitragePath` requires an injected `Solver` whose `supports()` accepts the hop types. `SolidlyStableSolver.supports()` rejects `BalancerWeightedHop` and `BalancerStableHop` because their `PoolInvariant` values aren't in the allowed set (`{CONSTANT_PRODUCT, BOUNDED_PRODUCT, SOLIDLY_STABLE}`). For mixed Balancer/V2 paths, a new solver is needed — either extending `SolidlyStableSolver` (add `BALANCER_WEIGHTED`, `BALANCER_STABLESWAP` to its accepted set and handle them in `_simulate_mixed_path`) or creating a new `BalancedPathSolver`. This is out of scope for this plan but documented as a follow-up.

**Post-plan consideration**: A `MultiTokenArbitragePath` would be a more principled solution but requires changes to `ArbitragePath`, the solver dispatch, swap amount construction, and the subscription model. That's a separate architecture plan, not something to bundle into this one.

### Step 9: `build_swap_amount()` — encoding Balancer swaps

Both pool classes need `build_swap_amount()` for `ArbitragePathPool`. Balancer V2 swaps are executed via the Vault's `swap()` method, not via a per-pool contract call. The encoded calldata requires:
- `SingleSwap` struct: `(bytes32 poolId, uint8 swapKind, address assetIn, address assetOut, uint256 amount, bytes userData)`
- `FundManagement` struct: `(address sender, bool fromInternalBalance, address payable recipient, bool toInternalBalance)`

Create a `BalancerV2SwapAmounts` class:

```python
# In a new file balancer/swap_amounts.py or in pools.py:

@dataclass(frozen=True, slots=True)
class BalancerV2SwapAmounts(AbstractSwapAmounts):
    pool_id: bytes
    vault: ChecksumAddress
    zero_for_one: bool
    amount_in: int
    amount_out: int
    token_in: ChecksumAddress
    token_out: ChecksumAddress

    def input_amount(self) -> int:
        return self.amount_in

    def output_amount(self) -> int:
        return self.amount_out

    def encode(self, recipient: ChecksumAddress | None = None) -> EncodedCall:
        # Encode Vault.swap() call
        ...
```

### Step 10: Create `BalancerBuilder`

```python
# src/degenbot/builders/balancer_builder.py

class BalancerBuilder:
    """Builds and updates Balancer V2 pools (weighted, stable, composable).

    Owns the full I/O choreography: RPC fetch → decode → construct →
    register.

    Pool type is determined via _detect_pool_type() which probes the
    contract for characteristics (has getNormalizedWeights → weighted,
    has getAmplificationParameter → stable).
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

        # 2. Fetch pool ID
        pool_id = self._fetch_pool_id(io, pool_address, state_block)

        # 3. Fetch tokens and balances from Vault
        tokens, balances = self._fetch_vault_tokens(
            io, pool_id, chain_id, state_block, request
        )

        # 4. Fetch fee
        fee = self._fetch_swap_fee(io, pool_address, state_block)

        # 5. Detect pool type and build
        pool_type = self._detect_pool_type(io, pool_address, state_block)
        if pool_type == "weighted":
            return self._build_weighted(
                io, pool_address, pool_id, tokens, balances, fee,
                chain_id, state_block, request,
            )
        if pool_type == "stable":
            return self._build_stable(
                io, pool_address, pool_id, tokens, balances, fee,
                chain_id, state_block, request,
            )

        msg = f"Unknown Balancer pool type at {pool_address}"
        raise DegenbotValueError(message=msg)

    def _build_weighted(self, io, address, pool_id, tokens, balances,
                         fee, chain_id, state_block, request):
        # Fetch weights from getNormalizedWeights()
        weights = self._fetch_weights(io, address, state_block)

        # Detect PowVersion from bytecode
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
                        fee, chain_id, state_block, request):
        # Fetch amp from getAmplificationParameter() → (value, isUpdating, precision)
        amp = self._fetch_amp(io, address, state_block)

        # Fetch rate providers from getRateProviders()
        rate_providers = self._fetch_rate_providers(io, address, state_block)

        # Detect BPT index (token whose rate provider == address zero?
        #   OR token whose address == pool address)
        bpt_idx = self._detect_bpt_index(tokens, rate_providers, address)

        # Compute base scaling factors from token decimals
        base_sf = tuple(_compute_scaling_factor(t) for t in tokens)

        # Compute construction-time scaling factors
        # For tokens with rate providers, rate = call getRate()
        # For tokens without, rate = ONE
        rates = self._fetch_rates(io, rate_providers, state_block)
        scaling_factors = tuple(
            bsf * rate // ONE for bsf, rate in zip(base_sf, rates, strict=True)
        )

        # Detect invariant version
        # Default: V2 for 2-token pools (MetaStable), V1 for N-token with BPT (Composable)
        invariant_version = request.invariant_version or (
            INVARIANT_V2 if bpt_idx is None else INVARIANT_V1
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
            # No rate_provider at construction — uses _StaticRateProvider
            invariant_version=invariant_version,
            chain_id=chain_id,
            state_block=state_block,
        )

        self._pools.add(pool, chain_id=chain_id, pool_address=pool.address)
        ...

    def update(self, pool, *, io, block_number) -> bool:
        # Fetch current balances from Vault via getPoolTokens()
        ...
        new_balances = ...

        if pool.balances == new_balances:
            return False

        update = BalancerV2PoolExternalUpdate(
            block_number=block_number_,
            balances=new_balances,
        )
        pool.external_update(update)
        return True

    @staticmethod
    def _detect_pool_type(io, address, block_identifier) -> str:
        """Determine weighted vs stable by probing contract methods."""
        # Try getNormalizedWeights() → "weighted"
        # Try getAmplificationParameter() → "stable"
        # Future: getWrappedTokenRate() / getMainToken() → "linear"
        ...

    @staticmethod
    def _detect_bpt_index(tokens, rate_providers, pool_address) -> int | None:
        """Detect the BPT index for ComposableStablePools.

        Heuristics:
        1. If any token's address matches the pool address → that's BPT
        2. If any rate provider address matches its token → likely not BPT
        3. If any rate provider returns address(0) → that token has no rate provider
        """
        ...
```

**Linear pool detection (future)**: Linear pools expose `getWrappedTokenRate()` and `getMainToken()` instead of `getAmplificationParameter()` or `getNormalizedWeights()`. When the `BalancerV2LinearPool` class is implemented, `_detect_pool_type()` should add a third branch: try `getWrappedTokenRate()` → "linear". Adding an early `raise` with a clear message ("Linear pools not yet supported") is better than silently misclassifying them as stable. This keeps the door open for a future implementation without requiring changes to the builder skeleton.
```

### Step 11: Type resolution — add Balancer probing

Update `resolve_pool_type_by_probing()` to add a `getPoolId()` probe **after** `getReserves` fails and **before** the STABLESWAP fallback. If `getPoolId()` succeeds, the pool is Balancer. The builder then sub-dispatches to weighted or stable.

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
    return PoolTypeDescriptor(
        family=PoolFamily.STABLESWAP,  # _derive_family returns STABLESWAP for both
        variant="balancer",  # Builder will sub-dispatch
        kind=derive_kind(PoolFamily.STABLESWAP, "balancer"),
        factory=factory,
    )
```

**Problem with this approach**: `kind="balancer"` is ambiguous — the builder returns one of two different pool classes (`BalancerV2Pool` or `BalancerV2StablePool`), but the type descriptor can only carry one `kind`. The DB `kind` column needs to distinguish them.

**Resolution**: The type resolver produces a coarse descriptor (`variant="balancer"`). The builder's `_detect_pool_type()` then refines it. But the DB row should store the *concrete* kind (`"balancer_weighted"` or `"balancer_stable"`), not the coarse one. This means the builder should update the pool's `kind` in the DB after construction, or the type resolver should do a secondary probe (getNormalizedWeights vs getAmplificationParameter) to produce the fine-grained kind.

**Better approach**: Do the full probe in the type resolver:

```python
# After getPoolId succeeds:
# Probe getNormalizedWeights → variant="balancer_weighted"
# Probe getAmplificationParameter → variant="balancer_stable"
try:
    io.call(to=address, data=encode_function_calldata("getNormalizedWeights()", None))
    variant = "balancer_weighted"
except Web3Exception:
    variant = "balancer_stable"

return PoolTypeDescriptor(
    family=PoolFamily.STABLESWAP,
    variant=variant,
    kind=f"balancer_{'weighted' if variant == 'balancer_weighted' else 'stable'}",
    factory=factory,
)
```

Now `derive_kind(STABLESWAP, "balancer_weighted")` → `"balancer_weighted"`, and `derive_kind(STABLESWAP, "balancer_stable")` → `"balancer_stable"`. No collision.

### Step 12: Register builder and factories in Bot

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
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool

# Balancer V2 Weighted Pool Factory (v3)
pool_type_registry.register(
    BalancerV2Pool,
    chain_id=1,
    factory_address="0x8E9aa87E45e92bad7D5F7F9Dd794cea12F21707B",
)

# Balancer V2 Stable Pool Factory (v1)
pool_type_registry.register(
    BalancerV2StablePool,
    chain_id=1,
    factory_address="0x8519F5A4A85678E0e03395586E2E223d70E9E09B",
)

# ComposableStablePool Factory (v2)
pool_type_registry.register(
    BalancerV2StablePool,
    chain_id=1,
    factory_address="0xA8936f4824B2E6407Fc0e94133909aeF7d48e876",
)
```

### Step 13: Add fields to `BuildPoolRequest`

```python
# Balancer options
bpt_idx: int | None = None        # Override BPT index detection
invariant_version: int | None = None  # Override: INVARIANT_V1 or INVARIANT_V2
```

Note: `pool_id` already exists on `BuildPoolRequest` (added for V4). The Balancer builder can reuse it — `getPoolId()` can be skipped if the pool ID is already known.

### Design decisions

- **`PoolFamily.STABLESWAP` for all Balancer pools**: `_derive_family()` classifies both `BalancerV2Pool` and `BalancerV2StablePool` as `STABLESWAP` (they have `tokens` but not `fee_token0`). Leaning into this avoids modifying shared infrastructure. The `variant` field (`"balancer_weighted"` / `"balancer_stable"`) provides the distinguishing information.
- **Single builder, not two**: `BalancerBuilder` handles all pool types, using `_detect_pool_type()` to branch internally. Matches the `CurvePoolBuilder` pattern.
- **`PoolInvariant.BALANCER_STABLESWAP`**: New enum value distinct from `CURVE_STABLESWAP`. The solver dispatches to a different mathematical function (Balancer StableMath vs Curve invariant). `BALANCER_WEIGHTED` already exists for weighted pools.
- **`BalancerPairView` adapter**: N-token pools can't satisfy `ArbitragePathPool` (requires `token0`/`token1`). The adapter wraps an N-token pool + a chosen pair into a 2-token view that satisfies the protocol. With `token_in`/`token_out` kwargs on `to_hop_state()`, `BalancerPairView` delegates cleanly without accessing private pool internals.
- **`to_hop_state()` with `token_in`/`token_out` kwargs**: Added keyword-only parameters (default `None`) so N-token pools can specify the exact pair. When absent, `zero_for_one` falls back to `(0, 1) / (1, 0)` — fully backward-compatible. This eliminates private-attribute coupling in `BalancerPairView`, and also resolves the same TODO in `CurveStableswapPool.to_hop_state()` (which already has a comment about this ambiguity). The `PoolSimulation` protocol should be updated to include these kwargs. Multi-token basket optimization still uses `BalancerMultiTokenHop`.
- **Fetch from Vault, not the pool**: Balancer V2 stores token balances in the Vault contract. The builder must call `getPoolTokens(poolId)` on the Vault.
- **BPT index detection heuristics**: Detect by checking which token address equals the pool address (self-referencing BPT). The `BuildPoolRequest.bpt_idx` override handles edge cases.
- **Invariant version detection**: Default V2 for 2-token pools (MetaStable), V1 for N-token pools with BPT (ComposableStable). The `BuildPoolRequest.invariant_version` override handles edge cases.
- **No `CacheAwareRateProvider` in builder**: The builder creates `_StaticRateProvider` (construction-time rates). Callers needing exact matching can inject a `CacheAwareRateProvider` post-construction. This matches how `CurveDataProviderImpl` is handled by `CurvePoolBuilder`.
- **No DB support in slice 1**: First slice fetches everything from chain. DB persistence for Balancer pools can be added in a follow-up slice.
- **Full probing in type resolver**: The type resolver probes both `getPoolId()` AND (`getNormalizedWeights()` or `getAmplificationParameter()`) to produce the fine-grained `kind` string directly. This avoids the coarse→fine retroactive update problem.

## Files Involved

**Primary:**
- `src/degenbot/builders/balancer_builder.py` — new file; builder for all Balancer pool types
- `src/degenbot/balancer/pools.py` — implement `to_hop_state()`, `external_update()`, fix `override_state`, add `variant` class attribute
- `src/degenbot/balancer/stable_pools.py` — implement `to_hop_state()`, `external_update()`, fix `override_state`, fix `tokens` return type, add `variant` class attribute
- `src/degenbot/balancer/types.py` — add `BalancerV2PoolExternalUpdate` dataclass + `BalancerV2PoolStateUpdated` message

**Secondary:**
- `src/degenbot/bot.py` — register `BalancerBuilder` for both pool classes
- `src/degenbot/balancer/__init__.py` — self-register factories in `pool_type_registry`
- `src/degenbot/balancer/deployments.py` — add factory addresses
- `src/degenbot/builders/type_resolution.py` — add `getPoolId()` + `getNormalizedWeights()` / `getAmplificationParameter()` probing
- `src/degenbot/builders/request.py` — add `bpt_idx` and `invariant_version` fields
- `src/degenbot/types/hop_types.py` — add `BalancerStableHop` dataclass, `PoolInvariant.BALANCER_STABLESWAP`, update `HopType` union
- `src/degenbot/arbitrage/optimizers/hop_types.py` — add `has_balancer_stableswap` property to `SolveInput`
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — add `token_in`/`token_out` kwargs to `to_hop_state()` (resolves existing TODO)
- `src/degenbot/balancer/pair_view.py` — new file; `BalancerPairView` adapter for `ArbitragePathPool`
- `src/degenbot/balancer/swap_amounts.py` — new file; `BalancerV2SwapAmounts` and `build_swap_amount()`

**No change needed:**
- `src/degenbot/types/hop_types.py` — `BalancerWeightedHop` and `BalancerMultiTokenHop` already exist
- `src/degenbot/balancer/libraries/` — math libraries already work
- `src/degenbot/builders/context.py` — `BuilderContext` already has everything `BalancerBuilder` needs
- `src/degenbot/builders/protocol.py` — `PoolBuilder` protocol already satisfied
- `src/degenbot/types/pool_protocols.py` — add `token_in`/`token_out` kwargs to `PoolSimulation.to_hop_state()`

## Implementation Order

### Slice 1: Fix bugs and add `variant` class attributes

1. Fix `BalancerV2StablePool.tokens` return type (`tuple[tuple[...]]` → `tuple[...]`)
2. Add `variant: ClassVar[str | None] = "balancer_weighted"` to `BalancerV2Pool`
3. Add `variant: ClassVar[str | None] = "balancer_stable"` to `BalancerV2StablePool`
4. Add `BalancerV2PoolExternalUpdate` dataclass to `balancer/types.py`
5. Add `BalancerV2PoolStateUpdated` PoolStateMessage subclass to `balancer/types.py`
6. Run: `just test-python` — expect all green

### Slice 2: `external_update()` and `to_hop_state()` on `BalancerV2Pool`

1. Implement `external_update()` on `BalancerV2Pool` — update state, notify subscribers with `BalancerV2PoolStateUpdated`
2. Implement `to_hop_state()` on `BalancerV2Pool` — return `BalancerWeightedHop`
3. Fix `calculate_tokens_out_from_tokens_in` and `calculate_tokens_in_from_tokens_out` to handle `override_state`
4. Write tests: construct pool, call `to_hop_state()`, verify hop fields; call `external_update()`, verify state changes and subscriber notification
5. Run: `just test-python` — expect all green

### Slice 3: `external_update()`, `to_hop_state()`, and `to_hop_state()` kwargs

1. Add `PoolInvariant.BALANCER_STABLESWAP` to `hop_types.py`
2. Add `BalancerStableHop` dataclass to `hop_types.py`
3. Update `HopType` union to include `BalancerStableHop`
4. Add `has_balancer_stableswap` property to `SolveInput`
5. Add `token_in: Erc20Token | None = None` and `token_out: Erc20Token | None = None` keyword-only kwargs to `PoolSimulation.to_hop_state()` protocol in `pool_protocols.py`
6. Implement `external_update()` on `BalancerV2StablePool`
7. Implement `to_hop_state()` on `BalancerV2StablePool` — accept `token_in`/`token_out` kwargs, return `BalancerStableHop`
8. Update `BalancerV2Pool.to_hop_state()` (from Slice 2) to accept `token_in`/`token_out` kwargs — when provided, they override `zero_for_one` for index selection
9. Update `CurveStableswapPool.to_hop_state()` to accept `token_in`/`token_out` kwargs (resolves existing TODO comment about N-token ambiguity)
10. Fix `calculate_tokens_out_from_tokens_in` and `calculate_tokens_in_from_tokens_out` to handle `override_state` on both pool classes
11. Write tests: construct pool, call `to_hop_state(zero_for_one=True)` and `to_hop_state(zero_for_one=True, token_in=..., token_out=...)`, verify hop fields; call `external_update()`, verify state changes
12. Run: `just test-python` — expect all green

### Slice 4: `BalancerPairView` and swap amount encoding

1. Create `balancer/pair_view.py` with `BalancerPairView` adapter
2. Create `balancer/swap_amounts.py` with `BalancerV2SwapAmounts`
3. Implement `build_swap_amount()` on both pool classes (returns `BalancerV2SwapAmounts`)
4. Write tests for `BalancerPairView` — verify `ArbitragePathPool` protocol satisfaction
5. Run: `just test-python` — expect all green

### Slice 5: Create `BalancerBuilder`

1. Create `src/degenbot/builders/balancer_builder.py`
2. Implement `_detect_pool_type()` — probe `getNormalizedWeights()` vs `getAmplificationParameter()`
3. Implement `_fetch_pool_id()`, `_fetch_vault_tokens()`, `_fetch_swap_fee()`
4. Implement `_build_weighted()` — fetch pool ID, vault, tokens+balances, fee, weights, PowVersion
5. Implement `_build_stable()` — fetch pool ID, vault, tokens+balances, fee, amp, rate providers, BPT index, invariant version, base scaling factors
6. Implement `update()` — fetch balances from Vault, apply via `external_update()`
7. Add `bpt_idx` and `invariant_version` fields to `BuildPoolRequest`
8. Add factory addresses to `deployments.py`
9. Write tests with `FakePoolIO` returning canned Balancer contract responses
10. Run: `just test-python` — expect all green

### Slice 6: Register builder and type resolution

1. Create `BalancerBuilder(ctx)` in `Bot.__init__`, register for both pool classes
2. Add `getPoolId()` + `getNormalizedWeights()` / `getAmplificationParameter()` probing to `type_resolution.py`
3. Self-register factories in `balancer/__init__.py` via `pool_type_registry.register()`
4. Same for async type resolution
5. Run: `just test-python` — expect all green

### Slice 7: Validate and clean up

1. Run `just lint` + `just test-all`
2. Integration test: `bot.build_pool("0x...BalancerPoolAddress...")` returns the correct class
3. Integration test: `pool.to_hop_state(zero_for_one=True)` returns the correct hop type
4. Update `balancer/CONTEXT.md` — add builder, pair view, swap amounts
5. Update `AGENTS.md` builder table
6. Remove empty `balancer/managers.py`
7. Run: `just test-all` — expect all green

## Testing

### Per-slice test runs

Each slice runs `just test-python`.

### New unit tests

```python
# tests/balancer/test_pool_methods.py


def test_weighted_to_hop_state():
    """BalancerV2Pool.to_hop_state returns BalancerWeightedHop."""
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


def test_weighted_external_update():
    """BalancerV2Pool.external_update updates state and notifies subscribers."""
    pool = BalancerV2Pool(...)
    update = BalancerV2PoolExternalUpdate(
        block_number=100,
        balances=(2000, 3000000),
    )
    pool.external_update(update)
    assert pool.balances == (2000, 3000000)


def test_stable_external_update():
    """BalancerV2StablePool.external_update updates state."""
    pool = BalancerV2StablePool(...)
    update = BalancerV2PoolExternalUpdate(
        block_number=100,
        balances=(200 * 10**18, 400 * 10**18),
    )
    pool.external_update(update)
    assert pool.balances == (200 * 10**18, 400 * 10**18)


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


# tests/builders/test_balancer_builder.py


def test_balancer_builder_detects_weighted():
    """Builder probes detect a weighted pool."""

def test_balancer_builder_detects_stable():
    """Builder probes detect a stable pool."""

def test_balancer_builder_builds_weighted():
    """BalancerBuilder constructs a BalancerV2Pool from chain data."""

def test_balancer_builder_builds_stable():
    """BalancerBuilder constructs a BalancerV2StablePool from chain data."""

def test_balancer_builder_update():
    """BalancerBuilder.update fetches new balances from Vault."""
```

### Integration tests

Integration tests require a live RPC endpoint with known Balancer pools. These should be marked as fork tests (not run in CI) and tested manually. Use the pools already tested in `tests/balancer/test_pools.py` and `tests/balancer/test_stable_pools.py`:

- Weighted: `0x5c6EeA3e4c60650E0156e8B2579267B04c6f227E` (WBTC/WETH 80/20)
- MetaStable: `0x32296969Ef14EB0c6d29669C550D4a0449130230` (wstETH/WETH)
- ComposableStable: `0x53BC3cBa3832ebeCBFa002c12023F8ab1AA3a3a0` (TUSD BSP)

## Benefits

- **Leverage**: `Bot.build_pool()` is the single entry point for all pool families, including Balancer.
- **Locality**: Balancer I/O concentrates in `BalancerBuilder` — matches all other pool families.
- **Depth**: Both pool classes become deep modules — I/O-free construction, state updates via `external_update()`, solver-compatible via `to_hop_state()`.
- **Deletion test satisfied**: Without a builder, Balancer pools are excluded from the architecture. Adding the builder brings them into the fold.

## Risks

- **Vault contract calls**: Balancer V2 stores balances in the Vault, not the pool. The builder must call `getPoolTokens(poolId)` on the Vault. This requires knowing the Vault address. Mitigation: centralized in `deployments.py`, well-known per chain.
- **N-token → 2-token impedance**: `ArbitragePath` assumes 2-token pools with `token0`/`token1`. Mitigation: `BalancerPairView` adapter. A proper `MultiTokenArbitragePath` is a separate plan.
- **BPT index detection heuristics**: Detecting which token is BPT may fail for unusual pool configurations. Mitigation: `BuildPoolRequest.bpt_idx` override.
- **Invariant version detection**: Defaults may be wrong for edge cases. Mitigation: `BuildPoolRequest.invariant_version` override.
- **Rate provider complexity**: Builder uses `_StaticRateProvider` at construction. Callers needing exact matching must inject `CacheAwareRateProvider` post-construction. Matches `CurvePoolBuilder` pattern.
- **No async builder in slice 1**: Sync-only initially. `AsyncBalancerBuilder` is a straightforward translation.
- **`PoolInvariant.BALANCER_STABLESWAP` and Rust**: Verified — the Rust code (`rust/`) does not reference `PoolInvariant` at all. Only Python solvers use it. Safe to add the new enum value without Rust changes.
- **Mixed-path solver gap**: `SolidlyStableSolver.supports()` rejects paths containing `BalancerWeightedHop` or `BalancerStableHop` (their `PoolInvariant` values aren't in the accepted set). The simulation functions `_simulate_mixed_path` and `_simulate_mixed_path_int` also don't handle these hop types — they fall through to `return 0`. For mixed Balancer/V2 paths, either (a) extend `SolidlyStableSolver` to accept and handle Balancer hops (using `swap_fn` for integer-evaluation, just like Solidly and Curve hops), or (b) create a new `BalancedPathSolver`. This is out of scope for this plan. `BalancerPairView` + `ArbitragePath` will work with a solver that *does* accept the hop types (e.g., a future `BalancedPathSolver`), but not with the existing `SolidlyStableSolver`.
- **BalancerWeightedHop in mixed paths**: Same issue — `BalancerWeightedHop` has `reserve_in`, `reserve_out`, `fee`, `weight_in`, `weight_out` but no `swap_fn`. The simulation functions can't evaluate it without either a `swap_fn` or a closed-form expression. For 2-token weighted pools, the WeightedMath formula could be used directly. For N-token, only the `BalancerMultiTokenSolver` handles them via `BalancerMultiTokenHop`. This is a solver-extension concern, not a builder concern.
- **`to_hop_state()` pair selection solved**: `token_in`/`token_out` kwargs on `to_hop_state()` address the hardcoded `(0,1)` limitation. No longer a risk — resolved in this plan.
- **`derive_kind` collision**: `WEIGHTED + "balancer"` and `STABLESWAP + "balancer"` both produce kind `"balancer"`. Resolved by using `STABLESWAP` + distinct variants (`"balancer_weighted"`, `"balancer_stable"`) producing kinds `"balancer_weighted"` and `"balancer_stable"`.

## Relationship to Other Plans

- **Plan 071** (Curve Hop State Pair Selection): Direct prerequisite — `to_hop_state()` kwargs must be on the `PoolSimulation` protocol before `BalancerV2Pool`, `BalancerV2StablePool`, and `BalancerPairView` can use them. Plan 070's Slice 3 should be implemented after Plan 071's Slice 1.
- **Plan 014** (Async REPL): Orthogonal — different pool family. Async builder follows same pattern as other async builders.
- **Plan 068** (Absorb Curve on-chain cache): Orthogonal — Curve-specific.
- **Plan 069** (Remove DyCalculation closures): Orthogonal — Curve-specific.
- **Future: Mixed-path solver for Balancer hops**: The `SolidlyStableSolver` pattern (golden-section / Newton with `swap_fn`-backed integer evaluation) can be extended to accept `BALANCER_WEIGHTED` and `BALANCER_STABLESWAP` invariant types. `BalancerWeightedHop` would need a `swap_fn` or a closed-form evaluation added. `BalancerStableHop` already carries `swap_fn`. This should be a separate plan.
- **Future: MultiTokenArbitragePath**: A more principled solution than `BalancerPairView` for N-token pools in cyclic arbitrage. Requires changes to `ArbitragePath`, solver dispatch, swap amount construction, and the subscription model. This should be a separate architecture plan.
- **Future: Vault event subscription**: For push-based state updates (currently only pull via `Bot.update()`), subscribe to Vault `Swap` events filtered by `poolId`. Requires extending `LogListener` with Balancer event decoders. This should be a separate plan.

## Status

[ ] Slice 1: Fix bugs, add variant class attributes, add external update types
[ ] Slice 2: `external_update()` and `to_hop_state()` on `BalancerV2Pool`
[ ] Slice 3: `external_update()`, `to_hop_state()`, and `to_hop_state()` kwargs
[ ] Slice 4: `BalancerPairView`, swap amounts, `build_swap_amount()`
[ ] Slice 5: Create `BalancerBuilder`
[ ] Slice 6: Register builder, type resolution, factory self-registration
[ ] Slice 7: Validate and clean up
