"""Aave-specific type definitions (rate mode, position enums)."""

from enum import Enum, auto


class TokenType(Enum):
    """Token types in Aave V3."""

    A_TOKEN = auto()
    V_TOKEN = auto()
    GHO_DISCOUNT = auto()
