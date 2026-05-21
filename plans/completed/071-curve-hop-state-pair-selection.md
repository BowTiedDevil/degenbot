# Plan 071: Add `token_in`/`token_out` kwargs to `to_hop_state()`

## Overview

Add `token_in`/`token_out` keyword-only arguments to the `to_hop_state()` method on `ArbitrageCapablePool` and `ArbitragePathPool` protocols, and the Curve + 2-token pool classes. When **both** are provided, they override `zero_for_one` for pair selection in N-token pools (Balancer, Curve). When absent, `zero_for_one` falls back to `(0, 1) / (1, 0)` — fully backward-compatible. This eliminates the hardcoded pair limitation in `CurveStableswapPool.to_hop_state()` (which has a code comment acknowledging the N-token ambiguity) and enables Plan 070's `BalancerPairView` to delegate pair selection through the pool's own `to_hop_state()` instead of accessing private pool internals.

## Problem

### Deletion test

If you delete the `token_in`/`token_out` kwargs from `to_hop_state()`, N-token pools revert to selecting only the `(0, 1)` pair. Callers that need a different pair must either (a) reach into private pool internals to compute indices themselves (coupling), or (b) create separate adapter classes that duplicate hop-construction logic (duplication). Both are worse than adding two keyword-only arguments.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|-------------|
| `to_hop_state()` hardcoded to `(0, 1)` | `curve/curve_stableswap_liquidity_pool.py:1124` — code comment: *"For N-token pools, this is ambiguous"* | 3-token and metapool Curve pools can only produce hops for the first two tokens. Caller cannot select pair (0, 2) or (1, 2). |
| `BalancerPairView` would need pool-private access | Plan 070 `BalancerPairView.to_hop_state()` — without kwargs, must reach into `_resolve_scaling_factors()`, `_upscale_balances()`, `_compute_invariant()`, `_skip_bpt_index()`, `_non_bpt_indices` | Private-attribute coupling breaks when pool internals change. Violates encapsulation. |
| No protocol-level support for pair selection | `types/pool_protocols.py` — `ArbitrageCapablePool.to_hop_state()` and `ArbitragePathPool.to_hop_state()` only accept `zero_for_one` and `state_override` | Code working with `ArbitrageCapablePool`-typed variables (which Curve pools satisfy) can't pass pair info. Must downcast to the concrete pool type. |
| Balancer pools' `to_hop_state()` will need pair selection when implemented | `balancer/pools.py:248`, `balancer/stable_pools.py:527` — currently raises `NotImplementedError` | Plan 070 will implement `to_hop_state()` on both Balancer pool classes. This plan defines the pair-selection design (both-or-neither `token_in`/`token_out`) so Plan 070 can adopt it directly. |

### Actual consumers

The callers of `to_hop_state()` today are:

1. **`ArbitragePath.__init__()`, `_resolve_state_overrides()`, `_refresh_hop_states()`, `notify()`** — all pass `zero_for_one` only. These callers work with `ArbitragePathPool`-typed pools (which have `token0`/`token1`). They will **never** pass `token_in`/`token_out` because the `ArbitragePath` token-chain resolution already determines the correct `zero_for_one` from `pool.token0`/`pool.token1`.

2. **`_uniswap_lp_cycle.py`** (legacy) — same pattern, `zero_for_one` computed from `pool.token0` alignment. Will never use `token_in`/`token_out`.

3. **Plan 070's `BalancerPairView.to_hop_state()`** — the primary future consumer. It wraps an N-token Balancer pool into a 2-token view for `ArbitragePath`, and will delegate via `pool.to_hop_state(zero_for_one=..., token_in=..., token_out=...)`. Note: Plan 070's `BalancerPairView.__init__` declares `pool: BalancerV2Pool | BalancerV2StablePool` (a concrete union, not `ArbitrageCapablePool`), so it can call the concrete pool's `to_hop_state()` directly regardless of whether the protocol declares the kwargs. The protocol-level kwargs benefit other future callers holding `ArbitrageCapablePool`-typed references.

4. **Future `MultiTokenArbitragePath`** or any caller working with `ArbitrageCapablePool`-typed N-token pools that needs to specify an arbitrary pair.

