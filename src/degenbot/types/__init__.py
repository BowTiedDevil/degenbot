"""Shared types: AddressComparable, state caches, pool enums, and aliases."""

from degenbot._ffi import PyLiquidityPool
from degenbot._ffi.dex_identity import PyDexIdentity as DexIdentity
from degenbot._ffi.dex_identity import dex_identity

from .address_comparable import AddressComparable
from .concrete import BoundedCache, KeyedDefaultDict
from .hop_types import (
    BalancerMultiTokenHop,
    BalancerWeightedHop,
    BoundedProductHop,
    ConstantProductHop,
    CurveStableswapHop,
    HopType,
    PoolInvariant,
    SolidlyStableHop,
    V3TickRangeInfo,
)
from .pool_protocols import (
    MultiTokenSwapCalculation,
    PoolSimulation,
    ReverseSimulatablePool,
    SimulationResult,
    StateManageablePool,
    TwoTokenSwapCalculation,
)

__all__ = (
    "AddressComparable",
    "BalancerMultiTokenHop",
    "BalancerWeightedHop",
    "BoundedCache",
    "BoundedProductHop",
    "ConstantProductHop",
    "CurveStableswapHop",
    "DexIdentity",
    "HopType",
    "KeyedDefaultDict",
    "MultiTokenSwapCalculation",
    "PoolInvariant",
    "PoolSimulation",
    "PyLiquidityPool",
    "ReverseSimulatablePool",
    "SimulationResult",
    "SolidlyStableHop",
    "StateManageablePool",
    "TwoTokenSwapCalculation",
    "V3TickRangeInfo",
    "dex_identity",
)
