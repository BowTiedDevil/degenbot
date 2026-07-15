"""Engine-facing arbitrage hop descriptors (Plan 102, slice 3).

The :class:`degenbot.arbitrage.solvers.hop_types.Solver` family uses its own
``HopType``/``BoundedProductHop`` shape for *solving*. This module holds a
distinct, **engine-facing** shape: the hop descriptors
``EngineRegistry.register_path`` builds from concrete pool objects to hand to
the Rust :class:`~degenbot._ffi.UniswapArbEngine` (and that the example's
``encode_cmd_stream`` reads back to build the on-chain command stream).

Lifted verbatim from ``examples/eth_backrun_helpers.py`` — frozen dataclasses
reading only pool attributes, so they carry no deployment-specific policy.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

if TYPE_CHECKING:
    from collections.abc import Sequence
    from fractions import Fraction
    from typing import Any

__all__ = [
    "HopInfo",
    "PathInfo",
    "SolidlyHopInfo",
    "V2HopInfo",
    "V3HopInfo",
    "V4HopInfo",
    "build_hops_from_pools",
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


def build_hops_from_pools(
    pools_and_zfos: Sequence[tuple[Any, bool]],
) -> list[HopInfo]:
    """Build hop descriptors from concrete pool objects + directions.

    Replaces caller-side per-hop dict construction by reading attributes
    directly off the pool objects. The V2 hop's directional fee is derived
    from ``fee_token0`` (``zfo=True``) / ``fee_token1`` (``zfo=False``),
    scaled to bips-of-10000.

    Returns:
        One :class:`HopInfo` per supplied ``(pool, zfo)`` pair.

    Raises:
        TypeError: If a pool is not a V2/V3/V4 pool instance.

    """
    hops: list[HopInfo] = []
    for pool, zfo in pools_and_zfos:
        # Aerodrome V2 pools (any stable/volatile mode) route to the Solidly
        # solve branch — checked before the V2 branch since AerodromeV2Pool is
        # NOT a UniswapV2Pool subclass.
        if isinstance(pool, AerodromeV2Pool):
            hops.append(
                SolidlyHopInfo(
                    pool_address=pool.address,
                    token0_address=pool.token0.address,
                    token1_address=pool.token1.address,
                    fee=pool.fee_token0 if zfo else pool.fee_token1,
                    stable=pool.stable,
                    variant=("aerodrome-v2-stable" if pool.stable else "aerodrome-v2-volatile"),
                    zfo=zfo,
                ),
            )
        elif isinstance(pool, UniswapV2Pool):
            # Camelot ``stable_swap`` mode routes to the Solidly solve branch;
            # regular V2 (and Camelot volatile) stays V2 (constant-product).
            if getattr(pool, "stable_swap", False):
                hops.append(
                    SolidlyHopInfo(
                        pool_address=pool.address,
                        token0_address=pool.token0.address,
                        token1_address=pool.token1.address,
                        fee=pool.fee_token0 if zfo else pool.fee_token1,
                        stable=True,
                        variant="camelot-v2-stable",
                        zfo=zfo,
                    ),
                )
            else:
                hops.append(
                    V2HopInfo(
                        pool_address=pool.address,
                        token0_address=pool.token0.address,
                        token1_address=pool.token1.address,
                        fee=int((pool.fee_token0 if zfo else pool.fee_token1) * 10000),
                        zfo=zfo,
                    ),
                )
        elif isinstance(pool, UniswapV3Pool):
            hops.append(
                V3HopInfo(
                    pool_address=pool.address,
                    token0_address=pool.token0.address,
                    token1_address=pool.token1.address,
                    fee=pool.fee,
                    zfo=zfo,
                ),
            )
        elif isinstance(pool, UniswapV4Pool):
            hops.append(
                V4HopInfo(
                    pool_manager_address=pool.address,
                    pool_id_hex=pool.pool_id.to_0x_hex(),
                    currency0_address=pool.token0.address,
                    currency1_address=pool.token1.address,
                    fee=pool.fee,
                    tick_spacing=pool.tick_spacing,
                    hook_address=pool.hook_address,
                    zfo=zfo,
                ),
            )
        else:
            msg = f"Unsupported pool type: {type(pool).__name__}"
            raise TypeError(msg)
    return hops