The kwargs are for consumers 3–4, not 1–2. No existing call site will change.

## Solution

### Step 1: Add `token_in`/`token_out` kwargs to `ArbitrageCapablePool` and `ArbitragePathPool` protocols

`to_hop_state()` lives on two protocols in `pool_protocols.py`:

- `ArbitrageCapablePool` (line 213) — the protocol Curve pools satisfy (has `to_hop_state` + `extract_fee`)
- `ArbitragePathPool` (line 265) — the protocol for `ArbitragePath` participants (also has `token0`/`token1`, `calculate_tokens_out_from_tokens_in`, `build_swap_amount`)

Both independently declare `to_hop_state()` (there is no inheritance relationship between them; both extend `PoolSimulation`, which does not have `to_hop_state()`). Both need the kwargs so that:
- Any code holding an `ArbitrageCapablePool`-typed variable (e.g., a future `MultiTokenArbitragePath`) can pass pair selection without downcasting.
- Any code holding an `ArbitragePathPool`-typed variable sees the same signature for consistency.
- Concrete pool classes that satisfy both protocols need only one `to_hop_state()` method, which must match both protocol signatures. Since the signatures are identical after this change, there is no conflict. If the two protocols ever diverged on `to_hop_state()` kwargs, concrete classes satisfying both would break — so the two protocol declarations must stay in sync.

`PoolSimulation` does **not** have `to_hop_state()` — it only declares `simulate_swap`, `subscribe`, `unsubscribe`. No change needed there.

```python
# In pool_protocols.py — ArbitrageCapablePool:
def to_hop_state(
    self,
    zero_for_one: bool,  # noqa: FBT001
    state_override: AbstractPoolState | None = None,
    *,
    token_in: Erc20Token | None = None,
    token_out: Erc20Token | None = None,
) -> HopType: ...

# In pool_protocols.py — ArbitragePathPool:
def to_hop_state(
    self,
    zero_for_one: bool,  # noqa: FBT001
    state_override: AbstractPoolState | None = None,
    *,
    token_in: Erc20Token | None = None,
    token_out: Erc20Token | None = None,
) -> HopType: ...
```

Keyword-only with `None` defaults — all existing call sites are unaffected.

### Step 2: Update `CurveStableswapPool.to_hop_state()`

```python
def to_hop_state(
    self,
    zero_for_one: bool,
    state_override: CurveStableswapPoolState | None = None,
    *,
    token_in: Erc20Token | None = None,
    token_out: Erc20Token | None = None,
) -> HopType:
    state = state_override or self.state
    balances = state.balances

    if token_in is not None and token_out is not None:
        try:
            i = self.tokens.index(token_in)
        except ValueError:
            msg = f"token_in ({token_in}) is not a top-level pool token"
            raise DegenbotValueError(msg) from None
        try:
            j = self.tokens.index(token_out)
        except ValueError:
            msg = f"token_out ({token_out}) is not a top-level pool token"
            raise DegenbotValueError(msg) from None
    elif token_in is not None or token_out is not None:
        msg = "token_in and token_out must both be provided, or both omitted"
        raise DegenbotValueError(msg)
    elif zero_for_one:
        i, j = 0, 1
    else:
        i, j = 1, 0

    def swap_fn(dx: int) -> int:
        return self.get_dy(i=i, j=j, dx=dx, override_state=state_override)

    return CurveStableswapHop(
        reserve_in=balances[i],
        reserve_out=balances[j],
        fee=Fraction(self.fee, self.FEE_DENOMINATOR),
        curve_a=self.a_coefficient,
        curve_n_coins=len(self._tokens),
        curve_d=0,
        token_index_in=i,
        token_index_out=j,
        precisions=self.precision_multipliers,
        swap_fn=swap_fn,
        invariant=PoolInvariant.CURVE_STABLESWAP,
    )
```

Key design points:

1. **Uses `self.tokens` (public property from `StableswapPoolState` mixin) instead of `self._tokens`** for index resolution. Both reference the same tuple, but `self.tokens` is the public API.

