"""Regenerate the Curve `StableSwap` oracle snapshot.

Authority
---------
The oracle math is the retired Python reference implementation, restored
verbatim beside this file (`curve_stableswap_reference.py`). That reference is
a direct port of the deployed Vyper pools: `_get_d`, `_get_y`, `_get_y_d`,
`_newton_y` and `_reduction_coefficient` (variant axes documented in
`docs/architecture/curve-stableswap-reference.md`). It imports nothing from the
Rust leaf it checks.

Row format
----------
One record per line, LF-terminated, no blank lines, no trailing newline after
the last record's LF:

    <tag> <args...> = <result>
    <tag> <args...> = ERROR:<REASON>

`<args...>` is the tag's positional argument list, with balance vectors rendered
as `[a,b,c]` (no spaces) and variant enums as their 1-based `u8` discriminants.
`tests/oracle_crosscheck_curve.rs` parses exactly this shape.

Usage
-----
    just py python scripts/oracles/generate_curve_snapshot.py           # write
    just py python scripts/oracles/generate_curve_snapshot.py --check   # compare

The output path is named by `--out`, defaulting to
`target/oracle-regen/oracle_curve_snapshot.txt`. The frozen snapshot under
`rust/crates/foundation/degenbot-math/tests/` is only ever read.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import curve_stableswap_reference as curve
from snapshot_io import run_generator

REPO_ROOT = Path(__file__).resolve().parents[2]
SNAPSHOT = REPO_ROOT / "rust/crates/foundation/degenbot-math/tests/oracle_curve_snapshot.txt"

# ── fixture sweep ──
# Balance vectors are xp (balances already scaled to 18 decimals) and `n_coins`
# is their length. The `get_d` rows name the D variant they exercise, and the
# `get_y_d` rows reuse each pool's own D, so a variant's D and its Y_D stay
# paired.

A_PRECISION = 100

THREE_STD = [1_500_000_000_000_000_000_000_000] * 3
THREE_AMPLIFIED = [
    1_500_000_000_000_000_000_000_000,
    1_600_000_000_000_000_000_000_000,
    1_400_000_000_000_000_000_000_000,
]
THREE_SKEWED = [
    5_000_000_000_000_000_000_000_000,
    9_000_000_000_000_000_000_000_000,
    7_000_000_000_000_000_000_000_000,
]
TWO_POOL = [10_000_000_000_000_000_000_000_000, 10_000_000_000_000]

# (tag, xp, amp, d_variant)
D_CASES: list[tuple[str, list[int], int, curve.DVariant]] = [
    ("3chain-dstd", THREE_STD, 2000, curve.DVariant.STANDARD),
    ("3chain-dalpha", THREE_STD, 2000, curve.DVariant.VARIANT_ALPHA),
    ("3chain-dgamma", THREE_AMPLIFIED, 1000, curve.DVariant.VARIANT_GAMMA_DP),
    ("2pool-dstd", TWO_POOL, 2000, curve.DVariant.STANDARD),
    ("3chain-skew", THREE_SKEWED, 500, curve.DVariant.STANDARD),
]

# (tag, i, j, x, xp, amp, y_variant, d_variant)
Y_CASES: list[tuple[str, int, int, int, list[int], int, curve.YVariant, curve.DVariant]] = [
    (
        "3-ystd",
        0,
        1,
        1_400_000_000_000_000_000_000_000,
        THREE_STD,
        2000,
        curve.YVariant.STANDARD,
        curve.DVariant.STANDARD,
    ),
    (
        "3-yv0",
        0,
        1,
        1_400_000_000_000_000_000_000_000,
        THREE_STD,
        2000,
        curve.YVariant.VARIANT_0,
        curve.DVariant.STANDARD,
    ),
    (
        "2-ystd",
        0,
        1,
        9_500_000_000_000,
        TWO_POOL,
        2000,
        curve.YVariant.STANDARD,
        curve.DVariant.STANDARD,
    ),
    (
        "3-yv1",
        2,
        0,
        1_300_000_000_000_000_000_000_000,
        [
            1_500_000_000_000_000_000_000_000,
            1_600_000_000_000_000_000_000_000,
            1_500_000_000_000_000_000_000_000,
        ],
        1000,
        curve.YVariant.VARIANT_1,
        curve.DVariant.VARIANT_ALPHA,
    ),
]

Y_D_VARIANTS = (curve.YDVariant.STANDARD, curve.YDVariant.VARIANT_0)

# `newton_y` has one fixed fixture in the snapshot: the crypto-pool solver's
# parameter set is carried by the test, so its row records only the balance
# vector it is solved against.
NEWTON_Y = {
    "ann": 2_700_000,
    "gamma": 10**14,
    "xp": [300_000_000_000_000_000_000, 500_000_000_000_000_000_000, 400_000_000_000_000_000_000],
    "d": 4 * 10**18 * 100,
    "token_index": 0,
    "n_coins": 3,
    "a_multiplier": A_PRECISION,
}

# (xp, fee_gamma, n_coins) — the dynamic-fee case and the `fee_gamma == 0` case.
DYNAMIC_FEE_CASES: list[tuple[list[int], int, int]] = [
    (NEWTON_Y["xp"], 10_000_000_000, 3),
    (NEWTON_Y["xp"], 0, 3),
]


def render_list(values: list[int]) -> str:
    """Render a balance vector as the snapshot's `[a,b,c]` form.

    Args:
        values: The vector to render.

    Returns:
        The bracketed, comma-separated, space-free rendering.
    """
    return "[" + ",".join(str(v) for v in values) + "]"


def render() -> str:
    """Compute every fixture through the oracle reference.

    Returns:
        The snapshot body: one record per line, LF-terminated.
    """
    records: list[str] = []

    d_results: list[tuple[str, list[int], int, curve.DVariant, int]] = []
    for tag, xp, amp, d_variant in D_CASES:
        d = curve.stableswap_get_d(xp, amp, len(xp), A_PRECISION, d_variant)
        d_results.append((tag, xp, amp, d_variant, d))
        records.append(
            f"get_d {tag} {render_list(xp)} {amp} {len(xp)} {A_PRECISION} {d_variant.value} = {d}"
        )

    for tag, i, j, x, xp, amp, y_variant, d_variant in Y_CASES:
        y = curve.stableswap_get_y(
            i,
            j,
            x=x,
            xp=xp,
            amp=amp,
            n_coins=len(xp),
            a_precision=A_PRECISION,
            y_variant=y_variant,
            d_variant=d_variant,
        )
        records.append(
            f"get_y {tag} {i} {j} {x} {render_list(xp)} {amp} {len(xp)} "
            f"{A_PRECISION} {y_variant.value} {d_variant.value} = {y}"
        )

    for tag, xp, amp, _d_variant, d in d_results:
        for i in range(len(xp)):
            for yd_variant in Y_D_VARIANTS:
                y = curve.stableswap_get_y_d(
                    amp,
                    i,
                    xp=xp,
                    d=d,
                    n_coins=len(xp),
                    a_precision=A_PRECISION,
                    yd_variant=yd_variant,
                )
                records.append(
                    f"get_y_d {tag} {amp} {i} {render_list(xp)} {d} {len(xp)} "
                    f"{A_PRECISION} {yd_variant.value} = {y}"
                )

    newton = NEWTON_Y
    y = curve.stableswap_newton_y(
        newton["ann"],
        newton["gamma"],
        xp=newton["xp"],
        d=newton["d"],
        token_index=newton["token_index"],
        n_coins=newton["n_coins"],
        a_multiplier=newton["a_multiplier"],
    )
    records.append(f"newton_y {render_list(newton['xp'])} = {y}")

    for xp, fee_gamma, n_coins in DYNAMIC_FEE_CASES:
        k = curve.stableswap_reduction_coefficient(xp, fee_gamma, n_coins)
        records.append(f"reduction {render_list(xp)} {fee_gamma} {n_coins} = {k}")

    return "".join(f"{record}\n" for record in records)


if __name__ == "__main__":
    sys.exit(run_generator(sys.argv, snapshot=SNAPSHOT, render=render))
