# pragma: no cover

import logging

import degenbot
import degenbot.registry
import degenbot.uniswap.managers
import dotenv
import pytest
import web3

env_file = dotenv.find_dotenv("tests.env")
env_values = dotenv.dotenv_values(env_file)


@pytest.fixture(scope="session")
def load_env() -> dict:
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


# Before every test, reset the degenbot web3 object to use a default node
@pytest.fixture(scope="function", autouse=True)
def initialize_degenbot_with_default_node(ethereum_full_node_web3):
    degenbot.set_web3(ethereum_full_node_web3)


@pytest.fixture(scope="function", autouse=True)
def set_degenbot_logging():
    degenbot.logger.setLevel(logging.DEBUG)


@pytest.fixture(scope="function", autouse=True)
def clear_degenbot_state() -> None:
    # Clear shared state dictionaries prior to each new test (activated on every test by autouse=True).
    # These dictionaries store module-level state, which will corrupt sequential tests if not reset
    degenbot.registry.all_pools._all_pools.clear()
    degenbot.registry.all_tokens._all_tokens.clear()
    degenbot.Erc20TokenHelperManager._state.clear()
    degenbot.uniswap.managers.UniswapLiquidityPoolManager._state.clear()
    degenbot.uniswap.V3LiquidityPool._lens_contracts.clear()


@pytest.fixture(scope="function")
def fork_mainnet_archive() -> degenbot.AnvilFork:
    fork = degenbot.AnvilFork(fork_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI)
    return fork


@pytest.fixture(scope="function")
def fork_mainnet() -> degenbot.AnvilFork:
    fork = degenbot.AnvilFork(fork_url=ETHEREUM_FULL_NODE_HTTP_URI)
    return fork


@pytest.fixture(scope="function")
def fork_arbitrum() -> degenbot.AnvilFork:
    fork = degenbot.AnvilFork(fork_url=ARBITRUM_FULL_NODE_HTTP_URI)
    return fork


@pytest.fixture(scope="function")
def fork_arbitrum_archive() -> degenbot.AnvilFork:
    fork = degenbot.AnvilFork(fork_url=ARBITRUM_ARCHIVE_NODE_HTTP_URI)
    return fork
