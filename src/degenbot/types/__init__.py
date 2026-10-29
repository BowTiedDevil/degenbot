"""Shared types: AddressComparable, state caches, pool enums, and aliases."""

from degenbot._ffi import Pool
from degenbot._ffi.dex_identity import DexIdentity, dex_identity

from .address_comparable import AddressComparable
from .concrete import BoundedCache, KeyedDefaultDict
from .pool_protocols import (
    MultiTokenSwapCalculation,
    MultiTokenSwapQuote,
    PoolSimulation,
    StateManageablePool,
    TwoTokenSwapCalculation,
    TwoTokenSwapQuote,
)

__all__ = (
    "AddressComparable",
    "BoundedCache",
    "DexIdentity",
    "KeyedDefaultDict",
    "MultiTokenSwapCalculation",
    "MultiTokenSwapQuote",
    "Pool",
    "PoolSimulation",
    "StateManageablePool",
    "TwoTokenSwapCalculation",
    "TwoTokenSwapQuote",
    "dex_identity",
)