2. **Requires both or neither** — if only `token_in` or only `token_out` is provided, raises `DegenbotValueError`. Silently ignoring a partially-specified pair is a footgun (a caller who passes `token_in` alone might expect the pool to infer `token_out` from direction, but the inference is fragile for N-token pools).

3. **Explicit ValueError catch with clear message** — if a caller passes a base-pool underlying token (which exists in `self.tokens_underlying` but not `self.tokens`), they get a clear error rather than a bare `ValueError: token is not in tuple`.

4. **Remove the existing code comment** about N-token ambiguity — the kwargs resolve it.

**Metapool consideration**: For metapools, `self.tokens` contains only the top-level tokens (e.g., `[LP_TOKEN, USDC]`). Underlying base-pool tokens are in `self.tokens_underlying`. A swap from a metapool token to a base-pool token uses a different `i, j` indexing scheme handled inside `get_dy()` itself (via `_resolve_metapool_inputs_via_io`). The `token_in`/`token_out` kwargs resolve against `self.tokens` only — they select the top-level pair. Metapool-underlying swaps are out of scope for `to_hop_state()` and should use `get_dy()` directly. This matches the existing behavior where `zero_for_one` also only selects from top-level tokens.

**Why not resolve against `self.tokens_underlying` too?** Metapool index resolution is complex — calling `get_dy()` for a metapool-underlying swap performs I/O via `_resolve_metapool_inputs_via_io` to bridge two separate indexing schemes. `to_hop_state()` should not silently trigger I/O or conflate the two schemes. The explicit error message guides callers to `get_dy()` for that case.

### Step 3: Update all other concrete `to_hop_state()` implementations

All 2-token pool implementations (UniswapV2Pool, UniswapV3Pool, UniswapV4Pool, AerodromeV2Pool, CamelotLiquidityPool) accept the new kwargs but ignore them — their `zero_for_one` dispatch is unambiguous because they only have `token0` and `token1`.

These kwargs exist for **type-checker compliance** (mypy/pyright would flag a signature mismatch between the protocol and the concrete class), not runtime correctness. `@runtime_checkable` protocols only check method existence, so omitting the kwargs wouldn't break protocol satisfaction at runtime — but it would produce type-checker warnings.

```python
# UniswapV2Pool, V3, V4, Aerodrome, Camelot — add kwargs, no logic change:
def to_hop_state(
    self,
    zero_for_one: bool,
    state_override: PoolState | None = None,
    *,
    token_in: Erc20Token | None = None,   # Type-checker compliance; ignored
    token_out: Erc20Token | None = None,  # Type-checker compliance; ignored
) -> HopType:
    # token_in/token_out are unused — 2-token pools determine pair from zero_for_one.
    # Callers should ensure these match pool.token0/pool.token1 if provided.
    state = state_override or self.state
    ...
```

No logic change in these classes. The inline comment warns that the kwargs are unused and that callers are responsible for consistency.

### Step 4: Balancer pool `to_hop_state()` — deferred to Plan 070

Balancer pool classes are not modified by this plan. Plan 070 will implement `to_hop_state()` on both `BalancerV2Pool` and `BalancerV2StablePool`, adopting the same pair-selection design:

- If both `token_in` and `token_out` provided → use `self.tokens.index()`
- If only one provided → raise `DegenbotValueError`
- If neither provided → fall back to `zero_for_one`

**Why defer?** Both Balancer pool `to_hop_state()` methods unconditionally raise `NotImplementedError`, so no code path can reach a signature mismatch at runtime. Adding kwargs to a `NotImplementedError` stub now, only for Plan 070 to rewrite the entire method shortly after, creates split ownership for no benefit. Plan 070 adds the kwargs and the body in one change.

A static-analysis gap exists between this plan and Plan 070 where the Balancer pool classes don't satisfy the updated `ArbitrageCapablePool` signature. This resolves automatically when Plan 070 ships.

**Note for Plan 070**: `BalancerV2StablePool.tokens` currently has an incorrect return type annotation (`tuple[tuple[Erc20Token, ...] | tuple[()]]` instead of `tuple[Erc20Token, ...]`). Plan 070's Slice 1 fixes this. The `self.tokens.index()` calls in `to_hop_state()` work at runtime regardless of the annotation, but the correct type is needed for type checking.

