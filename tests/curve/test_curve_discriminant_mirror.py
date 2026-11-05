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
from tests.helpers.seam import Bot, DyCalculationInputs


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


# The byte table above is a WIRE vocabulary: every byte the driver sends is
# decoded once, at the FFI boundary that receives it, and the core then stores
# typed axes. An unmapped byte therefore fails where it arrives (loudly, naming
# the axis) instead of defaulting to a style the pool never declared.

# The seven axes, each with a byte the declaration above does not name.
UNMAPPED_AXES: tuple[tuple[str, int], ...] = (
    ("swap_style", 0),
    ("swap_style", 12),
    ("lending_rate_style", 0),
    ("lending_rate_style", 8),
    ("d_variant", 0),
    ("d_variant", 7),
    ("y_variant", 0),
    ("y_variant", 4),
    ("yd_variant", 0),
    ("yd_variant", 3),
    ("metapool_rate_style", 0),
    ("metapool_rate_style", 4),
    ("metapool_underlying_style", 0),
    ("metapool_underlying_style", 4),
)


# The builder's snapshot carries the axes the calculator dispatches on; the two
# y-iteration axes it does not read (`lending_rate_style` is resolved into
# `resolved_rates` before the snapshot, `yd_variant` only enters the
# `get_y_d` step primitives the calculator does not call) have no setter.
BUILDER_AXES: tuple[tuple[str, int], ...] = tuple(
    (axis, byte) for axis, byte in UNMAPPED_AXES if axis not in {"lending_rate_style", "yd_variant"}
)


@pytest.mark.parametrize(("axis", "byte"), BUILDER_AXES)
def test_the_dy_inputs_builder_refuses_an_unmapped_axis_byte(axis: str, byte: int) -> None:
    """The ``curve_dy`` builder setter keeps its ``u8`` FFI signature and is the
    one conversion site: a byte outside the declaration raises instead of
    selecting a style."""
    inputs = DyCalculationInputs()
    with pytest.raises(ValueError, match=axis):
        setattr(inputs, axis, byte)


@pytest.mark.parametrize(("axis", "byte"), UNMAPPED_AXES)
def test_the_registration_door_refuses_an_unmapped_axis_byte(axis: str, byte: int) -> None:
    """``register_curve_pool`` carries the same seven bytes and decodes them at
    the door, so a registration cannot store an axis it never named."""
    bot = Bot()
    kwargs: dict[str, object] = {
        "address": "0x" + "11" * 20,
        "tokens": ["0x" + "01" * 20, "0x" + "02" * 20],
        "a_coefficient": 100,
        "a_precision": 100,
        "fee": 4_000_000,
        "admin_fee": 0,
        "rate_multipliers": [10**18, 10**18],
        "balances": [10**21, 10**21],
        "update_block": 0,
    }
    kwargs[axis] = byte
    with pytest.raises(ValueError, match=axis):
        bot.register_curve_pool(**kwargs)


@pytest.mark.parametrize(
    ("axis", "expected"),
    [
        ("swap_style", SwapStyle.STANDARD.value),
        ("lending_rate_style", LendingRateStyle.NONE.value),
        ("d_variant", DVariant.STANDARD.value),
        ("metapool_rate_style", MetapoolRateStyle.STANDARD.value),
        ("metapool_underlying_style", MetapoolUnderlyingStyle.STANDARD.value),
    ],
)
def test_a_defaulted_registration_stores_the_standard_axis(axis: str, expected: int) -> None:
    """The door's own kwarg defaults are real discriminants (the 1-based
    ``STANDARD``/``NONE`` members), so a caller that names no strategies
    registers the standard pool instead of byte 0 — which names no axis."""
    bot = Bot()
    pool_id = bot.register_curve_pool(
        address="0x" + "12" * 20,
        tokens=["0x" + "01" * 20, "0x" + "02" * 20],
        a_coefficient=100,
        a_precision=100,
        fee=4_000_000,
        admin_fee=0,
        rate_multipliers=[10**18, 10**18],
        balances=[10**21, 10**21],
        update_block=0,
    )
    handle = bot.get_pool(pool_id)
    assert handle is not None
    assert getattr(handle.curve(), axis) == expected


def test_the_y_axes_come_back_as_enum_member_names() -> None:
    """The two y-iteration axes answer with the member NAME the driver's enum
    spells, so a rebuild is a by-name lookup (``YVariant[name]``) that raises on
    drift instead of constructing a wrong member from a stale byte."""
    bot = Bot()
    pool_id = bot.register_curve_pool(
        address="0x" + "13" * 20,
        tokens=["0x" + "01" * 20, "0x" + "02" * 20],
        a_coefficient=100,
        a_precision=100,
        fee=4_000_000,
        admin_fee=0,
        rate_multipliers=[10**18, 10**18],
        balances=[10**21, 10**21],
        update_block=0,
        y_variant=YVariant.VARIANT_1.value,
        yd_variant=YDVariant.VARIANT_0.value,
    )
    handle = bot.get_pool(pool_id)
    assert handle is not None
    axis = handle.curve()
    assert axis.y_variant == "VARIANT_1"
    assert axis.yd_variant == "VARIANT_0"
    assert YVariant[axis.y_variant] is YVariant.VARIANT_1
    assert YDVariant[axis.yd_variant] is YDVariant.VARIANT_0
