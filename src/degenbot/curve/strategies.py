"""Resolved calculation strategies for a Curve pool instance.

The pure dy math now lives in the Rust core (``degenbot-curve-math``); the
Python companion only carries the resolved enum discriminants (``swap_style``,
``d_variant`` / ``y_variant`` / ``yd_variant``, lending + metapool rate styles)
so it can pass them to the Rust handle / builders. The former Python
``DyCalculator`` classes (``degenbot.curve.calculators``) were retired once the
swap path became Rust-owned (epic ``TV72EG``, task ``WKKMJM``) — nothing in the
runtime constructs or consumes a Python calculator anymore.
"""

from __future__ import annotations

import dataclasses

from degenbot.curve.types import (
    DVariant,
    LendingRateStyle,
    MetapoolRateStyle,
    MetapoolUnderlyingStyle,
    SwapStyle,
    YDVariant,
    YVariant,
)


@dataclasses.dataclass(slots=True, frozen=True)
class PoolStrategies:
    """Resolved calculation strategies for a Curve pool instance.

    Set at construction time by the builder from the pool address. The pool
    class is address-agnostic — it only reads these strategy values and passes
    them to the Rust core. The Rust core derives its math axes from the same
    enum discriminants, so no Python calculator is auto-constructed here.
    """

    d_variant: DVariant = DVariant.STANDARD
    y_variant: YVariant = YVariant.STANDARD
    yd_variant: YDVariant = YDVariant.STANDARD
    swap_style: SwapStyle = SwapStyle.STANDARD
    metapool_rate_style: MetapoolRateStyle = MetapoolRateStyle.STANDARD
    metapool_underlying_style: MetapoolUnderlyingStyle = MetapoolUnderlyingStyle.STANDARD
    lending_rate_style: LendingRateStyle = LendingRateStyle.NONE