### Step 5: Simplify `BalancerPairView.to_hop_state()` (Plan 070)

Documented here for design coherence; implemented by Plan 070.

With `token_in`/`token_out` on the concrete pool classes' `to_hop_state()`, `BalancerPairView` delegates cleanly without accessing private pool internals:

```python
# Plan 070's BalancerPairView:
def to_hop_state(self, zero_for_one, state_override=None):
    token_in = self._token0 if zero_for_one else self._token1
    token_out = self._token1 if zero_for_one else self._token0
    return self._pool.to_hop_state(
        zero_for_one=zero_for_one,
        state_override=state_override,
        token_in=token_in,
        token_out=token_out,
    )
```

`BalancerPairView.__init__` declares `pool: BalancerV2Pool | BalancerV2StablePool` (a concrete union type), not `ArbitrageCapablePool`. It can call `pool.to_hop_state(token_in=..., token_out=...)` directly on the concrete type — the protocol-level kwargs are not required for this delegation. The protocol-level kwargs benefit other callers holding `ArbitrageCapablePool`-typed references (consumer 4 in the "Actual consumers" section).

### Design decisions

- **Keyword-only with `None` defaults**: Fully backward-compatible. All existing call sites pass only `zero_for_one` and optionally `state_override`. The kwargs are opt-in for N-token pool callers.

- **Both-or-neither for `token_in`/`token_out`**: When only one is provided, the method raises `DegenbotValueError`. Partial specification is ambiguous — a caller who passes `token_in` alone might expect the pool to infer `token_out` from direction, but the inference is fragile for N-token pools (there may be N−1 candidates for `token_out`). Requiring both is explicit and unambiguous.

- **`token_in`/`token_out` override `zero_for_one`**: When both are provided, they determine `i, j` directly. The `zero_for_one` parameter is still required (not made optional) for backward compatibility and because `ArbitragePath` uses it exclusively.

- **Explicit error for non-top-level tokens**: When `token_in` or `token_out` is not found in the pool's `tokens`, raise `DegenbotValueError` with a clear message. This avoids the unhelpful bare `ValueError` from `tuple.index()`.

- **Target protocols are `ArbitrageCapablePool` and `ArbitragePathPool`**: Both independently declare `to_hop_state()`. Both need the kwargs for consistency (a concrete class satisfying both protocols has one `to_hop_state()` method that must match both signatures). `ArbitrageCapablePool` provides the richer benefit — it's the type N-token pool consumers hold. `PoolSimulation` does not have `to_hop_state()` and is not modified.

- **2-token pools accept but ignore the kwargs**: V2, V3, V4, Aerodrome, Camelot pools have unambiguous `token0`/`token1` pairs. The kwargs exist for type-checker compliance but have no effect. Validation (checking that provided tokens match `token0`/`token1`) is not added — it would couple 2-token pools to the `token_in`/`token_out` semantics they don't need. An inline comment instead warns callers that the kwargs are unused.

- **Metapool scope**: `token_in`/`token_out` resolve against `self.tokens` (top-level tokens only). Base-pool underlying token swaps are not supported through `to_hop_state()` — they use `get_dy()` directly. This matches existing `zero_for_one` behavior. Extending `to_hop_state()` for metapool-underlying pair selection would require I/O and cross-scheme index resolution that doesn't belong in a lightweight hop-state accessor.

- **Not adding `token_index_in`/`token_index_out` int kwargs**: Callers work with `Erc20Token` objects, not indices. Index resolution is the pool's responsibility. This avoids index-scheme mismatches between top-level and underlying tokens in metapools.

- **Uses `self.tokens` (public), not `self._tokens` (private)** for index resolution. Both reference the same tuple, but the public property is the stable API.

- **Two protocols must stay in sync**: Since `ArbitrageCapablePool` and `ArbitragePathPool` both independently declare `to_hop_state()`, and concrete classes satisfy both, the kwargs on both protocols must remain identical. If they ever diverged, concrete implementations would be unable to satisfy both simultaneously.

