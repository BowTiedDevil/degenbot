"""Regenerate the Balancer V2 oracle snapshot.

Authority
---------
The oracle math is the retired Python reference implementation, restored
verbatim beside this file: `balancer_fixed_point_reference.py`,
`balancer_log_exp_math_reference.py`, `balancer_weighted_math_reference.py` and
`balancer_stable_math_reference.py`. Each is a direct port of the vendored
Solidity the engine implements — `FixedPoint.sol` / `LogExpMath.sol`
(`pkg/solidity-utils` 4.0.0), `WeightedMath.sol` (`pkg/pool-weighted` 2.0.1) and
`StableMath.sol` (`pkg/pool-stable` 0.1.0) at the pin recorded in
`docs/architecture/balancer-contract-reference.md`. The reference imports nothing
from the Rust leaf it checks.

Two deployed generations differ in rounding and are carried as separate rows:
`fp_pow_down` / `fp_pow_up` (and every WeightedMath row) run once per
`PowVersion` — `V1` is the general `LogExpMath.pow` path with the
`MAX_POW_RELATIVE_ERROR` bound, `V2` adds the `y == ONE / TWO / FOUR` fast paths
— and the stable invariant runs once per `round_up` direction (`P_D` accumulation
always down, deployed `P_D` accumulation either way).

Row format
----------
One record per line, LF-terminated, no blank lines, no trailing newline after the
last record's LF:

    <tag> <args...> = <result>
    <tag> <args...> = ERROR:<REASON>

`<args...>` is the tag's positional argument list; balance/weight vectors render
as `[a,b]` (no spaces), the pow generation as `V1` / `V2`, and signed results
with a leading `-`. `tests/oracle_crosscheck_balancer.rs` parses exactly this
shape.

Usage
-----
    just py python scripts/oracles/generate_balancer_snapshot.py           # write
    just py python scripts/oracles/generate_balancer_snapshot.py --check   # compare

The output path is named by `--out`, defaulting to
`target/oracle-regen/oracle_balancer_snapshot.txt`. The frozen snapshot under
`rust/crates/foundation/degenbot-math/tests/` is only ever read.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import balancer_fixed_point_reference as fixed_point
import balancer_log_exp_math_reference as log_exp_math
import balancer_stable_math_reference as stable_math
import balancer_weighted_math_reference as weighted_math
from evm_shim import EVMRevertError, PowVersion
from snapshot_io import run_generator

REPO_ROOT = Path(__file__).resolve().parents[2]
SNAPSHOT = REPO_ROOT / "rust/crates/foundation/degenbot-math/tests/oracle_balancer_snapshot.txt"

ONE = 10**18
VERSIONS = (("V1", PowVersion.V1), ("V2", PowVersion.V2))
VERSION_BY_TAG = dict(VERSIONS)

# ── fixture sweep ──

# FixedPoint operand pairs (zero, identity, sub-underflow, mul-overflow at 2^128
# squared, and a wide-baseline pair).
FP_PAIRS: list[tuple[int, int]] = [
    (0, 0),
    (ONE, ONE),
    (ONE, 0),
    (0, ONE),
    (1, ONE),
    (ONE, 10**24),
    (10**6, ONE),
    (2**128, 2**128),
    (10**36, ONE),
]

FP_BINARY_TAGS = (
    ("fp_add", fixed_point.add),
    ("fp_sub", fixed_point.sub),
    ("fp_mul_down", fixed_point.mul_down),
    ("fp_mul_up", fixed_point.mul_up),
    ("fp_div_down", fixed_point.div_down),
    ("fp_div_up", fixed_point.div_up),
)

# `complement` saturates at zero above one: below-ONE, exact-ONE, just above
# ONE, and the doubling values that all clamp.
FP_COMPLEMENT_VALUES = [0, 1, 5 * 10**17, ONE, ONE + 1, 2 * ONE, 3 * ONE, 4 * ONE, 5 * ONE]

# (base, exponent) for the fixed-point power sweep: the 0^0 case, the identity
# exponents, the `y == TWO / FOUR` fast-path bases, non-integral and large
# exponents, bases just above ONE, and a base whose product with the exponent
# only fits the 20-decimal intermediate.
FP_POW_CASES: list[tuple[int, int]] = [
    (0, 0),
    (ONE, ONE),
    (5 * 10**17, ONE),
    (5 * 10**17, 2 * ONE),
    (5 * 10**17, 4 * ONE),
    (ONE, 333_333_333_333_333_333),
    (ONE, 3 * ONE),
    (2 * ONE, 3 * ONE),
    (5 * 10**17, 5 * 10**17),
    (3 * ONE, 5 * 10**17),
    (25 * 10**17, ONE),
    (25 * 10**17, 2 * ONE),
    (25 * 10**17, 4 * ONE),
    (10 * ONE, 25 * 10**16),
    (10 * ONE, 2 * 10**17),
    (ONE + 1, ONE),
    (ONE + 1, 2 * ONE),
    (ONE + 1, 4 * ONE),
    (123_000_000_000_000_000, 5 * ONE),
    (10**17, ONE),
    (10**17, 0),
    (2**200, ONE),
]

# (base, signed exponent) for `LogExpMath.pow` — the direct path the WeightedMath
# leaves call, so it carries the negative-exponent and zero-operand edges.
LM_POW_CASES: list[tuple[int, int]] = [
    (2 * ONE, ONE),
    (2 * ONE, 5 * 10**17),
    (2 * ONE, 3 * ONE),
    (5 * 10**17, ONE),
    (10 * ONE, 5 * 10**17),
    (10 * ONE, ONE),
    (10 * ONE, -5 * 10**17),
    (10_000_000_000_000_000_001, ONE),
    (123_000_000_000_000_000, 5 * ONE),
    (100 * ONE, 5 * 10**17),
    (ONE, 0),
    (0, 5),
    (5, 0),
]

# The ln/exp sweep straddles the 36-decimal band (0.9 .. 1.1) that `ln` switches
# algorithms on, plus the ONE boundary the truncation is sensitive to.
LM_LOG_EXP_VALUES = [
    ONE,
    2 * ONE,
    5 * 10**17,
    10 * ONE,
    123_000_000_000_000_000,
    100 * ONE,
    10**17,
    ONE + 1,
    ONE - 1,
    ONE + 10**17,
    ONE - 10**17,
]

# (argument, base) for `LogExpMath.log`.
LM_LOG_CASES: list[tuple[int, int]] = [
    (8 * ONE, 2 * ONE),
    (100 * ONE, 10 * ONE),
    (5 * 10**17, 2 * ONE),
    (ONE, 10 * ONE),
]

# (normalized weights, balances) for the weighted invariant: equal weights, a
# 2-token 50/50, a 3-token asymmetric set, and an intentionally inexact pair.
WM_INVARIANT_CASES: list[tuple[list[int], list[int]]] = [
    ([ONE, ONE], [ONE, ONE]),
    ([5 * 10**17, 5 * 10**17], [2 * ONE, 3 * ONE]),
    ([2 * ONE, ONE], [ONE, 2 * ONE]),
    ([333_333_333_333_333_333] * 3, [10 * ONE, 20 * ONE, 30 * ONE]),
    (
        [5 * 10**17, 333_333_333_333_333_333, 166_666_666_666_666_666],
        [5 * ONE, 3 * ONE, 2 * ONE],
    ),
    ([250_000_000_000_000_001, 749_999_999_999_999_999], [100 * ONE, 100 * ONE]),
]

# (balance_in, weight_in, balance_out, weight_out, amount_in)
WM_OUT_GIVEN_IN_CASES: list[tuple[int, int, int, int, int]] = [
    (10**24, ONE, 10**24, ONE, 100 * ONE),
    (10**24, ONE, 5 * 10**23, ONE, 100 * ONE),
    (5 * 10**23, ONE, 10**24, ONE, 50 * ONE),
    (10**24, 5 * 10**17, 10**24, 5 * 10**17, 1000 * ONE),
    (10**24, ONE, 10**24, ONE, 1),
    (10**24, 2 * ONE, 5 * 10**23, ONE, 10 * ONE),
]

# (balance_in, weight_in, balance_out, weight_out, amount_out)
WM_IN_GIVEN_OUT_CASES: list[tuple[int, int, int, int, int]] = [
    (10**24, ONE, 10**24, ONE, 100 * ONE),
    (10**24, ONE, 5 * 10**23, ONE, 50 * ONE),
    (5 * 10**23, ONE, 10**24, ONE, 100 * ONE),
    (10**24, ONE, 10**24, ONE, 1),
    (10**24, 2 * ONE, 5 * 10**23, ONE, 10 * ONE),
    (10**24, ONE, 10**24, ONE, 100 * ONE - 1),
]

# (amount, fee_percentage) — the fee rounding direction is the whole point, so
# the sweep includes amounts below one wei and the fee bound itself.
WM_FEE_CASES: list[tuple[int, int]] = [
    (0, 10**16),
    (1, 10**16),
    (ONE, 10**16),
    (100 * ONE, 10**16),
    (10**24, 10**16),
    (5 * 10**17, 10**16),
]

# (amplification_parameter, balances) for the stable invariant: balanced,
# imbalanced, 3-token, 4-token uniform, and two small-amp sets.
SM_INVARIANT_CASES: list[tuple[int, list[int]]] = [
    (200_000, [10**24, 10**24]),
    (200_000, [10**24, 1_200_000_000_000_000_000_000_000]),
    (100_000, [10**20, 2 * 10**20, 3 * 10**20]),
    (50_000, [5 * ONE] * 4),
    (1_000_000, [ONE, 2 * ONE]),
    (200_000, [ONE, 2 * ONE, 3 * ONE]),
]

# (amp, balances, token_in, token_out, amount, invariant). The provided
# invariant is the pool's own: the 3-token rows carry the `P_D` round-up value
# the stable-swap path uses, so the amount solves against the deployed invariant.
SM_SWAP_BASE_CASES: list[tuple[int, list[int], int, int, int, int]] = [
    (200_000, [10**24, 10**24], 0, 1, 100 * ONE, 2 * 10**24),
    (
        200_000,
        [10**24, 1_200_000_000_000_000_000_000_000],
        0,
        1,
        50 * ONE,
        2_199_954_397_522_062_696_802_504,
    ),
    (
        100_000,
        [10**20, 2 * 10**20, 3 * 10**20],
        0,
        1,
        5 * ONE,
        599_342_806_940_310_079_563,
    ),
    (
        100_000,
        [10**20, 2 * 10**20, 3 * 10**20],
        2,
        0,
        5 * ONE,
        599_342_806_940_310_079_563,
    ),
    (
        200_000,
        [10**24, 2 * 10**24, 3 * 10**24],
        0,
        2,
        100 * ONE,
        5_996_690_543_963_657_169_377_973,
    ),
]


def render_list(values: list[int]) -> str:
    """Render a vector as the snapshot's `[a,b]` form.

    Args:
        values: The vector to render.

    Returns:
        The bracketed, comma-separated, space-free rendering.
    """
    return "[" + ",".join(str(v) for v in values) + "]"


def outcome(fn: object, *args: object, **kwargs: object) -> str:
    """Render one call's result or revert.

    Args:
        fn: The reference implementation to call.
        *args: Positional arguments.
        **kwargs: Keyword arguments.

    Returns:
        The decimal result, or `ERROR:<REASON>` when the reference reverts.
    """
    try:
        return str(fn(*args, **kwargs))
    except EVMRevertError as exc:
        return f"ERROR:{exc.error}"


def render() -> str:
    """Compute every fixture through the oracle reference.

    Returns:
        The snapshot body: one record per line, LF-terminated.
    """
    records: list[str] = []

    for a, b in FP_PAIRS:
        for tag, fn in FP_BINARY_TAGS:
            records.append(f"{tag} {a} {b} = {outcome(fn, a, b)}")

    for value in FP_COMPLEMENT_VALUES:
        records.append(f"fp_complement {value} = {fixed_point.complement(value)}")

    for base, exponent in FP_POW_CASES:
        for tag, fn in (
            ("fp_pow_down", fixed_point.pow_down),
            ("fp_pow_up", fixed_point.pow_up),
        ):
            for version_tag, version in VERSIONS:
                records.append(
                    f"{tag} {base} {exponent} {version_tag} = "
                    f"{outcome(fn, base, exponent, version=version)}"
                )

    for base, exponent in LM_POW_CASES:
        records.append(f"lm_pow {base} {exponent} = {outcome(log_exp_math.pow, base, exponent)}")

    for value in LM_LOG_EXP_VALUES:
        records.append(f"lm_ln {value} = {outcome(log_exp_math.ln, value)}")
        records.append(f"lm_exp {value} = {outcome(log_exp_math.exp, value)}")

    for argument, base in LM_LOG_CASES:
        records.append(f"lm_log {argument} {base} = {outcome(log_exp_math.log, argument, base)}")

    for weights, balances in WM_INVARIANT_CASES:
        for version_tag, version in VERSIONS:
            records.append(
                f"wm_invariant {render_list(weights)} {render_list(balances)} {version_tag} = "
                f"{outcome(weighted_math.calculate_invariant, weights, balances, version=version)}"
            )

    for balance_in, weight_in, balance_out, weight_out, amount_in in WM_OUT_GIVEN_IN_CASES:
        for version_tag, version in VERSIONS:
            records.append(
                f"wm_out_given_in {balance_in} {weight_in} {balance_out} {weight_out} "
                f"{amount_in} {version_tag} = "
                f"{outcome(weighted_math._calc_out_given_in, balance_in, weight_in, balance_out, weight_out, amount_in, version=version)}"
            )

    for balance_in, weight_in, balance_out, weight_out, amount_out in WM_IN_GIVEN_OUT_CASES:
        for version_tag, version in VERSIONS:
            records.append(
                f"wm_in_given_out {balance_in} {weight_in} {balance_out} {weight_out} "
                f"{amount_out} {version_tag} = "
                f"{outcome(weighted_math._calc_in_given_out, balance_in, weight_in, balance_out, weight_out, amount_out, version=version)}"
            )

    for amount, fee_percentage in WM_FEE_CASES:
        for tag, fn in (
            ("wm_sub_fee", weighted_math._subtract_swap_fee_amount),
            ("wm_add_fee", weighted_math._add_swap_fee_amount),
        ):
            records.append(
                f"{tag} {amount} {fee_percentage} = {outcome(fn, amount, fee_percentage)}"
            )

    for amp, balances in SM_INVARIANT_CASES:
        records.append(
            f"sm_invariant_d_p {amp} {render_list(balances)} = "
            f"{outcome(stable_math._calculate_invariant, amp, list(balances))}"
        )
        for tag, round_up in (("sm_invariant_p_d_up", True), ("sm_invariant_p_d_down", False)):
            records.append(
                f"{tag} {amp} {render_list(balances)} = "
                f"{outcome(stable_math._calculate_invariant_deployed, amp, list(balances), round_up=round_up)}"
            )

    for amp, balances, token_in, token_out, amount, invariant in SM_SWAP_BASE_CASES:
        records.append(
            f"sm_out_given_in {amp} {render_list(balances)} {token_in} {token_out} "
            f"{amount} {invariant} = "
            f"{outcome(stable_math._calc_out_given_in, amp, list(balances), token_in, token_out, amount, invariant)}"
        )

    for amp, balances, token_in, token_out, amount, invariant in SM_SWAP_BASE_CASES:
        records.append(
            f"sm_in_given_out {amp} {render_list(balances)} {token_in} {token_out} "
            f"{amount} {invariant} = "
            f"{outcome(stable_math._calc_in_given_out, amp, list(balances), token_in, token_out, amount, invariant)}"
        )

    return "".join(f"{record}\n" for record in records)


if __name__ == "__main__":
    sys.exit(run_generator(sys.argv, snapshot=SNAPSHOT, render=render))
