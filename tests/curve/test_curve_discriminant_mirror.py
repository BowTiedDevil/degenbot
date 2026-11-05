"""The Curve strategy-enum discriminants are a stored contract, not a local choice.

A pool identity stores one byte per axis and the Rust core owns the numbers
(``degenbot_math::curve`` enumerates them; ``degenbot_pools::sim
::curve_strategies`` resolves a pool address onto those names). The driver's
``degenbot.curve.types`` mirror must agree byte for byte: the pool rebuilds the
enum from the stored byte (``DVariant(arm.d_variant)``) and the calc path
compares members, so a renumbered member would silently mis-dispatch a live
pool's math.

These expectations are the Rust discriminants restated. They are literals, not
derived from the enums, which is the point: editing an enum's value has to edit
this table too, in the same review.
"""

import pytest

from degenbot.curve.types import (
    DVariant,
    LendingRateStyle,
    MetapoolRateStyle,
    MetapoolUnderlyingStyle,
    SwapStyle,
    YDVariant,
    YVariant,
)


@pytest.mark.parametrize(
    ("enum", "expected"),
    [
        (
            DVariant,
            {
                "STANDARD": 1,
                "VARIANT_ALPHA": 2,
                "VARIANT_ALPHA_DP_ALPHA": 3,
                "VARIANT_DP_ALPHA": 4,
                "VARIANT_BETA_DP": 5,
                "VARIANT_GAMMA_DP": 6,
            },
        ),
        (
            YVariant,
            {
                "STANDARD": 1,
                "VARIANT_0": 2,
                "VARIANT_1": 3,
            },
        ),
        (
            YDVariant,
            {
                "STANDARD": 1,
                "VARIANT_0": 2,
            },
        ),
        (
            SwapStyle,
            {
                "STANDARD": 1,
                "RATE_ADJUSTED": 2,
                "RAW_BALANCE": 3,
                "CRYPTO": 4,
                "LIVE_ADMIN": 5,
                "LIVE_ADMIN_DYNAMIC": 6,
                "LIVE_ADMIN_DYNAMIC_PRECISION": 7,
                "LIVE_ADMIN_ORACLE": 8,
                "NO_ONE_FEE_RATE": 9,
                "CYTOKEN": 10,
                "RATE_ADJUSTED_NO_ONE": 11,
            },
        ),
        (
            LendingRateStyle,
            {
                "NONE": 1,
                "CTOKEN": 2,
                "YTOKEN": 3,
                "CYTOKEN": 4,
                "AETH": 5,
                "RETH": 6,
                "ORACLE": 7,
            },
        ),
        (
            MetapoolRateStyle,
            {
                "STANDARD": 1,
                "PRECISION_VP": 2,
                "REDEMPTION_VP": 3,
            },
        ),
        (
            MetapoolUnderlyingStyle,
            {
                "STANDARD": 1,
                "REDEMPTION": 2,
                "PRECISION_VP": 3,
            },
        ),
    ],
    ids=lambda value: value.__name__ if isinstance(value, type) else None,
)
def test_strategy_enum_discriminants_are_the_stored_bytes(
    enum: type[DVariant | YVariant | YDVariant | SwapStyle | LendingRateStyle | MetapoolRateStyle],
    expected: dict[str, int],
) -> None:
    """Every member keeps the byte the pool identity stores for that axis."""
    assert {member.name: member.value for member in enum} == expected
