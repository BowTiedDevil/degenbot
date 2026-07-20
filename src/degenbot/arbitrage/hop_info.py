"""Engine-facing arbitrage hop descriptors (display-only, NXM2BF).

These dataclasses are the **render shape** for the ``[profit]`` hop-detail
log: ``PyDispatchOutcome.path_infos`` reconstructs them from the Rust
``composers::HopInfo`` via the PyO3 ``path_info_to_py`` converter
(``degenbot-python/src/executor/mod.rs``) — a one-directional Rust→Python
reconstruction. The build-side Python relay (``build_hops_from_pools`` → store
on ``EngineRegistry.paths`` → re-extract via ``extract_path_info``) is retired:
``PyDispatchCandidate`` resolves its ``composers::PathInfo`` from a registered
``path_id`` via ``PyArbitrageEngine.path_info_for_core`` (Rust-side, over the
shared ``BotState``).

A follow-up (ergo ``WEFVGE``) switches rendering to plain dicts + deletes these
dataclasses; until then they are the display type the render path feeds.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from fractions import Fraction

__all__ = [
    "HopInfo",
    "PathInfo",
    "SolidlyHopInfo",
    "V2HopInfo",
    "V3HopInfo",
    "V4HopInfo",
]


@dataclasses.dataclass(frozen=True)
class V2HopInfo:
    """Engine-facing descriptor for a V2 hop in an arbitrage path."""

    pool_address: str
    token0_address: str
    token1_address: str
    fee: int  # fee as fraction of 10000 (e.g. 30 for 0.3%)
    zfo: bool


@dataclasses.dataclass(frozen=True)
class V3HopInfo:
    """Engine-facing descriptor for a V3 hop in an arbitrage path."""

    pool_address: str
    token0_address: str
    token1_address: str
    fee: int
    zfo: bool


@dataclasses.dataclass(frozen=True)
class V4HopInfo:
    """Engine-facing descriptor for a V4 hop in an arbitrage path."""

    pool_manager_address: str
    pool_id_hex: str
    currency0_address: str
    currency1_address: str
    fee: int
    tick_spacing: int
    hook_address: str
    zfo: bool


@dataclasses.dataclass(frozen=True)
class SolidlyHopInfo:
    """Engine-facing descriptor for a Solidly/Aerodrome/Camelot hop.

    Aerodrome V2 pools (stable or volatile) and Camelot V2 pools in
    ``stable_swap`` mode route to the Solidly solve branch. The Rust engine's
    ``derive_hop_type`` discriminates by reading the ``BotState`` pool identity
    at ``register_path`` time, so this descriptor is **informational** for the
    encoder / ``path_type`` — the engine's pool key lookup goes through the
    same ``register_path`` ``(pool_id, zero_for_one)`` tuple path as V2/V3/V4.
    """

    pool_address: str
    token0_address: str
    token1_address: str
    # Per-direction fee as a Fraction (Solidly fees use arbitrary
    # denominators, so bips-of-10000 — V2HopInfo's convention — don't fit).
    fee: Fraction
    # True for the stable (x³y + xy³) invariant, False for volatile.
    stable: bool
    # Kebab-case DexVariant (``aerodrome-v2-stable``, ``camelot-v2-stable``
    # etc.) — selects the solidly-math leaf at solve time.
    variant: str
    zfo: bool


HopInfo = V2HopInfo | V3HopInfo | V4HopInfo | SolidlyHopInfo


@dataclasses.dataclass
class PathInfo:
    """An arbitrage path's ordered hops (`path_type` derives the V2/V3/V4 mix)."""

    hops: list[HopInfo]

    @property
    def path_type(self) -> str:
        """Combined pool types: 'V3-V2', 'V3-V3', 'V2-V2', 'V4-V3', etc."""
        type_names = []
        for h in self.hops:
            if isinstance(h, V2HopInfo):
                type_names.append("V2")
            elif isinstance(h, V3HopInfo):
                type_names.append("V3")
            elif isinstance(h, V4HopInfo):
                type_names.append("V4")
            elif isinstance(h, SolidlyHopInfo):
                type_names.append("Solidly")
        return "-".join(type_names)
