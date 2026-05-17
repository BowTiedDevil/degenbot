# Plan 045: Replace `pool` Parameter with Explicit Data in DyCalculator

## Overview

Replace the `pool: CurveStableswapPool` parameter in `DyCalculator.calculate()` with a
`DyCalculationInputs` frozen dataclass that carries exactly the data each calculator needs.
This eliminates all 77 SLF001 (private member access) violations in the calculator modules
and completes the "separate I/O from calculation" architecture: calculators become pure
consumers of pre-resolved data, with all I/O and cache lookups happening in the pool's
`get_dy()` dispatch before the calculator is called.

## Problem

Every `DyCalculator` implementation currently receives `pool: CurveStableswapPool` and
accesses private members:

| Private member | Accessed by | Count | Category |
|---|---|---|---|
| `_xp()` | standard, live_admin, metapool | 13 | Pure math (rates×balances) |
| `_get_y()` | standard, live_admin, metapool | 13 | Invariant solver (delegates to pure fn) |
| `_resolve_rates()` | standard, live_admin | 5 | I/O (fetches lending rates) |
| `_tokens` | crypto, live_admin, metapool | 10 | State (token list/count) |
| `_get_virtual_price()` | metapool | 6 | I/O (data provider) |
| `_get_scaled_redemption_price()` | metapool | 4 | I/O (data provider) |
| `_data_provider` | crypto | 3 | I/O (data provider reference) |
| `_cached_contract_D` | crypto | 2 | Cache (mutable dict) |
| `_cached_gamma` | crypto | 2 | Cache (mutable dict) |
| `_cached_price_scale` | crypto | 2 | Cache (mutable dict) |
| `_block_timestamps` | crypto | 1 | Cache (mutable dict) |
| `_a()` | crypto | 1 | State (A ramping) |
| `_newton_y()` | crypto | 1 | Invariant solver |
| `_fetch_token_balance()` | live_admin | 4 | I/O (data provider) |
| `_get_admin_balances()` | live_admin | 4 | I/O (data provider) |
| `base_pool._tokens` | metapool | 3 | Cross-pool state |

**Total: 77 SLF001 violations.**

The calculators were extracted in Plan 039 with `pool` as the parameter — the pragmatic
choice at the time. Now that calculators exist and their access patterns are stable,
we can replace `pool` with a narrower, explicitly-constructed data object.

## Solution

### Step 1: Define `DyCalculationInputs` in `curve/types.py`

A frozen dataclass carrying all data any calculator might need. The pool constructs it
in `get_dy()` before calling the calculator. Fields are grouped by category.

```python
@dataclasses.dataclass(slots=True, frozen=True)
class DyCalculationInputs:
    """Pre-resolved data for a single dy calculation.

    Constructed by CurveStableswapPool.get_dy() before delegating to the
    injected DyCalculator. The calculator reads only from this object —
    never from the pool directly. All I/O, cache lookups, and rate
    resolution happen before this object is created.
    """

    # ── Pool constants ──
    PRECISION: int
    FEE_DENOMINATOR: int
    fee: int
    n_coins: int

    # ── Pool state ──
    balances: tuple[int, ...]
    rate_multipliers: tuple[int, ...]
    precision_multipliers: tuple[int, ...]
    offpeg_fee_multiplier: int
    fee_gamma: int
    mid_fee: int
    out_fee: int
    address: ChecksumAddress

    # ── Pre-resolved rates (after lending-rate I/O) ──
    resolved_rates: tuple[int, ...]

    # ── Pre-computed XP (rate-adjusted balances) ──
    xp: tuple[int, ...]

    # ── Pre-resolved block data (I/O done before construction) ──
    block_number: int
    block_timestamp: int
    amp: int  # resolved A coefficient (after ramping computation)

    # ── I/O results for crypto pools ──
    d: int | None = None  # on-chain D for crypto
    gamma: int | None = None  # on-chain gamma for crypto
    price_scale: tuple[int, ...] | None = None  # on-chain price_scale for crypto

    # ── I/O results for live-admin pools ──
    live_balances: tuple[int, ...] | None = None  # fetched token balances
    admin_balances: tuple[int, ...] | None = None  # fetched admin balances
    effective_balances: tuple[int, ...] | None = None  # live - admin

    # ── I/O results for metapool pools ──
    virtual_price: int | None = None
    scaled_redemption_price: int | None = None

    # ── Callable: invariant solver (needed by standard & live_admin) ──
    # These are pure math closures — they carry amp, n_coins, etc. internally.
    get_y: Callable[[int, int, int, Sequence[int]], int] | None = None
    newton_y: Callable[[int, int, Sequence[int], int, int], int] | None = None
```

