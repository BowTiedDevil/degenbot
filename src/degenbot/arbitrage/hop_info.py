"""Engine-facing arbitrage hop descriptors (Plan 102, slice 3).

The :class:`degenbot.arbitrage.solvers.hop_types.Solver` family uses its own
``HopType``/``BoundedProductHop`` shape for *solving*. This module holds a
distinct, **engine-facing** shape: the hop descriptors
``EngineRegistry.register_path`` builds from concrete pool objects to hand to
the Rust :class:`~degenbot.degenbot_rs.UniswapArbEngine` (and that the example's
``encode_cmd_stream`` reads back to build the on-chain command stream).

Lifted verbatim from ``examples/eth_backrun_helpers.py`` — frozen dataclasses
reading only pool attributes, so they carry no deployment-specific policy.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

if TYPE_CHECKING:
    from collections.abc import Sequence
    from typing import Any

__all__ = [
    "HopInfo",
    "PathInfo",
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


HopInfo = V2HopInfo | V3HopInfo | V4HopInfo


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
        if isinstance(pool, LiquidityPool):
            hops.append(
                V2HopInfo(
                    pool_address=pool.address,
                    token0_address=pool.token0.address,
                    token1_address=pool.token1.address,
                    fee=int((pool.fee_token0 if zfo else pool.fee_token1) * 10000),
                    zfo=zfo,
                )
            )
        elif isinstance(pool, UniswapV3Pool):
            hops.append(
                V3HopInfo(
                    pool_address=pool.address,
                    token0_address=pool.token0.address,
                    token1_address=pool.token1.address,
                    fee=pool.fee,
                    zfo=zfo,
                )
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
                )
            )
        else:
            msg = f"Unsupported pool type: {type(pool).__name__}"
            raise TypeError(msg)
    return hops
