"""Tier-2 behavioral dual-driver parity — V2 swap calc (ADR-005 standalone claim).

The behavioral companion to the Rust `parity_v2_swap.rs` test. Proves the
**same** canonical fixture driven through the **Python consumer** path
(`RustBot`, the PyO3 binding) produces the **same** `amount_out` as the
closed-form Uniswap V2 `getAmountOut` reference — which the Rust consumer
test (`rust/crates/degenbot/tests/parity_v2_swap.rs`) independently also
asserts.

Both consumers hit the same `BotState::register_v2_pool` +
`BotState::calculate_tokens_out` core. The closed form is the **shared
oracle**: if the PyO3 arg-extraction → core-call → result-wrap seam ever drops
precision, changes rounding, or swaps a direction flag, this test diverges
from the closed form and the parity breaks — surfacing the FFI regression
that Tier-1 reachability can't catch (reachability proves the symbol is
*resolvable*, not that the delegation is *lossless*).

## The shared contract

The fixture constants below MUST mirror `rust/crates/degenbot/tests/
parity_v2_swap.rs` exactly. The expected output is the closed-form V2
`getAmountOut`:

    amount_in_with_fee = amount_in * 997
    numerator          = amount_in_with_fee * reserve_out
    denominator        = reserve_in * 1000 + amount_in_with_fee
    amount_out         = numerator // denominator   (EVM integer division)
"""

from __future__ import annotations

from degenbot.bot import RustBot

# ---- the shared canonical fixture (mirror in the Rust parity test) ----
_TOKEN0 = "0x" + "0" * 39 + "A"
_TOKEN1 = "0x" + "0" * 39 + "B"
_POOL = "0x" + "0" * 39 + "C"
# The Uniswap V2 mainnet factory (the `UNISWAP_V2` preset's `factory`). With
# `RustBot(chain_id=0)` the (chain, factory) pair is not in the deployments
# JSON, so the CREATE2 verification is skipped — ad-hoc registration with the
# arbitrary pool address above is preserved. This matches the Rust consumer
# path, where the core's `register_v2_pool` does no CREATE2 check.
_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"

_RESERVE0 = 1_000_000_000_000  # 1e6 USDC (6dp)
_RESERVE1 = 500_000_000_000_000_000  # 0.5 WETH (18dp)

_AMOUNT_IN = 1_000_000_000  # 1000 USDC (6dp)
_ZERO_FOR_ONE = True  # token0 (USDC) → token1 (WETH)

# Canonical expected output, derived from the closed form (see module doc):
#   amount_in_with_fee = 1_000_000_000 * 997           = 997_000_000_000
#   numerator          = 997_000_000_000 * 5e17        = 498_500_000_000_000_000_000_000_000
#   denominator        = 1e15 * 1000 + 997_000_000_000 = 1_000_997_000_000_000
#   amount_out         = numerator // denominator      = 498_003_490_519_951
_EXPECTED_AMOUNT_OUT = 498_003_490_519_951

# Canonical expected input, derived from the closed-form V2 `getAmountIn`:
#   amount_in = 1 + (reserve_in * amount_out * fee_denom) //
#                  ((reserve_out - amount_out) * gamma_numer)
# The target `amount_out` is `_EXPECTED_AMOUNT_OUT`; this round-trips back to
# `_AMOUNT_IN`. Mirrors the Rust test's `EXPECTED_AMOUNT_IN`.
_EXPECTED_AMOUNT_IN = 1_000_000_000


def _register_canonical_v2_pool(py_bot: RustBot) -> int:
    """Register the canonical V2 pool through the Python consumer path.

    Mirrors the Rust test's `register_canonical_v2_pool` helper — same
    fixture, same `UNISWAP_V2` fee params (997/1000 retained-fraction, both
    directions), same reserves. Returns the deterministic pool id (1).
    """
    return py_bot.register_v2_pool(
        address=_POOL,
        token0=_TOKEN0,
        token1=_TOKEN1,
        reserve0=_RESERVE0,
        reserve1=_RESERVE1,
        gamma_numer0=997,
        fee_denom0=1000,
        gamma_numer1=997,
        fee_denom1=1000,
        factory=_FACTORY,
    )