**Why a flat dataclass instead of per-calculator context objects?**
Each calculator already knows which fields it needs. Adding a `Protocol` per calculator
family would create 5 protocols for marginal benefit. The single flat dataclass is
the simplest structure that eliminates all SLF001 errors. Fields that a calculator
doesn't use are `None` and ignored — this is explicit in the calculator code.

**Why `Callable` closures for `get_y` and `newton_y` instead of inlining the calls?**
`_get_y` is a thin wrapper around `stableswap_get_y` that resolves amp, n_coins,
a_precision, y_variant, d_variant from pool state. The calculator shouldn't need to
know about A ramping, variant enums, or A_PRECISION. Passing a closure means the
calculator calls `get_y(i, j, x, xp)` and gets the correct result without knowing
*how* y is computed.

### Step 2: Update `DyCalculator` Protocol

```python
class DyCalculator(Protocol):
    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int: ...
```

Removes `pool` and `block_number` (now inside `inputs`). Adds `inputs`.
The `override_state` parameter is kept separately because it's a per-call override,
not part of the pre-resolved inputs.

### Step 3: Update `PoolStrategies`

No change — the `dy_calculator`, `metapool_dy_calculator`, `metapool_underlying_dy_calculator`
fields already hold calculator instances. The calculator interface changes, not the
container.

### Step 4: Update `CurveStableswapPool.get_dy()` to construct `DyCalculationInputs`

The pool moves all I/O and cache resolution *before* the calculator call:

```python
def get_dy(self, i, j, dx, block_identifier=None, override_state=None) -> int:
    block_number = self._resolve_block_number(block_identifier)
    pool_balances = override_state.balances if override_state is not None else self.balances

    # ── I/O: block timestamp ──
    if block_number not in self._block_timestamps:
        if self._data_provider is None:
            raise MissingCurveData(...)
        self._block_timestamps[block_number] = self._data_provider.block_timestamp(block_number)
    block_timestamp = self._block_timestamps[block_number]

    # ── Resolve amp (A ramping) ──
    amp = self._a(timestamp=block_timestamp)

    # ── Resolve rates (lending I/O) ──
    resolved_rates = self._resolve_rates(
        rates=self.rate_multipliers,
        block_number=block_number,
        pool_balances=pool_balances,
    )

    # ── Compute XP ──
    xp = self._xp(rates=resolved_rates, balances=pool_balances)

    # ── Construct get_y closure ──
    def get_y(i: int, j: int, x: int, xp_: Sequence[int]) -> int:
        return self._get_y(i, j, x, xp_)

    inputs = DyCalculationInputs(
        PRECISION=self.PRECISION,
        FEE_DENOMINATOR=self.FEE_DENOMINATOR,
        fee=self.fee,
        n_coins=len(self.tokens),
        balances=pool_balances,
        rate_multipliers=self.rate_multipliers,
        precision_multipliers=self.precision_multipliers,
        offpeg_fee_multiplier=self.offpeg_fee_multiplier,
        fee_gamma=self.fee_gamma,
        mid_fee=self.mid_fee,
        out_fee=self.out_fee,
        address=self.address,
        resolved_rates=resolved_rates,
        xp=xp,
        block_number=block_number,
        block_timestamp=block_timestamp,
        amp=amp,
        get_y=get_y,
    )

    return self._strategies.dy_calculator.calculate(
        i, j, dx, inputs=inputs, override_state=override_state
    )
```