## Files Involved

**Primary:**
- `src/degenbot/types/pool_protocols.py` — add `token_in`/`token_out` kwargs to `ArbitrageCapablePool.to_hop_state()` and `ArbitragePathPool.to_hop_state()`
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — implement `token_in`/`token_out` pair selection, remove N-token ambiguity comment

**Secondary:**
- `src/degenbot/uniswap/v2_liquidity_pool.py` — add kwargs (ignored, type-checker compliance)
- `src/degenbot/uniswap/v3_liquidity_pool.py` — add kwargs (ignored, type-checker compliance)
- `src/degenbot/uniswap/v4_liquidity_pool.py` — add kwargs (ignored, type-checker compliance)
- `src/degenbot/aerodrome/pools.py` — add kwargs (ignored, type-checker compliance)
- `src/degenbot/camelot/pools.py` — add kwargs (ignored, type-checker compliance)

**Test doubles (updated in Slice 1 alongside the Curve pool):**
- `tests/arbitrage/fake_curve_pool.py` — `FakeCurveStableswapPool.to_hop_state()` must accept the kwargs for Slice 1 tests to pass
- `tests/types/test_pool_protocols.py` — `FakeArbitragePool.to_hop_state()` must accept the kwargs
- `tests/arbitrage/mock_pools.py` — mock pool `to_hop_state()` must accept the kwargs
- `tests/arbitrage/test_path/conftest.py` — four test double `to_hop_state()` methods must accept the kwargs

**No change needed:**
- `src/degenbot/arbitrage/path/arbitrage_path.py` — all call sites use `zero_for_one` only, unaffected
- `src/degenbot/arbitrage/_legacy/` — all call sites use `zero_for_one` only, unaffected
- `src/degenbot/arbitrage/optimizers/` — internal `_mobius_math.py` `to_hop_state()` is on `V3TickRangeHop`, not the pool protocol
- `src/degenbot/types/pool_protocols.py` — `PoolSimulation` does not have `to_hop_state()`; not modified

## Implementation Order

### Slice 1: Protocols, Curve pool, and test doubles

1. Add `token_in: Erc20Token | None = None` and `token_out: Erc20Token | None = None` keyword-only kwargs to `ArbitrageCapablePool.to_hop_state()` and `ArbitragePathPool.to_hop_state()` in `pool_protocols.py`
2. Update `CurveStableswapPool.to_hop_state()` — accept kwargs, implement both-or-neither validation, use `self.tokens.index()` for pair selection, replace N-token ambiguity comment
3. Update `FakeCurveStableswapPool.to_hop_state()` in `tests/arbitrage/fake_curve_pool.py` — add the kwargs with the same both-or-neither logic (the test double must accept `token_in`/`token_out` so that Slice 1 tests can use it)
4. Update remaining test doubles in `tests/types/test_pool_protocols.py`, `tests/arbitrage/mock_pools.py`, and `tests/arbitrage/test_path/conftest.py` — add kwargs to `to_hop_state()` signatures (these don't need both-or-neither logic since they're 2-token fakes; the kwargs are just accepted and ignored)
5. Write tests: verify `to_hop_state` with `token_in`/`token_out` selects the correct pair, verify both-or-neither validation, verify explicit error for non-top-level tokens
6. Run: `just test-python` — expect all green

### Slice 2: 2-token pool classes

1. Add kwargs to `UniswapV2Pool.to_hop_state()` (ignored, with inline comment)
2. Add kwargs to `UniswapV3Pool.to_hop_state()` (ignored, with inline comment)
3. Add kwargs to `UniswapV4Pool.to_hop_state()` (ignored, with inline comment)
4. Add kwargs to `AerodromeV2Pool.to_hop_state()` (ignored, with inline comment)
5. Add kwargs to `CamelotLiquidityPool.to_hop_state()` (ignored, with inline comment)
6. Run: `just test-python` — expect all green

Balancer pool classes are not modified — Plan 070 will add the kwargs alongside the `to_hop_state()` implementation.

### Slice 3: Validate and clean up

