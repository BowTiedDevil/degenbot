"""Vulture false-positive whitelist.

Each entry references a name vulture flags as "unused" that is actually a
protocol/API contract the static analyzer can't see through. Run
`just dead-code` to regenerate the vulture report; a clean codebase exits 0
with this whitelist, so any *new* dead code stands out immediately.

Add to this file when vulture flags a name that is:
- a required parameter in a framework-signatured method (SQLAlchemy
  ``TypeDecorator``, context-manager ``__exit__``, dunder protocols),
- a parameter in a ``Protocol`` definition (part of the documented contract),
- a name used only in string-form type annotations (``cast("..."``,
  ``Annotated["Foo", ...]``) — vulture doesn't parse str-annotations,
- or any other shape that can't be removed without uglifying the public API.

Do **not** add to this file to silence real dead code — delete the code instead.
Regenerate candidate entries from a clean tree with:
    vulture src/degenbot --min-confidence 80 --make-whitelist
"""

constant_product_calc_exact_in  # uniswap/v2_functions.py: test-only — byte-parity reference for Rust `v2_math` (tests/rust/test_v2_math_parity.py)
constant_product_calc_exact_out  # uniswap/v2_functions.py: test-only — byte-parity reference for Rust `v2_math` (tests/rust/test_v2_math_parity.py)
dialect  # database/models/base.py: SQLAlchemy TypeDecorator.process_bind_param signature (framework-required)
dialect  # database/models/base.py: SQLAlchemy TypeDecorator.process_result_value signature (framework-required)
exc_type  # provider/__init__.py: __exit__ context-manager protocol parameter
exc_val  # provider/__init__.py: __exit__ context-manager protocol parameter
exc_tb  # provider/__init__.py: __exit__ context-manager protocol parameter
publisher  # types/concrete.py: Subscriber.notify Protocol parameter, part of the documented pub-sub contract
_a  # curve/curve_stableswap_liquidity_pool.py: A-ramp math pinned by tests/curve/test_curve_stableswap_pool.py
AETH  # curve/types.py: LendingRateStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
admin_balances  # curve/types.py: CurveDataProvider Protocol member / state-model field — set via kwargs, read by Rust core
d_variant  # curve/strategies.py + curve/types.py: PoolStrategies/state field, read generically (dataclasses) and by the Rust builder
CTOKEN  # curve/types.py: LendingRateStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
CYTOKEN  # curve/types.py: LendingRateStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
evm_divide  # calculations/evm_math.py: test-only — EVM-division reference used by tests/arbitrage/test_solvers/test_shared_state_topology.py
lending_rates  # curve/types.py: CurveDataProvider Protocol member — part of the documented data-provider contract
metapool_rate_style  # curve/strategies.py: PoolStrategies field, read generically and passed to the Rust core
metapool_underlying_style  # curve/strategies.py: PoolStrategies field, read generically and passed to the Rust core
NO_ONE_FEE_RATE  # curve/types.py: SwapStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
n_coins  # curve/types.py: state-model field, populated from the Rust state / golden cassettes
ORACLE  # curve/types.py: LendingRateStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
PRECISION  # curve/curve_stableswap_liquidity_pool.py: public Curve 1e18 precision constant (docs/external-bot API surface)
PRECISION_VP  # curve/types.py: MetapoolRateStyle/MetapoolUnderlyingStyle member — Rust-mirror enum discriminant
price_scale  # curve/types.py: CurveDataProvider Protocol member / state-model field — set via kwargs, read by Rust core
RAW_BALANCE  # curve/types.py: SwapStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
redemption_price  # curve/types.py: CurveDataProvider Protocol member — part of the documented data-provider contract
REDEMPTION  # curve/types.py: MetapoolUnderlyingStyle member — Rust-mirror enum discriminant
REDEMPTION_VP  # curve/types.py: MetapoolRateStyle member — Rust-mirror enum discriminant
RETH  # curve/types.py: LendingRateStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
resolve_d_variant  # curve/_variant_groups.py: test-only — address→variant table pinned by tests/curve/test_variant_groups.py
resolve_y_variant  # curve/_variant_groups.py: test-only — address→variant table pinned by tests/curve/test_variant_groups.py
resolve_yd_variant  # curve/_variant_groups.py: test-only — address→variant table pinned by tests/curve/test_variant_groups.py
RATE_ADJUSTED  # curve/types.py: SwapStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
RATE_ADJUSTED_NO_ONE  # curve/types.py: SwapStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
token_balance  # curve/types.py: CurveDataProvider Protocol member — part of the documented data-provider contract
token_total_supply  # curve/types.py: CurveDataProvider Protocol member — part of the documented data-provider contract
YTOKEN  # curve/types.py: LendingRateStyle member — Rust-mirror enum discriminant (shared schema, matched generically)
xp  # curve/types.py: state-model field, populated from the Rust state / golden cassettes
y_variant  # curve/strategies.py + curve/types.py: PoolStrategies/state field, read generically (dataclasses) and by the Rust builder
yd_variant  # curve/strategies.py + curve/types.py: PoolStrategies/state field, read generically (dataclasses) and by the Rust builder
