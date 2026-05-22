"""ERC-20 token with on-chain metadata and balance tracking."""

from .erc20 import UNKNOWN_DECIMALS, UNKNOWN_NAME, UNKNOWN_SYMBOL, Erc20Token
from .ether_placeholder import EtherPlaceholder

__all__ = (
    "UNKNOWN_DECIMALS",
    "UNKNOWN_NAME",
    "UNKNOWN_SYMBOL",
    "Erc20Token",
    "EtherPlaceholder",
)
