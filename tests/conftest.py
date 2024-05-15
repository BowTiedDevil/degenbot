import logging
from typing import Any

import degenbot
import degenbot.config
import degenbot.logging
import degenbot.manager
import degenbot.registry
import degenbot.uniswap.managers
import dotenv
import pytest
import web3
from degenbot.fork.anvil_fork import AnvilFork
from degenbot.uniswap.v3_liquidity_pool import V3LiquidityPool

env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)


@pytest.fixture(scope="session")
def load_env() -> dict[str, Any]:
    env_file = dotenv.find_dotenv("tests.env")
    return dotenv.dotenv_values(env_file)


ARBITRUM_ARCHIVE_NODE_HTTP_URI = f"https://rpc.ankr.com/arbitrum/{env_values['ANKR_API_KEY']}"
# ARBITRUM_FULL_NODE_HTTP_URI = "http://localhost:8547"
ARBITRUM_FULL_NODE_HTTP_URI = f"https://rpc.ankr.com/arbitrum/{env_values['ANKR_API_KEY']}"

ETHEREUM_ARCHIVE_NODE_HTTP_URI = "http://localhost:8543"
# ETHEREUM_ARCHIVE_NODE_HTTP_URI = f"https://rpc.ankr.com/eth/{env_values['ANKR_API_KEY']}"
ETHEREUM_FULL_NODE_HTTP_URI = "http://localhost:8545"


# Set up a web3 connection to an Arbitrum full node
@pytest.fixture(scope="session")
def arbitrum_full_node_web3() -> web3.Web3:
    w3 = web3.Web3(web3.HTTPProvider(ARBITRUM_FULL_NODE_HTTP_URI))
    return w3


# Set up a web3 connection to an Ethereum archive node
@pytest.fixture(scope="session")
def ethereum_archive_node_web3() -> web3.Web3:
    w3 = web3.Web3(web3.HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI))
    return w3


# Set up a web3 connection to an Ethereum full node
@pytest.fixture(scope="session")
def ethereum_full_node_web3() -> web3.Web3:
    w3 = web3.Web3(web3.HTTPProvider(ETHEREUM_FULL_NODE_HTTP_URI))
    return w3


# After each test, clear shared state dictionaries
@pytest.fixture(autouse=True)
def initialize_and_reset_after_each_test(ethereum_full_node_web3):
    # degenbot.config.set_web3(ethereum_full_node_web3)
    yield
    degenbot.registry.all_pools._all_pools.clear()
    degenbot.registry.all_tokens._all_tokens.clear()
    degenbot.manager.token_manager.Erc20TokenHelperManager._state.clear()
    degenbot.uniswap.managers.UniswapLiquidityPoolManager._state.clear()
    V3LiquidityPool._lens_contracts.clear()


@pytest.fixture(autouse=True)
def set_degenbot_logging():
    degenbot.logging.logger.setLevel(logging.DEBUG)


@pytest.fixture()
def fork_mainnet_archive() -> AnvilFork:
    fork = AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI)
    return fork


@pytest.fixture()
def fork_mainnet() -> AnvilFork:
    fork = AnvilFork(fork_url=ETHEREUM_FULL_NODE_HTTP_URI)
    return fork


@pytest.fixture()
def fork_arbitrum() -> AnvilFork:
    fork = AnvilFork(fork_url=ARBITRUM_FULL_NODE_HTTP_URI)
    return fork


@pytest.fixture()
def fork_arbitrum_archive() -> AnvilFork:
    fork = AnvilFork(fork_url=ARBITRUM_ARCHIVE_NODE_HTTP_URI)
    return fork
