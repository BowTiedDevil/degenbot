"""Tier-2 behavioral dual-driver parity — V4 concentrated-liquidity swap.

The behavioral companion to the Rust `parity_v4_swap.rs` test. Proves the
**same** canonical V4 fixture driven through the **Python consumer** (`RustBot`,
the PyO3 binding) produces the **same** `amount_out` as the Rust consumer
(`BotState` directly). The V4 path is the sign-flipped twin of V3
(`calculate_tokens_out` inverts `zero_for_one` before delegating to
`v4_simulate_swap`); this fixture exercises that inversion.

## The shared contract (HRT356 — single source of truth)

The fixture + expected output are loaded from the SHARED file
`tests/standalone_parity/fixtures/v4_swap.json`, which the Rust parity test
(`rust/crates/degenbot/tests/parity_v4_swap.rs`) ALSO loads. A fixture edit
that drifts the expected output fails BOTH sides mechanically — closing the
V3/V4 fixture-drift gap documented in AGENTS.md "Known gap — V3/V4 fixture
drift" (the V4 constants were previously copied between the two sides with no
mechanical link).

## pool_manager reconciliation

This shared fixture canonicalizes `pool_manager` to `0x…0044` (the value the
Rust side previously used). The Python side previously used
`0x4444…4444` (forty 4s) — a drift neither test could catch because
`pool_manager` is non-load-bearing for the swap output (V4 admits pools by
`pool_id`, not CREATE2). The shared fixture forces agreement.
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot.bot import RustBot

# ---- the shared canonical fixture (loaded, not copied) ----
_FIXTURE_PATH = Path(__file__).parent / "fixtures" / "v4_swap.json"
_FIXTURE = json.loads(_FIXTURE_PATH.read_text())
_F = _FIXTURE["fixture"]
_P = _FIXTURE["probe"]
_E = _FIXTURE["expected"]

_POOL_MANAGER = _F["pool_manager"]
_CURRENCY0 = _F["currency0"]
_CURRENCY1 = _F["currency1"]
_HOOKS = _F["hooks"]
_POOL_ID_HEX = _F["pool_id_hex"]
_FEE = _F["fee"]
_TICK_SPACING = _F["tick_spacing"]
_SQRT_PRICE_1TO1 = int(_F["sqrt_price_x96"])  # 2^96, the Q96.32 repr of 1.0
_LIQUIDITY = int(_F["liquidity"])
_TICK = _F["tick"]

_AMOUNT_IN = int(_P["amount_in"])
_ZERO_FOR_ONE = _P["zero_for_one"]

_EXPECTED_AMOUNT_OUT_ZFO = int(_E["amount_out_zfo"])


def _register_canonical_v4_pool(py_bot: RustBot) -> int:
    """Register the canonical V4 pool through the Python consumer path.

    Mirrors the Rust test's inline registration — same fixture (loaded from the
    same shared file), same `Tracked` coverage, same tick-0 seed. Returns the
    deterministic pool id (1).
    """
    tick_data = {_TICK: (_LIQUIDITY, 0, 0)}
    return py_bot.register_v4_pool(
        pool_manager=_POOL_MANAGER,
        pool_id_hex=_POOL_ID_HEX,
        currency0=_CURRENCY0,
        currency1=_CURRENCY1,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        hook_flags=0,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        liquidity=_LIQUIDITY,
        tick=_TICK,
        block=0,
        tick_data=tick_data,
        coverage="tracked",
    )


def test_python_consumer_v4_swap_matches_recorded_constant() -> None:
    """The RustBot Python driver reproduces the recorded V4 swap constant.

    Python side of the Tier-2 dual-driver gate (V4 CL path). The Rust side
    (`rust/crates/degenbot/tests/parity_v4_swap.rs`) loads the same fixture file
    and drives it through `BotState` directly; both MUST equal
    `_EXPECTED_AMOUNT_OUT_ZFO`. Catches a lossy FFI seam on the V4 sign-flipped
    path.
    """
    py_bot = RustBot()
    pool_id = _register_canonical_v4_pool(py_bot)
    assert pool_id == 1, "first registered pool gets id 1 (parity contract)"

    amount_out = py_bot.calculate_tokens_out(
        pool_id, zero_for_one=_ZERO_FOR_ONE, amount_in=_AMOUNT_IN
    )
    assert amount_out == _EXPECTED_AMOUNT_OUT_ZFO, (
        "Python consumer V4 swap must match the recorded constant — divergence "
        "from the Rust consumer means the PyO3 delegation is lossy on the V4 "
        f"sign-flipped path (got {amount_out}, want {_EXPECTED_AMOUNT_OUT_ZFO})"
    )

    # Monotonicity (in-tick).
    bigger_in = _AMOUNT_IN * 10
    bigger_out = py_bot.calculate_tokens_out(
        pool_id, zero_for_one=_ZERO_FOR_ONE, amount_in=bigger_in
    )
    assert bigger_out > amount_out, (
        f"V4 in-tick swap must be monotonic: {bigger_out} !> {amount_out}"
    )

    # Direction symmetry at the 1:1 price (catches a V4 sign-flip regression).
    ofz_out = py_bot.calculate_tokens_out(pool_id, zero_for_one=False, amount_in=_AMOUNT_IN)
    assert ofz_out == amount_out, (
        f"1:1-price V4 swap must be direction-symmetric (zfo {amount_out} != ofz {ofz_out})"
    )
