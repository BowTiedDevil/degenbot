"""Tier-2 behavioral dual-driver parity — V4 concentrated-liquidity swap.

The behavioral companion to the Rust `parity_v4_swap.rs` test. Proves the
**same** canonical V4 fixture driven through the **Python consumer** (`PyBot`,
the PyO3 binding) produces the **same** `amount_out` as the Rust consumer.

V4 uses the **same CL math** as V3 but the `amountSpecified` sign convention
flips (V4 exact-input is *negative*, opposite to V3's positive). So this
exercises a distinct FFI-seam path from the V3 seed: the Rust core's sign
flip in `simulate_swap` + the V4 `register_v4_pool` admission (pool_manager +
pool_id_hex + currency0/1/fee/tick_spacing/hook_flags). Divergence between the
Rust and Python consumers = a lossy seam on the V4 path.

## The shared contract

The fixture constants below MUST mirror `rust/crates/degenbot/tests/
parity_v4_swap.rs` exactly. A 1:1-price V4 pool (sqrt_price_x96 = 2^96,
tick 0), liquidity 1e12, fee 500 (0.05% — V4 default-low, **distinct from
the V3 seed's 3000**, ruling out an accidental hardcoding), tick_spacing 10,
no hooks (`hook_flags = 0`), tick 0 seeded + `"tracked"` coverage.
`amount_in = 1_000_000_000` → deterministic output `998_501_997`.
"""

from __future__ import annotations

from degenbot.bot import PyBot

# ---- the shared canonical fixture (mirror in the Rust parity test) ----
_POOL_MANAGER = "0x" + "4" * 40
_CURRENCY0 = "0x" + "0" * 40  # native (address(0))
_CURRENCY1 = "0x" + "0" * 39 + "1"
_HOOKS = "0x" + "0" * 40  # no hooks

_FEE = 500  # 0.05% (V4 default-low tier — distinct from V3's 3000)
_TICK_SPACING = 10
# `pool_id_hex` — a 0x-prefixed 32-byte hex string. The Rust core derives the
# pool's identity key from this; the value is arbitrary for the in-spec
# admission path (no CREATE2 check on V4 — the pool_id IS the key).
_POOL_ID_HEX = "0x" + "ee" * 32
# A 1:1 price: sqrt_price_x96 = 2^96 (the Q96.32 repr of 1.0).
_SQRT_PRICE_1TO1 = 1 << 96
_LIQUIDITY = 1_000_000_000_000  # 1e12
_TICK = 0

_AMOUNT_IN = 1_000_000_000  # 1e9 — small, stays in-tick
_ZERO_FOR_ONE = True  # currency0 → currency1

# Canonical V4 swap output, deterministic from the fixture above. Symmetric
# for zfo/ofz at the 1:1 price. Both consumers MUST reproduce this.
_EXPECTED_AMOUNT_OUT_ZFO = 998_501_997


def _register_canonical_v4_pool(py_bot: PyBot) -> int:
    """Register the canonical V4 pool through the Python consumer path.

    Mirrors the Rust test's inline registration — same fixture, same
    `Tracked` coverage, same tick-0 seed. Returns the deterministic pool id (1).
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
    """The PyBot Python driver reproduces the recorded V4 swap constant.

    Python side of the Tier-2 dual-driver gate (V4 CL path). The Rust side
    (`rust/crates/degenbot/tests/parity_v4_swap.rs`) drives the same fixture
    through `BotState` directly; both MUST equal `_EXPECTED_AMOUNT_OUT_ZFO`.
    Catches a lossy FFI seam on the V4 sign-flipped path.
    """
    py_bot = PyBot()
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
    ofz_out = py_bot.calculate_tokens_out(
        pool_id, zero_for_one=False, amount_in=_AMOUNT_IN
    )
    assert ofz_out == amount_out, (
        f"1:1-price V4 swap must be direction-symmetric (zfo {amount_out} != ofz {ofz_out})"
    )
