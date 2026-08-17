"""Tier-2 behavioral dual-driver parity — V3 concentrated-liquidity swap.

The behavioral companion to the Rust `parity_v3_swap.rs` test. Proves the
**same** canonical V3 fixture driven through the **Python consumer** (`Bot`,
the PyO3 binding) produces the **same** `amount_out` as the Rust consumer
(`BotState` directly).

The V3 CL swap math has no simple closed form (the V2 test's hand-derived
`getAmountOut` oracle doesn't apply — `v3_simulate_swap` routes through
`compute_swap_step_v3` + the tick walk). So this Tier-2 seed asserts the
**direct FFI-seam-lossless claim**: Python-consumer == Rust-consumer ==
recorded constant, plus monotonicity + direction-symmetry sanity checks.
Divergence = a lossy FFI seam on the CL swap path, which the V2-only gate
cannot catch.

## The shared contract (HRT356 — single source of truth)

The fixture + expected output are loaded from the SHARED file
`tests/standalone_parity/fixtures/v3_swap.json`, which the Rust parity test
(`rust/crates/degenbot/tests/parity_v3_swap.rs`) ALSO loads. A fixture edit
that drifts the expected output fails BOTH sides mechanically — closing the
V3/V4 fixture-drift gap documented in AGENTS.md "Known gap — V3/V4 fixture
drift" (where the constants were previously copied between the two sides with
no mechanical link, so a one-sided edit left both tests green but testing
different fixtures).
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot._ffi import Bot

# ---- the shared canonical fixture (loaded, not copied) ----
_FIXTURE_PATH = Path(__file__).parent / "fixtures" / "v3_swap.json"
_FIXTURE = json.loads(_FIXTURE_PATH.read_text())
_F = _FIXTURE["fixture"]
_P = _FIXTURE["probe"]
_E = _FIXTURE["expected"]

_TOKEN0 = _F["token0"]
_TOKEN1 = _F["token1"]
_POOL = _F["pool"]
_FACTORY = _F["factory"]
_FEE = _F["fee"]
_TICK_SPACING = _F["tick_spacing"]
_SQRT_PRICE_1TO1 = int(_F["sqrt_price_x96"])  # 2^96, the Q96.32 repr of 1.0
_LIQUIDITY = int(_F["liquidity"])
_TICK = _F["tick"]

_AMOUNT_IN = int(_P["amount_in"])
_ZERO_FOR_ONE = _P["zero_for_one"]

_EXPECTED_AMOUNT_OUT_ZFO = int(_E["amount_out_zfo"])


def _register_canonical_v3_pool(py_bot: Bot) -> int:
    """Register the canonical V3 pool through the Python consumer path.

    Mirrors the Rust test's inline registration — same fixture (loaded from the
    same shared file), same `Tracked` coverage, same tick-0 seed. Returns the
    deterministic pool id (1).
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
    """The Bot Python driver reproduces the recorded V3 swap constant.

    Python side of the Tier-2 dual-driver gate (V3 CL path). The Rust side
    (`rust/crates/degenbot/tests/parity_v3_swap.rs`) loads the same fixture
    file and drives it through `BotState` directly; both MUST equal
    `_EXPECTED_AMOUNT_OUT_ZFO`. Divergence = a lossy FFI seam on the CL swap
    path.
    """
    py_bot = Bot()
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

    # Monotonicity sanity: a larger in-tick input -> strictly-larger output.
    bigger_in = _AMOUNT_IN * 10
    bigger_out = py_bot.calculate_tokens_out(
        pool_id, zero_for_one=_ZERO_FOR_ONE, amount_in=bigger_in
    )
    assert bigger_out > amount_out, (
        f"V3 in-tick swap must be monotonic: {bigger_out} !> {amount_out}"
    )

    # Symmetry sanity: at the 1:1 price zfo == ofz.
    ofz_out = py_bot.calculate_tokens_out(pool_id, zero_for_one=False, amount_in=_AMOUNT_IN)
    assert ofz_out == amount_out, (
        f"1:1-price V3 swap must be direction-symmetric (zfo {amount_out} != ofz {ofz_out})"
    )
