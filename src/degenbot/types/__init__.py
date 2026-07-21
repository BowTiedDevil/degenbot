"""Shared types: AddressComparable, state caches, pool enums, and aliases."""

from degenbot._ffi import PyLiquidityPool
from degenbot._ffi.dex_identity import PyDexIdentity as DexIdentity
from degenbot._ffi.dex_identity import dex_identity

from .address_comparable import AddressComparable
from .concrete import BoundedCache, KeyedDefaultDict
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
    "BoundedCache",
    "DexIdentity",
    "KeyedDefaultDict",
    "MultiTokenSwapCalculation",
    "PoolSimulation",
    "PyLiquidityPool",
    "ReverseSimulatablePool",
    "SimulationResult",
    "StateManageablePool",
    "TwoTokenSwapCalculation",
    "dex_identity",
)
