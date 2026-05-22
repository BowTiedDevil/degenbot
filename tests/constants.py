"""
Centralized mainnet addresses and constants shared across test files.

Replaces the duplicated address strings previously declared in individual
conftest.py files and test modules.
"""

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import WRAPPED_NATIVE_TOKENS

# Wrapped native tokens by chain
WETH_ETH = WRAPPED_NATIVE_TOKENS[1]

# Common ERC-20 tokens on Ethereum mainnet
WBTC_ETH = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
DAI_ETH = get_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
USDC_ETH = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
USDT_ETH = get_checksum_address("0xdAC17F958D2ee523a22062069945771353116FA3")

# Uniswap V2 factory on Ethereum mainnet
UNISWAP_V2_FACTORY_ETH = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")

# Uniswap V3 factory on Ethereum mainnet
UNISWAP_V3_FACTORY_ETH = get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")

# Well-known pool addresses on Ethereum mainnet
UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
UNISWAP_V3_WBTC_WETH_POOL = get_checksum_address("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD")