def test_python_consumer_matches_closed_form() -> None:
    """The RustBot Python driver reproduces the closed-form V2 getAmountOut.

    This is the Python side of the Tier-2 dual-driver gate. The Rust side
    (`rust/crates/degenbot/tests/parity_v2_swap.rs`) drives the same fixture
    through `BotState` directly; both must equal `_EXPECTED_AMOUNT_OUT`.
    Divergence = a lossy FFI seam (arg extraction, rounding, or direction
    flag), which the static reachability gate cannot detect.
    """
    py_bot = RustBot()  # chain_id=0 → no CREATE2 verification (ad-hoc path).
    pool_id = _register_canonical_v2_pool(py_bot)
    assert pool_id == 1, "first registered pool gets id 1 (parity contract)"

    amount_out = py_bot.calculate_tokens_out(
        pool_id, zero_for_one=_ZERO_FOR_ONE, amount_in=_AMOUNT_IN
    )
    assert amount_out == _EXPECTED_AMOUNT_OUT, (
        "Python consumer (RustBot FFI seam) must match the closed-form V2 "
        "getAmountOut — divergence from the Rust consumer test means the "
        f"PyO3 delegation is lossy (got {amount_out}, want {_EXPECTED_AMOUNT_OUT})"
    )


def test_python_consumer_reverse_matches_closed_form() -> None:
    """The RustBot Python driver reproduces the closed-form V2 getAmountIn.

    Mirror of the Rust test's
    `standalone_rust_consumer_reverse_matches_closed_form`. The same canonical
    fixture, driving `calculate_tokens_in` for the target `amount_out` the
    forward test derived. The `amount_in` recovered MUST round-trip back to
    `_AMOUNT_IN`. Proves the FFI seam is lossless in both directions.
    """
    py_bot = RustBot()
    pool_id = _register_canonical_v2_pool(py_bot)
    assert pool_id == 1, "first registered pool gets id 1 (parity contract)"

    amount_in = py_bot.calculate_tokens_in(
        pool_id, zero_for_one=_ZERO_FOR_ONE, amount_out=_EXPECTED_AMOUNT_OUT
    )
    assert amount_in == _EXPECTED_AMOUNT_IN, (
        "Python consumer reverse calc must match the closed-form V2 getAmountIn "
        "and round-trip _AMOUNT_IN — divergence means the PyO3 delegation is "
        f"lossy in the reverse direction (got {amount_in}, want {_EXPECTED_AMOUNT_IN})"
    )


def test_closed_form_reference_is_self_consistent() -> None:
    """Guard against the expected constant drifting from the closed form.

    Mirror of the Rust test's self-consistency guard: if the reserve/amount
    constants change without updating `_EXPECTED_AMOUNT_OUT`, this re-derives
    the closed form and catches it.
    """
    amount_in_with_fee = _AMOUNT_IN * 997
    numerator = amount_in_with_fee * _RESERVE1
    denominator = _RESERVE0 * 1000 + amount_in_with_fee
    derived = numerator // denominator
    assert derived == _EXPECTED_AMOUNT_OUT, (
        "_EXPECTED_AMOUNT_OUT must match the closed-form re-derivation: "
        f"derived {derived}, constant {_EXPECTED_AMOUNT_OUT}"
    )

    # Reverse-direction closed form: amount_in = 1 + (reserve_in * amount_out
    # * fee_denom) // ((reserve_out - amount_out) * gamma_numer). Verifies
    # _EXPECTED_AMOUNT_IN round-trips back to _AMOUNT_IN.
    rev_numerator = _RESERVE0 * _EXPECTED_AMOUNT_OUT * 1000
    rev_denominator = (_RESERVE1 - _EXPECTED_AMOUNT_OUT) * 997
    rev_derived = 1 + rev_numerator // rev_denominator
    assert rev_derived == _EXPECTED_AMOUNT_IN, (
        "_EXPECTED_AMOUNT_IN must match the closed-form getAmountIn "
        f"re-derivation: derived {rev_derived}, constant {_EXPECTED_AMOUNT_IN}"
    )