1. Run `just lint` + `just test-all`
2. Verify all existing `to_hop_state()` call sites still work (they only pass `zero_for_one`)
3. Update `curve/CONTEXT.md` if terminology changed
4. Run: `just test-all` — expect all green

## Testing

### Per-slice test runs

Each slice runs `just test-python`.

### New unit tests

Tests use `FakeCurveStableswapPool` from `tests/arbitrage/fake_curve_pool.py` (updated to accept the kwargs in Slice 1, step 3), because constructing a real `CurveStableswapPool` requires a full data provider setup. The `FakeCurveStableswapPool` already has `to_hop_state()` — the update extends it with the new kwargs.

Note: `FakeCurveStableswapPool.tokens` returns `tuple[FakeToken, ...]`, not `tuple[Erc20Token, ...]`. `FakeToken` has `__eq__` and `__hash__` based on address, making `tuple.index()` work correctly at runtime. The type mismatch exists only in annotations and does not affect test correctness.

```python
# tests/curve/test_to_hop_state_pair_selection.py
from tests.arbitrage.fake_curve_pool import FakeCurveStableswapPool
from tests.fakes.tokens import FakeToken


def _make_2coin_pool() -> FakeCurveStableswapPool:
    """2-coin pool: USDC/USDT (both 6 decimals)."""
    usdc = FakeToken(address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", decimals=6, symbol="USDC")
    usdt = FakeToken(address="0xdAC17F958D2ee523a2206206994597C13D831ec7", decimals=6, symbol="USDT")
    return FakeCurveStableswapPool(
        tokens=(usdc, usdt),
        balances=(10_000_000 * 10**6, 10_000_000 * 10**6),
        a_coefficient=1000,
        fee=4_000_000,
    )


def _make_3coin_pool() -> FakeCurveStableswapPool:
    """3-coin pool: DAI/USDC/USDT."""
    dai = FakeToken(address="0x6B175474E89094C44Da98b954EedeAC495271d0F", decimals=18, symbol="DAI")
    usdc = FakeToken(address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", decimals=6, symbol="USDC")
    usdt = FakeToken(address="0xdAC17F958D2ee523a2206206994597C13D831ec7", decimals=6, symbol="USDT")
    return FakeCurveStableswapPool(
        tokens=(dai, usdc, usdt),
        balances=(5_000_000 * 10**18, 5_000_000 * 10**6, 5_000_000 * 10**6),
        a_coefficient=1000,
        fee=4_000_000,
    )


def test_to_hop_state_default_pair():
    """to_hop_state without token_in/token_out uses (0, 1)."""
    pool = _make_2coin_pool()
    hop = pool.to_hop_state(zero_for_one=True)
    assert hop.token_index_in == 0
    assert hop.token_index_out == 1


def test_to_hop_state_explicit_pair():
    """to_hop_state with token_in/token_out selects the specified pair."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=True,
        token_in=pool.tokens[0],
        token_out=pool.tokens[2],
    )
    assert hop.token_index_in == 0
    assert hop.token_index_out == 2


def test_to_hop_state_explicit_reverse_pair():
    """to_hop_state with reversed token_in/token_out."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=False,
        token_in=pool.tokens[2],
        token_out=pool.tokens[0],
    )
    assert hop.token_index_in == 2
    assert hop.token_index_out == 0


def test_to_hop_state_both_or_neither():
    """Providing only token_in (without token_out) raises DegenbotValueError."""
    pool = _make_3coin_pool()
    with pytest.raises(DegenbotValueError, match="both be provided"):
        pool.to_hop_state(zero_for_one=True, token_in=pool.tokens[0])

    with pytest.raises(DegenbotValueError, match="both be provided"):
        pool.to_hop_state(zero_for_one=True, token_out=pool.tokens[2])


def test_to_hop_state_non_top_level_token():
    """Passing a token not in self.tokens raises DegenbotValueError."""
    pool = _make_2coin_pool()
    other_token = FakeToken(address="0xdead000000000000000000000000000000000000", symbol="OTH")
    with pytest.raises(DegenbotValueError, match="not a top-level pool token"):
        pool.to_hop_state(
            zero_for_one=True,
            token_in=other_token,
            token_out=pool.tokens[1],
        )


def test_to_hop_state_swap_fn_uses_correct_indices():
    """The swap_fn closure captures the correct i, j indices."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=True,
        token_in=pool.tokens[0],
        token_out=pool.tokens[2],
    )
    # swap_fn should call _get_dy(i=0, j=2, ...)
    dx = 10**18
    expected = pool._get_dy(0, 2, dx)
    assert hop.swap_fn(dx) == expected
```