For metapool pools, `get_dy()` also resolves `virtual_price`, `scaled_redemption_price`,
and the base pool's state. For crypto pools, `get_dy()` resolves `d`, `gamma`,
`price_scale` (with cache lookup + data provider fallback). For live-admin pools,
`get_dy()` resolves `live_balances`, `admin_balances`, `effective_balances`.

The key principle: **all `self._xxx` access stays in `get_dy()` (which is on the pool
class itself, where private access is allowed). The calculator receives only public
attributes of `DyCalculationInputs`.**

### Step 5: Update each calculator to use `inputs` instead of `pool`

Each calculator's `calculate()` signature changes from `*, pool, block_number, override_state`
to `*, inputs, override_state`. The body replaces `pool.XXX` with `inputs.xxx`.

Example — `StandardDyCalculator`:

```python
def calculate(
    self,
    i: int,
    j: int,
    dx: int,
    *,
    inputs: DyCalculationInputs,
    override_state: CurveStableswapPoolState | None = None,
) -> int:
    pool_balances = override_state.balances if override_state is not None else inputs.balances
    rates = inputs.resolved_rates
    xp = inputs.xp
    x = xp[i] + (dx * rates[i] // inputs.PRECISION)
    y = inputs.get_y(i, j, x, xp)
    dy = xp[j] - y - 1
    fee = inputs.fee * dy // inputs.FEE_DENOMINATOR
    return (dy - fee) * inputs.PRECISION // rates[j]
```

Wait — standard calculators use `pool._resolve_rates()` and `pool._xp()` to compute from
`override_state.balances` when it's present. With the new design, the pool pre-resolves
rates and xp for the *current* balances, but override_state.balances might be different.

**Design decision: `override_state` handling.**

When `override_state` is provided, the calculator needs rates and xp computed from the
*override* balances, not the pool's current balances. There are two options:

**Option A**: The pool constructs two sets of inputs — one for current state, one for
override state. The calculator uses whichever is non-None.

**Option B**: The calculator re-computes rates and xp from override_state when present.
This is the simpler approach — the calculator already receives `resolved_rates` and `xp`
for the current state, and if `override_state` is provided, it re-resolves locally.

But option B means the calculator would need `_resolve_rates()` (I/O) and `_xp()` (pure
math) — which takes us back to private access or duplicating the logic.

**Option C**: The pool always resolves for the active balances (override or current),
and the calculator always uses `inputs.resolved_rates` and `inputs.xp`. The calculator
never re-resolves — the pool does it once.

This is the cleanest option. `get_dy()` checks for `override_state` and resolves rates/xp
from the appropriate balances before constructing `DyCalculationInputs`:

```python
pool_balances = override_state.balances if override_state is not None else self.balances
resolved_rates = self._resolve_rates(
    rates=self.rate_multipliers, block_number=block_number, pool_balances=pool_balances
)
xp = self._xp(rates=resolved_rates, balances=pool_balances)
```

The calculator then simply uses `inputs.resolved_rates` and `inputs.xp` — no need to
check `override_state` at all. The `override_state` parameter becomes unnecessary in
most calculators. However, some metapool calculators still need `override_state.base`
for delegating to the base pool's `calc_token_amount()`, etc.

**Decision: Option C.** Drop `override_state` from calculator signatures for
non-metapool calculators. Keep it only for metapool calculators that delegate to
`base_pool.calc_token_amount()`, `base_pool.get_dy()`, `base_pool.calc_withdraw_one_coin()`.

Actually, looking more carefully, the metapool calculators pass `override_state.base`
to the base pool's methods. This means the calculator still needs `override_state`.
But that's fine — it's a `CurveStableswapPoolState` object, which is a public type.
No private access needed.

### Step 6: Handle live-admin I/O in `get_dy()`, not in calculators

Currently, `LiveAdminDyCalculator` calls `pool._fetch_token_balance()` and
`pool._get_admin_balances()`. These are I/O operations. In the new design, the pool's
`get_dy()` checks the swap style and pre-fetches these values before constructing
`DyCalculationInputs`.

