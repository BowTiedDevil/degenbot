"""Chainlink price feed oracle contracts."""

from degenbot._ffi import PyChainlinkPriceFeed as ChainlinkPriceFeed
from degenbot.chainlink.price_feed import ChainlinkPriceContract

__all__ = ["ChainlinkPriceContract", "ChainlinkPriceFeed"]
