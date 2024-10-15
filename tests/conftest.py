import logging

import dotenv
import pytest
import web3

import degenbot
import degenbot.config
import degenbot.logging
import degenbot.managers
import degenbot.managers.erc20_token_manager
import degenbot.registry
import degenbot.registry.all_pools
import degenbot.registry.all_tokens
import degenbot.types
import degenbot.uniswap.managers
from degenbot.anvil_fork import AnvilFork

env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)

ARBITRUM_ARCHIVE_NODE_HTTP_URI = f"https://rpc.ankr.com/arbitrum/{env_values['ANKR_API_KEY']}"
ETHEREUM_ARCHIVE_NODE_HTTP_URI = "http://localhost:8544"
BASE_FULL_NODE_HTTP_URI = "http://localhost:8543"


# Set up a web3 connection to a Base full node
@pytest.fixture(scope="session")
def base_full_node_web3() -> web3.Web3:
    w3 = web3.Web3(web3.HTTPProvider(BASE_FULL_NODE_HTTP_URI))
    return w3


# Set up a web3 connection to an Arbitrum archive node
@pytest.fixture(scope="session")
def arbitrum_archive_node_web3() -> web3.Web3:
    w3 = web3.Web3(web3.HTTPProvider(ARBITRUM_ARCHIVE_NODE_HTTP_URI))
    return w3


# Set up a web3 connection to an Ethereum archive node
@pytest.fixture(scope="session")
def ethereum_archive_node_web3() -> web3.Web3:
    w3 = web3.Web3(web3.HTTPProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI))
    return w3


@pytest.fixture(autouse=True)
def initialize_and_reset_after_each_test():
    """
    After each test, clear and reset global values & singletons to a fresh state
    """
    yield  # the fixture will pause here until the test completes
    degenbot.config.connection_manager.connections.clear()
    degenbot.config.connection_manager._default_chain_id = None
    degenbot.registry.all_pools.pool_registry._all_pools.clear()
    degenbot.registry.all_tokens.token_registry._all_tokens.clear()
    degenbot.managers.erc20_token_manager.Erc20TokenManager._state.clear()
    degenbot.types.AbstractPoolManager.instances.clear()


@pytest.fixture(autouse=True)
def set_degenbot_logging():
    degenbot.logging.logger.setLevel(logging.DEBUG)


@pytest.fixture()
def fork_base() -> AnvilFork:
    fork = AnvilFork(fork_url=BASE_FULL_NODE_HTTP_URI)
    return fork


@pytest.fixture()
def fork_mainnet() -> AnvilFork:
    fork = AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI)
    return fork


@pytest.fixture()
def fork_arbitrum() -> AnvilFork:
    fork = AnvilFork(fork_url=ARBITRUM_ARCHIVE_NODE_HTTP_URI)
    return fork