Similarly, `CryptoDyCalculator` accesses `pool._cached_contract_D`,
`pool._cached_gamma`, `pool._cached_price_scale` (mutable caches). In the new design,
`get_dy()` resolves these values (with cache lookup + data provider fallback) and
passes the resolved values as `inputs.d`, `inputs.gamma`, `inputs.price_scale`.

**The mutable cache writes currently happen inside the calculator:**
```python
pool._cached_contract_D[block_number] = d
```
These must move to `get_dy()`, which already manages other caches
(`_block_timestamps`). The calculator should not mutate pool state.

### Step 7: Handle metapool cross-pool calls

Metapool calculators currently call `pool.base_pool.get_dy()`,
`pool.base_pool.calc_token_amount()`, `pool.base_pool.calc_withdraw_one_coin()`.
These are cross-pool calls on a *different* pool object. With the new design, the
metapool calculator receives `inputs.base_pool: CurveStableswapPool | None` (a public
property already) and calls its *public* methods. No SLF001 issue — `base_pool.get_dy()`,
`base_pool.calc_token_amount()`, `base_pool.calc_withdraw_one_coin()` are all public.

However, `base_pool._tokens` and `pool._tokens` are accessed for `len()`. Since
`tokens` is a public property on the `StableswapPoolState` mixin, these become
`len(inputs.base_pool.tokens)` and `inputs.n_coins` — no SLF001.

### Step 8: Handle `get_y` for metapool and live-admin that compute xp from different balances

In live-admin calculators, the balances used are `live - admin`, not the pool's stored
balances. The xp must be computed from these different balances. In the new design:

1. The pool pre-fetches `live_balances` and `admin_balances`, computes
   `effective_balances = live - admin`
2. The pool computes `resolved_rates` and `xp` from `effective_balances` (not from
   `pool.balances`)
3. The calculator simply uses `inputs.xp` and `inputs.resolved_rates`

This means the live-admin path in `get_dy()` constructs a *different* `DyCalculationInputs`
than the standard path — with balances/xp/rates derived from live-admin data instead of
stored state. This is correct: the calculator shouldn't care where the numbers came from.

## Implementation Order (Vertical Slices)

### Slice 1: `DyCalculationInputs` dataclass + `DyCalculator` protocol update

1. Define `DyCalculationInputs` in `types.py`
2. Update `DyCalculator` protocol to use `inputs: DyCalculationInputs` instead of `pool`
3. Run: ruff confirms 0 SLF001 in types.py (no SLF001 there currently)
4. Run: tests fail (calculators still use old signature) — expected

### Slice 2: Convert `StandardDyCalculator` + `get_dy()` non-metapool path

1. Update `get_dy()` to construct `DyCalculationInputs` for non-metapool pools
2. Update `StandardDyCalculator.calculate()` to read from `inputs`
3. Verify: 0 SLF001 in `standard.py`
4. Verify: all existing tests pass

### Slice 3: Convert remaining standard calculators

1. `RateAdjustedDyCalculator`, `RateAdjustedNoOneDyCalculator`,
   `RawBalanceDyCalculator`, `NoOneFeeRateDyCalculator`, `CytokenDyCalculator`
2. Each is a 5-10 line change replacing `pool.xxx` with `inputs.xxx`
3. Verify: 0 SLF001 in `standard.py`
4. Verify: all existing tests pass

### Slice 4: Convert `CryptoDyCalculator`

1. Move I/O (D/gamma/price_scale cache resolution) from calculator to `get_dy()`
2. Move cache *writes* from calculator to `get_dy()`
3. Calculator reads `inputs.d`, `inputs.gamma`, `inputs.price_scale`, `inputs.newton_y`
4. Verify: 0 SLF001 in `crypto.py`
5. Verify: all existing tests pass

### Slice 5: Convert `LiveAdmin*` calculators