### Integration tests

These are covered by the existing Curve fork tests — `to_hop_state(zero_for_one=True)` is unchanged for 2-token pools. The new kwargs add functionality without removing any.

## Benefits

- **Leverage**: One protocol change unblocks pair selection for both Curve (N-token) and Balancer (N-token) pools. No per-adapter duplication.
- **Locality**: Pair selection logic stays in the pool class where the token-index mapping lives. Adapters don't duplicate it.
- **Depth**: `to_hop_state()` becomes a deeper seam — it handles pair selection internally instead of requiring the caller to compute indices externally.
- **Resolves N-token ambiguity comment**: The Curve pool's code comment about N-token ambiguity is replaced with working pair selection.

## Risks

- **Protocol signature change**: Adding kwargs to `ArbitrageCapablePool` and `ArbitragePathPool` is backward-compatible (keyword-only, default `None`), but any external code implementing these protocols must add the kwargs for type-checker compliance. Mitigation: `@runtime_checkable` protocols only check for method existence, not signature — existing implementations continue to satisfy the protocol at runtime. Type-checker compliance is a soft requirement, not a hard break.

- **Metapool token resolution complexity**: For metapools, `token_in`/`token_out` resolve against `self.tokens` only. A swap involving underlying base-pool tokens (e.g., metapool LP token → base-pool USDC) requires a different index scheme. Mitigation: the explicit `DegenbotValueError` with message *"is not a top-level pool token"* guides callers to `get_dy()` directly. This matches existing behavior — `zero_for_one` also only addresses top-level tokens.

- **No runtime enforcement of kwargs**: Python protocols with `@runtime_checkable` don't enforce argument signatures. A pool class could omit the kwargs and still satisfy the protocol. Mitigation: this is the existing design of the protocol system — it's structural, not nominal. The kwargs are a convention that pool classes should follow, enforced by the type checker rather than the runtime.

- **Five pool classes changed for no behavioral effect**: V2, V3, V4, Aerodrome, Camelot all gain unused kwargs. Mitigation: these exist exclusively for type-checker compliance. The noise is minimal (2 extra keyword-only parameters per class) and the alternative (omitting them and suppressing type-checker warnings) is worse for maintainability.

- **Two independent protocols must stay in sync**: `ArbitrageCapablePool` and `ArbitragePathPool` both independently declare `to_hop_state()`. If their signatures ever diverged, concrete classes satisfying both would break. Mitigation: this is an inherent property of multiple independent protocols sharing a method — it already exists today (both declare the same parameter list). The risk is low given the stability of the `to_hop_state()` interface.

## Relationship to Other Plans

- **Plan 070** (Balancer Builder): Complementary — Plan 070's `BalancerPairView.to_hop_state()` delegates via `token_in`/`token_out` instead of accessing private pool internals. This plan is **convenient** for Plan 070 but not a hard prerequisite — Plan 070 holds a concrete union type (`BalancerV2Pool | BalancerV2StablePool`), so it can call `to_hop_state(token_in=..., token_out=...)` on the concrete class regardless of whether the protocol declares the kwargs. The protocol-level kwargs benefit other future callers holding `ArbitrageCapablePool`-typed references. Plan 070 Slice 1 must fix the `BalancerV2StablePool.tokens` return type annotation (documented in Step 4).
- **Plan 068** (Absorb Curve on-chain cache): Orthogonal — different concern.
- **Plan 069** (Remove DyCalculation closures): Orthogonal — different concern.

## Status

[x] Slice 1: Protocols, Curve pool, and test doubles
[x] Slice 2: All other pool classes
[x] Slice 3: Validate and clean up
