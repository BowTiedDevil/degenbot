"""Shared types: AddressComparable, state caches, pool enums, and aliases."""

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
from .pool_pickle import PoolPickleMixin
from .pool_protocols import (
    ArbitrageCapablePool,
    ArbitragePathPool,
    MultiTokenSwapCalculation,
    PoolSimulation,
    ReverseSimulatablePool,
    SimulationResult,
    StateManageablePool,
    TwoTokenSwapCalculation,
)

__all__ = (
    "AddressComparable",
    "ArbitrageCapablePool",
    "ArbitragePathPool",
    "BalancerMultiTokenHop",
    "BalancerWeightedHop",
    "BoundedCache",
    "BoundedProductHop",
    "ConstantProductHop",
    "CurveStableswapHop",
    "HopType",
    "KeyedDefaultDict",
    "MultiTokenSwapCalculation",
    "PoolInvariant",
    "PoolPickleMixin",
    "PoolSimulation",
    "ReverseSimulatablePool",
    "SimulationResult",
    "SolidlyStableHop",
    "StateManageablePool",
    "TwoTokenSwapCalculation",
    "V3TickRangeInfo",
)