1. Move I/O (live_balances, admin_balances fetching) from calculator to `get_dy()`
2. Calculator reads `inputs.live_balances`, `inputs.admin_balances`,
   `inputs.effective_balances`
3. Verify: 0 SLF001 in `live_admin.py`
4. Verify: all existing tests pass

### Slice 6: Convert metapool calculators

1. Add `virtual_price`, `scaled_redemption_price`, `base_pool` to inputs
2. Replace `pool._get_virtual_price()` / `pool._get_scaled_redemption_price()`
   with `inputs.virtual_price` / `inputs.scaled_redemption_price`
3. Replace `pool.base_pool._tokens` with `inputs.base_pool.tokens`
4. Replace `pool._tokens` with `inputs.n_coins`
5. Keep `override_state` parameter for metapool calculators (base pool delegation)
6. Verify: 0 SLF001 in `metapool.py`
7. Verify: all existing tests pass

### Slice 7: Validate and clean up

1. Run `ruff check --select SLF001 src/degenbot/curve/calculators/` — expect 0 errors
2. Run full test suite
3. Run mypy
4. Remove now-unused TYPE_CHECKING imports of `CurveStableswapPool` from calculators
5. Update `CONTEXT.md` if needed

## Risks

- **`get_y` closure captures `self`**: The closure `def get_y(i, j, x, xp): return self._get_y(i, j, x, xp)`
  technically still accesses private pool state. But the SLF001 lint fires at the *call site*
  (inside the calculator), not at the definition site (inside the pool class itself, where
  `_get_y` is a method on `self`). The calculator calls `inputs.get_y(i, j, x, xp)` —
  no private access. This is the same pattern as `CurveDataProvider` — a closure that hides
  the implementation.

- **`override_state` for non-metapool**: With Option C, non-metapool calculators don't
  receive `override_state` — the pool pre-resolves rates/xp from the override balances.
  If a future calculator needs different override handling, the dataclass can be extended.

- **Performance**: `DyCalculationInputs` construction creates one extra object per `get_dy()`
  call. The cost is negligible — a few attribute reads and tuple construction, dwarfed by
  the invariant solve and any potential RPC calls.

- **Partial applicability of fields**: Not every calculator uses every field. This is
  acceptable — the unused fields are `None` and the type annotations make the contract clear.
  An alternative with per-calculator protocols would be more precise but add 5+ protocols
  for marginal benefit.

## Relationship to Other Plans

- **Plan 039** (DyCalculator Seam): This plan deepens Plan 039 by removing the `pool`
  coupling that Plan 039 explicitly noted as a risk: *"The pool parameter is the pragmatic
  seam."* Plan 039 extracted 14 branches into 17 calculator classes. This plan narrows
  the calculator interface from "the whole pool" to "exactly the data you need."

- **Plan 040** (Curve Data Provider): Completes the I/O independence. The `get_y` closure
  and `newton_y` closure encapsulate invariant solving; `resolved_rates`, `virtual_price`,
  etc. encapsulate I/O results. The calculator is fully I/O-free.

- **Plan 013** (Curve I/O-Free Architecture): This plan is the final step in making
  calculators truly I/O-free. After this, calculators are pure functions of `DyCalculationInputs`
  with no knowledge of `CurveStableswapPool` internals.

## Status: Complete ✅

All slices completed in a single pass:

- [x] Slice 1: Define `DyCalculationInputs` + update `DyCalculator` protocol
- [x] Slice 2: Convert `StandardDyCalculator` + `get_dy()` non-metapool path
- [x] Slice 3: Convert remaining standard calculators
- [x] Slice 4: Convert `CryptoDyCalculator`
- [x] Slice 5: Convert `LiveAdmin*` calculators
- [x] Slice 6: Convert metapool calculators
- [x] Slice 7: Validate and clean up

### Results

- **77 SLF001 errors → 0** across `src/degenbot/curve/calculators/` and entire project
- All 2631 tests pass
- mypy clean on all changed files
- ruff SLF001: 0 across `src/`
- No behavior changes — calculators are pure consumers of `DyCalculationInputs` instead of `pool`
