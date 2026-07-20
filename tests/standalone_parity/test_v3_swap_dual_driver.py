"""Tier-2 behavioral dual-driver parity — V3 concentrated-liquidity swap.

The behavioral companion to the Rust `parity_v3_swap.rs` test. Proves the
**same** canonical V3 fixture driven through the **Python consumer** (`PyBot`,
the PyO3 binding) produces the **same** `amount_out` as the Rust consumer
(`BotState` directly).

The V3 CL swap math has no simple closed form (the V2 test's hand-derived
`getAmountOut` oracle doesn't apply — `v3_simulate_swap` routes through
`compute_swap_step_v3` + the tick walk). So this Tier-2 seed asserts the
**direct FFI-seam-lossless claim**: Python-consumer == Rust-consumer ==
recorded constant, plus monotonicity + direction-symmetry sanity checks.
Divergence = a lossy FFI seam on the CL swap path, which the V2-only gate
cannot catch.

## The shared contract

The fixture constants below MUST mirror `rust/crates/degenbot/tests/
parity_v3_swap.rs` exactly. A 1:1-price V3 pool (sqrt_price_x96 = 2^96,
tick 0), liquidity 1e12, fee 3000 (0.3%), tick_spacing 60, tick 0 seeded in
`tick_data` + `"tracked"` coverage so the swap stays in-tick (no
`MissingTickWord`). `amount_in = 1_000_000_000` → deterministic output
`996_006_981` (symmetric for zfo/ofz at the 1:1 price).
"""

from __future__ import annotations

from degenbot.bot import PyBot

# ---- the shared canonical fixture (mirror in the Rust parity test) ----
_TOKEN0 = "0x" + "0" * 39 + "A"
_TOKEN1 = "0x" + "0" * 39 + "B"
_POOL = "0x" + "0" * 39 + "C"
# chain_id=0 → (chain, factory) is not in the deployments JSON, so the V3
# CREATE2 verification is skipped (ad-hoc registration path). Matches the
# Rust consumer, where `BotState::register_v3_pool` does no CREATE2 check.
_FACTORY = "0x" + "0" * 39 + "F"

_FEE = 3_000  # 0.3%
_TICK_SPACING = 60
# A 1:1 price: sqrt_price_x96 = 2^96 (the Q96.32 repr of 1.0).
_SQRT_PRICE_1TO1 = 1 << 96
_LIQUIDITY = 1_000_000_000_000  # 1e12
_TICK = 0

_AMOUNT_IN = 1_000_000_000  # 1e9 — small, stays in-tick
_ZERO_FOR_ONE = True  # token0 → token1

# Canonical V3 swap output, deterministic from the fixture above. Symmetric
# for zfo/ofz at the 1:1 price. Both consumers MUST reproduce this —
# divergence = a lossy FFI seam on the CL swap path.
_EXPECTED_AMOUNT_OUT_ZFO = 996_006_981


def _register_canonical_v3_pool(py_bot: PyBot) -> int:
    """Register the canonical V3 pool through the Python consumer path.

    Mirrors the Rust test's inline registration — same fixture, same
    `Tracked` coverage, same tick-0 seed. Returns the deterministic pool id (1).
    """
    # tick 0: liquidity_gross = LIQUIDITY, liquidity_net = 0, block = 0.
    tick_data = {_TICK: (_LIQUIDITY, 0, 0)}
    return py_bot.register_v3_pool(
        address=_POOL,
        token0=_TOKEN0,
        token1=_TOKEN1,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        factory=_FACTORY,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        liquidity=_LIQUIDITY,
        tick=_TICK,
        tick_data=tick_data,
        update_block=0,
        coverage="tracked",
    )


def test_python_consumer_v3_swap_matches_recorded_constant() -> None:
    """The PyBot Python driver reproduces the recorded V3 swap constant.

    Python side of the Tier-2 dual-driver gate (V3 CL path). The Rust side
    (`rust/crates/degenbot/tests/parity_v3_swap.rs`) drives the same fixture
    through `BotState` directly; both MUST equal `_EXPECTED_AMOUNT_OUT_ZFO`.
    Divergence = a lossy FFI seam on the CL swap path.
    """
    py_bot = PyBot()
    pool_id = _register_canonical_v3_pool(py_bot)
    assert pool_id == 1, "first registered pool gets id 1 (parity contract)"

    amount_out = py_bot.calculate_tokens_out(
        pool_id, zero_for_one=_ZERO_FOR_ONE, amount_in=_AMOUNT_IN
    )
    assert amount_out == _EXPECTED_AMOUNT_OUT_ZFO, (
        "Python consumer V3 swap must match the recorded constant — divergence "
        "from the Rust consumer means the PyO3 delegation is lossy on the CL "
        f"swap path (got {amount_out}, want {_EXPECTED_AMOUNT_OUT_ZFO})"
    )

    # Monotonicity sanity: a larger in-tick input → strictly-larger output.
    bigger_in = _AMOUNT_IN * 10
    bigger_out = py_bot.calculate_tokens_out(
        pool_id, zero_for_one=_ZERO_FOR_ONE, amount_in=bigger_in
    )
    assert bigger_out > amount_out, (
        f"V3 in-tick swap must be monotonic: {bigger_out} !> {amount_out}"
    )

    # Symmetry sanity: at the 1:1 price zfo == ofz.
    ofz_out = py_bot.calculate_tokens_out(
        pool_id, zero_for_one=False, amount_in=_AMOUNT_IN
    )
    assert ofz_out == amount_out, (
        f"1:1-price V3 swap must be direction-symmetric (zfo {amount_out} != ofz {ofz_out})"
    )
