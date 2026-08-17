"""Shared runtime configuration constants for the settlement-arbitrage ``BotRunner``.

Central home for the driver's module-level tunables and deployment addresses,
extracted from ``examples/eth_backrun_v2_v3_v4_rust.py`` (epic 5TSYKN). Each
``degenbot.runner`` module imports what it needs from here instead of the
example, so the example can be thinned to an entrypoint.

Note: the *canonical defaults* for the same tunables live in
:class:`degenbot.runner.config.ArbitrageConfig` (the frozen config value object
``main()`` builds). These module-level values are the driver's live operating
values — several are read directly by the hot-path modules (e.g.
``FEE_PERCENTILES`` by :mod:`~degenbot.runner.consume`, ``MIN_PROFIT_NET`` by
:mod:`~degenbot.runner.dispatch`) and are settable via env vars / CLI.
"""

from __future__ import annotations

import os

from eth_typing import ChainId

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import WRAPPED_NATIVE_TOKENS

WETH_ADDRESS = WRAPPED_NATIVE_TOKENS[ChainId.ETH]
MULTICALL3_ADDRESS = "0xcA11bde05977b3631167028862bE2a173976CA11"

# Verified standard ERC-20 intermediates for Ethereum mainnet.
# Every token here is confirmed to have NO transfer fees and NO rebase.
ETH_MAINNET_ALLOWED_TOKENS: set[str] = {
    "0x163f8C2467924be0ae7B5347228CABF260318753",  # WLD
    "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",  # PyUSD
    "0xB8c77482e45F1F44dE1745F52C74426C631bDD52",  # BNB
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",  # WETH
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    "0xdAC17F958D2ee523a2206206994597C13D831ec7",  # USDT
    "0x6B175474E89094C44Da98b954EedeAC495271d0F",  # DAI
    "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",  # WBTC
    "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",  # UNI
    "0x514910771AF9Ca656af840dff83E8264EcF986CA",  # LINK
    "0x6B3595068778DD592e39A122f4f5a5cF09C90fE2",  # SUSHI
    "0xD533a949740bb3306d119CC777fa900bA034cd52",  # CRV
    "0xc00e94Cb662C3520282E6f5717214004A7f26888",  # COMP
    "0x0bc529c00C6401aEF6D220BE8C6Ea1667F6Ad93e",  # YFI
    "0x7D1AfA7B718fb893dB30A3aBc0Cfc608AaCfeBB0",  # MATIC/POL
}

# Only build paths where intermediate hops use these tokens. Set to None to
# allow all tokens. Pools connecting a non-whitelisted token are excluded,
# eliminating tax/fee-on-transfer tokens that waste sim gas and always revert.
ALLOWED_INTERMEDIATE_TOKENS: set[str] | None = ETH_MAINNET_ALLOWED_TOKENS

# Dispatch tunables read directly by the hot-path modules.
MIN_PROFIT_NET = 1
MIN_PROFIT_MARGIN_BPS = int(os.environ.get("DEGENBOT_MIN_PROFIT_MARGIN_BPS", "0"))
FEE_PERCENTILES = (10, 50)

# Build/registration tunables.
REG_QUEUE_BOUND = int(os.environ.get("DEGENBOT_REG_QUEUE_BOUND", "64"))
REG_WORKERS = int(os.environ.get("DEGENBOT_REG_WORKERS", "4"))

# Path permutation filter (set by main() from the --permutation CLI flag).
PATH_PERMUTATION_FILTER: set[str] | None = None  # e.g. {"V3-V4-V3"}

# V3 factories (Ethereum mainnet).
UNISWAP_V3_MAINNET_FACTORY = "0x33128a8fC17869897dcE68Ed026d694621f6FDfD"
SUSHISWAP_V3_MAINNET_FACTORY = "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
PANCAKESWAP_V3_MAINNET_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"

# V4 PoolManager (Ethereum mainnet).
UNISWAP_V4_POOL_MANAGER_ADDRESS = get_checksum_address("0x000000000004444c5dc75cB358380D2e3De08A90")

# Executor code injection via the in-process revm sim. When active, the
# cmd_executor runtime bytecode is injected at a fresh address through the
# SimulateContext's inject_code / executor_runtime_bytecode fields, which the
# engine's `apply_simulation_overrides` writes into the per-block CacheDB —
# testing the V2/V3/V4-capable executor WITHOUT deploying it on mainnet.
INJECT_EXECUTOR_CODE = os.environ.get("INJECT_EXECUTOR_CODE", "1") == "1"
INJECTED_EXECUTOR_ADDRESS = get_checksum_address(
    os.environ.get(
        "INJECTED_EXECUTOR_ADDRESS",
        "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1",
    ),
)

# Cap on per-batch `[sim-fail]` lines emitted by the renderer. A thin-margin
# revert storm can otherwise flood the log during a stalled head.
_SIM_FAIL_RENDER_CAP = 25
